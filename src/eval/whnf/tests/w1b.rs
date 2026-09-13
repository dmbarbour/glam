use std::sync::{Arc, Mutex, Weak};

use crate::core::{CoreValueFactory, EvaluationFailure, Value};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{
    CompletionSubscriptions, EvalContext, EvaluationPollContext, EvaluationTaskId,
    EvaluationValueAccess, EvaluationWaitToken, EvaluationWorkCoordinator,
};
use crate::number::Number;
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

use super::*;

fn isolated_values() -> CoreValueFactory {
    CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new())
}

fn instruction(index: usize) -> Value {
    Value::Number(Number::from_usize(index))
}

fn instruction_index(value: &Value) -> usize {
    let Value::Number(number) = value else {
        panic!("synthetic WHNF focus must be an instruction number")
    };
    number
        .to_u64_if_integer()
        .and_then(|index| usize::try_from(index).ok())
        .expect("synthetic WHNF instruction must fit usize")
}

fn synthetic_wait(context: &EvalContext) -> CoreWaitToken {
    let values = context.values();
    let wait = values
        .ids()
        .evaluation_wait()
        .expect("synthetic wait identity should allocate");
    let producer = EvaluationTaskId::from_nonzero(
        values
            .ids()
            .evaluation_task()
            .expect("synthetic producer identity should allocate"),
    );
    let coordinator = Arc::new(Mutex::new(Weak::<EvaluationWorkCoordinator>::new()));
    let completion = CompletionSubscriptions::for_wait(values.runtime_id(), wait, coordinator);
    CoreWaitToken(EvaluationWaitToken::new(
        wait,
        values,
        context.session_id(),
        producer,
        completion,
    ))
}

#[test]
fn immediate_completion_returns_whnf_in_one_step() {
    let context = EvalContext::isolated(isolated_values());
    let poll = EvaluationPollContext::for_context(&context);
    poll.with_value_access(&context, |access| {
        let expected = Value::binary_from_text("immediate");
        let work = RegionalWhnfWork {
            focus: access.values().duplicate_value(&expected),
            frames: Vec::new(),
            followed: BTreeSet::new(),
        };
        let mut budget = WhnfStepBudget::new(1);
        let outcome = drive_regional(&access, work, &mut budget, |access, work| {
            RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus))
        });
        let RegionalWhnfDrive::Ready(actual) = outcome else {
            panic!("immediate work must complete")
        };
        assert_eq!(actual, expected);
        assert_eq!(budget.remaining(), 0);
    });
}

#[test]
fn tail_delegation_is_iterative_and_does_not_push_or_root() {
    const DELEGATIONS: usize = 100_000;

    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let poll = EvaluationPollContext::for_context(&context);
    let roots_before = values.managed_root_registrations_for_test();
    poll.with_value_access(&context, |access| {
        let expected = Value::binary_from_text("tail");
        let frames = Vec::with_capacity(8);
        let frame_capacity = frames.capacity();
        let work = RegionalWhnfWork {
            focus: access.values().duplicate_value(&expected),
            frames,
            followed: BTreeSet::new(),
        };
        let mut transitions = 0;
        let mut budget = WhnfStepBudget::new(DELEGATIONS + 1);
        let outcome = drive_regional(&access, work, &mut budget, |access, work| {
            assert!(work.frames.is_empty());
            assert_eq!(work.frames.capacity(), frame_capacity);
            if transitions < DELEGATIONS {
                transitions += 1;
                RegionalWhnfStep::Delegate(access.values().duplicate_value(&work.focus))
            } else {
                RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus))
            }
        });
        let RegionalWhnfDrive::Ready(actual) = outcome else {
            panic!("bounded tail delegation must complete")
        };
        assert_eq!(actual, expected);
        assert_eq!(transitions, DELEGATIONS);
        assert_eq!(budget.remaining(), 0);
    });
    assert_eq!(
        values.managed_root_registrations_for_test(),
        roots_before,
        "tail delegation must not register a durable root"
    );
}

#[test]
fn budget_yield_retains_work_without_inventing_a_dependency() {
    let context = EvalContext::isolated(isolated_values());
    let poll = EvaluationPollContext::for_context(&context);
    poll.with_value_access(&context, |access| {
        let work = RegionalWhnfWork {
            focus: Value::binary_from_text("yield"),
            frames: Vec::new(),
            followed: BTreeSet::new(),
        };
        let mut transitions = 0;
        let mut budget = WhnfStepBudget::new(3);
        let outcome = drive_regional(&access, work, &mut budget, |access, work| {
            transitions += 1;
            RegionalWhnfStep::Delegate(access.values().duplicate_value(&work.focus))
        });
        let RegionalWhnfDrive::Yielded(work) = outcome else {
            panic!("budget exhaustion must yield without a dependency")
        };
        assert_eq!(transitions, 3);
        assert_eq!(budget.remaining(), 0);

        let mut resume_budget = WhnfStepBudget::new(1);
        let resumed = drive_regional(&access, work, &mut resume_budget, |access, work| {
            RegionalWhnfStep::Ready(access.values().duplicate_value(&work.focus))
        });
        assert!(matches!(resumed, RegionalWhnfDrive::Ready(_)));
    });
}

#[derive(Clone)]
enum SyntheticOperation {
    Context {
        event: &'static str,
        context: &'static str,
        child: usize,
    },
    DemandThen {
        event: &'static str,
        child: usize,
        continuation: usize,
    },
    Suspend {
        event: &'static str,
        dependency: WhnfDependency,
        resume: usize,
    },
    Return {
        event: &'static str,
        result: &'static str,
    },
    Fail {
        event: &'static str,
        message: &'static str,
    },
}

struct SyntheticAlgebra {
    operations: Vec<SyntheticOperation>,
    dependency_ready: bool,
    events: Vec<&'static str>,
}

impl SyntheticAlgebra {
    fn reduce(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        work: &mut RegionalWhnfWork,
    ) -> RegionalWhnfStep {
        let index = instruction_index(&work.focus);
        let operation = self
            .operations
            .get(index)
            .unwrap_or_else(|| panic!("missing synthetic WHNF instruction {index}"))
            .clone();
        match operation {
            SyntheticOperation::Context {
                event,
                context,
                child,
            } => {
                self.events.push(event);
                work.frames.push(RegionalWhnfFrame {
                    kind: WhnfFrameKind::DiagnosticContext,
                    cursor: usize::MAX,
                    retained: vec![Value::binary_from_text(context)],
                });
                RegionalWhnfStep::Delegate(instruction(child))
            }
            SyntheticOperation::DemandThen {
                event,
                child,
                continuation,
            } => {
                self.events.push(event);
                work.frames.push(RegionalWhnfFrame {
                    kind: WhnfFrameKind::DemandThenInspect,
                    cursor: continuation,
                    retained: Vec::new(),
                });
                RegionalWhnfStep::Delegate(instruction(child))
            }
            SyntheticOperation::Suspend {
                event,
                dependency,
                resume,
            } => {
                self.events.push(event);
                if self.dependency_ready {
                    RegionalWhnfStep::Delegate(instruction(resume))
                } else {
                    RegionalWhnfStep::Boundary(RegionalBoundaryRequest::Dependency(dependency))
                }
            }
            SyntheticOperation::Return { event, result } => {
                self.events.push(event);
                let Some(frame) = work.frames.pop() else {
                    return RegionalWhnfStep::Ready(Value::binary_from_text(result));
                };
                match frame.kind {
                    WhnfFrameKind::DemandThenInspect => {
                        assert!(frame.retained.is_empty());
                        self.events.push("post-demand");
                        RegionalWhnfStep::Delegate(instruction(frame.cursor))
                    }
                    WhnfFrameKind::DiagnosticContext => {
                        RegionalWhnfStep::Delegate(instruction(index))
                    }
                    _ => panic!("synthetic return encountered an unsupported frame"),
                }
            }
            SyntheticOperation::Fail { event, message } => {
                self.events.push(event);
                let mut failure = EvaluationFailure::message(message);
                while let Some(mut frame) = work.frames.pop() {
                    assert_eq!(frame.kind, WhnfFrameKind::DiagnosticContext);
                    assert_eq!(frame.retained.len(), 1);
                    let context = frame.retained.pop().expect("one context was retained");
                    failure = failure.with_context_in(access.values(), context);
                }
                RegionalWhnfStep::Failed(Arc::new(failure))
            }
        }
    }
}

#[test]
fn dependency_resumes_at_the_recorded_phase_without_replaying_completed_work() {
    let values = isolated_values();
    let context = EvalContext::isolated(values.clone());
    let wait = synthetic_wait(&context);
    let expected_wait = wait.clone();
    let poll = EvaluationPollContext::for_context(&context);

    poll.with_value_access(&context, |access| {
        let mut algebra = SyntheticAlgebra {
            operations: vec![
                SyntheticOperation::Context {
                    event: "outer-context",
                    context: "outer",
                    child: 1,
                },
                SyntheticOperation::DemandThen {
                    event: "completed-prefix",
                    child: 2,
                    continuation: 4,
                },
                SyntheticOperation::Suspend {
                    event: "dependency-check",
                    dependency: WhnfDependency::Wait(wait),
                    resume: 3,
                },
                SyntheticOperation::Return {
                    event: "child-ready",
                    result: "child",
                },
                SyntheticOperation::Context {
                    event: "after-suspension",
                    context: "after",
                    child: 5,
                },
                SyntheticOperation::Fail {
                    event: "failure",
                    message: "boom",
                },
            ],
            dependency_ready: false,
            events: Vec::new(),
        };
        let work = RegionalWhnfWork {
            focus: instruction(0),
            frames: Vec::new(),
            followed: BTreeSet::new(),
        };
        let mut budget = WhnfStepBudget::new(10);
        let first = drive_regional(&access, work, &mut budget, |access, work| {
            algebra.reduce(access, work)
        });
        let RegionalWhnfDrive::Boundary { work, request } = first else {
            panic!("the first poll must stop at its dependency")
        };
        assert_eq!(instruction_index(&work.focus), 2);
        assert_eq!(work.frames.len(), 2);
        assert_eq!(work.frames[0].kind, WhnfFrameKind::DiagnosticContext);
        assert_eq!(work.frames[1].kind, WhnfFrameKind::DemandThenInspect);
        assert_eq!(work.frames[1].cursor, 4);
        let RegionalBoundaryRequest::Dependency(WhnfDependency::Wait(observed)) = request else {
            panic!("the synthetic suspension must retain its wait dependency")
        };
        assert_eq!(observed, expected_wait);
        assert_eq!(
            algebra.events,
            ["outer-context", "completed-prefix", "dependency-check"]
        );

        algebra.dependency_ready = true;
        let mut resume_budget = WhnfStepBudget::new(10);
        let resumed = drive_regional(&access, work, &mut resume_budget, |access, work| {
            algebra.reduce(access, work)
        });
        let RegionalWhnfDrive::Failed(failure) = resumed else {
            panic!("the resumed continuation must reach its permanent failure")
        };
        assert_eq!(
            algebra.events,
            [
                "outer-context",
                "completed-prefix",
                "dependency-check",
                "dependency-check",
                "child-ready",
                "post-demand",
                "after-suspension",
                "failure",
            ]
        );
        assert_eq!(
            failure.contexts(),
            [
                Value::binary_from_text("outer"),
                Value::binary_from_text("after"),
            ]
        );
    });
}

//! Durable owners for saturated builtin computations.
//!
//! A builtin source roots its operands once, then delegates each semantic
//! demand to the ordinary WHNF owner. Family-specific immediate operations run
//! only after those demands complete and never retain raw values across polls.

use std::sync::Arc;

use crate::core::{Builtin, EvaluatedValue, EvaluationFailure, EvaluationHalt, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, WorkDependency,
    poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::comparison_machine::{ComparisonBuiltinMachine, ComparisonBuiltinPoll};
use super::dict_machine::DictBuiltinMachine;
use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::strategy_machine::{StrategyDemandMachine, StrategyDemandPoll};
use super::value::number_from_evaluated;
use super::whnf::WhnfComputation;

pub(crate) enum BuiltinTaskPoll {
    Ready(RuntimeValueRoot),
    ScheduleSpark(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

pub(crate) enum BuiltinTaskMachine {
    Assertion(AssertionBuiltinMachine),
    Conditional(ConditionalBuiltinMachine),
    Comparison(ComparisonBuiltinMachine),
    Dictionary(DictBuiltinMachine),
    Numeric(NumericBuiltinMachine),
    Provenance(ProvenanceBuiltinMachine),
    Strategy(StrategyBuiltinMachine),
}

impl BuiltinTaskMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::AssertUnit
                | Builtin::IfResult
                | Builtin::MatchResult
                | Builtin::Greater
                | Builtin::GreaterEqual
                | Builtin::Equal
                | Builtin::NotEqual
                | Builtin::LessEqual
                | Builtin::Less
                | Builtin::Add
                | Builtin::Subtract
                | Builtin::Multiply
                | Builtin::Divide
                | Builtin::Floor
                | Builtin::Mod
                | Builtin::InspectOrigin
                | Builtin::Seq
                | Builtin::Spark
                | Builtin::DictSingleton
                | Builtin::DictUnion
                | Builtin::DictUpdate
                | Builtin::MergeDuplicate
        )
    }

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        assert!(Self::supports(builtin));
        assert_eq!(
            arguments.len(),
            builtin.arity(),
            "a builtin source must contain one saturated call"
        );
        match builtin {
            Builtin::AssertUnit => Self::Assertion(AssertionBuiltinMachine::new(arguments)),
            Builtin::IfResult | Builtin::MatchResult => {
                Self::Conditional(ConditionalBuiltinMachine::new(builtin, arguments))
            }
            Builtin::Greater
            | Builtin::GreaterEqual
            | Builtin::Equal
            | Builtin::NotEqual
            | Builtin::LessEqual
            | Builtin::Less => Self::Comparison(ComparisonBuiltinMachine::new(builtin, arguments)),
            Builtin::InspectOrigin => Self::Provenance(ProvenanceBuiltinMachine::new(arguments)),
            Builtin::DictSingleton
            | Builtin::DictUnion
            | Builtin::DictUpdate
            | Builtin::MergeDuplicate => {
                Self::Dictionary(DictBuiltinMachine::new(builtin, arguments))
            }
            Builtin::Seq | Builtin::Spark => {
                Self::Strategy(StrategyBuiltinMachine::new(builtin, arguments))
            }
            _ => Self::Numeric(NumericBuiltinMachine::new(builtin, arguments)),
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        match self {
            Self::Assertion(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::Conditional(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::Comparison(machine) => {
                match machine.poll(poll_context, context, durable_context, step_budget) {
                    ComparisonBuiltinPoll::Ready(value) => BuiltinTaskPoll::Ready(value),
                    ComparisonBuiltinPoll::Pending(dependency) => {
                        BuiltinTaskPoll::Pending(dependency)
                    }
                    ComparisonBuiltinPoll::Yielded => BuiltinTaskPoll::Yielded,
                    ComparisonBuiltinPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
                }
            }
            Self::Dictionary(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::Numeric(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::Provenance(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::Strategy(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
        }
    }
}

enum StrategyPhase {
    First,
    SparkRequested,
}

pub(crate) struct StrategyBuiltinMachine {
    builtin: Builtin,
    demand: StrategyDemandMachine,
    target: RuntimeValueRoot,
    phase: StrategyPhase,
}

impl StrategyBuiltinMachine {
    fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let [first, target]: [RuntimeValueRoot; 2] = arguments
            .try_into()
            .expect("a strategy source retains two operands");
        Self {
            builtin,
            demand: StrategyDemandMachine::new(first),
            target,
            phase: StrategyPhase::First,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if self.builtin == Builtin::Spark {
            return match self.phase {
                StrategyPhase::First => {
                    let useful = self.demand.is_useful_spark(context);
                    if useful {
                        self.phase = StrategyPhase::SparkRequested;
                        BuiltinTaskPoll::ScheduleSpark(self.demand.source().clone())
                    } else {
                        BuiltinTaskPoll::Ready(self.target.clone())
                    }
                }
                StrategyPhase::SparkRequested => BuiltinTaskPoll::Ready(self.target.clone()),
            };
        }

        match self
            .demand
            .poll(poll_context, context, durable_context, step_budget)
        {
            StrategyDemandPoll::Ready => BuiltinTaskPoll::Ready(self.target.clone()),
            StrategyDemandPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
            StrategyDemandPoll::Yielded => BuiltinTaskPoll::Yielded,
            StrategyDemandPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
        }
    }
}

pub(crate) struct ProvenanceBuiltinMachine {
    demand: WhnfComputation,
}

impl ProvenanceBuiltinMachine {
    fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        let [origin]: [RuntimeValueRoot; 1] = arguments
            .try_into()
            .expect("origin inspection retains one operand");
        Self {
            demand: WhnfComputation::from_root(origin),
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        let origin = match poll_demand(&mut self.demand, poll_context, durable_context, step_budget)
        {
            DemandPoll::Ready(value) => value,
            DemandPoll::Failed(failure) => {
                let failure = context.with_value_access(|access| {
                    EvaluationHalt::failure(failure.into_failure()).with_context(
                        access.values(),
                        super::value::evaluation_context_frame_in(
                            access.values(),
                            "compilation_origin",
                        ),
                    )
                });
                return BuiltinTaskPoll::Failed(
                    context.root_failure(failure.into_permanent_failure()),
                );
            }
            other => return other.into_builtin_poll(),
        };
        let origin = EvaluatedValue::try_from(context.project_root(&origin))
            .expect("origin demand must reach WHNF")
            .into_value();
        let Value::Opaque(origin) = origin else {
            return permanent_failure(
                context,
                "origin inspection requires an opaque compilation origin",
            );
        };
        match crate::diagnostic::inspect_compilation_origin(durable_context.values(), &origin) {
            Some(origin) => BuiltinTaskPoll::Ready(context.root_value(origin)),
            None => permanent_failure(
                context,
                "origin inspection requires an opaque compilation origin",
            ),
        }
    }
}

enum AssertionPhase {
    Value,
    DiagnosticContext { received: &'static str },
}

pub(crate) struct AssertionBuiltinMachine {
    arguments: Vec<RuntimeValueRoot>,
    phase: AssertionPhase,
    demand: Option<WhnfComputation>,
}

impl AssertionBuiltinMachine {
    fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        Self {
            arguments,
            phase: AssertionPhase::Value,
            demand: None,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        let argument = match self.phase {
            AssertionPhase::Value => &self.arguments[1],
            AssertionPhase::DiagnosticContext { .. } => &self.arguments[0],
        };
        let demand = self
            .demand
            .get_or_insert_with(|| WhnfComputation::from_root(argument.clone()));
        let value = match poll_demand(demand, poll_context, durable_context, step_budget) {
            DemandPoll::Ready(value) => value,
            other => return other.into_builtin_poll(),
        };
        self.demand = None;
        let value = EvaluatedValue::try_from(context.project_root(&value))
            .expect("assertion operand demand must reach WHNF")
            .into_value();

        match self.phase {
            AssertionPhase::Value => {
                let is_unit = context.with_value_access(|access| {
                    access
                        .values()
                        .same_representation(&value, &durable_context.values().unit())
                });
                if is_unit {
                    return BuiltinTaskPoll::Ready(self.arguments[2].clone());
                }
                self.phase = AssertionPhase::DiagnosticContext {
                    received: value.diagnostic_kind_name(),
                };
                BuiltinTaskPoll::Yielded
            }
            AssertionPhase::DiagnosticContext { received } => {
                let Value::Binary(diagnostic_context) = value else {
                    return permanent_failure(
                        context,
                        "unit assertion diagnostic context must be text",
                    );
                };
                permanent_failure(
                    context,
                    format!(
                        "{}: unit expected, received {received}",
                        String::from_utf8_lossy(&diagnostic_context)
                    ),
                )
            }
        }
    }
}

pub(crate) struct ConditionalBuiltinMachine {
    builtin: Builtin,
    results: RuntimeValueRoot,
    demand: Option<WhnfComputation>,
    front: Option<ListFrontMachine>,
}

impl ConditionalBuiltinMachine {
    fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let [results]: [RuntimeValueRoot; 1] = arguments
            .try_into()
            .expect("a conditional source retains one result list");
        Self {
            builtin,
            results,
            demand: None,
            front: None,
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        if self.front.is_none() {
            let demand = self
                .demand
                .get_or_insert_with(|| WhnfComputation::from_root(self.results.clone()));
            let results = match poll_demand(demand, poll_context, durable_context, step_budget) {
                DemandPoll::Ready(value) => value,
                other => return other.into_builtin_poll(),
            };
            let value = EvaluatedValue::try_from(context.project_root(&results))
                .expect("conditional result-list demand must reach WHNF")
                .into_value();
            if !matches!(value, Value::List(_)) {
                return permanent_failure(
                    context,
                    format!(
                        "{} search did not produce a result list",
                        conditional_name(self.builtin)
                    ),
                );
            }
            self.demand = None;
            self.front = Some(ListFrontMachine::unowned(results));
        }

        match self
            .front
            .as_mut()
            .expect("conditional list-front owner must be installed")
            .poll(poll_context, context, durable_context, step_budget)
        {
            ListFrontPoll::Ready(Some((result, _tail))) => BuiltinTaskPoll::Ready(result),
            ListFrontPoll::Ready(None) => permanent_failure(
                context,
                match self.builtin {
                    Builtin::IfResult => "if search exhausted despite its required `else` branch",
                    Builtin::MatchResult => {
                        "match search exhausted despite its compiler-provided fallback"
                    }
                    _ => unreachable!(),
                },
            ),
            ListFrontPoll::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
            ListFrontPoll::Yielded => BuiltinTaskPoll::Yielded,
            ListFrontPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
        }
    }
}

enum DemandPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl DemandPoll {
    fn into_builtin_poll(self) -> BuiltinTaskPoll {
        match self {
            Self::Ready(_) => unreachable!("ready demand must be consumed by its family owner"),
            Self::Pending(dependency) => BuiltinTaskPoll::Pending(dependency),
            Self::Yielded => BuiltinTaskPoll::Yielded,
            Self::Failed(failure) => BuiltinTaskPoll::Failed(failure),
        }
    }
}

fn poll_demand(
    demand: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    durable_context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> DemandPoll {
    match poll_whnf_computation(demand, poll_context, durable_context, step_budget) {
        WhnfOwnerPoll::Ready(value) => DemandPoll::Ready(value),
        WhnfOwnerPoll::Pending(dependency) => DemandPoll::Pending(dependency),
        WhnfOwnerPoll::Yielded => DemandPoll::Yielded,
        WhnfOwnerPoll::Failed(failure) => DemandPoll::Failed(failure),
        WhnfOwnerPoll::External(boundary) => {
            unreachable!("builtin operand demand produced an external {boundary:?} boundary")
        }
    }
}

fn permanent_failure(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<String>,
) -> BuiltinTaskPoll {
    BuiltinTaskPoll::Failed(
        context.root_failure(Arc::new(EvaluationFailure::message(message.into()))),
    )
}

fn conditional_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::IfResult => "if",
        Builtin::MatchResult => "match",
        _ => unreachable!("conditional name requested for another builtin"),
    }
}

pub(crate) struct NumericBuiltinMachine {
    builtin: Builtin,
    arguments: Vec<RuntimeValueRoot>,
    next: usize,
    demand: Option<WhnfComputation>,
    numbers: Vec<Number>,
}

impl NumericBuiltinMachine {
    fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        let argument_count = arguments.len();
        Self {
            builtin,
            arguments,
            next: 0,
            demand: None,
            numbers: Vec::with_capacity(argument_count),
        }
    }

    fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        while self.next < self.arguments.len() {
            let demand = self.demand.get_or_insert_with(|| {
                WhnfComputation::from_root(self.arguments[self.next].clone())
            });
            let value =
                match poll_whnf_computation(demand, poll_context, durable_context, step_budget) {
                    WhnfOwnerPoll::Ready(value) => value,
                    WhnfOwnerPoll::Pending(dependency) => {
                        return BuiltinTaskPoll::Pending(dependency);
                    }
                    WhnfOwnerPoll::Yielded => return BuiltinTaskPoll::Yielded,
                    WhnfOwnerPoll::Failed(failure) => return BuiltinTaskPoll::Failed(failure),
                    WhnfOwnerPoll::External(boundary) => {
                        unreachable!("numeric demand produced an external {boundary:?} boundary")
                    }
                };
            let value = EvaluatedValue::try_from(context.project_root(&value))
                .expect("numeric operand demand must reach WHNF");
            let number = match number_from_evaluated(value, numeric_name(self.builtin)) {
                Ok(number) => number,
                Err(error) => {
                    return BuiltinTaskPoll::Failed(
                        context.root_failure(error.into_permanent_failure()),
                    );
                }
            };
            self.numbers.push(number);
            self.next += 1;
            self.demand = None;
        }

        match numeric_result(self.builtin, &self.numbers) {
            Ok(result) => BuiltinTaskPoll::Ready(context.root_value(Value::Number(result))),
            Err(message) => BuiltinTaskPoll::Failed(
                context.root_failure(Arc::new(EvaluationFailure::message(message))),
            ),
        }
    }
}

fn numeric_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Add => "add",
        Builtin::Subtract => "subtract",
        Builtin::Multiply => "multiply",
        Builtin::Divide => "divide",
        Builtin::Floor => "floor",
        Builtin::Mod => "mod",
        _ => unreachable!("numeric name requested for another builtin"),
    }
}

fn numeric_result(builtin: Builtin, arguments: &[Number]) -> Result<Number, &'static str> {
    match builtin {
        Builtin::Add => Ok(arguments[0].add(&arguments[1])),
        Builtin::Subtract => Ok(arguments[0].sub(&arguments[1])),
        Builtin::Multiply => Ok(arguments[0].mul(&arguments[1])),
        Builtin::Divide => arguments[0]
            .checked_div(&arguments[1])
            .ok_or("divide builtin cannot divide by zero"),
        Builtin::Floor => Ok(arguments[0].floor()),
        Builtin::Mod => arguments[0]
            .checked_mod(&arguments[1])
            .ok_or("mod builtin cannot divide by zero"),
        _ => unreachable!("numeric result requested for another builtin"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::{CoreValueFactory, ListThunk, PromisedValue};
    use crate::evaluation::EvalContext;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    #[test]
    fn numeric_machine_resumes_at_the_second_operand_without_replaying_the_first() {
        let context = context();
        let first_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&first_demands);
        let first = Value::semantic_thunk(context.values(), "first numeric operand", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Number(1.into()))
        });
        let second = PromisedValue::new(context.values(), "second numeric operand");
        let sum = Value::builtin_call(
            context.values(),
            Builtin::Add,
            vec![first, Value::Promised(second.clone())],
        );

        let blocked = crate::eval::eval_value(&context, &sum)
            .expect_err("the second operand must remain an exact suspension boundary");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);

        crate::core::set_test_promise(context.values(), &second, Value::Number(2.into()))
            .expect("the second operand should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &sum).expect("numeric work must resume"),
            Value::Number(3.into())
        );
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn numeric_machine_retains_numeric_failure_contracts() {
        let context = context();
        let divide = Value::builtin_call(
            context.values(),
            Builtin::Divide,
            vec![Value::Number(1.into()), Value::Number(0.into())],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &divide)
                .expect_err("division by zero must remain a permanent failure")
                .to_string(),
            "divide builtin cannot divide by zero"
        );

        let invalid = Value::builtin_call(
            context.values(),
            Builtin::Floor,
            vec![Value::binary_from_text("not a number")],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &invalid)
                .expect_err("numeric kind validation must remain a permanent failure")
                .to_string(),
            "floor builtin requires number values"
        );
    }

    #[test]
    fn assertion_machine_resumes_diagnostic_context_without_replaying_the_value() {
        let context = context();
        let value_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&value_demands);
        let value = Value::semantic_thunk(context.values(), "asserted value", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Number(7.into()))
        });
        let diagnostic_context =
            PromisedValue::new(context.values(), "unit assertion diagnostic context");
        let assertion = Value::builtin_call(
            context.values(),
            Builtin::AssertUnit,
            vec![
                Value::Promised(diagnostic_context.clone()),
                value,
                Value::Number(99.into()),
            ],
        );

        let blocked = crate::eval::eval_value(&context, &assertion)
            .expect_err("assertion failure should suspend on its diagnostic context");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        assert_eq!(value_demands.load(Ordering::Relaxed), 1);

        crate::core::set_test_promise(
            context.values(),
            &diagnostic_context,
            Value::binary_from_text("definition foo"),
        )
        .expect("the diagnostic context should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &assertion)
                .expect_err("the resumed assertion must report its failure")
                .to_string(),
            "definition foo: unit expected, received Number"
        );
        assert_eq!(value_demands.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn conditional_machine_selects_first_result_and_retains_exhaustion_diagnostics() {
        let context = context();
        for builtin in [Builtin::IfResult, Builtin::MatchResult] {
            let selection = Value::builtin_call(
                context.values(),
                builtin,
                vec![Value::List(crate::core::List::from_values(vec![
                    Value::Number(1.into()),
                    Value::Number(2.into()),
                ]))],
            );
            assert_eq!(
                crate::eval::eval_value(&context, &selection)
                    .expect("a non-empty search should select its first result"),
                Value::Number(1.into())
            );
        }

        let empty_if = Value::builtin_call(
            context.values(),
            Builtin::IfResult,
            vec![Value::List(crate::core::List::empty())],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &empty_if)
                .expect_err("an if search should never exhaust its else branch")
                .to_string(),
            "if search exhausted despite its required `else` branch"
        );

        let empty_match = Value::builtin_call(
            context.values(),
            Builtin::MatchResult,
            vec![Value::List(crate::core::List::empty())],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &empty_match)
                .expect_err("an empty match should be diagnosed")
                .to_string(),
            "match search exhausted despite its compiler-provided fallback"
        );
    }

    #[test]
    fn provenance_machine_suspends_then_preserves_its_demand_context() {
        let context = context();
        let origin = PromisedValue::new(context.values(), "origin operand");
        let inspection = Value::builtin_call(
            context.values(),
            Builtin::InspectOrigin,
            vec![Value::Promised(origin.clone())],
        );

        let blocked = crate::eval::eval_value(&context, &inspection)
            .expect_err("unassigned origin demand must remain resumable");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());

        let failed = Value::semantic_thunk(context.values(), "failed origin", |_| {
            Err(EvaluationHalt::new("origin production failed"))
        });
        crate::core::set_test_promise(context.values(), &origin, failed)
            .expect("the origin operand should accept its assignment");
        let failure = crate::eval::eval_value(&context, &inspection)
            .expect_err("origin demand failure must propagate")
            .into_permanent_failure();
        assert_eq!(failure.to_string(), "origin production failed");
        assert_eq!(
            failure.contexts(),
            [super::super::value::evaluation_context_frame(
                "compilation_origin"
            )]
        );
    }

    #[test]
    fn comparison_machine_resumes_second_operand_without_replaying_the_first() {
        let context = context();
        let first_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&first_demands);
        let first =
            Value::semantic_thunk(context.values(), "first comparison operand", move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(Value::Number(7.into()))
            });
        let second = PromisedValue::new(context.values(), "second comparison operand");
        let comparison = Value::builtin_call(
            context.values(),
            Builtin::Equal,
            vec![first, Value::Promised(second.clone())],
        );

        crate::eval::eval_value(&context, &comparison)
            .expect_err("the second comparison operand must suspend");
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
        crate::core::set_test_promise(context.values(), &second, Value::Number(7.into()))
            .expect("the second operand should accept its assignment");
        crate::eval::eval_value(&context, &comparison).expect("comparison must resume");
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn comparison_machine_resumes_a_lazy_list_tail_without_replaying_its_prefix() {
        let context = context();
        let prefix_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&prefix_demands);
        let prefix = Value::semantic_thunk(context.values(), "comparison list prefix", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Number(1.into()))
        });
        let tail = PromisedValue::new(context.values(), "comparison list tail");
        let left = Value::List(crate::core::List::concat(
            crate::core::List::from_values(vec![prefix]),
            crate::core::List::from_thunk(ListThunk::Promised(tail.clone())),
        ));
        let right = Value::List(crate::core::List::from_values(vec![
            Value::Number(1.into()),
            Value::Number(2.into()),
        ]));
        let comparison = Value::builtin_call(context.values(), Builtin::Equal, vec![left, right]);

        crate::eval::eval_value(&context, &comparison)
            .expect_err("the deferred list tail must suspend comparison");
        assert_eq!(prefix_demands.load(Ordering::Relaxed), 1);
        crate::core::set_test_promise(
            context.values(),
            &tail,
            Value::List(crate::core::List::from_values(vec![Value::Number(
                2.into(),
            )])),
        )
        .expect("the list tail should accept its assignment");
        crate::eval::eval_value(&context, &comparison).expect("list comparison must resume");
        assert_eq!(prefix_demands.load(Ordering::Relaxed), 1);
    }
}

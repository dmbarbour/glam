use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use glam::reflection::{
    EffectRequestSpec, IsolatedEffectSearch, IsolatedSearchPoll, IsolatedTaskHost, RequestContext,
    RequestResult, SpecializationRequestInput, SpecializationRequestPoll,
    SpecializationRequestWork, TaskHalt, TaskSpecialization,
};
use glam::{EvaluationRuntime, Value};

const ECHO_TAG: [&str; 4] = ["embedding_test", "v0", "request", "echo"];

#[derive(Default)]
struct TestRequestCounts {
    starts: AtomicUsize,
    demands: AtomicUsize,
    resumes: AtomicUsize,
}

#[derive(Clone)]
struct TestEffects {
    counts: Arc<TestRequestCounts>,
}

#[derive(Clone, Copy)]
enum TestRequest {
    Echo,
}

enum TestRequestWork {
    EchoStart(Option<Value>),
    EchoWaiting,
}

impl SpecializationRequestWork<TestEffects> for TestRequestWork {
    fn poll(
        &mut self,
        specialization: &TestEffects,
        input: Option<SpecializationRequestInput>,
        _context: &mut RequestContext<'_, TestEffects>,
    ) -> Result<SpecializationRequestPoll, TaskHalt> {
        match self {
            Self::EchoStart(value) => {
                assert!(input.is_none());
                let value = value.take().expect("echo input must be requested once");
                *self = Self::EchoWaiting;
                specialization.counts.demands.fetch_add(1, Ordering::SeqCst);
                Ok(SpecializationRequestPoll::Demand(value))
            }
            Self::EchoWaiting => {
                specialization.counts.resumes.fetch_add(1, Ordering::SeqCst);
                match input {
                    Some(SpecializationRequestInput::Value(value)) => {
                        Ok(SpecializationRequestPoll::Complete(RequestResult::Return(
                            value.into_value(),
                        )))
                    }
                    Some(SpecializationRequestInput::Failed(error)) => Err(error),
                    None => panic!("echo request resumed without its demand completion"),
                }
            }
        }
    }
}

fn echo_spec() -> EffectRequestSpec<TestRequest> {
    EffectRequestSpec::hidden(ECHO_TAG, 1, TestRequest::Echo)
}

impl TaskSpecialization for TestEffects {
    type Host = IsolatedTaskHost<()>;
    type Request = TestRequest;
    type RequestWork = TestRequestWork;
    type Snapshot = ();
    type Journal = ();

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        vec![echo_spec()]
    }

    fn start_request(&self, request: Self::Request, arguments: Vec<Value>) -> Self::RequestWork {
        match request {
            TestRequest::Echo => {
                self.counts.starts.fetch_add(1, Ordering::SeqCst);
                let [value]: [Value; 1] = arguments
                    .try_into()
                    .expect("effect decoding validates echo request arity");
                TestRequestWork::EchoStart(Some(value))
            }
        }
    }
}

#[test]
fn external_effect_specialization_uses_only_public_embedding_apis() {
    let runtime = EvaluationRuntime::new(0).unwrap();
    let assembler = glam::Assembler::builder()
        .evaluation_runtime(runtime.clone())
        .build()
        .unwrap();
    let values = assembler.values();
    let (promise, resolver) = assembler.promise("external request-work demand");
    let effect = echo_spec().effect(&values, [promise]).unwrap();
    let host = Arc::new(
        IsolatedTaskHost::new(&values, values.empty_dict(), ())
            .expect("isolated host should accept its runtime-local environment"),
    );
    let counts = Arc::new(TestRequestCounts::default());
    let specialization = TestEffects {
        counts: counts.clone(),
    };
    let mut search = IsolatedEffectSearch::new(&runtime, &effect, specialization, host).unwrap();

    for _ in 0..32 {
        match search.poll(64) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Blocked(blocked) => {
                assert!(blocked.waiting_on_dependency());
                assert_eq!(counts.starts.load(Ordering::SeqCst), 1);
                assert_eq!(counts.demands.load(Ordering::SeqCst), 1);
                assert_eq!(counts.resumes.load(Ordering::SeqCst), 0);
                resolver.resolve(values.integer(42)).unwrap();
                break;
            }
            IsolatedSearchPoll::Complete(_) => {
                panic!("unresolved request argument completed without blocking")
            }
            IsolatedSearchPoll::Failed(error) => panic!("external effect search failed: {error}"),
            IsolatedSearchPoll::Cancelled => panic!("external effect search was cancelled"),
        }
    }

    for _ in 0..32 {
        match search.poll(64) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Complete(branches) => {
                assert_eq!(branches.len(), 1);
                let result = branches[0]
                    .value()
                    .expect("echo branch should complete successfully");
                assert_eq!(
                    assembler
                        .evaluator()
                        .eval(result)
                        .unwrap()
                        .as_i64()
                        .unwrap(),
                    Some(42)
                );
                assert_eq!(counts.starts.load(Ordering::SeqCst), 1);
                assert_eq!(counts.demands.load(Ordering::SeqCst), 1);
                assert_eq!(counts.resumes.load(Ordering::SeqCst), 1);
                return;
            }
            IsolatedSearchPoll::Blocked(blocked) => {
                panic!("external effect search blocked: {:?}", blocked.error())
            }
            IsolatedSearchPoll::Failed(error) => panic!("external effect search failed: {error}"),
            IsolatedSearchPoll::Cancelled => panic!("external effect search was cancelled"),
        }
    }
    panic!("external request-work fixture did not complete within its bounded polls");
}

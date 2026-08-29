use std::sync::Arc;

use glam::reflection::{
    EffectRequestSpec, IsolatedEffectSearch, IsolatedSearchPoll, IsolatedTaskHost, RequestContext,
    RequestResult, TaskHalt, TaskSpecialization,
};
use glam::{EvaluationRuntime, Value};

const ECHO_TAG: [&str; 4] = ["embedding_test", "v0", "request", "echo"];

#[derive(Clone, Copy)]
struct TestEffects;

#[derive(Clone, Copy)]
enum TestRequest {
    Echo,
}

fn echo_spec() -> EffectRequestSpec<TestRequest> {
    EffectRequestSpec::hidden(ECHO_TAG, 1, TestRequest::Echo)
}

impl TaskSpecialization for TestEffects {
    type Host = IsolatedTaskHost<()>;
    type Request = TestRequest;
    type Snapshot = ();
    type Journal = ();

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        vec![echo_spec()]
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskHalt> {
        match request {
            TestRequest::Echo => {
                let [value]: [Value; 1] = arguments
                    .try_into()
                    .map_err(|_| TaskHalt::new("echo received the wrong arity"))?;
                Ok(RequestResult::Return(
                    context.evaluate(&value)?.into_value(),
                ))
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
    let effect = echo_spec().effect(&values, [values.integer(42)]).unwrap();
    let host = Arc::new(
        IsolatedTaskHost::new(&values, values.empty_dict(), ())
            .expect("isolated host should accept its runtime-local environment"),
    );
    let mut search = IsolatedEffectSearch::new(&runtime, &effect, TestEffects, host).unwrap();

    loop {
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
                break;
            }
            IsolatedSearchPoll::Blocked(blocked) => {
                panic!("external effect search blocked: {:?}", blocked.error())
            }
            IsolatedSearchPoll::Failed(error) => panic!("external effect search failed: {error}"),
            IsolatedSearchPoll::Cancelled => panic!("external effect search was cancelled"),
        }
    }
}

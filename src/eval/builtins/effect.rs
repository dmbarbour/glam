use super::super::*;

mod implementation;

use implementation::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    match builtin {
        Builtin::EffectMap => {
            let [function, items] = super::exact(arguments, "effect map")?;
            eval_effect_map_builtin(&function, &items)
        }
        Builtin::EffectMapRun => {
            let [function, items, results, api] = super::exact(arguments, "effect map run")?;
            eval_effect_map_run_builtin(context, &function, &items, &results, &api)
        }
        Builtin::EffectMapContinue => {
            let [function, items, results, result] =
                super::exact(arguments, "effect map continuation")?;
            eval_effect_map_continue_builtin(&function, &items, &results, &result)
        }
        _ => unreachable!("effect-map dispatcher received another builtin"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CoreValueFactory, Dict, PromisedValue};
    use crate::evaluation::EvalContext;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    #[test]
    fn effect_call_defers_api_lookup_and_application_to_the_access_owner() {
        let context = context();
        let name = Key::binary_from_text("add");
        let method = PromisedValue::new(context.values(), "deferred effect API method");
        let api = Value::Dict(Dict::new_sync().insert(name, Value::Promised(method.clone())));
        let arguments = Value::List(List::from_values(vec![
            Value::Number(1.into()),
            Value::Number(2.into()),
        ]));

        let operation = super::super::apply_builtin(
            &context,
            Builtin::EffectCall,
            vec![Value::binary_from_text("add"), arguments],
            api,
        )
        .expect("effect-call construction must not demand the API method");
        assert!(matches!(operation, Value::Lazy(_)));

        let blocked = crate::eval::eval_value(&context, &operation)
            .expect_err("demand must stop at the unresolved API method");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());

        crate::core::set_test_promise(context.values(), &method, Value::Builtin(Builtin::Add))
            .expect("the API method promise should accept its assignment");
        assert_eq!(
            crate::eval::eval_value(&context, &operation)
                .expect("the retained access and application must resume"),
            Value::Number(3.into())
        );
    }
}

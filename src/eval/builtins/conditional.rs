//! Selection policy for compiler-generated pure conditional searches.

use super::*;

pub(super) fn apply(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    let name = match builtin {
        Builtin::IfResult => "if",
        Builtin::MatchResult => "match",
        _ => unreachable!("conditional dispatcher received another builtin"),
    };
    let [results] = super::exact(arguments, name)?;
    let Value::List(results) = eval_value_in(context, &results)? else {
        return Err(EvaluationHalt::new(format!(
            "{name} search did not produce a result list"
        )));
    };

    match pop_list_front_in(context, &results)? {
        Some((result, _)) => Ok(result),
        None if builtin == Builtin::IfResult => Err(EvaluationHalt::new(
            "if search exhausted despite its required `else` branch",
        )),
        None => Err(EvaluationHalt::new(
            "match search exhausted despite its compiler-provided fallback",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    fn select(builtin: Builtin, values: Vec<Value>) -> Result<Value, EvaluationHalt> {
        let context = crate::eval::test_support::test_context();
        super::super::apply_builtin(
            &context,
            builtin,
            Vec::new(),
            Value::List(List::from_values(values)),
        )
    }

    #[test]
    fn selectors_return_the_first_search_result() {
        for builtin in [Builtin::IfResult, Builtin::MatchResult] {
            assert_eq!(
                select(
                    builtin,
                    vec![Value::Number(1.into()), Value::Number(2.into())]
                )
                .expect("a non-empty search should select its first result"),
                Value::Number(1.into())
            );
        }
    }

    #[test]
    fn selection_does_not_force_the_branch_result() {
        let forced = Arc::new(AtomicBool::new(false));
        let forced_by_thunk = forced.clone();
        let selected = Value::deferred(
            &crate::core::test_value_factory(),
            "selected conditional result",
            move |_| {
                forced_by_thunk.store(true, Ordering::Relaxed);
                Err(EvaluationHalt::new("selected result was forced"))
            },
        );

        let result = select(Builtin::IfResult, vec![selected])
            .expect("selection should not observe the chosen result");
        assert!(matches!(result, Value::Lazy(_)));
        assert!(!forced.load(Ordering::Relaxed));
    }

    #[test]
    fn empty_if_result_reports_the_broken_else_invariant() {
        let error = select(Builtin::IfResult, vec![])
            .expect_err("an if search should never exhaust its required else branch");
        assert_eq!(
            error.to_string(),
            "if search exhausted despite its required `else` branch"
        );
    }

    #[test]
    fn empty_match_result_reports_match_exhaustion() {
        let error =
            select(Builtin::MatchResult, vec![]).expect_err("an empty match should be diagnosed");
        assert_eq!(
            error.to_string(),
            "match search exhausted despite its compiler-provided fallback"
        );
    }
}

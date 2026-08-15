//! The cached closed Glam function behind the executable's default logger.
//!
//! Terminal observation is client policy rather than part of the built-in g
//! compiler. This module uses the g front end's private semantic IR only to
//! lower that policy once; callers receive an ordinary closed function value.

use std::sync::Arc;

use super::*;

struct CachedDiagnosticFormatter(Value);

pub(super) fn value(values: &CoreValueFactory) -> Value {
    cached(values).0.clone()
}

fn cached(values: &CoreValueFactory) -> Arc<CachedDiagnosticFormatter> {
    values.cached(|| CachedDiagnosticFormatter(build(values)))
}

fn build(values: &CoreValueFactory) -> Value {
    fn field(local: BindingId, path: &[&str]) -> ResolvedExpr<Value> {
        ResolvedExpr::Access {
            base: Box::new(ResolvedExpr::Local(local)),
            path: path
                .iter()
                .map(|name| ResolvedPathPart::Key(name_as_key(name)))
                .collect(),
        }
    }

    fn append(items: impl IntoIterator<Item = ResolvedExpr<Value>>) -> ResolvedExpr<Value> {
        items
            .into_iter()
            .reduce(|left, right| apply_builtin(Builtin::Append, [left, right]))
            .unwrap_or_else(|| ResolvedExpr::Embedded(Value::List(crate::core::List::empty())))
    }

    let mut locals = ResolverContext::default();
    let diagnostic = locals.push_internal_binding("<diagnostic>");
    let lines = locals.push_internal_binding("<diagnostic-lines>");
    let continuation_line = locals.push_internal_binding("<diagnostic-continuation-line>");
    let context_line = locals.push_internal_binding("<diagnostic-context-line>");

    let header = || field(diagnostic, &["viewer", "header"]);
    let indented_continuations = apply_builtin(
        Builtin::ListConcat,
        [apply_builtin(
            Builtin::Map,
            [
                ResolvedExpr::lambda(
                    vec![continuation_line],
                    append([
                        ResolvedExpr::Embedded(Value::binary_from_text("\n")),
                        field(diagnostic, &["viewer", "indent"]),
                        ResolvedExpr::Local(continuation_line),
                    ]),
                ),
                apply_builtin(Builtin::ListTail, [ResolvedExpr::Local(lines)]),
            ],
        )],
    );
    let context_lines = apply_builtin(
        Builtin::ListConcat,
        [apply_builtin(
            Builtin::Map,
            [
                ResolvedExpr::lambda(
                    vec![context_line],
                    append([
                        ResolvedExpr::Embedded(Value::binary_from_text("\n")),
                        ResolvedExpr::Local(context_line),
                    ]),
                ),
                field(diagnostic, &["viewer", "context_lines"]),
            ],
        )],
    );
    let formatted = append([
        header(),
        apply_builtin(Builtin::ListHead, [ResolvedExpr::Local(lines)]),
        indented_continuations,
        context_lines,
        ResolvedExpr::Embedded(Value::binary_from_text("\n")),
    ]);
    let binary = apply_builtin(
        Builtin::Anno,
        [
            ResolvedExpr::Embedded(Value::Atom(atom_from_str("binary"))),
            formatted,
        ],
    );
    let with_lines = ResolvedExpr::apply(
        ResolvedExpr::lambda(vec![lines], binary),
        [apply_builtin(
            Builtin::TextLines,
            [field(diagnostic, &["msg", "text"])],
        )],
    );
    evaluate_closed(values, ResolvedExpr::lambda(vec![diagnostic], with_lines))
}

fn evaluate_closed(values: &CoreValueFactory, expression: ResolvedExpr<Value>) -> Value {
    let value = lower_resolved_expr(values, expression);
    crate::eval::eval_value(
        &crate::evaluation::EvalContext::private_closed(values.clone()),
        &value,
    )
    .expect("default diagnostic formatter must be a closed function")
}

fn apply_builtin(
    builtin: Builtin,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(Value::Builtin(builtin)), arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatter_is_cached_after_exposing_its_function() {
        let values = crate::compiler::test_value_factory();
        let first = value(&values);
        let second = value(&values);
        assert!(matches!(first, Value::Function(_)));
        assert_eq!(first, second);
    }

    #[test]
    fn formatter_cache_is_owned_by_one_runtime() {
        let first = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        let second = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        assert!(!Arc::ptr_eq(&cached(&first), &cached(&second)));
    }
}

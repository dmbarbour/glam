//! The cached closed Glam function behind the executable's default logger.
//!
//! Terminal observation is client policy rather than part of the built-in g
//! compiler. This module uses the g front end's private semantic IR only to
//! lower that policy once; callers receive an ordinary closed function value.

use std::sync::LazyLock;

use super::*;

static FORMATTER: LazyLock<Value> = LazyLock::new(build);

pub(super) fn value() -> Value {
    (*FORMATTER).clone()
}

fn build() -> Value {
    fn severity_values(info: &str, warning: &str, error: &str) -> Value {
        Value::Dict(
            Dict::new_sync()
                .insert(
                    Key::from_value(&keys::INFO_VALUE)
                        .expect("canonical info severity must be keyable"),
                    Value::binary_from_text(info),
                )
                .insert(
                    Key::from_value(&keys::WARN_VALUE)
                        .expect("canonical warning severity must be keyable"),
                    Value::binary_from_text(warning),
                )
                .insert(
                    Key::from_value(&keys::ERROR_VALUE)
                        .expect("canonical error severity must be keyable"),
                    Value::binary_from_text(error),
                ),
        )
    }

    fn field(local: BindingId, path: &[&str]) -> ResolvedExpr<Value> {
        ResolvedExpr::Access {
            base: Box::new(ResolvedExpr::Local(local)),
            path: path
                .iter()
                .map(|name| ResolvedPathPart::Key(name_as_key(name)))
                .collect(),
        }
    }

    fn indexed(
        value: Value,
        indices: impl IntoIterator<Item = ResolvedExpr<Value>>,
    ) -> ResolvedExpr<Value> {
        ResolvedExpr::Access {
            base: Box::new(ResolvedExpr::Embedded(value)),
            path: indices
                .into_iter()
                .map(|index| ResolvedPathPart::Index(Box::new(index)))
                .collect(),
        }
    }

    fn append(items: impl IntoIterator<Item = ResolvedExpr<Value>>) -> ResolvedExpr<Value> {
        items
            .into_iter()
            .reduce(|left, right| apply_builtin(Builtin::Append, [left, right]))
            .unwrap_or_else(|| ResolvedExpr::Embedded(Value::List(crate::core::List::empty())))
    }

    let severity_labels = severity_values("info", "warning", "error");
    let plain_colors = severity_values("", "", "");
    let ansi_colors = severity_values("\x1b[36m", "\x1b[33m", "\x1b[31m");
    let color_prefixes = Value::Dict(
        Dict::new_sync()
            .insert(Key::binary_from_text("none"), plain_colors)
            .insert(Key::binary_from_text("ansi16"), ansi_colors.clone())
            .insert(Key::binary_from_text("ansi256"), ansi_colors.clone())
            .insert(Key::binary_from_text("truecolor"), ansi_colors),
    );
    let color_suffixes = Value::Dict(
        Dict::new_sync()
            .insert(Key::binary_from_text("none"), Value::binary_from_text(""))
            .insert(
                Key::binary_from_text("ansi16"),
                Value::binary_from_text("\x1b[0m"),
            )
            .insert(
                Key::binary_from_text("ansi256"),
                Value::binary_from_text("\x1b[0m"),
            )
            .insert(
                Key::binary_from_text("truecolor"),
                Value::binary_from_text("\x1b[0m"),
            ),
    );

    let mut locals = ResolverContext::default();
    let diagnostic = locals.push_binding("<diagnostic>");
    let lines = locals.push_binding("<diagnostic-lines>");
    let continuation_line = locals.push_binding("<diagnostic-continuation-line>");

    let severity = || field(diagnostic, &["msg", "severity"]);
    let color = || field(diagnostic, &["viewer", "color"]);
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
    let formatted = append([
        field(diagnostic, &["viewer", "location"]),
        indexed(color_prefixes, [color(), severity()]),
        indexed(severity_labels, [severity()]),
        indexed(color_suffixes, [color()]),
        ResolvedExpr::Embedded(Value::binary_from_text(": ")),
        apply_builtin(Builtin::ListHead, [ResolvedExpr::Local(lines)]),
        indented_continuations,
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
    evaluate_closed(ResolvedExpr::lambda(vec![diagnostic], with_lines))
}

fn evaluate_closed(expression: ResolvedExpr<Value>) -> Value {
    let value = lower_resolved_expr(expression);
    crate::eval::eval_value(&crate::evaluation::EvalContext::standalone(), &value)
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
        assert!(matches!(&*FORMATTER, Value::Function(_)));
    }
}

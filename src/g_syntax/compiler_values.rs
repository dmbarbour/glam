//! Closed values owned by the built-in g compiler.
//!
//! A user-defined compiler naturally shares the values captured by its own
//! definition. The Rust bootstrap has no enclosing glam value, so this module
//! provides the equivalent ownership explicitly: every closed helper is
//! lowered once, then cloned through its shared backing value.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use super::*;

#[derive(Clone)]
pub(in crate::g_syntax) struct BuiltinModule {
    pub(in crate::g_syntax) value: Value,
    pub(in crate::g_syntax) definitions: Value,
}

struct GCompilerValues {
    math: BuiltinModule,
    list: BuiltinModule,
    std: BuiltinModule,
    empty_object_defs: Value,
    constant_object_defs: Value,
    reflection_annotator: Value,
}

static EFFECT_VALUES: LazyLock<Mutex<HashMap<Key, Value>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static COMPILER_VALUES: LazyLock<GCompilerValues> = LazyLock::new(GCompilerValues::build);

impl GCompilerValues {
    fn build() -> Self {
        let not = build_not();
        let could = build_could(not.clone());
        let constant_object_defs = build_constant_object_defs();

        let math_value = Value::Dict(
            Dict::new_sync()
                .insert(name_as_key("floor"), Value::Builtin(Builtin::Floor))
                .insert(name_as_key("mod"), Value::Builtin(Builtin::Mod)),
        );
        let list_value = Value::Dict(
            Dict::new_sync()
                .insert(name_as_key("slice"), Value::Builtin(Builtin::Slice))
                .insert(name_as_key("split"), Value::Builtin(Builtin::ListSplit))
                .insert(
                    name_as_key("split_end"),
                    Value::Builtin(Builtin::ListSplitEnd),
                )
                .insert(name_as_key("map"), Value::Builtin(Builtin::Map))
                .insert(name_as_key("concat"), Value::Builtin(Builtin::ListConcat))
                .insert(name_as_key("len"), Value::Builtin(Builtin::ListLen))
                .insert(name_as_key("head"), Value::Builtin(Builtin::ListHead))
                .insert(name_as_key("tail"), Value::Builtin(Builtin::ListTail))
                .insert(name_as_key("pure"), Value::Builtin(Builtin::ListEffect)),
        );
        let std_value = Value::Dict(
            Dict::new_sync()
                .insert(name_as_key("anno"), Value::Builtin(Builtin::Anno))
                .insert(name_as_key("seq"), Value::Builtin(Builtin::Seq))
                .insert(name_as_key("spark"), Value::Builtin(Builtin::Spark))
                .insert(name_as_key("net_arity"), Value::Builtin(Builtin::NetArity))
                .insert(name_as_key("not"), not.clone())
                .insert(name_as_key("could"), could.clone())
                .insert(name_as_key("math"), math_value.clone())
                .insert(name_as_key("list"), list_value.clone())
                .insert(
                    name_as_key("eff"),
                    Value::Dict(
                        Dict::new_sync()
                            .insert(name_as_key("map"), Value::Builtin(Builtin::EffectMap)),
                    ),
                ),
        );

        let make_module = |value: Value| BuiltinModule {
            definitions: apply_closed(constant_object_defs.clone(), [value.clone()]),
            value,
        };

        Self {
            math: make_module(math_value),
            list: make_module(list_value),
            std: make_module(std_value),
            empty_object_defs: build_empty_object_defs(),
            constant_object_defs,
            reflection_annotator: build_reflection_annotator(),
        }
    }
}

pub(in crate::g_syntax) fn builtin_module(name: &str) -> Option<BuiltinModule> {
    match name {
        "math" => Some(COMPILER_VALUES.math.clone()),
        "list" => Some(COMPILER_VALUES.list.clone()),
        "std" | "prelude" => Some(COMPILER_VALUES.std.clone()),
        _ => None,
    }
}

#[cfg(test)]
pub(in crate::g_syntax) fn builtin_list_module() -> Dict {
    value_dict(&COMPILER_VALUES.list.value)
}

pub(in crate::g_syntax) fn empty_object_defs() -> Value {
    COMPILER_VALUES.empty_object_defs.clone()
}

pub(in crate::g_syntax) fn constant_object_defs(value: Value) -> Value {
    apply_closed(COMPILER_VALUES.constant_object_defs.clone(), [value])
}

pub(in crate::g_syntax) fn reflection_annotator_resolved(
    guard: ResolvedExpr<Value>,
    final_defs: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(COMPILER_VALUES.reflection_annotator.clone()),
        [guard, final_defs],
    )
}

pub(in crate::g_syntax) fn reflection_annotator_value(guard: Value, final_defs: Value) -> Value {
    evaluate_closed(reflection_annotator_resolved(
        ResolvedExpr::Provided(guard),
        ResolvedExpr::Provided(final_defs),
    ))
}

pub(in crate::g_syntax) fn effect_value(name: &str) -> Value {
    effect_path_value(&[name])
}

pub(in crate::g_syntax) fn effect_path_value(path: &[&str]) -> Value {
    let path: Arc<[Key]> = path.iter().map(Key::atom_from_text).collect();
    let cache_key = Key::List(path.clone());
    let mut values = EFFECT_VALUES
        .lock()
        .expect("g compiler effect-value cache must not be poisoned");
    values
        .entry(cache_key)
        .or_insert_with(|| build_effect_path_value(path))
        .clone()
}

#[cfg(test)]
fn value_dict(value: &Value) -> Dict {
    let Value::Dict(dict) = value else {
        unreachable!("cached built-in module must be a dictionary")
    };
    dict.clone()
}

fn apply_closed(function: Value, arguments: impl IntoIterator<Item = Value>) -> Value {
    evaluate_closed(ResolvedExpr::apply(
        ResolvedExpr::Embedded(function),
        arguments.into_iter().map(ResolvedExpr::Provided),
    ))
}

fn evaluate_closed(expression: ResolvedExpr<Value>) -> Value {
    let value = lower_resolved_expr(expression);
    crate::eval::eval_value(&crate::evaluation::EvalContext::standalone(), &value)
        .expect("closed g compiler helper must evaluate without session capabilities")
}

fn apply_builtin(
    builtin: Builtin,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(Value::Builtin(builtin)), arguments)
}

fn effect_call(
    name: &str,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(effect_value(name)), arguments)
}

fn effect_path_call(
    path: &[&str],
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(effect_path_value(path)), arguments)
}

fn assert_unit(value: ResolvedExpr<Value>, target: ResolvedExpr<Value>) -> ResolvedExpr<Value> {
    let payload = apply_builtin(
        Builtin::DictSingleton,
        [
            ResolvedExpr::Embedded(Value::Atom(atom_from_str("value"))),
            value,
        ],
    );
    let annotation = apply_builtin(
        Builtin::DictSingleton,
        [
            ResolvedExpr::Embedded(Value::Atom(atom_from_str("assert_unit"))),
            payload,
        ],
    );
    apply_builtin(Builtin::Anno, [annotation, target])
}

fn effect_then(
    operation: ResolvedExpr<Value>,
    next: ResolvedExpr<Value>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let result = locals.push_binding("<effect-result>");
    let continuation =
        ResolvedExpr::lambda(vec![result], assert_unit(ResolvedExpr::Local(result), next));
    locals.truncate(base_len);
    effect_call("seq", [operation, continuation])
}

fn build_effect_path_value(path: Arc<[Key]>) -> Value {
    let mut locals = ResolverContext::default();
    let api = locals.push_binding("<effect-api>");
    let body = ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Local(api)),
        path: path.iter().cloned().map(ResolvedPathPart::Key).collect(),
    };
    let effect = apply_builtin(
        Builtin::DictSingleton,
        [
            ResolvedExpr::Embedded(Value::Atom(atom_from_str("eff"))),
            ResolvedExpr::lambda(vec![api], body),
        ],
    );
    evaluate_closed(effect)
}

fn build_not() -> Value {
    let mut locals = ResolverContext::default();
    let condition = locals.push_binding("<not-condition>");
    let fail_operation = ResolvedExpr::Embedded(effect_value("fail"));
    let true_operation = effect_call("r", [ResolvedExpr::Embedded((*keys::UNIT_VALUE).clone())]);
    let returned_failure = effect_call("r", [fail_operation]);
    let fail_if_condition_succeeds = effect_then(
        ResolvedExpr::Local(condition),
        returned_failure,
        &mut locals,
    );
    let succeed_if_condition_fails = effect_call("r", [true_operation]);
    let alternate = effect_call(
        "alt",
        [fail_if_condition_succeeds, succeed_if_condition_fails],
    );
    let select_operation = effect_call("cut", [alternate]);
    let selected = locals.push_binding("<selected-operation>");
    let run_selected_operation =
        ResolvedExpr::lambda(vec![selected], ResolvedExpr::Local(selected));
    let body = effect_call("seq", [select_operation, run_selected_operation]);
    evaluate_closed(ResolvedExpr::lambda(vec![condition], body))
}

fn build_could(not: Value) -> Value {
    let mut locals = ResolverContext::default();
    let condition = locals.push_binding("<could-condition>");
    let inner = ResolvedExpr::apply(
        ResolvedExpr::Embedded(not.clone()),
        [ResolvedExpr::Local(condition)],
    );
    evaluate_closed(ResolvedExpr::lambda(
        vec![condition],
        ResolvedExpr::apply(ResolvedExpr::Embedded(not), [inner]),
    ))
}

fn build_empty_object_defs() -> Value {
    let mut locals = ResolverContext::default();
    let prior_self = locals.push_binding("<object-prior-self>");
    let final_self = locals.push_binding("<object-final-self>");
    let without_spec = apply_builtin(
        Builtin::DictUpdate,
        [
            ResolvedExpr::List(vec![ResolvedExpr::Embedded(Value::Atom(atom_from_str(
                "spec",
            )))]),
            ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
            ResolvedExpr::Local(prior_self),
        ],
    );
    evaluate_closed(ResolvedExpr::lambda(
        vec![prior_self, final_self],
        without_spec,
    ))
}

fn build_constant_object_defs() -> Value {
    let mut locals = ResolverContext::default();
    let value = locals.push_binding("<constant-object-definitions>");
    let prior_self = locals.push_binding("<object-prior-self>");
    let final_self = locals.push_binding("<object-final-self>");
    evaluate_closed(ResolvedExpr::lambda(
        vec![value, prior_self, final_self],
        ResolvedExpr::Local(value),
    ))
}

fn build_reflection_annotator() -> Value {
    let mut locals = ResolverContext::default();
    let guard = locals.push_binding("<reflection-guard>");
    let final_defs = locals.push_binding("<reflection-final-definitions>");
    let target = locals.push_binding("<reflection-target>");

    let state_path = |field: &str| {
        ResolvedExpr::List(vec![
            ResolvedExpr::Local(guard),
            ResolvedExpr::Embedded(Value::Atom(atom_from_str(field))),
        ])
    };
    let final_refl = ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Local(final_defs)),
        path: vec![ResolvedPathPart::Key(name_as_key("refl"))],
    };

    let item = locals.push_binding("<reflection-item>");
    let item_field = |name| ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Local(item)),
        path: vec![ResolvedPathPart::Key(name_as_key(name))],
    };
    let require_unit = effect_then(
        item_field("value"),
        effect_call("r", [ResolvedExpr::Embedded((*keys::UNIT_VALUE).clone())]),
        &mut locals,
    );
    let handle = locals.push_binding("<reflection-task-handle>");
    let task_record = apply_builtin(
        Builtin::DictUnion,
        [
            apply_builtin(
                Builtin::DictSingleton,
                [
                    ResolvedExpr::Embedded(Value::Atom(atom_from_str("key"))),
                    item_field("key"),
                ],
            ),
            apply_builtin(
                Builtin::DictSingleton,
                [
                    ResolvedExpr::Embedded(Value::Atom(atom_from_str("task"))),
                    ResolvedExpr::Local(handle),
                ],
            ),
        ],
    );
    let launch_item = effect_call(
        "seq",
        [
            effect_call("refl_task", [require_unit]),
            ResolvedExpr::lambda(vec![handle], effect_call("r", [task_record])),
        ],
    );
    let launcher = ResolvedExpr::lambda(vec![item], launch_item);

    let items = locals.push_binding("<reflection-items>");
    let mapped = ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::EffectMap)),
        [launcher, ResolvedExpr::Local(items)],
    );
    let records = locals.push_binding("<reflection-task-records>");
    let store_records = effect_path_call(
        &["heap", "set"],
        [state_path("tasks"), ResolvedExpr::Local(records)],
    );
    let map_and_store = effect_call(
        "cut",
        [effect_call(
            "seq",
            [mapped, ResolvedExpr::lambda(vec![records], store_records)],
        )],
    );
    let scanner = effect_call(
        "seq",
        [
            effect_call("dict_items", [final_refl]),
            ResolvedExpr::lambda(vec![items], map_and_store),
        ],
    );

    let scanner_handle = locals.push_binding("<reflection-scanner-handle>");
    let launch_and_remember = effect_call(
        "seq",
        [
            effect_call("refl_task", [scanner]),
            ResolvedExpr::lambda(
                vec![scanner_handle],
                effect_path_call(
                    &["heap", "set"],
                    [state_path("claim"), ResolvedExpr::Local(scanner_handle)],
                ),
            ),
        ],
    );
    let existing = locals.push_binding("<reflection-claim>");
    let guard_is_empty = ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::Equal)),
        [
            ResolvedExpr::Local(existing),
            ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
        ],
    );
    let start_if_missing = effect_then(guard_is_empty, launch_and_remember, &mut locals);
    let already_started = effect_call("r", [ResolvedExpr::Embedded((*keys::UNIT_VALUE).clone())]);
    let choose = effect_call("alt", [start_if_missing, already_started]);
    let ensure_tasks = effect_call(
        "cut",
        [effect_call(
            "seq",
            [
                effect_path_call(&["heap", "get"], [state_path("claim")]),
                ResolvedExpr::lambda(vec![existing], choose),
            ],
        )],
    );
    let annotation = apply_builtin(
        Builtin::DictSingleton,
        [
            ResolvedExpr::Embedded(Value::Atom(atom_from_str("refl"))),
            ensure_tasks,
        ],
    );
    let annotated = apply_builtin(Builtin::Anno, [annotation, ResolvedExpr::Local(target)]);
    evaluate_closed(ResolvedExpr::lambda(
        vec![guard, final_defs, target],
        annotated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_compiler_values_are_cached_after_exposing_their_functions() {
        let first_effect = effect_value("compiler_cache_test");
        let second_effect = effect_value("compiler_cache_test");
        assert_eq!(first_effect, second_effect);
        assert!(matches!(first_effect, Value::Dict(_)));

        let first_std = builtin_module("std").expect("std should be built in");
        let second_std = builtin_module("std").expect("std should remain built in");
        assert_eq!(first_std.value, second_std.value);
        assert_eq!(first_std.definitions, second_std.definitions);
        assert!(matches!(first_std.definitions, Value::Function(_)));
        assert!(matches!(
            COMPILER_VALUES.reflection_annotator,
            Value::Function(_)
        ));
    }
}

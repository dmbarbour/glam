//! Closed values owned by the built-in g compiler.
//!
//! A user-defined compiler naturally shares the values captured by its own
//! definition. The Rust bootstrap has no enclosing glam value, so this module
//! provides the equivalent ownership explicitly: every closed helper is
//! lowered once, then cloned through its shared backing value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    pure_if_runner: Value,
    pure_match_runner: Value,
    macro_environment: Value,
    effects: Mutex<HashMap<Key, Value>>,
}

trait EffectValueCache {
    fn effects(&self) -> &Mutex<HashMap<Key, Value>>;
}

impl EffectValueCache for GCompilerValues {
    fn effects(&self) -> &Mutex<HashMap<Key, Value>> {
        &self.effects
    }
}

struct BuildingEffectValues<'a>(&'a Mutex<HashMap<Key, Value>>);

impl EffectValueCache for BuildingEffectValues<'_> {
    fn effects(&self) -> &Mutex<HashMap<Key, Value>> {
        self.0
    }
}

fn cache(values: &CoreValueFactory) -> Arc<GCompilerValues> {
    values.cached(|| GCompilerValues::build(values))
}

fn with_values<R>(values: &CoreValueFactory, use_values: impl FnOnce(&GCompilerValues) -> R) -> R {
    use_values(&cache(values))
}

impl GCompilerValues {
    fn build(values: &CoreValueFactory) -> Self {
        let effects = Mutex::new(HashMap::new());
        let build_cache = BuildingEffectValues(&effects);
        let not = build_not(values, &build_cache);
        let could = build_could(values, not.clone());
        let constant_object_defs = build_constant_object_defs(values);

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
                .insert(name_as_key("at"), Value::Builtin(Builtin::ListAt))
                .insert(name_as_key("head"), Value::Builtin(Builtin::ListHead))
                .insert(name_as_key("tail"), Value::Builtin(Builtin::ListTail))
                .insert(name_as_key("pure"), Value::Builtin(Builtin::ListEffect)),
        );
        let std_value = Value::Dict(
            Dict::new_sync()
                .insert(name_as_key("anno"), Value::Builtin(Builtin::Anno))
                .insert(name_as_key("seq"), Value::Builtin(Builtin::Seq))
                .insert(name_as_key("spark"), Value::Builtin(Builtin::Spark))
                .insert(
                    name_as_key("interaction_net"),
                    Value::Builtin(Builtin::InteractionNet),
                )
                .insert(name_as_key("net_arity"), Value::Builtin(Builtin::NetArity))
                .insert(
                    name_as_key("object_from_dict"),
                    Value::Builtin(Builtin::ObjectFromDict),
                )
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
            definitions: apply_closed(values, constant_object_defs.clone(), [value.clone()]),
            value,
        };

        Self {
            math: make_module(math_value),
            list: make_module(list_value),
            std: make_module(std_value),
            empty_object_defs: build_empty_object_defs(values),
            constant_object_defs,
            reflection_annotator: build_reflection_annotator(values, &build_cache),
            pure_if_runner: build_pure_conditional_runner(values, Builtin::IfResult),
            pure_match_runner: build_pure_conditional_runner(values, Builtin::MatchResult),
            macro_environment: build_macro_environment(values),
            effects,
        }
    }

    fn pure_conditional_runner(&self, selector: Builtin) -> &Value {
        match selector {
            Builtin::IfResult => &self.pure_if_runner,
            Builtin::MatchResult => &self.pure_match_runner,
            _ => unreachable!("pure conditional runner requires a result selector"),
        }
    }
}

pub(in crate::g_syntax) fn builtin_module(
    values: &CoreValueFactory,
    name: &str,
) -> Option<BuiltinModule> {
    with_values(values, |compiler| match name {
        "math" => Some(compiler.math.clone()),
        "list" => Some(compiler.list.clone()),
        "std" | "prelude" => Some(compiler.std.clone()),
        _ => None,
    })
}

#[cfg(test)]
pub(in crate::g_syntax) fn builtin_list_module(values: &CoreValueFactory) -> Dict {
    with_values(values, |compiler| value_dict(&compiler.list.value))
}

pub(in crate::g_syntax) fn empty_object_defs(values: &CoreValueFactory) -> Value {
    with_values(values, |compiler| compiler.empty_object_defs.clone())
}

pub(in crate::g_syntax) fn constant_object_defs(values: &CoreValueFactory, value: Value) -> Value {
    let function = with_values(values, |compiler| compiler.constant_object_defs.clone());
    apply_closed(values, function, [value])
}

pub(in crate::g_syntax) fn reflection_annotator_resolved(
    values: &CoreValueFactory,
    guard: ResolvedExpr<Value>,
    final_defs: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(values, |compiler| {
            compiler.reflection_annotator.clone()
        })),
        [guard, final_defs],
    )
}

pub(in crate::g_syntax) fn reflection_annotator_value(
    values: &CoreValueFactory,
    guard: Value,
    final_defs: Value,
) -> Value {
    evaluate_closed(
        values,
        reflection_annotator_resolved(
            values,
            ResolvedExpr::Provided(guard),
            ResolvedExpr::Provided(final_defs),
        ),
    )
}

pub(in crate::g_syntax) fn run_pure_conditional_resolved(
    values: &CoreValueFactory,
    operation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(values, |compiler| {
            compiler.pure_conditional_runner(Builtin::IfResult).clone()
        })),
        [operation],
    )
}

pub(in crate::g_syntax) fn run_pure_match_resolved(
    values: &CoreValueFactory,
    search: ResolvedExpr<Value>,
    line: usize,
) -> ResolvedExpr<Value> {
    let cache = cache(values);
    let exhausted = effect_call(
        values,
        cache.as_ref(),
        "r",
        [ResolvedExpr::Embedded(Value::error(
            values,
            format!("match exhausted on line {line}"),
        ))],
    );
    let operation = effect_call(
        values,
        cache.as_ref(),
        "cut",
        [effect_call(
            values,
            cache.as_ref(),
            "alt",
            [search, exhausted],
        )],
    );
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(values, |compiler| {
            compiler
                .pure_conditional_runner(Builtin::MatchResult)
                .clone()
        })),
        [operation],
    )
}

pub(in crate::g_syntax) fn run_pure_open_match_resolved(
    operation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin(Builtin::ListEffect, [operation])
}

/// Extends a file-provided macro environment through the language's ordinary
/// `with` operation, introducing the authoritative language declaration.
pub(in crate::g_syntax) fn macro_environment(
    values: &CoreValueFactory,
    base: Value,
    language: Value,
) -> Value {
    let function = with_values(values, |compiler| compiler.macro_environment.clone());
    apply_closed(values, function, [base, language])
}

pub(in crate::g_syntax) fn effect_value(values: &CoreValueFactory, name: &str) -> Value {
    effect_path_value(values, &[name])
}

pub(in crate::g_syntax) fn effect_path_value(values: &CoreValueFactory, path: &[&str]) -> Value {
    let cache = cache(values);
    effect_path_value_with_cache(values, cache.as_ref(), path)
}

fn effect_path_value_with_cache(
    values: &CoreValueFactory,
    cache: &dyn EffectValueCache,
    path: &[&str],
) -> Value {
    let path: Arc<[Key]> = path.iter().map(Key::atom_from_text).collect();
    let cache_key = Key::List(path.clone());
    let mut effects = cache
        .effects()
        .lock()
        .expect("g compiler effect-value cache must not be poisoned");
    effects
        .entry(cache_key)
        .or_insert_with(|| build_effect_path_value(values, path))
        .clone()
}

#[cfg(test)]
fn value_dict(value: &Value) -> Dict {
    let Value::Dict(dict) = value else {
        unreachable!("cached built-in module must be a dictionary")
    };
    dict.clone()
}

fn apply_closed(
    values: &CoreValueFactory,
    function: Value,
    arguments: impl IntoIterator<Item = Value>,
) -> Value {
    evaluate_closed(
        values,
        ResolvedExpr::apply(
            ResolvedExpr::Embedded(function),
            arguments.into_iter().map(ResolvedExpr::Provided),
        ),
    )
}

fn evaluate_closed(values: &CoreValueFactory, expression: ResolvedExpr<Value>) -> Value {
    let value = lower_resolved_expr(values, expression);
    crate::eval::eval_value(
        &crate::evaluation::EvalContext::private_closed(values.clone()),
        &value,
    )
    .expect("closed g compiler helper must evaluate without session capabilities")
}

fn apply_builtin(
    builtin: Builtin,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(Value::Builtin(builtin)), arguments)
}

fn effect_call(
    values: &CoreValueFactory,
    cache: &dyn EffectValueCache,
    name: &str,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(effect_path_value_with_cache(values, cache, &[name])),
        arguments,
    )
}

fn effect_path_call(
    values: &CoreValueFactory,
    cache: &dyn EffectValueCache,
    path: &[&str],
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(effect_path_value_with_cache(values, cache, path)),
        arguments,
    )
}

fn assert_unit(
    diagnostic_context: &'static str,
    value: ResolvedExpr<Value>,
    target: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin(
        Builtin::AssertUnit,
        [
            ResolvedExpr::Embedded(Value::binary_from_text(diagnostic_context)),
            value,
            target,
        ],
    )
}

fn effect_then(
    values: &CoreValueFactory,
    cache: &dyn EffectValueCache,
    operation: ResolvedExpr<Value>,
    next: ResolvedExpr<Value>,
    diagnostic_context: &'static str,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let result = locals.push_internal_binding("<effect-result>");
    let continuation = ResolvedExpr::lambda(
        vec![result],
        assert_unit(diagnostic_context, ResolvedExpr::Local(result), next),
    );
    locals.truncate(base_len);
    effect_call(values, cache, "seq", [operation, continuation])
}

fn build_effect_path_value(values: &CoreValueFactory, path: Arc<[Key]>) -> Value {
    let mut locals = ResolverContext::default();
    let api = locals.push_internal_binding("<effect-api>");
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
    evaluate_closed(values, effect)
}

fn build_not(values: &CoreValueFactory, cache: &dyn EffectValueCache) -> Value {
    let mut locals = ResolverContext::default();
    let condition = locals.push_internal_binding("<not-condition>");
    let fail_operation =
        ResolvedExpr::Embedded(effect_path_value_with_cache(values, cache, &["fail"]));
    let true_operation = effect_call(values, cache, "r", [ResolvedExpr::Embedded(values.unit())]);
    let returned_failure = effect_call(values, cache, "r", [fail_operation]);
    let fail_if_condition_succeeds = effect_then(
        values,
        cache,
        ResolvedExpr::Local(condition),
        returned_failure,
        "`not` condition",
        &mut locals,
    );
    let succeed_if_condition_fails = effect_call(values, cache, "r", [true_operation]);
    let alternate = effect_call(
        values,
        cache,
        "alt",
        [fail_if_condition_succeeds, succeed_if_condition_fails],
    );
    let select_operation = effect_call(values, cache, "cut", [alternate]);
    let selected = locals.push_internal_binding("<selected-operation>");
    let run_selected_operation =
        ResolvedExpr::lambda(vec![selected], ResolvedExpr::Local(selected));
    let body = effect_call(
        values,
        cache,
        "seq",
        [select_operation, run_selected_operation],
    );
    evaluate_closed(values, ResolvedExpr::lambda(vec![condition], body))
}

fn build_could(values: &CoreValueFactory, not: Value) -> Value {
    let mut locals = ResolverContext::default();
    let condition = locals.push_internal_binding("<could-condition>");
    let inner = ResolvedExpr::apply(
        ResolvedExpr::Embedded(not.clone()),
        [ResolvedExpr::Local(condition)],
    );
    evaluate_closed(
        values,
        ResolvedExpr::lambda(
            vec![condition],
            ResolvedExpr::apply(ResolvedExpr::Embedded(not), [inner]),
        ),
    )
}

fn build_pure_conditional_runner(values: &CoreValueFactory, selector: Builtin) -> Value {
    assert!(matches!(selector, Builtin::IfResult | Builtin::MatchResult));
    let mut locals = ResolverContext::default();
    let operation = locals.push_internal_binding("<conditional-operation>");
    let results = apply_builtin(Builtin::ListEffect, [ResolvedExpr::Local(operation)]);
    let selected = apply_builtin(selector, [results]);
    evaluate_closed(values, ResolvedExpr::lambda(vec![operation], selected))
}

fn build_macro_environment(values: &CoreValueFactory) -> Value {
    let mut locals = ResolverContext::default();
    let environment_parameter = locals.push_internal_binding("<macro-environment>");
    let language_parameter = locals.push_internal_binding("<macro-language>");
    let prior = locals.push_internal_binding("<macro-environment-prior>");
    let final_environment = locals.push_internal_binding("<macro-environment-final>");

    let singleton = |key: &str, value| {
        apply_builtin(
            Builtin::DictSingleton,
            [
                ResolvedExpr::Embedded(Value::Atom(atom_from_str(key))),
                value,
            ],
        )
    };
    let prior_language = ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Local(prior)),
        path: vec![ResolvedPathPart::Key(name_as_key("language"))],
    };
    let assertion_payload = apply_builtin(
        Builtin::DictUnion,
        [
            singleton(
                "name",
                ResolvedExpr::Embedded(Value::binary_from_text("language")),
            ),
            singleton("value", prior_language),
        ],
    );
    let assertion = singleton("assert_undefined", assertion_payload);
    let language = apply_builtin(
        Builtin::Anno,
        [assertion, ResolvedExpr::Local(language_parameter)],
    );
    let extended = apply_builtin(
        Builtin::DictUpdate,
        [
            ResolvedExpr::List(vec![ResolvedExpr::Embedded(Value::Atom(atom_from_str(
                "language",
            )))]),
            language,
            ResolvedExpr::Local(prior),
        ],
    );
    let definitions = ResolvedExpr::lambda(vec![prior, final_environment], extended);
    let result = apply_builtin(
        Builtin::ObjectWithDefs,
        [ResolvedExpr::Local(environment_parameter), definitions],
    );
    evaluate_closed(
        values,
        ResolvedExpr::lambda(vec![environment_parameter, language_parameter], result),
    )
}

fn build_empty_object_defs(values: &CoreValueFactory) -> Value {
    let mut locals = ResolverContext::default();
    let prior_self = locals.push_internal_binding("<object-prior-self>");
    let final_self = locals.push_internal_binding("<object-final-self>");
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
    evaluate_closed(
        values,
        ResolvedExpr::lambda(vec![prior_self, final_self], without_spec),
    )
}

fn build_constant_object_defs(values: &CoreValueFactory) -> Value {
    let mut locals = ResolverContext::default();
    let value = locals.push_internal_binding("<constant-object-definitions>");
    let prior_self = locals.push_internal_binding("<object-prior-self>");
    let final_self = locals.push_internal_binding("<object-final-self>");
    evaluate_closed(
        values,
        ResolvedExpr::lambda(
            vec![value, prior_self, final_self],
            ResolvedExpr::Local(value),
        ),
    )
}

fn build_reflection_annotator(values: &CoreValueFactory, cache: &dyn EffectValueCache) -> Value {
    let mut locals = ResolverContext::default();
    let guard = locals.push_internal_binding("<reflection-guard>");
    let final_defs = locals.push_internal_binding("<reflection-final-definitions>");
    let target = locals.push_internal_binding("<reflection-target>");

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

    let item = locals.push_internal_binding("<reflection-item>");
    let item_field = |name| ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Local(item)),
        path: vec![ResolvedPathPart::Key(name_as_key(name))],
    };
    let require_unit = effect_then(
        values,
        cache,
        item_field("value"),
        effect_call(values, cache, "r", [ResolvedExpr::Embedded(values.unit())]),
        "`refl.*` task result",
        &mut locals,
    );
    let handle = locals.push_internal_binding("<reflection-task-handle>");
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
        values,
        cache,
        "seq",
        [
            effect_path_call(values, cache, &["task", "new"], [require_unit]),
            ResolvedExpr::lambda(vec![handle], effect_call(values, cache, "r", [task_record])),
        ],
    );
    let launcher = ResolvedExpr::lambda(vec![item], launch_item);

    let items = locals.push_internal_binding("<reflection-items>");
    let mapped = ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::EffectMap)),
        [launcher, ResolvedExpr::Local(items)],
    );
    let records = locals.push_internal_binding("<reflection-task-records>");
    let store_records = effect_path_call(
        values,
        cache,
        &["heap", "set"],
        [state_path("tasks"), ResolvedExpr::Local(records)],
    );
    let map_and_store = effect_call(
        values,
        cache,
        "cut",
        [effect_call(
            values,
            cache,
            "seq",
            [mapped, ResolvedExpr::lambda(vec![records], store_records)],
        )],
    );
    let scanner = effect_call(
        values,
        cache,
        "seq",
        [
            effect_call(values, cache, "dict_items", [final_refl]),
            ResolvedExpr::lambda(vec![items], map_and_store),
        ],
    );

    let scanner_handle = locals.push_internal_binding("<reflection-scanner-handle>");
    let launch_and_remember = effect_call(
        values,
        cache,
        "seq",
        [
            effect_path_call(values, cache, &["task", "new"], [scanner]),
            ResolvedExpr::lambda(
                vec![scanner_handle],
                effect_path_call(
                    values,
                    cache,
                    &["heap", "set"],
                    [state_path("claim"), ResolvedExpr::Local(scanner_handle)],
                ),
            ),
        ],
    );
    let existing = locals.push_internal_binding("<reflection-claim>");
    let guard_is_empty = ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::Equal)),
        [
            ResolvedExpr::Local(existing),
            ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
        ],
    );
    let start_if_missing = effect_then(
        values,
        cache,
        guard_is_empty,
        launch_and_remember,
        "automatic reflection boundary claim test",
        &mut locals,
    );
    let already_started = effect_call(values, cache, "r", [ResolvedExpr::Embedded(values.unit())]);
    let choose = effect_call(values, cache, "alt", [start_if_missing, already_started]);
    let ensure_tasks = effect_call(
        values,
        cache,
        "cut",
        [effect_call(
            values,
            cache,
            "seq",
            [
                effect_path_call(values, cache, &["heap", "get"], [state_path("claim")]),
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
    evaluate_closed(
        values,
        ResolvedExpr::lambda(vec![guard, final_defs, target], annotated),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::number::Number;

    #[test]
    fn closed_compiler_values_are_cached_after_exposing_their_functions() {
        let values = crate::compiler::test_value_factory();
        let first_effect = effect_value(&values, "compiler_cache_test");
        let second_effect = effect_value(&values, "compiler_cache_test");
        assert_eq!(first_effect, second_effect);
        assert!(matches!(first_effect, Value::Dict(_)));

        let first_std = builtin_module(&values, "std").expect("std should be built in");
        let second_std = builtin_module(&values, "std").expect("std should remain built in");
        assert_eq!(first_std.value, second_std.value);
        assert_eq!(first_std.definitions, second_std.definitions);
        assert!(matches!(first_std.definitions, Value::Function(_)));
        with_values(&values, |compiler| {
            assert!(matches!(compiler.reflection_annotator, Value::Function(_)));
            assert!(matches!(compiler.pure_if_runner, Value::Function(_)));
            assert!(matches!(compiler.pure_match_runner, Value::Function(_)));
            assert!(matches!(compiler.macro_environment, Value::Function(_)));
        });
    }

    #[test]
    fn compiler_bundle_is_runtime_local_and_resolved_once_per_compilation_scope() {
        let first_runtime = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        let second_runtime = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        assert!(!Arc::ptr_eq(
            &cache(&first_runtime),
            &cache(&second_runtime)
        ));

        let compilation = first_runtime.scoped();
        let before = compilation.extension_lookup_count();
        let _ = effect_value(&compilation, "r");
        let _ = effect_value(&compilation, "seq");
        let _ = builtin_module(&compilation, "std");
        assert_eq!(compilation.extension_lookup_count() - before, 1);
    }

    #[test]
    fn macro_environment_extends_a_dictionary_with_ordinary_introduction_rules() {
        let values = crate::compiler::test_value_factory();
        let base = Value::Dict(
            Dict::new_sync().insert(name_as_key("existing"), Value::Number(Number::integer(1))),
        );
        let environment = macro_environment(&values, base, Value::binary_from_text("g0"));

        let existing = evaluate_closed(
            &values,
            ResolvedExpr::Access {
                base: Box::new(ResolvedExpr::Provided(environment.clone())),
                path: vec![ResolvedPathPart::Key(name_as_key("existing"))],
            },
        );
        let language = evaluate_closed(
            &values,
            ResolvedExpr::Access {
                base: Box::new(ResolvedExpr::Provided(environment)),
                path: vec![ResolvedPathPart::Key(name_as_key("language"))],
            },
        );
        assert_eq!(existing, Value::Number(Number::integer(1)));
        assert_eq!(language, Value::binary_from_text("g0"));
    }

    #[test]
    fn macro_environment_reinstantiates_an_adapting_object() {
        let values = crate::compiler::test_value_factory();
        let mut locals = ResolverContext::default();
        let base = locals.push_internal_binding("<base>");
        let self_value = locals.push_internal_binding("<self>");
        let language = ResolvedExpr::Access {
            base: Box::new(ResolvedExpr::Local(self_value)),
            path: vec![ResolvedPathPart::Key(name_as_key("language"))],
        };
        let definitions = ResolvedExpr::lambda(
            vec![base, self_value],
            apply_builtin(
                Builtin::DictUpdate,
                [
                    ResolvedExpr::List(vec![ResolvedExpr::Embedded(Value::Atom(atom_from_str(
                        "adapted",
                    )))]),
                    language,
                    ResolvedExpr::Local(base),
                ],
            ),
        );
        let object = evaluate_closed(
            &values,
            apply_builtin(
                Builtin::ObjectInstanceFromParts,
                [
                    ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
                    ResolvedExpr::List(Vec::new()),
                    definitions,
                ],
            ),
        );
        let environment = macro_environment(&values, object, Value::binary_from_text("g0"));
        let adapted = evaluate_closed(
            &values,
            ResolvedExpr::Access {
                base: Box::new(ResolvedExpr::Provided(environment)),
                path: vec![ResolvedPathPart::Key(name_as_key("adapted"))],
            },
        );
        assert_eq!(adapted, Value::binary_from_text("g0"));
    }
}

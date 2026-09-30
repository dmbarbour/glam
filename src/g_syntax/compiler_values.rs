//! Closed values owned by the built-in g compiler.
//!
//! A user-defined compiler naturally shares the values captured by its own
//! definition. The Rust bootstrap has no enclosing glam value, so this module
//! provides the equivalent ownership explicitly: every closed helper is
//! lowered once, then cloned through its shared backing value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::core::{RuntimeCacheFamily, RuntimeCacheFamilyRecord};
use crate::runtime::RuntimeValueRoot;

use super::*;

pub(in crate::g_syntax) struct BuiltinModule {
    pub(in crate::g_syntax) value: Value,
    pub(in crate::g_syntax) definitions: Value,
}

struct RootedBuiltinModule {
    value: RuntimeValueRoot,
    definitions: RuntimeValueRoot,
}

impl RootedBuiltinModule {
    fn visit_runtime_roots(&self, visit: &mut dyn FnMut(&RuntimeValueRoot)) {
        let Self { value, definitions } = self;
        visit(value);
        visit(definitions);
    }
}

struct GCompilerValues {
    runtime: crate::runtime::EvaluationRuntimeId,
    math: RootedBuiltinModule,
    list: RootedBuiltinModule,
    std: RootedBuiltinModule,
    empty_object_defs: RuntimeValueRoot,
    constant_object_defs: RuntimeValueRoot,
    reflection_annotator: RuntimeValueRoot,
    pure_if_runner: RuntimeValueRoot,
    pure_match_runner: RuntimeValueRoot,
    defined_or: RuntimeValueRoot,
    require_defined: RuntimeValueRoot,
    macro_environment: RuntimeValueRoot,
    effects: Mutex<HashMap<Key, RuntimeValueRoot>>,
}

// SAFETY: every retained Glam value has a compile-exhaustive visit below.
// The only mutable family member is `effects`; its insertion gateway builds
// and checks roots against the requesting compiler runtime before publication.
unsafe impl RuntimeCacheFamily for GCompilerValues {
    const CACHE_RECORD: RuntimeCacheFamilyRecord =
        RuntimeCacheFamilyRecord::same_runtime_roots("g compiler values", file!());

    fn visit_runtime_roots(&self, visit: &mut dyn FnMut(&RuntimeValueRoot)) {
        let Self {
            runtime: _,
            math,
            list,
            std,
            empty_object_defs,
            constant_object_defs,
            reflection_annotator,
            pure_if_runner,
            pure_match_runner,
            defined_or,
            require_defined,
            macro_environment,
            effects,
        } = self;
        math.visit_runtime_roots(visit);
        list.visit_runtime_roots(visit);
        std.visit_runtime_roots(visit);
        for root in [
            empty_object_defs,
            constant_object_defs,
            reflection_annotator,
            pure_if_runner,
            pure_match_runner,
            defined_or,
            require_defined,
            macro_environment,
        ] {
            visit(root);
        }
        let effect_roots = effects
            .lock()
            .expect("g compiler effect-value cache must not be poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for root in &effect_roots {
            visit(root);
        }
    }
}

trait EffectValueCache {
    fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId;
    fn effects(&self) -> &Mutex<HashMap<Key, RuntimeValueRoot>>;
}

impl EffectValueCache for GCompilerValues {
    fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        self.runtime
    }

    fn effects(&self) -> &Mutex<HashMap<Key, RuntimeValueRoot>> {
        &self.effects
    }
}

struct BuildingEffectValues<'a> {
    runtime: crate::runtime::EvaluationRuntimeId,
    effects: &'a Mutex<HashMap<Key, RuntimeValueRoot>>,
}

impl EffectValueCache for BuildingEffectValues<'_> {
    fn runtime_id(&self) -> crate::runtime::EvaluationRuntimeId {
        self.runtime
    }

    fn effects(&self) -> &Mutex<HashMap<Key, RuntimeValueRoot>> {
        self.effects
    }
}

fn cache(values: &CoreValueFactory) -> Arc<GCompilerValues> {
    values.cached(|| GCompilerValues::build(values))
}

pub(in crate::g_syntax) fn prepare(values: &CoreValueFactory) {
    drop(cache(values));
}

pub(in crate::g_syntax) fn defined_or_root(values: &CoreValueFactory) -> RuntimeValueRoot {
    cache(values).defined_or.clone()
}

pub(in crate::g_syntax) fn require_defined_root(values: &CoreValueFactory) -> RuntimeValueRoot {
    cache(values).require_defined.clone()
}

pub(in crate::g_syntax) fn fail_effect_root(values: &CoreValueFactory) -> RuntimeValueRoot {
    let compiler = cache(values);
    values.with_runtime_value_access(|access| {
        access.root_runtime_value(effect_path_value_with_cache(
            &access,
            compiler.as_ref(),
            &["fail"],
        ))
    })
}

fn with_values<R>(values: &CoreValueFactory, use_values: impl FnOnce(&GCompilerValues) -> R) -> R {
    use_values(&cache(values))
}

fn root_value(access: &RuntimeValueAccess<'_>, value: Value) -> RuntimeValueRoot {
    access.root_runtime_value(value)
}

fn project_value(access: &RuntimeValueAccess<'_>, root: &RuntimeValueRoot) -> Value {
    assert_eq!(
        root.runtime_id(),
        access.runtime_id(),
        "cached compiler value and requesting compiler must share one runtime"
    );
    root.clone_core_with(access)
}

fn project_module(access: &RuntimeValueAccess<'_>, module: &RootedBuiltinModule) -> BuiltinModule {
    BuiltinModule {
        value: project_value(access, &module.value),
        definitions: project_value(access, &module.definitions),
    }
}

fn build_module(
    values: &CoreValueFactory,
    constant_object_defs: &RuntimeValueRoot,
    build_value: impl for<'scope> FnOnce(&RuntimeValueAccess<'scope>) -> Value,
) -> RootedBuiltinModule {
    let value = values.construct_runtime_value_root(|access| build_value(access));
    let definitions = evaluate_closed(values, |access| {
        ResolvedExpr::apply(
            ResolvedExpr::Embedded(project_value(access, constant_object_defs)),
            [ResolvedExpr::Provided(project_value(access, &value))],
        )
    });
    RootedBuiltinModule { value, definitions }
}

impl GCompilerValues {
    fn build(values: &CoreValueFactory) -> Self {
        Self::build_with_root_checkpoint(values, || {})
    }

    fn build_with_root_checkpoint(
        values: &CoreValueFactory,
        mut rooted_checkpoint: impl FnMut(),
    ) -> Self {
        let effects = Mutex::new(HashMap::new());
        let build_cache = BuildingEffectValues {
            runtime: values.runtime_id(),
            effects: &effects,
        };
        let not = build_not(values, &build_cache);
        rooted_checkpoint();
        let could = build_could(values, &not);
        rooted_checkpoint();
        let constant_object_defs = build_constant_object_defs(values);
        rooted_checkpoint();

        let pure_if_runner = build_pure_conditional_runner(values, Builtin::IfResult);
        rooted_checkpoint();
        let defined_or = build_defined_or(values, &build_cache, &pure_if_runner);
        rooted_checkpoint();
        let math = build_module(values, &constant_object_defs, |_| {
            Value::Dict(
                Dict::new_sync()
                    .insert(name_as_key("floor"), Value::Builtin(Builtin::Floor))
                    .insert(name_as_key("mod"), Value::Builtin(Builtin::Mod)),
            )
        });
        rooted_checkpoint();
        let list = build_module(values, &constant_object_defs, |_| {
            Value::Dict(
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
            )
        });
        rooted_checkpoint();
        let std = build_module(values, &constant_object_defs, |access| {
            Value::Dict(
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
                    .insert(name_as_key("not"), project_value(access, &not))
                    .insert(name_as_key("could"), project_value(access, &could))
                    .insert(name_as_key("math"), project_value(access, &math.value))
                    .insert(name_as_key("list"), project_value(access, &list.value))
                    .insert(
                        name_as_key("eff"),
                        Value::Dict(
                            Dict::new_sync()
                                .insert(name_as_key("map"), Value::Builtin(Builtin::EffectMap)),
                        ),
                    ),
            )
        });
        rooted_checkpoint();
        let empty_object_defs = build_empty_object_defs(values);
        rooted_checkpoint();
        let reflection_annotator = build_reflection_annotator(values, &build_cache);
        rooted_checkpoint();
        let require_defined = build_require_defined(values, &defined_or);
        rooted_checkpoint();
        let pure_match_runner = build_pure_conditional_runner(values, Builtin::MatchResult);
        rooted_checkpoint();
        let macro_environment = build_macro_environment(values);
        rooted_checkpoint();
        Self {
            runtime: values.runtime_id(),
            math,
            list,
            std,
            empty_object_defs,
            constant_object_defs,
            reflection_annotator,
            require_defined,
            defined_or,
            pure_if_runner,
            pure_match_runner,
            macro_environment,
            effects,
        }
    }

    fn pure_conditional_runner(&self, selector: Builtin) -> &RuntimeValueRoot {
        match selector {
            Builtin::IfResult => &self.pure_if_runner,
            Builtin::MatchResult => &self.pure_match_runner,
            _ => unreachable!("pure conditional runner requires a result selector"),
        }
    }
}

pub(in crate::g_syntax) fn builtin_module(
    access: &RuntimeValueAccess<'_>,
    name: &str,
) -> Option<BuiltinModule> {
    with_values(access.values(), |compiler| match name {
        "math" => Some(project_module(access, &compiler.math)),
        "list" => Some(project_module(access, &compiler.list)),
        "std" | "prelude" => Some(project_module(access, &compiler.std)),
        _ => None,
    })
}

#[cfg(test)]
pub(in crate::g_syntax) fn builtin_list_module(values: &CoreValueFactory) -> Dict {
    values.with_runtime_value_access(|access| {
        with_values(values, |compiler| {
            value_dict(&project_value(&access, &compiler.list.value))
        })
    })
}

pub(in crate::g_syntax) fn empty_object_defs(access: &RuntimeValueAccess<'_>) -> Value {
    with_values(access.values(), |compiler| {
        project_value(access, &compiler.empty_object_defs)
    })
}

pub(in crate::g_syntax) fn constant_object_defs(
    access: &RuntimeValueAccess<'_>,
    value: Value,
) -> RuntimeValueRoot {
    let expression = ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(access.values(), |compiler| {
            project_value(access, &compiler.constant_object_defs)
        })),
        [ResolvedExpr::Provided(value)],
    );
    root_value(access, lower_resolved_expr_in(access, expression))
}

pub(in crate::g_syntax) fn reflection_annotator_resolved(
    access: &RuntimeValueAccess<'_>,
    guard: ResolvedExpr<Value>,
    final_defs: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(access.values(), |compiler| {
            project_value(access, &compiler.reflection_annotator)
        })),
        [guard, final_defs],
    )
}

pub(in crate::g_syntax) fn reflection_annotator_root(
    access: &RuntimeValueAccess<'_>,
    guard: Value,
    final_defs: Value,
) -> RuntimeValueRoot {
    let expression = reflection_annotator_resolved(
        access,
        ResolvedExpr::Provided(guard),
        ResolvedExpr::Provided(final_defs),
    );
    root_value(access, lower_resolved_expr_in(access, expression))
}

#[cfg(test)]
pub(in crate::g_syntax) fn reflection_annotator_value(
    values: &CoreValueFactory,
    guard: Value,
    final_defs: Value,
) -> Value {
    values.with_runtime_value_access(|access| {
        let root = reflection_annotator_root(&access, guard, final_defs);
        project_value(&access, &root)
    })
}

pub(in crate::g_syntax) fn run_pure_conditional_resolved(
    access: &RuntimeValueAccess<'_>,
    operation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(access.values(), |compiler| {
            project_value(access, compiler.pure_conditional_runner(Builtin::IfResult))
        })),
        [operation],
    )
}

pub(in crate::g_syntax) fn run_pure_match_resolved(
    access: &RuntimeValueAccess<'_>,
    search: ResolvedExpr<Value>,
    line: usize,
) -> ResolvedExpr<Value> {
    let cache = cache(access.values());
    let error_key = Key::abstract_global_path([
        "g_compiler".to_owned(),
        "match_exhausted".to_owned(),
        line.to_string(),
    ]);
    let candidate = root_value(
        access,
        Value::Lazy(crate::core::LazyValue::error_in(
            access,
            format!("match exhausted on line {line}"),
        )),
    );
    let error = cache
        .effects()
        .lock()
        .expect("g compiler effect-value cache must not be poisoned")
        .entry(error_key)
        .or_insert(candidate)
        .clone();
    let exhausted = effect_call(
        access,
        cache.as_ref(),
        "r",
        [ResolvedExpr::Embedded(project_value(access, &error))],
    );
    let operation = effect_call(
        access,
        cache.as_ref(),
        "cut",
        [effect_call(
            access,
            cache.as_ref(),
            "alt",
            [search, exhausted],
        )],
    );
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(access.values(), |compiler| {
            project_value(
                access,
                compiler.pure_conditional_runner(Builtin::MatchResult),
            )
        })),
        [operation],
    )
}

pub(in crate::g_syntax) fn run_pure_open_match_resolved(
    access: &RuntimeValueAccess<'_>,
    operation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin(access, Builtin::ListEffect, [operation])
}

/// Extends a file-provided macro environment through the language's ordinary
/// `with` operation, introducing the authoritative language declaration.
pub(in crate::g_syntax) fn macro_environment(
    access: &RuntimeValueAccess<'_>,
    base: Value,
    language: Value,
) -> RuntimeValueRoot {
    let expression = ResolvedExpr::apply(
        ResolvedExpr::Embedded(with_values(access.values(), |compiler| {
            project_value(access, &compiler.macro_environment)
        })),
        [
            ResolvedExpr::Provided(base),
            ResolvedExpr::Provided(language),
        ],
    );
    root_value(access, lower_resolved_expr_in(access, expression))
}

pub(in crate::g_syntax) fn effect_value(access: &RuntimeValueAccess<'_>, name: &str) -> Value {
    effect_path_value(access, &[name])
}

#[cfg(test)]
pub(in crate::g_syntax) fn effect_test_value(values: &CoreValueFactory, name: &str) -> Value {
    values.with_runtime_value_access(|access| effect_value(&access, name))
}

#[cfg(test)]
pub(in crate::g_syntax) fn run_pure_open_match_test_resolved(
    values: &CoreValueFactory,
    operation: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    values.with_runtime_value_access(|access| run_pure_open_match_resolved(&access, operation))
}

pub(in crate::g_syntax) fn effect_path_value(
    access: &RuntimeValueAccess<'_>,
    path: &[&str],
) -> Value {
    let cache = cache(access.values());
    effect_path_value_with_cache(access, cache.as_ref(), path)
}

fn effect_path_value_with_cache(
    access: &RuntimeValueAccess<'_>,
    cache: &dyn EffectValueCache,
    path: &[&str],
) -> Value {
    assert_eq!(
        cache.runtime_id(),
        access.runtime_id(),
        "a compiler effect cache cannot be accessed from another runtime"
    );
    let path: Arc<[Key]> = path.iter().map(Key::atom_from_text).collect();
    let cache_key = Key::List(path.clone());
    if let Some(root) = cache
        .effects()
        .lock()
        .expect("g compiler effect-value cache must not be poisoned")
        .get(&cache_key)
        .cloned()
    {
        return project_value(access, &root);
    }

    // Construction may allocate and, after the managed representation switch,
    // may require scoped value access. Races may build an equivalent closed
    // candidate twice; only publication is serialized.
    let candidate = build_effect_path_value(access, path);
    assert_eq!(
        candidate.runtime_id(),
        access.runtime_id(),
        "a cached compiler effect must belong to the requesting runtime"
    );
    let root = cache
        .effects()
        .lock()
        .expect("g compiler effect-value cache must not be poisoned")
        .entry(cache_key)
        .or_insert(candidate)
        .clone();
    project_value(access, &root)
}

#[cfg(test)]
fn value_dict(value: &Value) -> Dict {
    let Value::Dict(dict) = value else {
        unreachable!("cached built-in module must be a dictionary")
    };
    dict.clone()
}

pub(in crate::g_syntax) fn evaluate_closed(
    values: &CoreValueFactory,
    construct: impl for<'scope> FnOnce(&RuntimeValueAccess<'scope>) -> ResolvedExpr<Value>,
) -> RuntimeValueRoot {
    let input = values.construct_runtime_value_root(|access| {
        let expression = construct(access);
        lower_resolved_expr_in(access, expression)
    });
    crate::evaluation::EvalContext::private_closed(values.clone())
        .evaluate_root_whnf(input)
        .expect("closed g compiler helper must evaluate without session capabilities")
}

fn apply_builtin(
    _access: &RuntimeValueAccess<'_>,
    builtin: Builtin,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(ResolvedExpr::Embedded(Value::Builtin(builtin)), arguments)
}

fn effect_call(
    access: &RuntimeValueAccess<'_>,
    cache: &dyn EffectValueCache,
    name: &str,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(effect_path_value_with_cache(access, cache, &[name])),
        arguments,
    )
}

fn effect_path_call(
    access: &RuntimeValueAccess<'_>,
    cache: &dyn EffectValueCache,
    path: &[&str],
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(effect_path_value_with_cache(access, cache, path)),
        arguments,
    )
}

fn assert_unit(
    access: &RuntimeValueAccess<'_>,
    diagnostic_context: &'static str,
    value: ResolvedExpr<Value>,
    target: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    apply_builtin(
        access,
        Builtin::AssertUnit,
        [
            ResolvedExpr::Embedded(Value::binary_from_text(diagnostic_context)),
            value,
            target,
        ],
    )
}

fn effect_then(
    access: &RuntimeValueAccess<'_>,
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
        assert_unit(
            access,
            diagnostic_context,
            ResolvedExpr::Local(result),
            next,
        ),
    );
    locals.truncate(base_len);
    effect_call(access, cache, "seq", [operation, continuation])
}

fn build_effect_path_value(access: &RuntimeValueAccess<'_>, path: Arc<[Key]>) -> RuntimeValueRoot {
    let mut locals = ResolverContext::default();
    let api = locals.push_internal_binding("<effect-api>");
    let body = ResolvedExpr::Access {
        base: Box::new(ResolvedExpr::Local(api)),
        path: path.iter().cloned().map(ResolvedPathPart::Key).collect(),
    };
    let handler = lower_resolved_expr_in(access, ResolvedExpr::lambda(vec![api], body));
    access.root_runtime_value(Value::Dict(
        Dict::new_sync().insert(name_as_key("eff"), handler),
    ))
}

fn build_not(values: &CoreValueFactory, cache: &dyn EffectValueCache) -> RuntimeValueRoot {
    evaluate_closed(values, |access| {
        let mut locals = ResolverContext::default();
        let condition = locals.push_internal_binding("<not-condition>");
        let fail_operation =
            ResolvedExpr::Embedded(effect_path_value_with_cache(access, cache, &["fail"]));
        let true_operation =
            effect_call(access, cache, "r", [ResolvedExpr::Embedded(access.unit())]);
        let returned_failure = effect_call(access, cache, "r", [fail_operation]);
        let fail_if_condition_succeeds = effect_then(
            access,
            cache,
            ResolvedExpr::Local(condition),
            returned_failure,
            "`not` condition",
            &mut locals,
        );
        let succeed_if_condition_fails = effect_call(access, cache, "r", [true_operation]);
        let alternate = effect_call(
            access,
            cache,
            "alt",
            [fail_if_condition_succeeds, succeed_if_condition_fails],
        );
        let select_operation = effect_call(access, cache, "cut", [alternate]);
        let selected = locals.push_internal_binding("<selected-operation>");
        let run_selected_operation =
            ResolvedExpr::lambda(vec![selected], ResolvedExpr::Local(selected));
        let body = effect_call(
            access,
            cache,
            "seq",
            [select_operation, run_selected_operation],
        );
        ResolvedExpr::lambda(vec![condition], body)
    })
}

fn build_could(values: &CoreValueFactory, not: &RuntimeValueRoot) -> RuntimeValueRoot {
    evaluate_closed(values, |access| {
        let not = project_value(access, not);
        let mut locals = ResolverContext::default();
        let condition = locals.push_internal_binding("<could-condition>");
        let inner = ResolvedExpr::apply(
            ResolvedExpr::Embedded(access.duplicate_value(&not)),
            [ResolvedExpr::Local(condition)],
        );
        ResolvedExpr::lambda(
            vec![condition],
            ResolvedExpr::apply(ResolvedExpr::Embedded(not), [inner]),
        )
    })
}

fn build_defined_or(
    values: &CoreValueFactory,
    cache: &dyn EffectValueCache,
    pure_if_runner: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    evaluate_closed(values, |access| {
        let mut locals = ResolverContext::default();
        let fallback = locals.push_internal_binding("<defined-fallback>");
        let candidate = locals.push_internal_binding("<defined-candidate>");
        let is_undefined = apply_builtin(
            access,
            Builtin::PatternDictIsEmpty,
            [ResolvedExpr::Local(candidate)],
        );
        let use_fallback = effect_call(access, cache, "r", [ResolvedExpr::Local(fallback)]);
        let undefined_branch = effect_then(
            access,
            cache,
            is_undefined,
            use_fallback,
            "defined-or condition",
            &mut locals,
        );
        let defined_branch = effect_call(access, cache, "r", [ResolvedExpr::Local(candidate)]);
        let choice = effect_call(
            access,
            cache,
            "cut",
            [effect_call(
                access,
                cache,
                "alt",
                [undefined_branch, defined_branch],
            )],
        );
        let selected = ResolvedExpr::apply(
            ResolvedExpr::Embedded(project_value(access, pure_if_runner)),
            [choice],
        );
        ResolvedExpr::lambda(vec![fallback, candidate], selected)
    })
}

fn build_require_defined(
    values: &CoreValueFactory,
    defined_or: &RuntimeValueRoot,
) -> RuntimeValueRoot {
    evaluate_closed(values, |access| {
        let mut locals = ResolverContext::default();
        let name = locals.push_internal_binding("<required-name>");
        let candidate = locals.push_internal_binding("<required-candidate>");
        let singleton = |key: &str, value| {
            apply_builtin(
                access,
                Builtin::DictSingleton,
                [
                    ResolvedExpr::Embedded(Value::Atom(atom_from_str(key))),
                    value,
                ],
            )
        };
        let message = singleton(
            "msg",
            singleton(
                "text",
                ResolvedExpr::Embedded(Value::binary_from_text("required value is undefined")),
            ),
        );
        let failure = apply_builtin(
            access,
            Builtin::DictUnion,
            [message, singleton("name", ResolvedExpr::Local(name))],
        );
        let failure = apply_builtin(
            access,
            Builtin::Anno,
            [
                ResolvedExpr::Embedded(Value::Atom(atom_from_str("error"))),
                failure,
            ],
        );
        let required = ResolvedExpr::apply(
            ResolvedExpr::Embedded(project_value(access, defined_or)),
            [failure, ResolvedExpr::Local(candidate)],
        );
        ResolvedExpr::lambda(vec![name, candidate], required)
    })
}

fn build_pure_conditional_runner(values: &CoreValueFactory, selector: Builtin) -> RuntimeValueRoot {
    assert!(matches!(selector, Builtin::IfResult | Builtin::MatchResult));
    evaluate_closed(values, |access| {
        let mut locals = ResolverContext::default();
        let operation = locals.push_internal_binding("<conditional-operation>");
        let results = apply_builtin(
            access,
            Builtin::ListEffect,
            [ResolvedExpr::Local(operation)],
        );
        let selected = apply_builtin(access, selector, [results]);
        ResolvedExpr::lambda(vec![operation], selected)
    })
}

fn build_macro_environment(values: &CoreValueFactory) -> RuntimeValueRoot {
    evaluate_closed(values, |access| {
        let mut locals = ResolverContext::default();
        let environment_parameter = locals.push_internal_binding("<macro-environment>");
        let language_parameter = locals.push_internal_binding("<macro-language>");
        let prior = locals.push_internal_binding("<macro-environment-prior>");
        let final_environment = locals.push_internal_binding("<macro-environment-final>");

        let singleton = |key: &str, value| {
            apply_builtin(
                access,
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
            access,
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
            access,
            Builtin::Anno,
            [assertion, ResolvedExpr::Local(language_parameter)],
        );
        let extended = apply_builtin(
            access,
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
            access,
            Builtin::ObjectWithDefs,
            [ResolvedExpr::Local(environment_parameter), definitions],
        );
        ResolvedExpr::lambda(vec![environment_parameter, language_parameter], result)
    })
}

fn build_empty_object_defs(values: &CoreValueFactory) -> RuntimeValueRoot {
    evaluate_closed(values, |access| {
        let mut locals = ResolverContext::default();
        let prior_self = locals.push_internal_binding("<object-prior-self>");
        let final_self = locals.push_internal_binding("<object-final-self>");
        let without_spec = apply_builtin(
            access,
            Builtin::DictUpdate,
            [
                ResolvedExpr::List(vec![ResolvedExpr::Embedded(Value::Atom(atom_from_str(
                    "spec",
                )))]),
                ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
                ResolvedExpr::Local(prior_self),
            ],
        );
        ResolvedExpr::lambda(vec![prior_self, final_self], without_spec)
    })
}

fn build_constant_object_defs(values: &CoreValueFactory) -> RuntimeValueRoot {
    evaluate_closed(values, |_| {
        let mut locals = ResolverContext::default();
        let value = locals.push_internal_binding("<constant-object-definitions>");
        let prior_self = locals.push_internal_binding("<object-prior-self>");
        let final_self = locals.push_internal_binding("<object-final-self>");
        ResolvedExpr::lambda(
            vec![value, prior_self, final_self],
            ResolvedExpr::Local(value),
        )
    })
}

fn build_reflection_annotator(
    values: &CoreValueFactory,
    cache: &dyn EffectValueCache,
) -> RuntimeValueRoot {
    evaluate_closed(values, |access| {
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
            access,
            cache,
            item_field("value"),
            effect_call(access, cache, "r", [ResolvedExpr::Embedded(access.unit())]),
            "`refl.*` task result",
            &mut locals,
        );
        let handle = locals.push_internal_binding("<reflection-task-handle>");
        let task_record = apply_builtin(
            access,
            Builtin::DictUnion,
            [
                apply_builtin(
                    access,
                    Builtin::DictSingleton,
                    [
                        ResolvedExpr::Embedded(Value::Atom(atom_from_str("key"))),
                        item_field("key"),
                    ],
                ),
                apply_builtin(
                    access,
                    Builtin::DictSingleton,
                    [
                        ResolvedExpr::Embedded(Value::Atom(atom_from_str("task"))),
                        ResolvedExpr::Local(handle),
                    ],
                ),
            ],
        );
        let launch_item = effect_call(
            access,
            cache,
            "seq",
            [
                effect_path_call(access, cache, &["task", "new"], [require_unit]),
                ResolvedExpr::lambda(vec![handle], effect_call(access, cache, "r", [task_record])),
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
            access,
            cache,
            &["heap", "set"],
            [state_path("tasks"), ResolvedExpr::Local(records)],
        );
        let map_and_store = effect_call(
            access,
            cache,
            "cut",
            [effect_call(
                access,
                cache,
                "seq",
                [mapped, ResolvedExpr::lambda(vec![records], store_records)],
            )],
        );
        let scanner = effect_call(
            access,
            cache,
            "seq",
            [
                effect_call(access, cache, "dict_items", [final_refl]),
                ResolvedExpr::lambda(vec![items], map_and_store),
            ],
        );

        let scanner_handle = locals.push_internal_binding("<reflection-scanner-handle>");
        let launch_and_remember = effect_call(
            access,
            cache,
            "seq",
            [
                effect_path_call(access, cache, &["task", "new"], [scanner]),
                ResolvedExpr::lambda(
                    vec![scanner_handle],
                    effect_path_call(
                        access,
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
            access,
            cache,
            guard_is_empty,
            launch_and_remember,
            "automatic reflection boundary claim test",
            &mut locals,
        );
        let already_started =
            effect_call(access, cache, "r", [ResolvedExpr::Embedded(access.unit())]);
        let choose = effect_call(access, cache, "alt", [start_if_missing, already_started]);
        let ensure_tasks = effect_call(
            access,
            cache,
            "cut",
            [effect_call(
                access,
                cache,
                "seq",
                [
                    effect_path_call(access, cache, &["heap", "get"], [state_path("claim")]),
                    ResolvedExpr::lambda(vec![existing], choose),
                ],
            )],
        );
        let annotation = apply_builtin(
            access,
            Builtin::DictSingleton,
            [
                ResolvedExpr::Embedded(Value::Atom(atom_from_str("refl"))),
                ensure_tasks,
            ],
        );
        let annotated = apply_builtin(
            access,
            Builtin::Anno,
            [annotation, ResolvedExpr::Local(target)],
        );
        ResolvedExpr::lambda(vec![guard, final_defs, target], annotated)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::number::Number;
    use std::sync::Barrier;

    fn fresh_test_values() -> CoreValueFactory {
        CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::compiler_test_values(),
        )
    }

    fn project_test_value(values: &CoreValueFactory, root: &RuntimeValueRoot) -> Value {
        values.with_runtime_value_access(|access| project_value(&access, root))
    }

    fn effect_test_value(values: &CoreValueFactory, name: &str) -> Value {
        prepare(values);
        values.with_runtime_value_access(|access| effect_value(&access, name))
    }

    fn builtin_test_module(values: &CoreValueFactory, name: &str) -> Option<BuiltinModule> {
        prepare(values);
        values.with_runtime_value_access(|access| builtin_module(&access, name))
    }

    fn evaluate_test_expression(
        values: &CoreValueFactory,
        expression: ResolvedExpr<Value>,
    ) -> RuntimeValueRoot {
        evaluate_closed(values, |_| expression)
    }

    fn apply_test_closed(
        values: &CoreValueFactory,
        function: Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> RuntimeValueRoot {
        evaluate_closed(values, |_| {
            ResolvedExpr::apply(
                ResolvedExpr::Embedded(function),
                arguments.into_iter().map(ResolvedExpr::Provided),
            )
        })
    }

    fn macro_test_environment(
        values: &CoreValueFactory,
        base: Value,
        language: Value,
    ) -> RuntimeValueRoot {
        prepare(values);
        values.with_runtime_value_access(|access| macro_environment(&access, base, language))
    }

    #[test]
    fn closed_compiler_values_are_cached_after_exposing_their_functions() {
        let values = crate::compiler::test_value_factory();
        let first_effect = effect_test_value(&values, "compiler_cache_test");
        let second_effect = effect_test_value(&values, "compiler_cache_test");
        values.assert_same_representation_for_test(&first_effect, &second_effect);
        assert!(matches!(first_effect, Value::Dict(_)));

        let first_std = builtin_test_module(&values, "std").expect("std should be built in");
        let second_std = builtin_test_module(&values, "std").expect("std should remain built in");
        values.assert_same_representation_for_test(&first_std.value, &second_std.value);
        values.assert_same_representation_for_test(&first_std.definitions, &second_std.definitions);
        assert!(matches!(first_std.definitions, Value::Function(_)));
        with_values(&values, |compiler| {
            assert!(matches!(
                project_test_value(&values, &compiler.reflection_annotator),
                Value::Function(_)
            ));
            assert!(matches!(
                project_test_value(&values, &compiler.pure_if_runner),
                Value::Function(_)
            ));
            assert!(matches!(
                project_test_value(&values, &compiler.pure_match_runner),
                Value::Function(_)
            ));
            assert!(matches!(
                project_test_value(&values, &compiler.macro_environment),
                Value::Function(_)
            ));
        });
    }

    #[test]
    fn compiler_candidate_survives_collection_between_rooted_build_steps() {
        let values = fresh_test_values();
        let mut checkpoints = 0usize;
        let compiler = GCompilerValues::build_with_root_checkpoint(&values, || {
            assert!(
                !crate::core::thread_has_runtime_value_access_for_test(),
                "compiler build checkpoints must remain outside managed access"
            );
            values
                .collect_managed_for_test()
                .expect("each completed compiler helper must root its managed result");
            checkpoints += 1;
        });

        assert_eq!(checkpoints, 13);
        values
            .collect_managed_for_test()
            .expect("the unpublished complete candidate must retain all of its roots");
        let mut roots = 0usize;
        compiler.visit_runtime_roots(&mut |root| {
            assert_eq!(root.runtime_id(), values.runtime_id());
            roots += 1;
        });
        assert!(roots >= 14);
        assert!(matches!(
            project_test_value(&values, &compiler.std.value),
            Value::Dict(_)
        ));
        assert!(matches!(
            project_test_value(&values, &compiler.macro_environment),
            Value::Function(_)
        ));
    }

    #[test]
    fn compiler_cache_publishes_complete_rooted_bundle() {
        let values = fresh_test_values();
        let registrations_before = values.managed_root_registrations_for_test();
        let compiler = cache(&values);
        let registrations_after_build = values.managed_root_registrations_for_test();
        assert!(
            registrations_after_build > registrations_before,
            "initial compiler cache construction should publish its durable roots"
        );
        let cached = cache(&values);
        assert!(Arc::ptr_eq(&compiler, &cached));
        assert_eq!(
            values.managed_root_registrations_for_test(),
            registrations_after_build,
            "reusing the installed compiler cache must not register replacement roots"
        );
        let roots = [
            &compiler.math.value,
            &compiler.math.definitions,
            &compiler.list.value,
            &compiler.list.definitions,
            &compiler.std.value,
            &compiler.std.definitions,
            &compiler.empty_object_defs,
            &compiler.constant_object_defs,
            &compiler.reflection_annotator,
            &compiler.pure_if_runner,
            &compiler.pure_match_runner,
            &compiler.defined_or,
            &compiler.require_defined,
            &compiler.macro_environment,
        ];
        assert!(
            roots
                .iter()
                .all(|root| root.runtime_id() == values.runtime_id())
        );
        assert!(
            compiler
                .effects
                .lock()
                .expect("compiler effect cache should not be poisoned")
                .values()
                .all(|root| root.runtime_id() == values.runtime_id())
        );
        let live = values
            .collect_managed_for_test()
            .expect("closed cache construction must release managed access");
        assert!(live.root_entries() >= roots.len());
        assert!(roots.iter().all(|root| matches!(
            project_test_value(&values, root),
            Value::Dict(_) | Value::Function(_)
        )));
    }

    #[test]
    fn closed_evaluation_result_is_owned_across_return_publication() {
        let values = fresh_test_values();

        let immediate = evaluate_test_expression(
            &values,
            ResolvedExpr::Embedded(Value::Number(Number::integer(42))),
        );
        values
            .collect_managed_for_test()
            .expect("an immediate closed result should not retain managed state");
        values
            .collect_managed_for_test()
            .expect("publishing an immediate result should remain traceable");
        values.assert_same_representation_for_test(
            &project_test_value(&values, &immediate),
            &Value::Number(Number::integer(42)),
        );

        let mut locals = ResolverContext::default();
        let argument = locals.push_internal_binding("<closed-identity-argument>");
        let identity = evaluate_test_expression(
            &values,
            ResolvedExpr::lambda(vec![argument], ResolvedExpr::Local(argument)),
        );
        values
            .collect_managed_for_test()
            .expect("the return/publication gap should be a valid collection boundary");
        values
            .collect_managed_for_test()
            .expect("a published managed closed result must remain traceable");
        assert!(matches!(
            project_test_value(&values, &identity),
            Value::Function(_)
        ));
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
        let _ = effect_test_value(&compilation, "r");
        let _ = effect_test_value(&compilation, "seq");
        let _ = builtin_test_module(&compilation, "std");
        assert_eq!(compilation.extension_lookup_count() - before, 1);
    }

    #[test]
    fn compiler_cache_construction_is_safe_under_forced_concurrency() {
        const THREADS: usize = 8;

        let values = fresh_test_values();
        let barrier = Arc::new(Barrier::new(THREADS));
        let builders = (0..THREADS)
            .map(|_| {
                let values = values.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    values.cached(|| {
                        barrier.wait();
                        GCompilerValues::build(&values)
                    })
                })
            })
            .collect::<Vec<_>>();

        let cached = builders
            .into_iter()
            .map(|builder| {
                builder
                    .join()
                    .expect("compiler cache builder should not panic")
            })
            .collect::<Vec<_>>();
        assert!(
            cached
                .iter()
                .all(|candidate| Arc::ptr_eq(candidate, &cached[0])),
            "all racing builders must receive the installed compiler bundle"
        );
    }

    #[test]
    fn cached_macro_environment_is_safe_under_forced_concurrency() {
        const THREADS: usize = 8;

        let values = fresh_test_values();
        let function = project_test_value(&values, &cache(&values).macro_environment);
        let barrier = Arc::new(Barrier::new(THREADS));
        let evaluators = (0..THREADS)
            .map(|index| {
                let values = values.clone();
                let function = function.duplicate_for_test(&values);
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let base = Value::Dict(Dict::new_sync().insert(
                        name_as_key("existing"),
                        Value::Number(Number::integer(index as i64)),
                    ));
                    barrier.wait();
                    let environment =
                        apply_test_closed(&values, function, [base, Value::binary_from_text("g0")]);
                    let environment = project_test_value(&values, &environment);
                    evaluate_test_expression(
                        &values,
                        ResolvedExpr::Access {
                            base: Box::new(ResolvedExpr::Provided(environment)),
                            path: vec![ResolvedPathPart::Key(name_as_key("language"))],
                        },
                    )
                })
            })
            .collect::<Vec<_>>();

        for evaluator in evaluators {
            let result = evaluator
                .join()
                .expect("cached compiler helper evaluation should not panic");
            values.assert_same_representation_for_test(
                &project_test_value(&values, &result),
                &Value::binary_from_text("g0"),
            );
        }
    }

    #[test]
    fn macro_environment_extends_a_dictionary_with_ordinary_introduction_rules() {
        let values = crate::compiler::test_value_factory();
        let base = Value::Dict(
            Dict::new_sync().insert(name_as_key("existing"), Value::Number(Number::integer(1))),
        );
        let environment = macro_test_environment(&values, base, Value::binary_from_text("g0"));
        let environment_value = project_test_value(&values, &environment);

        let existing = evaluate_test_expression(
            &values,
            ResolvedExpr::Access {
                base: Box::new(ResolvedExpr::Provided(
                    environment_value.duplicate_for_test(&values),
                )),
                path: vec![ResolvedPathPart::Key(name_as_key("existing"))],
            },
        );
        let language = evaluate_test_expression(
            &values,
            ResolvedExpr::Access {
                base: Box::new(ResolvedExpr::Provided(environment_value)),
                path: vec![ResolvedPathPart::Key(name_as_key("language"))],
            },
        );
        values.assert_same_representation_for_test(
            &project_test_value(&values, &existing),
            &Value::Number(Number::integer(1)),
        );
        values.assert_same_representation_for_test(
            &project_test_value(&values, &language),
            &Value::binary_from_text("g0"),
        );
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
            ResolvedExpr::apply(
                ResolvedExpr::Embedded(Value::Builtin(Builtin::DictUpdate)),
                [
                    ResolvedExpr::List(vec![ResolvedExpr::Embedded(Value::Atom(atom_from_str(
                        "adapted",
                    )))]),
                    language,
                    ResolvedExpr::Local(base),
                ],
            ),
        );
        let object = evaluate_test_expression(
            &values,
            ResolvedExpr::apply(
                ResolvedExpr::Embedded(Value::Builtin(Builtin::ObjectInstanceFromParts)),
                [
                    ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
                    ResolvedExpr::List(Vec::new()),
                    definitions,
                ],
            ),
        );
        let object_value = project_test_value(&values, &object);
        let environment =
            macro_test_environment(&values, object_value, Value::binary_from_text("g0"));
        let environment_value = project_test_value(&values, &environment);
        let adapted = evaluate_test_expression(
            &values,
            ResolvedExpr::Access {
                base: Box::new(ResolvedExpr::Provided(environment_value)),
                path: vec![ResolvedPathPart::Key(name_as_key("adapted"))],
            },
        );
        values.assert_same_representation_for_test(
            &project_test_value(&values, &adapted),
            &Value::binary_from_text("g0"),
        );
    }
}

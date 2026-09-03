use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use bytes::Bytes;

use super::Error;
use super::error::net_build_error;
use crate::core::Value as CoreValue;
use crate::core::{
    Builtin, CoreValueFactory, Dict, EvaluationFailure, Key, LazyValue, List, PromisedValue,
    RuntimeValueAccess, RuntimeValueObserver,
};
use crate::core_net::{CoreDataKey, CoreSpecialization};
use crate::interaction_net::{NetBuilder as CoreNetBuilder, Port as CorePort};
use crate::number::Number;
use crate::runtime::{EvaluationRuntimeId, RuntimeValueRoot};

#[cfg(test)]
mod access_inventory;
#[cfg(test)]
mod prototype;
#[cfg(test)]
mod scoped_construction_tests {
    use super::*;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};
    use glam_gc::CollectionError;

    fn values() -> Values {
        Values::from_core_factory(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    #[test]
    fn recursive_construction_reuses_one_mutator() {
        let values = values();

        let constructed = values.with_access(|access| {
            assert!(matches!(
                values.core.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
            let target = access.wrap(CoreValue::Number(Number::integer(42)));
            let annotation = access.atom_from_text("array");
            let annotated = access
                .anno(&annotation, &target)
                .expect("nested annotation construction should succeed");
            let key = access.atom_from_text("member");
            let selected = access
                .access(&annotated, &key)
                .expect("nested access construction should succeed");
            assert!(matches!(
                values.core.collect_managed_for_test(),
                Err(CollectionError::ActiveMutator)
            ));
            selected
        });

        assert_eq!(constructed.runtime_id(), values.runtime_id());
        values
            .core
            .collect_managed_for_test()
            .expect("the one outer construction region must release its mutator");
    }
}

/// An assembly-time value rooted in exactly one [`EvaluationRuntime`].
///
/// Values cannot be transferred between runtimes. Construct them through
/// [`Values`], obtained from the target runtime or assembler.
#[derive(Clone)]
pub struct Value(pub(super) RuntimeValueRoot);

/// A runtime-local value whose outer shell has reached weak-head normal form.
///
/// Nested dictionary members, list elements, function results, and other
/// contained values may remain lazy. Converting this witness back to [`Value`]
/// discards only the static outer-WHNF guarantee. The witness carries weak,
/// non-retaining authority to attempt observation through its issuing runtime;
/// extraction fails after that value domain disappears.
#[derive(Clone)]
pub struct EvaluatedValue {
    value: Value,
    observer: RuntimeValueObserver,
}

#[cfg(test)]
mod facade_trait_contract_tests {
    use super::{EvaluatedValue, Value};

    // Trait selection becomes ambiguous if an opaque public handle regains a
    // representation-derived relation. This gives the facade a compile-time
    // negative contract without exposing a semantic equality operation.
    macro_rules! assert_does_not_implement {
        ($module:ident, $type:ident, $trait:path) => {
            mod $module {
                use super::$type;

                trait AmbiguousIfImplemented<Discriminator> {
                    fn verify() {}
                }

                struct Implemented;

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
                impl<T: ?Sized + $trait> AmbiguousIfImplemented<Implemented> for T {}

                const _: fn() = || {
                    <$type as AmbiguousIfImplemented<_>>::verify();
                };
            }
        };
    }

    assert_does_not_implement!(value_not_partial_eq, Value, PartialEq);
    assert_does_not_implement!(value_not_eq, Value, Eq);
    assert_does_not_implement!(value_not_partial_ord, Value, PartialOrd);
    assert_does_not_implement!(value_not_ord, Value, Ord);
    assert_does_not_implement!(value_not_hash, Value, std::hash::Hash);
    assert_does_not_implement!(evaluated_value_not_partial_eq, EvaluatedValue, PartialEq);
    assert_does_not_implement!(evaluated_value_not_eq, EvaluatedValue, Eq);
    assert_does_not_implement!(evaluated_value_not_partial_ord, EvaluatedValue, PartialOrd);
    assert_does_not_implement!(evaluated_value_not_ord, EvaluatedValue, Ord);
    assert_does_not_implement!(evaluated_value_not_hash, EvaluatedValue, std::hash::Hash);
}

/// Runtime-selected construction service for Glam values.
///
/// Every value produced here carries this factory's runtime provenance.
/// Composite constructors reject members from another runtime instead of
/// implicitly adopting them.
#[derive(Clone)]
pub struct Values {
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) core: CoreValueFactory,
}

/// One bounded, runtime-qualified public-value construction region.
///
/// This carrier is private and lifetime-bound: public constructors may batch
/// nested semantic helpers through it, but callbacks and durable public state
/// retain only [`Values`] or rooted [`Value`] handles.
pub(super) struct ScopedValues<'scope> {
    owner: &'scope Values,
    access: RuntimeValueAccess<'scope>,
}

/// Host-owned domain for unforgeable values exchanged with one effect
/// specialization.
///
/// Glam code may carry and return an issued token but cannot inspect its Rust
/// payload or manufacture another token in the same domain. Domains are local
/// to one evaluation runtime and are intended for short-lived handler
/// capabilities, not persistence or IPC.
pub struct EffectTokenDomain<T> {
    values: Values,
    pub(super) state: Arc<EffectTokenDomainState<T>>,
}

pub(super) struct EffectTokenDomainState<T> {
    next_id: AtomicU64,
    payloads: Mutex<HashMap<NonZeroU64, Arc<T>>>,
}

struct EffectToken<T> {
    id: NonZeroU64,
    domain: Weak<EffectTokenDomainState<T>>,
}

impl<T> Clone for EffectTokenDomain<T> {
    fn clone(&self) -> Self {
        Self {
            values: self.values.clone(),
            state: self.state.clone(),
        }
    }
}

impl<T> fmt::Debug for EffectTokenDomain<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectTokenDomain")
            .field("runtime", &self.values.runtime_id())
            .finish_non_exhaustive()
    }
}

impl<T> EffectTokenDomain<T>
where
    T: Send + Sync + 'static,
{
    /// Creates a revocable token domain in the value factory's runtime.
    pub fn new(values: &Values) -> Self {
        Self {
            values: values.clone(),
            state: Arc::new(EffectTokenDomainState {
                next_id: AtomicU64::new(1),
                payloads: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Issues one opaque token carrying `payload` in this domain.
    pub fn issue(&self, payload: T) -> Value {
        let id = NonZeroU64::new(self.state.next_id.fetch_add(1, Ordering::Relaxed))
            .expect("effect token IDs exhausted for one domain");
        let replaced = self
            .state
            .payloads
            .lock()
            .expect("effect token domain mutex should not be poisoned")
            .insert(id, Arc::new(payload));
        assert!(replaced.is_none(), "effect token IDs remain unique");
        self.values
            .wrap(CoreValue::Opaque(crate::core::OpaqueValue::new(Arc::new(
                EffectToken {
                    id,
                    domain: Arc::downgrade(&self.state),
                },
            ))))
    }

    /// Resolves a token only when it was issued by this exact domain.
    ///
    /// A same-runtime value of another kind or from another token domain is a
    /// successful miss. A value from another runtime is rejected.
    pub fn resolve(&self, token: &EvaluatedValue) -> Result<Option<Arc<T>>, Error> {
        token.require_observer(&self.values)?;
        token.with_core(|value| {
            let CoreValue::Opaque(token) = value else {
                return None;
            };
            let token = token.downcast::<EffectToken<T>>()?;
            if !Weak::ptr_eq(&token.domain, &Arc::downgrade(&self.state)) {
                return None;
            }
            self.state
                .payloads
                .lock()
                .expect("effect token domain mutex should not be poisoned")
                .get(&token.id)
                .cloned()
        })
    }
}

impl<T> Drop for EffectToken<T> {
    fn drop(&mut self) {
        let Some(domain) = self.domain.upgrade() else {
            return;
        };
        domain
            .payloads
            .lock()
            .expect("effect token domain mutex should not be poisoned")
            .remove(&self.id);
    }
}

// SAFETY: the token stores only a scalar ID and a weak route to its external
// domain. The generic payload remains in the domain map rather than inside the
// opaque value, so no Glam value or managed pointer is hidden by type erasure.
// Token retirement remains an external lifecycle operation for I9/I10.
unsafe impl<T> crate::core::OpaquePayloadFamily for EffectToken<T>
where
    T: Send + Sync + 'static,
{
    const PAYLOAD_RECORD: crate::core::OpaquePayloadRecord =
        crate::core::OpaquePayloadRecord::external("revocable effect token", "src/api/value.rs");
}

impl Values {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn core(&self) -> &CoreValueFactory {
        &self.core
    }

    pub(super) fn with_access<R>(
        &self,
        operation: impl for<'scope> FnOnce(ScopedValues<'scope>) -> R,
    ) -> R {
        self.core.with_runtime_value_access(|access| {
            debug_assert!(access.belongs_to(&self.core));
            operation(ScopedValues {
                owner: self,
                access,
            })
        })
    }

    pub(crate) fn wrap(&self, value: CoreValue) -> Value {
        self.with_access(|values| values.wrap(value))
    }

    pub(crate) fn from_core_factory(core: CoreValueFactory) -> Self {
        Self {
            runtime: core.runtime_id(),
            core,
        }
    }

    pub(crate) fn require(&self, value: &Value) -> Result<(), Error> {
        value.require_runtime(self.runtime)
    }

    pub(crate) fn clone_core(&self, value: &Value) -> Result<CoreValue, Error> {
        self.with_access(|values| values.clone_core(value))
    }

    pub(crate) fn clone_runtime_root(&self, value: &RuntimeValueRoot) -> Result<CoreValue, Error> {
        if value.runtime_id() != self.runtime {
            return Err(Error::new(format!(
                "value belongs to evaluation runtime {}, expected evaluation runtime {}",
                value.runtime_id().get(),
                self.runtime.get()
            )));
        }
        Ok(self
            .core
            .with_runtime_value_access(|access| value.clone_core_with(&access)))
    }

    /// Injects host bytes as compact binary data.
    pub fn bytes(&self, bytes: impl Into<Bytes>) -> Value {
        self.with_access(|values| values.wrap(CoreValue::Binary(bytes.into())))
    }

    pub fn text(&self, text: impl AsRef<str>) -> Value {
        self.with_access(|values| values.wrap(CoreValue::binary_from_text(text.as_ref())))
    }

    pub fn atom_from_text(&self, text: impl AsRef<str>) -> Value {
        self.with_access(|values| values.atom_from_text(text.as_ref()))
    }

    pub fn integer(&self, value: i64) -> Value {
        self.with_access(|values| values.wrap(CoreValue::Number(Number::integer(value))))
    }

    /// Returns Glam's cached semantic unit value `()`.
    pub fn unit(&self) -> Value {
        self.with_access(|values| values.wrap(values.core().unit()))
    }

    pub fn rational(&self, numerator: i64, denominator: i64) -> Option<Value> {
        self.with_access(|values| {
            Number::from_ratio_i64(numerator, denominator)
                .map(|number| values.wrap(CoreValue::Number(number)))
        })
    }

    pub fn number_from_f64(&self, value: f64) -> Option<Value> {
        self.with_access(|values| {
            Number::from_f64(value).map(|number| values.wrap(CoreValue::Number(number)))
        })
    }

    pub fn number_from_text(&self, text: impl AsRef<str>) -> Result<Value, Error> {
        self.with_access(|values| {
            Number::parse(text.as_ref())
                .map(|number| values.wrap(CoreValue::Number(number)))
                .map_err(Error::new)
        })
    }

    pub fn list(&self, values: impl IntoIterator<Item = Value>) -> Result<Value, Error> {
        self.with_access(|access| {
            let values = values
                .into_iter()
                .map(|value| access.clone_core(&value))
                .collect::<Result<Vec<_>, Error>>()?;
            Ok(access.wrap(CoreValue::List(List::from_values(values))))
        })
    }

    pub fn record<I, S>(&self, entries: I) -> Result<Value, Error>
    where
        I: IntoIterator<Item = (S, Value)>,
        S: AsRef<str>,
    {
        self.with_access(|values| {
            let mut dict = Dict::new_sync();
            for (name, value) in entries {
                dict = dict.insert(Key::atom_from_text(name), values.clone_core(&value)?);
            }
            Ok(values.wrap(CoreValue::Dict(dict)))
        })
    }

    pub fn dictionary(
        &self,
        entries: impl IntoIterator<Item = (Value, Value)>,
    ) -> Result<Value, Error> {
        self.with_access(|values| {
            let mut dict = Dict::new_sync();
            for (key, value) in entries {
                let key = Key::from_value(values.core_value(&key)?)
                    .ok_or_else(|| Error::new("dictionary key is not immediately keyable"))?;
                dict = dict.insert(key, values.clone_core(&value)?);
            }
            Ok(values.wrap(CoreValue::Dict(dict)))
        })
    }

    /// Constructs Glam's immediate empty dictionary/undefined value.
    pub fn empty_dict(&self) -> Value {
        self.with_access(|values| values.wrap(CoreValue::Dict(Dict::new_sync())))
    }

    /// Constructs the ordinary lazy `base.[key]` semantic accessor.
    ///
    /// Neither operand is evaluated while constructing the value. Demand on
    /// the returned value evaluates the key and follows the same dictionary
    /// access semantics as `.g` source, including returning `{}` for a
    /// missing key.
    pub fn access(&self, base: &Value, key: Value) -> Result<Value, Error> {
        self.with_access(|values| values.access(base, &key))
    }

    /// Constructs the ordinary lazy `anno Annotation Target` semantic value.
    ///
    /// Annotation interpretation occurs only when the returned value is
    /// demanded; this method does not provide a separate host-side annotation
    /// interpreter.
    pub fn anno(&self, annotation: Value, target: Value) -> Result<Value, Error> {
        self.with_access(|values| values.anno(&annotation, &target))
    }

    /// Constructs ordinary left-associated application without demanding the
    /// function or any argument.
    ///
    /// An empty argument sequence returns the original value. All values must
    /// belong to this factory's runtime.
    pub fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        self.with_access(|values| values.apply(function, arguments))
    }

    /// Constructs successive ordinary dictionary accesses without demanding
    /// the root or keys.
    pub fn access_path(
        &self,
        root: &Value,
        keys: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        self.with_access(|values| {
            values.require(root)?;
            keys.into_iter()
                .try_fold(root.clone(), |value, key| values.access(&value, &key))
        })
    }

    /// Constructs successive atom-key accesses from complete names.
    ///
    /// Dots inside a name remain part of that one name; this method does not
    /// parse dotted path text.
    pub fn access_names<I, S>(&self, root: &Value, names: I) -> Result<Value, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.with_access(|values| {
            values.require(root)?;
            names.into_iter().try_fold(root.clone(), |value, name| {
                let key = values.atom_from_text(name.as_ref());
                values.access(&value, &key)
            })
        })
    }

    /// Constructs the ordinary `slice Start End Value` computation without
    /// demanding the list or binary value.
    pub fn list_slice(&self, value: &Value, range: Range<usize>) -> Result<Value, Error> {
        self.with_access(|values| {
            Ok(values.wrap(CoreValue::builtin_call(
                values.core(),
                Builtin::Slice,
                vec![
                    CoreValue::Number(Number::from_usize(range.start)),
                    CoreValue::Number(Number::from_usize(range.end)),
                    values.clone_core(value)?,
                ],
            )))
        })
    }

    /// Constructs `anno 'binary Value` without demanding `Value`.
    pub fn anno_binary(&self, value: Value) -> Result<Value, Error> {
        self.with_access(|values| {
            let annotation = values.atom_from_text("binary");
            values.anno(&annotation, &value)
        })
    }

    /// Constructs `anno 'array Value` without demanding `Value`.
    pub fn anno_array(&self, value: Value) -> Result<Value, Error> {
        self.with_access(|values| {
            if matches!(values.core_value(&value)?, CoreValue::List(list) if list.value_slice().is_some())
            {
                return Ok(value);
            }
            let annotation = values.atom_from_text("array");
            values.anno(&annotation, &value)
        })
    }

    /// Constructs `anno 'deque Value` without demanding `Value`.
    pub fn anno_deque(&self, value: Value) -> Result<Value, Error> {
        self.with_access(|values| {
            let annotation = values.atom_from_text("deque");
            values.anno(&annotation, &value)
        })
    }

    /// Constructs an ordinary semantic singleton dictionary without
    /// demanding its key or value.
    pub fn dict_singleton(&self, key: Value, value: Value) -> Result<Value, Error> {
        self.with_access(|values| {
            Ok(values.wrap(CoreValue::builtin_call(
                values.core(),
                Builtin::DictSingleton,
                vec![values.clone_core(&key)?, values.clone_core(&value)?],
            )))
        })
    }

    /// Constructs ordinary hierarchical dictionary union without demanding
    /// either dictionary.
    pub fn dict_union(&self, left: Value, right: Value) -> Result<Value, Error> {
        self.with_access(|values| {
            Ok(values.wrap(CoreValue::builtin_call(
                values.core(),
                Builtin::DictUnion,
                vec![values.clone_core(&left)?, values.clone_core(&right)?],
            )))
        })
    }

    /// Constructs an ordinary semantic dictionary path update without
    /// demanding the dictionary, path, or replacement value.
    pub fn dict_update(
        &self,
        dictionary: Value,
        path: Value,
        new_value: Value,
    ) -> Result<Value, Error> {
        self.with_access(|values| {
            Ok(values.wrap(CoreValue::builtin_call(
                values.core(),
                Builtin::DictUpdate,
                vec![
                    values.clone_core(&path)?,
                    values.clone_core(&new_value)?,
                    values.clone_core(&dictionary)?,
                ],
            )))
        })
    }

    /// Returns the cached closed Glam helper `\fallback value -> ...` which
    /// selects `fallback` exactly when `value` is logically equal to `{}`.
    pub fn defined_or_function(&self) -> Value {
        self.with_access(|values| values.wrap(crate::g_syntax::defined_or_value(values.core())))
    }

    /// Constructs the standard failing effect without evaluating anything.
    pub fn fail_effect(&self) -> Value {
        self.with_access(|values| values.wrap(crate::g_syntax::fail_effect_value(values.core())))
    }

    /// Returns the cached closed Glam helper which asserts that its second
    /// argument is logically defined, using its first argument as the name in
    /// a structured failure.
    pub fn require_defined_function(&self) -> Value {
        self.with_access(|values| {
            values.wrap(crate::g_syntax::require_defined_value(values.core()))
        })
    }

    pub fn empty_object(&self, name: Value) -> Result<Value, Error> {
        self.with_access(|values| {
            let spec = CoreValue::Dict(
                Dict::new_sync()
                    .insert(Key::atom_from_text("name"), values.clone_core(&name)?)
                    .insert(
                        Key::atom_from_text("deps"),
                        CoreValue::List(List::from_values(Vec::new())),
                    )
                    .insert(
                        Key::atom_from_text("defs"),
                        CoreValue::Builtin(Builtin::ObjectDefaultDefs),
                    ),
            );
            Ok(values.wrap(CoreValue::builtin_call(
                values.core(),
                Builtin::ObjectInstance,
                vec![spec],
            )))
        })
    }

    pub fn after_reflection(&self, effect: Value, target: Value) -> Result<Value, Error> {
        self.with_access(|values| {
            let annotation = CoreValue::Dict(
                Dict::new_sync().insert(Key::atom_from_text("refl"), values.clone_core(&effect)?),
            );
            Ok(values.wrap(CoreValue::builtin_call(
                values.core(),
                Builtin::Anno,
                vec![annotation, values.clone_core(&target)?],
            )))
        })
    }

    pub fn abstract_global_path<I, S>(&self, parts: I) -> Value
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.with_access(|values| {
            values.wrap(CoreValue::Atom(crate::core::Atom::from_key(
                &Key::abstract_global_path(parts),
            )))
        })
    }
}

impl ScopedValues<'_> {
    fn core(&self) -> &CoreValueFactory {
        self.owner.core()
    }

    pub(super) fn wrap(&self, value: CoreValue) -> Value {
        debug_assert_eq!(self.owner.runtime, self.core().runtime_id());
        debug_assert!(self.access.belongs_to(self.core()));
        Value(RuntimeValueRoot::new(self.core(), value))
    }

    fn require(&self, value: &Value) -> Result<(), Error> {
        self.owner.require(value)
    }

    pub(super) fn core_value<'access>(
        &'access self,
        value: &'access Value,
    ) -> Result<&'access CoreValue, Error> {
        self.require(value)?;
        Ok(value.as_core())
    }

    fn clone_core(&self, value: &Value) -> Result<CoreValue, Error> {
        self.core_value(value).cloned()
    }

    fn atom_from_text(&self, text: &str) -> Value {
        let key = Key::binary_from_text(text);
        self.wrap(CoreValue::Atom(crate::core::Atom::from_key(&key)))
    }

    fn access(&self, base: &Value, key: &Value) -> Result<Value, Error> {
        Ok(
            self.wrap(CoreValue::Lazy(crate::core::LazyValue::from_access(
                self.core(),
                Arc::from([CoreDataKey::Index]),
                Arc::from([self.clone_core(base)?, self.clone_core(key)?]),
            ))),
        )
    }

    fn anno(&self, annotation: &Value, target: &Value) -> Result<Value, Error> {
        Ok(self.wrap(CoreValue::builtin_call(
            self.core(),
            Builtin::Anno,
            vec![self.clone_core(annotation)?, self.clone_core(target)?],
        )))
    }

    fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        self.require(function)?;
        let arguments = arguments
            .into_iter()
            .map(|argument| self.clone_core(&argument))
            .collect::<Result<Vec<_>, Error>>()?;
        if arguments.is_empty() {
            return Ok(function.clone());
        }
        Ok(self.wrap(CoreValue::Lazy(LazyValue::from_application(
            self.core(),
            self.clone_core(function)?,
            Arc::from(arguments),
        ))))
    }
}

impl Value {
    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.0.runtime_id()
    }

    pub(super) fn require_runtime(&self, runtime: EvaluationRuntimeId) -> Result<(), Error> {
        if self.runtime_id() == runtime {
            Ok(())
        } else {
            Err(Error::new(format!(
                "value belongs to evaluation runtime {}, expected evaluation runtime {}",
                self.runtime_id().get(),
                runtime.get()
            )))
        }
    }

    pub(crate) fn from_runtime_root(value: RuntimeValueRoot) -> Self {
        Self(value)
    }

    pub(crate) fn as_core(&self) -> &CoreValue {
        self.0.as_core()
    }

    pub(crate) fn into_core(self) -> CoreValue {
        self.0.into_core()
    }

    pub(crate) fn into_runtime_root(self) -> RuntimeValueRoot {
        self.0
    }
}

impl ValueKind {
    pub(super) fn from_core(value: &CoreValue) -> Self {
        match value {
            CoreValue::Atom(_) => ValueKind::Atom,
            CoreValue::Number(_) => ValueKind::Number,
            CoreValue::Binary(_) => ValueKind::Binary,
            CoreValue::List(_) => ValueKind::List,
            CoreValue::Dict(_) => ValueKind::Dict,
            CoreValue::Builtin(_) | CoreValue::PartialBuiltin(_) | CoreValue::Function(_) => {
                ValueKind::Function
            }
            CoreValue::Net(_) => ValueKind::Net,
            CoreValue::Lazy(_) | CoreValue::Promised(_) => ValueKind::Lazy,
            CoreValue::Metadata(_) => ValueKind::Sealed,
            CoreValue::Opaque(_) => ValueKind::Opaque,
        }
    }
}

impl EvaluatedValue {
    pub(crate) fn from_whnf(values: &Values, value: Value) -> Self {
        debug_assert!(!matches!(
            value.as_core(),
            CoreValue::Lazy(_) | CoreValue::Promised(_)
        ));
        debug_assert_eq!(value.runtime_id(), values.runtime_id());
        Self {
            value,
            observer: values.core.runtime_value_observer(),
        }
    }

    pub fn as_value(&self) -> &Value {
        &self.value
    }

    pub fn into_value(self) -> Value {
        self.value
    }

    fn require_observer(&self, values: &Values) -> Result<(), Error> {
        self.value.require_runtime(values.runtime)?;
        if self.observer.belongs_to(values.core()) {
            Ok(())
        } else {
            Err(Error::new(
                "evaluated value observer belongs to another value domain",
            ))
        }
    }

    fn observation_values(&self) -> Result<Values, Error> {
        self.value.require_runtime(self.observer.runtime_id())?;
        self.observer
            .upgrade()
            .map(Values::from_core_factory)
            .ok_or_else(|| {
                Error::new(format!(
                    "evaluation runtime {} is no longer available for value observation",
                    self.observer.runtime_id().get()
                ))
            })
    }

    pub(crate) fn with_core<R>(
        &self,
        operation: impl for<'scope> FnOnce(&'scope CoreValue) -> R,
    ) -> Result<R, Error> {
        let values = self.observation_values()?;
        values.with_access(|access| {
            let value = access.core_value(self.as_value())?;
            Ok(operation(value))
        })
    }

    /// Compares this evaluated outer value with another retained runtime
    /// representation without demanding the other value.
    ///
    /// This is representation identity/structure, not Glam's logical
    /// equality. It is primarily useful when an effect protocol expects a
    /// canonical immediate value such as an atom or unit.
    pub fn same_representation(&self, other: &Value) -> Result<bool, Error> {
        let values = self.observation_values()?;
        values.with_access(|access| {
            let left = access.core_value(self.as_value())?;
            let right = access.core_value(other)?;
            Ok(left == right)
        })
    }

    /// Extracts owned compact binary data under matching live runtime
    /// authority. The returned bytes do not borrow the value domain.
    pub fn as_bytes(&self) -> Result<Option<Bytes>, Error> {
        self.with_core(|value| match value {
            CoreValue::Binary(bytes) => Some(bytes.clone()),
            _ => None,
        })
    }

    pub fn as_i64(&self) -> Result<Option<i64>, Error> {
        self.with_core(|value| match value {
            CoreValue::Number(number) => number.to_i64_if_integer(),
            _ => None,
        })
    }

    pub fn as_u64(&self) -> Result<Option<u64>, Error> {
        self.with_core(|value| match value {
            CoreValue::Number(number) => number.to_u64_if_integer(),
            _ => None,
        })
    }

    pub fn as_rational_i64(&self) -> Result<Option<(i64, i64)>, Error> {
        self.with_core(|value| match value {
            CoreValue::Number(number) => number.to_ratio_i64(),
            _ => None,
        })
    }

    /// Converts a number lossily to a finite `f64`.
    pub fn as_f64(&self) -> Result<Option<f64>, Error> {
        self.with_core(|value| match value {
            CoreValue::Number(number) => number.to_f64(),
            _ => None,
        })
    }

    /// Returns canonical exact integer or `numerator/denominator` text.
    pub fn number_text(&self) -> Result<Option<String>, Error> {
        self.with_core(|value| match value {
            CoreValue::Number(number) => Some(number.to_string()),
            _ => None,
        })
    }

    /// Clones the members of one strict value-array representation without
    /// demanding any member.
    pub fn array_items(&self) -> Result<Option<Vec<Value>>, Error> {
        let values = self.observation_values()?;
        values.with_access(|access| {
            let CoreValue::List(list) = access.core_value(self.as_value())? else {
                return Ok(None);
            };
            Ok(list.value_slice().map(|items| {
                items
                    .iter()
                    .cloned()
                    .map(|item| access.wrap(item))
                    .collect()
            }))
        })
    }
}

impl fmt::Debug for EvaluatedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EvaluatedValue")
    }
}

impl From<EvaluatedValue> for Value {
    fn from(value: EvaluatedValue) -> Self {
        value.into_value()
    }
}

/// The unique host capability for completing one promised [`Value`].
///
/// This handle is affine: it cannot be cloned and is consumed by
/// [`resolve`](Self::resolve), [`fail`](Self::fail), or
/// [`fail_message`](Self::fail_message). Dropping it unresolved permanently
/// fails the promised value.
///
/// Completion wakes every same-runtime work item currently blocked on the
/// unresolved promise. Sharing the value and registering that exact wake do
/// not keep an observing session alive.
///
/// The resolver is consumed by every terminal operation, so attempting to
/// complete the same public promise twice is a compile-time error:
///
/// ```compile_fail
/// let assembler = glam::Assembler::default();
/// let (_, resolver) = assembler.promise("host input");
/// resolver.resolve(assembler.values().integer(1)).unwrap();
/// resolver.fail_message("too late").unwrap();
/// ```
#[must_use = "dropping an unresolved promise resolver fails its value"]
pub struct PromiseResolver {
    pub(super) runtime: EvaluationRuntimeId,
    pub(super) promise: Option<PromisedValue>,
}

impl PromiseResolver {
    /// Completes the promise successfully with `value`.
    pub fn resolve(mut self, value: Value) -> Result<(), Error> {
        if let Err(error) = value.require_runtime(self.runtime) {
            self.promise.take();
            return Err(error);
        }
        let promise = self
            .promise
            .take()
            .expect("a live promise resolver must retain its promise");
        let label = promise.label().clone();
        promise
            .set(value.into_core())
            .map_err(|_| Error::new(format!("promise `{label}` was already completed")))?;
        Ok(())
    }

    /// Completes the promise with an arbitrary Glam value as its permanent
    /// producer error.
    pub fn fail(self, failure: Value) -> Result<(), Error> {
        let mut resolver = self;
        if let Err(error) = failure.require_runtime(resolver.runtime) {
            resolver.promise.take();
            return Err(error);
        }
        resolver.fail_with(Arc::new(EvaluationFailure::emission(failure.into_core())))
    }

    /// Completes the promise with a conventional textual producer error.
    pub fn fail_message(self, message: impl Into<Arc<str>>) -> Result<(), Error> {
        self.fail_with(Arc::new(EvaluationFailure::message(message.into())))
    }

    fn fail_with(mut self, failure: Arc<EvaluationFailure>) -> Result<(), Error> {
        let promise = self
            .promise
            .take()
            .expect("a live promise resolver must retain its promise");
        let label = promise.label().clone();
        promise
            .fail(failure)
            .map_err(|_| Error::new(format!("promise `{label}` was already completed")))?;
        Ok(())
    }
}

impl Drop for PromiseResolver {
    fn drop(&mut self) {
        let Some(promise) = self.promise.take() else {
            return;
        };
        let message = format!(
            "promise resolver for `{}` was dropped before completion",
            promise.label()
        );
        let _ = promise.fail_message(message);
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Value")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueKind {
    Atom,
    Number,
    Binary,
    List,
    Dict,
    Function,
    Net,
    Lazy,
    Sealed,
    Opaque,
}

/// An opaque port created during one [`Assembler::net`] construction.
///
/// The lifetime prevents ports from escaping their construction callback or
/// being mixed between builders. Copying a handle does not copy the net value;
/// wiring either copy twice is rejected by the checked builder.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NetPort<'net> {
    pub(super) port: CorePort,
    brand: PhantomData<fn(&'net mut ()) -> &'net mut ()>,
}

impl fmt::Debug for NetPort<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NetPort(..)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetBind<'net> {
    pub application: NetPort<'net>,
    pub argument: NetPort<'net>,
    pub result: NetPort<'net>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetCopy<'net> {
    pub input: NetPort<'net>,
    pub outputs: Vec<NetPort<'net>>,
}

/// Checked, core-specialized construction of one closed interaction net.
///
/// This deliberately exposes only the operations needed by the future
/// `interaction_net` effect replay. Returning a port from the callback selects
/// the net's sole exposed port; every other port must be wired exactly once.
pub struct NetBuilder<'net> {
    pub(super) values: Values,
    pub(super) builder: CoreNetBuilder<CoreSpecialization>,
    brand: PhantomData<fn(&'net mut ()) -> &'net mut ()>,
}

impl<'net> NetBuilder<'net> {
    pub fn bind(&mut self) -> NetBind<'net> {
        let [application, argument, result] = self.builder.bind();
        NetBind {
            application: self.port(application),
            argument: self.port(argument),
            result: self.port(result),
        }
    }

    pub fn copy(&mut self, outputs: usize) -> NetCopy<'net> {
        let copy = self.builder.copy(outputs);
        NetCopy {
            input: self.port(copy.input),
            outputs: copy
                .outputs
                .into_iter()
                .map(|port| self.port(port))
                .collect(),
        }
    }

    pub fn data(&mut self, value: Value) -> Result<NetPort<'net>, Error> {
        let value = self
            .values
            .with_access(|values| values.clone_core(&value))?;
        let port = self.builder.data(value);
        Ok(self.port(port))
    }

    pub fn wire(&mut self, left: NetPort<'net>, right: NetPort<'net>) -> Result<(), Error> {
        self.builder
            .try_wire(left.port, right.port)
            .map_err(net_build_error)
    }

    pub(super) fn new(values: Values) -> Self {
        Self {
            values,
            builder: CoreNetBuilder::new(),
            brand: PhantomData,
        }
    }

    fn port(&self, port: CorePort) -> NetPort<'net> {
        NetPort {
            port,
            brand: PhantomData,
        }
    }
}

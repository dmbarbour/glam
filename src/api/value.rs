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
};
use crate::core_net::{CoreDataKey, CoreSpecialization};
use crate::interaction_net::{NetBuilder as CoreNetBuilder, Port as CorePort};
use crate::number::Number;
use crate::runtime::{EvaluationRuntimeId, RuntimeValueRoot};

#[cfg(test)]
mod prototype;

/// An assembly-time value rooted in exactly one [`EvaluationRuntime`].
///
/// Values cannot be transferred between runtimes. Construct them through
/// [`Values`], obtained from the target runtime or assembler.
#[derive(Clone, PartialEq, Eq)]
pub struct Value(pub(super) RuntimeValueRoot);

/// A runtime-local value whose outer shell has reached weak-head normal form.
///
/// Nested dictionary members, list elements, function results, and other
/// contained values may remain lazy. Converting this witness back to [`Value`]
/// discards only the static outer-WHNF guarantee.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluatedValue(Value);

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
    pub fn resolve(&self, token: &EvaluatedValue) -> Option<Arc<T>> {
        let CoreValue::Opaque(token) = token.as_value().as_core() else {
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

impl Values {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn core(&self) -> &CoreValueFactory {
        &self.core
    }

    pub(super) fn wrap(&self, value: CoreValue) -> Value {
        debug_assert_eq!(self.runtime, self.core.runtime_id());
        Value(RuntimeValueRoot::new(&self.core, value))
    }

    pub(crate) fn from_core_factory(core: CoreValueFactory) -> Self {
        Self {
            runtime: core.runtime_id(),
            core,
        }
    }

    pub(super) fn require(&self, value: &Value) -> Result<(), Error> {
        value.require_runtime(self.runtime)
    }

    /// Injects host bytes as compact binary data.
    pub fn bytes(&self, bytes: impl Into<Bytes>) -> Value {
        self.wrap(CoreValue::Binary(bytes.into()))
    }

    pub fn text(&self, text: impl AsRef<str>) -> Value {
        self.wrap(CoreValue::binary_from_text(text.as_ref()))
    }

    pub fn atom_from_text(&self, text: impl AsRef<str>) -> Value {
        let key = Key::binary_from_text(text.as_ref());
        self.wrap(CoreValue::Atom(crate::core::Atom::from_key(&key)))
    }

    pub fn integer(&self, value: i64) -> Value {
        self.wrap(CoreValue::Number(Number::integer(value)))
    }

    /// Returns Glam's cached semantic unit value `()`.
    pub fn unit(&self) -> Value {
        self.wrap(self.core.unit())
    }

    pub fn rational(&self, numerator: i64, denominator: i64) -> Option<Value> {
        Number::from_ratio_i64(numerator, denominator)
            .map(|number| self.wrap(CoreValue::Number(number)))
    }

    pub fn number_from_f64(&self, value: f64) -> Option<Value> {
        Number::from_f64(value).map(|number| self.wrap(CoreValue::Number(number)))
    }

    pub fn number_from_text(&self, text: impl AsRef<str>) -> Result<Value, Error> {
        Number::parse(text.as_ref())
            .map(|number| self.wrap(CoreValue::Number(number)))
            .map_err(Error::new)
    }

    pub fn list(&self, values: impl IntoIterator<Item = Value>) -> Result<Value, Error> {
        let values = values
            .into_iter()
            .map(|value| {
                self.require(&value)?;
                Ok(value.into_core())
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(self.wrap(CoreValue::List(List::from_values(values))))
    }

    pub fn record<I, S>(&self, entries: I) -> Result<Value, Error>
    where
        I: IntoIterator<Item = (S, Value)>,
        S: AsRef<str>,
    {
        let mut dict = Dict::new_sync();
        for (name, value) in entries {
            self.require(&value)?;
            dict = dict.insert(Key::atom_from_text(name), value.into_core());
        }
        Ok(self.wrap(CoreValue::Dict(dict)))
    }

    pub fn dictionary(
        &self,
        entries: impl IntoIterator<Item = (Value, Value)>,
    ) -> Result<Value, Error> {
        let mut dict = Dict::new_sync();
        for (key, value) in entries {
            self.require(&key)?;
            self.require(&value)?;
            let key = Key::from_value(key.as_core())
                .ok_or_else(|| Error::new("dictionary key is not immediately keyable"))?;
            dict = dict.insert(key, value.into_core());
        }
        Ok(self.wrap(CoreValue::Dict(dict)))
    }

    /// Constructs Glam's immediate empty dictionary/undefined value.
    pub fn empty_dict(&self) -> Value {
        self.wrap(CoreValue::Dict(Dict::new_sync()))
    }

    /// Constructs the ordinary lazy `base.[key]` semantic accessor.
    ///
    /// Neither operand is evaluated while constructing the value. Demand on
    /// the returned value evaluates the key and follows the same dictionary
    /// access semantics as `.g` source, including returning `{}` for a
    /// missing key.
    pub fn access(&self, base: &Value, key: Value) -> Result<Value, Error> {
        self.require(base)?;
        self.require(&key)?;
        Ok(
            self.wrap(CoreValue::Lazy(crate::core::LazyValue::from_access(
                &self.core,
                Arc::from([CoreDataKey::Index]),
                Arc::from([base.as_core().clone(), key.into_core()]),
            ))),
        )
    }

    /// Constructs the ordinary lazy `anno Annotation Target` semantic value.
    ///
    /// Annotation interpretation occurs only when the returned value is
    /// demanded; this method does not provide a separate host-side annotation
    /// interpreter.
    pub fn anno(&self, annotation: Value, target: Value) -> Result<Value, Error> {
        self.require(&annotation)?;
        self.require(&target)?;
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::Anno,
            vec![annotation.into_core(), target.into_core()],
        )))
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
        self.require(function)?;
        let arguments = arguments
            .into_iter()
            .map(|argument| {
                self.require(&argument)?;
                Ok(argument.into_core())
            })
            .collect::<Result<Vec<_>, Error>>()?;
        if arguments.is_empty() {
            return Ok(function.clone());
        }
        Ok(self.wrap(CoreValue::Lazy(LazyValue::from_application(
            &self.core,
            function.as_core().clone(),
            Arc::from(arguments),
        ))))
    }

    /// Constructs successive ordinary dictionary accesses without demanding
    /// the root or keys.
    pub fn access_path(
        &self,
        root: &Value,
        keys: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error> {
        self.require(root)?;
        let keys = keys
            .into_iter()
            .map(|key| {
                self.require(&key)?;
                Ok(key)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        keys.into_iter()
            .try_fold(root.clone(), |value, key| self.access(&value, key))
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
        self.require(root)?;
        names.into_iter().try_fold(root.clone(), |value, name| {
            self.access(&value, self.atom_from_text(name))
        })
    }

    /// Constructs the ordinary `slice Start End Value` computation without
    /// demanding the list or binary value.
    pub fn list_slice(&self, value: &Value, range: Range<usize>) -> Result<Value, Error> {
        self.require(value)?;
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::Slice,
            vec![
                CoreValue::Number(Number::from_usize(range.start)),
                CoreValue::Number(Number::from_usize(range.end)),
                value.as_core().clone(),
            ],
        )))
    }

    /// Constructs `anno 'binary Value` without demanding `Value`.
    pub fn anno_binary(&self, value: Value) -> Result<Value, Error> {
        self.anno(self.atom_from_text("binary"), value)
    }

    /// Constructs `anno 'array Value` without demanding `Value`.
    pub fn anno_array(&self, value: Value) -> Result<Value, Error> {
        self.require(&value)?;
        if matches!(value.as_core(), CoreValue::List(list) if list.value_slice().is_some()) {
            return Ok(value);
        }
        self.anno(self.atom_from_text("array"), value)
    }

    /// Constructs `anno 'deque Value` without demanding `Value`.
    pub fn anno_deque(&self, value: Value) -> Result<Value, Error> {
        self.anno(self.atom_from_text("deque"), value)
    }

    /// Constructs an ordinary semantic singleton dictionary without
    /// demanding its key or value.
    pub fn dict_singleton(&self, key: Value, value: Value) -> Result<Value, Error> {
        self.require(&key)?;
        self.require(&value)?;
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::DictSingleton,
            vec![key.into_core(), value.into_core()],
        )))
    }

    /// Constructs ordinary hierarchical dictionary union without demanding
    /// either dictionary.
    pub fn dict_union(&self, left: Value, right: Value) -> Result<Value, Error> {
        self.require(&left)?;
        self.require(&right)?;
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::DictUnion,
            vec![left.into_core(), right.into_core()],
        )))
    }

    /// Constructs an ordinary semantic dictionary path update without
    /// demanding the dictionary, path, or replacement value.
    pub fn dict_update(
        &self,
        dictionary: Value,
        path: Value,
        new_value: Value,
    ) -> Result<Value, Error> {
        self.require(&dictionary)?;
        self.require(&path)?;
        self.require(&new_value)?;
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::DictUpdate,
            vec![
                path.into_core(),
                new_value.into_core(),
                dictionary.into_core(),
            ],
        )))
    }

    /// Returns the cached closed Glam helper `\fallback value -> ...` which
    /// selects `fallback` exactly when `value` is logically equal to `{}`.
    pub fn defined_or_function(&self) -> Value {
        self.wrap(crate::g_syntax::defined_or_value(&self.core))
    }

    /// Constructs the standard failing effect without evaluating anything.
    pub fn fail_effect(&self) -> Value {
        self.wrap(crate::g_syntax::fail_effect_value(&self.core))
    }

    /// Returns the cached closed Glam helper which asserts that its second
    /// argument is logically defined, using its first argument as the name in
    /// a structured failure.
    pub fn require_defined_function(&self) -> Value {
        self.wrap(crate::g_syntax::require_defined_value(&self.core))
    }

    pub fn empty_object(&self, name: Value) -> Result<Value, Error> {
        self.require(&name)?;
        let spec = CoreValue::Dict(
            Dict::new_sync()
                .insert(Key::atom_from_text("name"), name.into_core())
                .insert(
                    Key::atom_from_text("deps"),
                    CoreValue::List(List::from_values(Vec::new())),
                )
                .insert(
                    Key::atom_from_text("defs"),
                    CoreValue::Builtin(Builtin::ObjectDefaultDefs),
                ),
        );
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::ObjectInstance,
            vec![spec],
        )))
    }

    pub fn after_reflection(&self, effect: Value, target: Value) -> Result<Value, Error> {
        self.require(&effect)?;
        self.require(&target)?;
        let annotation = CoreValue::Dict(
            Dict::new_sync().insert(Key::atom_from_text("refl"), effect.into_core()),
        );
        Ok(self.wrap(CoreValue::builtin_call(
            &self.core,
            Builtin::Anno,
            vec![annotation, target.into_core()],
        )))
    }

    pub fn abstract_global_path<I, S>(&self, parts: I) -> Value
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.wrap(CoreValue::Atom(crate::core::Atom::from_key(
            &Key::abstract_global_path(parts),
        )))
    }
}

impl Value {
    pub fn runtime_id(&self) -> EvaluationRuntimeId {
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

    #[cfg(test)]
    pub(crate) fn is_undefined(&self) -> bool {
        matches!(self.0.as_core(), CoreValue::Dict(dict) if dict.is_empty())
    }

    pub(crate) fn kind(&self) -> ValueKind {
        match self.0.as_core() {
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

    pub(crate) fn as_binary(&self) -> Option<&[u8]> {
        match self.0.as_core() {
            CoreValue::Binary(bytes) => Some(bytes.as_ref()),
            _ => None,
        }
    }

    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_i64_if_integer(),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn as_number_text(&self) -> Option<String> {
        match self.0.as_core() {
            CoreValue::Number(number) => Some(number.to_string()),
            _ => None,
        }
    }

    pub(crate) fn from_core(values: &CoreValueFactory, value: CoreValue) -> Self {
        Self::from_runtime(values.runtime_id(), value)
    }

    pub(crate) fn from_runtime_root(value: RuntimeValueRoot) -> Self {
        Self(value)
    }

    pub(super) fn from_runtime(runtime: EvaluationRuntimeId, value: CoreValue) -> Self {
        Self(RuntimeValueRoot::from_runtime(runtime, value))
    }

    pub(crate) fn as_core(&self) -> &CoreValue {
        self.0.as_core()
    }

    pub(crate) fn into_core(self) -> CoreValue {
        self.0.into_core()
    }
}

impl EvaluatedValue {
    pub(crate) fn from_whnf(value: Value) -> Self {
        debug_assert!(!matches!(
            value.as_core(),
            CoreValue::Lazy(_) | CoreValue::Promised(_)
        ));
        Self(value)
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self.0.as_core() {
            CoreValue::Binary(bytes) => Some(bytes.as_ref()),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_i64_if_integer(),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_u64_if_integer(),
            _ => None,
        }
    }

    pub fn as_rational_i64(&self) -> Option<(i64, i64)> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_ratio_i64(),
            _ => None,
        }
    }

    /// Converts a number lossily to a finite `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match self.0.as_core() {
            CoreValue::Number(number) => number.to_f64(),
            _ => None,
        }
    }

    /// Returns canonical exact integer or `numerator/denominator` text.
    pub fn number_text(&self) -> Option<String> {
        match self.0.as_core() {
            CoreValue::Number(number) => Some(number.to_string()),
            _ => None,
        }
    }

    /// Clones the members of one strict value-array representation without
    /// demanding any member.
    pub fn array_items(&self) -> Option<Vec<Value>> {
        let CoreValue::List(list) = self.0.as_core() else {
            return None;
        };
        list.value_slice().map(|items| {
            items
                .iter()
                .cloned()
                .map(|item| Value::from_runtime(self.0.runtime_id(), item))
                .collect()
        })
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
        formatter
            .debug_struct("Value")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
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
        self.values.require(&value)?;
        let port = self.builder.data(value.into_core());
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

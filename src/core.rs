use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroU64;
#[cfg(test)]
use std::sync::LazyLock;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use internment::Intern;
use rpds::RedBlackTreeMapSync;

use crate::core_net::{CoreDataKey, CoreRuntimeNet};
use crate::evaluation::{
    EvalContext, EvaluationTaskHandle, EvaluationTaskId, EvaluationWaitToken,
    ReflectionTaskResultPolicy,
};
use crate::number::Number;
use crate::runtime::{EvaluationRuntimeId, RuntimeIds, RuntimeValueRoot};

mod evaluation_halt;
pub(crate) mod keys;
pub(crate) use evaluation_halt::EvaluationHalt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LazyId(NonZeroU64);

impl LazyId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PromiseId(NonZeroU64);

impl PromiseId {
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DeferredValueId {
    Lazy(LazyId),
    Promise(PromiseId),
}

impl DeferredValueId {
    pub(crate) fn get(self) -> u64 {
        match self {
            Self::Lazy(id) => id.get(),
            Self::Promise(id) => id.get(),
        }
    }
}

impl From<LazyId> for DeferredValueId {
    fn from(id: LazyId) -> Self {
        Self::Lazy(id)
    }
}

impl From<PromiseId> for DeferredValueId {
    fn from(id: PromiseId) -> Self {
        Self::Promise(id)
    }
}

impl PartialOrd for DeferredValueId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeferredValueId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.get().cmp(&other.get()).then_with(|| {
            let kind = |id| match id {
                Self::Lazy(_) => 0,
                Self::Promise(_) => 1,
            };
            kind(*self).cmp(&kind(*other))
        })
    }
}

/// A value whose outer shell has reached weak-head normal form.
///
/// Containers may still contain lazy fields. The wrapper prevents a computed
/// lazy result cache from storing another deferred outer shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluatedValue(Value);

impl EvaluatedValue {
    pub(crate) fn into_value(self) -> Value {
        self.0
    }
}

pub(crate) type LazyResult = Result<EvaluatedValue, Arc<EvaluationFailure>>;

impl TryFrom<Value> for EvaluatedValue {
    type Error = Value;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if matches!(value, Value::Lazy(_) | Value::Promised(_)) {
            Err(value)
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvaluationFailure {
    kind: EvaluationFailureKind,
    contexts: Arc<[Value]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EvaluationFailureKind {
    Emission(Value),
    DependencyCycle(Arc<LazyCycle>),
}

impl EvaluationFailure {
    pub(crate) fn message(message: impl AsRef<str>) -> Self {
        Self::emission(Value::binary_from_text(message.as_ref()))
    }

    pub(crate) fn emission(emission: Value) -> Self {
        Self {
            kind: EvaluationFailureKind::Emission(emission),
            contexts: Arc::from([]),
        }
    }

    pub(crate) fn dependency_cycle(cycle: Arc<LazyCycle>) -> Self {
        Self {
            kind: EvaluationFailureKind::DependencyCycle(cycle),
            contexts: Arc::from([]),
        }
    }

    pub(crate) fn with_context(&self, context: Value) -> Self {
        let mut contexts = Vec::with_capacity(self.contexts.len() + 1);
        contexts.push(context);
        contexts.extend(self.contexts.iter().cloned());
        Self {
            kind: self.kind.clone(),
            contexts: contexts.into(),
        }
    }

    pub(crate) fn emission_value(&self) -> Option<&Value> {
        match &self.kind {
            EvaluationFailureKind::Emission(emission) => Some(emission),
            EvaluationFailureKind::DependencyCycle(_) => None,
        }
    }

    pub(crate) fn contexts(&self) -> &[Value] {
        &self.contexts
    }

    #[cfg(test)]
    pub(crate) fn dependency_cycle_value(&self) -> Option<&Arc<LazyCycle>> {
        match &self.kind {
            EvaluationFailureKind::DependencyCycle(cycle) => Some(cycle),
            EvaluationFailureKind::Emission(_) => None,
        }
    }
}

impl fmt::Display for EvaluationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            EvaluationFailureKind::Emission(emission) => {
                if let Some(message) = immediate_failure_text(emission) {
                    formatter.write_str(&message)
                } else {
                    write!(
                        formatter,
                        "evaluation failed with {}",
                        emission.diagnostic_kind_name()
                    )
                }
            }
            EvaluationFailureKind::DependencyCycle(cycle) => {
                formatter.write_str("lazy dependency cycle")?;
                for member in cycle.members.iter() {
                    write!(formatter, " -> {} ({})", member.id.get(), member.label)?;
                }
                if let Some(first) = cycle.members.first() {
                    write!(formatter, " -> {}", first.id.get())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for EvaluationFailure {}

fn immediate_failure_text(emission: &Value) -> Option<Arc<str>> {
    match emission {
        Value::Binary(text) => Some(Arc::from(String::from_utf8_lossy(text).as_ref())),
        Value::Dict(emission) => {
            let text = emission
                .get(&*keys::MSG)
                .and_then(|message| match message {
                    Value::Dict(message) => message.get(&*keys::TEXT),
                    _ => None,
                })?;
            match text {
                Value::Binary(text) => Some(Arc::from(String::from_utf8_lossy(text).as_ref())),
                _ => None,
            }
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LazyCycle {
    pub(crate) members: Box<[LazyCycleMember]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LazyCycleMember {
    pub(crate) id: LazyId,
    pub(crate) label: Arc<str>,
}

#[derive(Clone)]
pub struct LazyValue(Arc<LazyCell>);

struct LazyCell {
    id: LazyId,
    label: Arc<str>,
    source: Mutex<Option<LazySource>>,
    result: OnceLock<LazyResult>,
}

/// The one terminal assignment retained by a named promise.
///
/// Successful assignments may still name deferred values, which observers
/// follow normally. The failure arm is permanent: retryable demand halts never
/// enter this cell.
pub(crate) type PromiseAssignment = Result<RuntimeValueRoot, Arc<EvaluationFailure>>;

#[derive(Clone)]
pub(crate) struct PromisedValue {
    id: PromiseId,
    runtime: EvaluationRuntimeId,
    label: Arc<str>,
    assignment: Arc<OnceLock<PromiseAssignment>>,
    task: Option<Arc<TaskPromise>>,
}

pub(crate) struct TaskPromise {
    owner: EvaluationTaskId,
    wait: EvaluationWaitToken,
}

/// Runtime-selected construction authority for values which allocate stable
/// evaluator identities.
#[derive(Clone)]
pub(crate) struct CoreValueFactory {
    runtime: EvaluationRuntimeId,
    ids: Arc<RuntimeIds>,
    cache: Arc<RuntimeValueCache>,
    local_extensions: Option<SharedExtensionMap>,
}

type ExtensionMap = HashMap<TypeId, Box<dyn Any + Send + Sync>>;
type SharedExtensionMap = Arc<Mutex<ExtensionMap>>;

/// Small canonical value set owned directly by one runtime.
struct CoreValues {
    unit: Value,
    object_reflection_guard: Value,
    tuple: Value,
    info: Value,
    warn: Value,
    error: Value,
    initial_metadata: Value,
}

impl CoreValues {
    fn new() -> Self {
        let atom = |key: &Key| match key {
            Key::Atom(atom) => Value::Atom(*atom),
            _ => Value::Atom(Atom::from_key(key)),
        };
        Self {
            unit: atom(&keys::UNIT),
            object_reflection_guard: atom(&keys::OBJECT_REFLECTION_GUARD),
            tuple: atom(&keys::TUPLE),
            info: atom(&keys::INFO),
            warn: atom(&keys::WARN),
            error: atom(&keys::ERROR),
            initial_metadata: Value::Metadata(MetadataCarrier::new(Value::Dict(Dict::new_sync()))),
        }
    }
}

/// Runtime-owned storage for canonical core values and closed values
/// constructed by optional compiler layers. The core does not depend on those
/// layers: `TypeId` supplies the private namespace, while each layer owns the
/// concrete cached type.
struct RuntimeValueCache {
    core: CoreValues,
    extensions: Mutex<ExtensionMap>,
    #[cfg(test)]
    extension_lookups: AtomicUsize,
}

impl CoreValueFactory {
    pub(crate) fn new(runtime: EvaluationRuntimeId, ids: Arc<RuntimeIds>) -> Self {
        Self {
            runtime,
            ids,
            cache: Arc::new(RuntimeValueCache {
                core: CoreValues::new(),
                extensions: Mutex::new(HashMap::new()),
                #[cfg(test)]
                extension_lookups: AtomicUsize::new(0),
            }),
            local_extensions: None,
        }
    }

    /// Creates a compilation-local view which remembers resolved runtime
    /// attachments without duplicating the runtime-owned values themselves.
    pub(crate) fn scoped(&self) -> Self {
        Self {
            runtime: self.runtime,
            ids: self.ids.clone(),
            cache: self.cache.clone(),
            local_extensions: Some(Arc::new(Mutex::new(HashMap::new()))),
        }
    }

    pub(crate) fn ids(&self) -> &Arc<RuntimeIds> {
        &self.ids
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    fn deferred_value_id(&self) -> NonZeroU64 {
        self.ids.deferred_value()
    }

    pub(crate) fn unit(&self) -> Value {
        self.cache.core.unit.clone()
    }

    pub(crate) fn object_reflection_guard(&self) -> Value {
        self.cache.core.object_reflection_guard.clone()
    }

    pub(crate) fn tuple(&self) -> Value {
        self.cache.core.tuple.clone()
    }

    pub(crate) fn info(&self) -> Value {
        self.cache.core.info.clone()
    }

    pub(crate) fn warn(&self) -> Value {
        self.cache.core.warn.clone()
    }

    pub(crate) fn error(&self) -> Value {
        self.cache.core.error.clone()
    }

    pub(crate) fn initial_metadata(&self) -> Value {
        self.cache.core.initial_metadata.clone()
    }

    fn atom(&self, atom: Atom) -> Value {
        if atom == Atom::from_key(&keys::UNIT) {
            self.unit()
        } else if atom == Atom::from_key(&keys::OBJECT_REFLECTION_GUARD) {
            self.object_reflection_guard()
        } else if atom == Atom::from_key(&keys::TUPLE) {
            self.tuple()
        } else if atom == Atom::from_key(&keys::INFO) {
            self.info()
        } else if atom == Atom::from_key(&keys::WARN) {
            self.warn()
        } else if atom == Atom::from_key(&keys::ERROR) {
            self.error()
        } else {
            Value::Atom(atom)
        }
    }

    pub(crate) fn key_value(&self, key: &Key) -> Value {
        key.to_value_with(self)
    }

    /// Returns one runtime-local cache entry, allowing harmless duplicate
    /// construction when callers race. Only the completed value is installed.
    pub(crate) fn cached<T>(&self, build: impl FnOnce() -> T) -> Arc<T>
    where
        T: Any + Send + Sync,
    {
        let type_id = TypeId::of::<T>();
        if let Some(value) = self.local_extensions.as_ref().and_then(|extensions| {
            extensions
                .lock()
                .expect("local value-cache mutex should not be poisoned")
                .get(&type_id)
                .and_then(|value| value.downcast_ref::<Arc<T>>())
                .cloned()
        }) {
            return value;
        }
        #[cfg(test)]
        self.cache.extension_lookups.fetch_add(1, Ordering::Relaxed);
        if let Some(value) = self
            .cache
            .extensions
            .lock()
            .expect("runtime value-cache mutex should not be poisoned")
            .get(&type_id)
            .and_then(|value| value.downcast_ref::<Arc<T>>())
            .cloned()
        {
            self.remember_local(type_id, value.clone());
            return value;
        }

        let candidate = Arc::new(build());
        let value = {
            let mut values = self
                .cache
                .extensions
                .lock()
                .expect("runtime value-cache mutex should not be poisoned");
            values
                .entry(type_id)
                .or_insert_with(|| Box::new(candidate.clone()))
                .downcast_ref::<Arc<T>>()
                .expect("a runtime value-cache type ID has one concrete type")
                .clone()
        };
        self.remember_local(type_id, value.clone());
        value
    }

    fn remember_local<T>(&self, type_id: TypeId, value: Arc<T>)
    where
        T: Any + Send + Sync,
    {
        if let Some(extensions) = &self.local_extensions {
            extensions
                .lock()
                .expect("local value-cache mutex should not be poisoned")
                .entry(type_id)
                .or_insert_with(|| Box::new(value));
        }
    }

    #[cfg(test)]
    pub(crate) fn extension_lookup_count(&self) -> usize {
        self.cache.extension_lookups.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
pub(crate) fn test_value_factory() -> CoreValueFactory {
    static FACTORY: LazyLock<CoreValueFactory> = LazyLock::new(|| {
        CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            RuntimeIds::compiler_test_values(),
        )
    });
    FACTORY.clone()
}

impl LazyValue {
    fn with_source(
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
        source: LazySource,
    ) -> Self {
        Self(Arc::new(LazyCell {
            id: LazyId(values.deferred_value_id()),
            label: label.into(),
            source: Mutex::new(Some(source)),
            result: OnceLock::new(),
        }))
    }

    pub(crate) fn computed_fixpoint(
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
        computation: FixpointComputation,
    ) -> Self {
        Self::with_source(
            values,
            label,
            LazySource::ComputedFixpoint(Arc::new(computation)),
        )
    }

    pub(crate) fn deferred(
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
        thunk: impl Fn(&EvalContext) -> Result<Value, EvaluationHalt> + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(values, label, LazySource::Deferred(Arc::new(thunk)))
    }

    pub(crate) fn error(values: &CoreValueFactory, message: impl Into<Arc<str>>) -> Self {
        let value = Self::with_source(values, "error", LazySource::Error);
        let result = value.cache(Err(Arc::new(EvaluationFailure::message(message.into()))));
        debug_assert!(result.is_err(), "new lazy errors must cache a failure");
        value
    }

    pub(crate) fn id(&self) -> LazyId {
        self.0.id
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.0.label
    }

    /// Clones the producer while this lazy remains unresolved.
    ///
    /// Terminal results are published before the shared source is removed.
    /// A worker which wins this snapshot may therefore finish concurrent work,
    /// while later observers take the lock-free cached-result path.
    pub(crate) fn source_snapshot(&self) -> Option<LazySource> {
        if self.0.result.get().is_some() {
            return None;
        }
        let source = self.0.source.lock().expect("lazy source cell was poisoned");
        if self.0.result.get().is_some() {
            return None;
        }
        Some(
            source
                .as_ref()
                .expect("an unresolved lazy value must retain its source")
                .clone(),
        )
    }

    pub(crate) fn cached(&self) -> Option<LazyResult> {
        self.0.result.get().cloned()
    }

    pub(crate) fn cache(&self, result: LazyResult) -> LazyResult {
        let _ = self.0.result.set(result);
        let result = self
            .0
            .result
            .get()
            .expect("lazy cache should contain a value after set")
            .clone();

        // Publish the terminal result before removing its producer. Workers
        // which already cloned the source may finish harmlessly; a subsequent
        // cache attempt observes this same canonical result.
        let source = {
            let mut source = self.0.source.lock().expect("lazy source cell was poisoned");
            source.take()
        };
        drop(source);
        result
    }
}

impl PromisedValue {
    pub(crate) fn new(values: &CoreValueFactory, label: impl Into<Arc<str>>) -> Self {
        Self {
            id: PromiseId(values.deferred_value_id()),
            runtime: values.runtime_id(),
            label: label.into(),
            assignment: Arc::new(OnceLock::new()),
            task: None,
        }
    }

    pub(crate) fn fixpoint(
        context: &EvalContext,
        label: impl Into<Arc<str>>,
    ) -> Result<Self, Arc<str>> {
        let id = PromiseId(context.values().deferred_value_id());
        let assignment = Arc::new(OnceLock::new());
        let (owner, wait) = context.register_promise(&assignment)?;
        Ok(Self {
            id,
            runtime: context.values().runtime_id(),
            label: label.into(),
            assignment,
            task: Some(Arc::new(TaskPromise { owner, wait })),
        })
    }

    pub(crate) fn id(&self) -> PromiseId {
        self.id
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.runtime
    }

    pub(crate) fn label(&self) -> &Arc<str> {
        &self.label
    }

    pub(crate) fn task(&self) -> Option<&TaskPromise> {
        self.task.as_deref()
    }

    pub(crate) fn set(&self, value: Value) -> Result<(), Value> {
        if let Err(assignment) = self
            .assignment
            .set(Ok(RuntimeValueRoot::from_runtime(self.runtime, value)))
        {
            return Err(assignment
                .expect("setting a promised value always supplies a successful value")
                .into_core());
        }
        self.publish_task_assignment();
        Ok(())
    }

    pub(crate) fn fail(
        &self,
        failure: Arc<EvaluationFailure>,
    ) -> Result<(), Arc<EvaluationFailure>> {
        if let Err(assignment) = self.assignment.set(Err(failure)) {
            return Err(assignment.expect_err("failing a promised value always supplies an error"));
        }
        self.publish_task_assignment();
        Ok(())
    }

    pub(crate) fn fail_message(
        &self,
        message: impl Into<Arc<str>>,
    ) -> Result<(), Arc<EvaluationFailure>> {
        self.fail(Arc::new(EvaluationFailure::message(message.into())))
    }

    pub(crate) fn assignment(&self) -> Option<Result<Value, Arc<EvaluationFailure>>> {
        self.assignment
            .get()
            .cloned()
            .map(|assignment| assignment.map(RuntimeValueRoot::into_core))
    }

    fn publish_task_assignment(&self) {
        let Some(task) = &self.task else {
            return;
        };
        task.wait().publish_promise_assignment(
            self.assignment
                .get()
                .expect("a completed task promise must retain its assignment"),
        );
    }
}

impl PartialEq for LazyValue {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for LazyValue {}

impl fmt::Debug for LazyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyValue")
            .field("id", &self.id())
            .field("label", self.label())
            .finish_non_exhaustive()
    }
}

impl PartialEq for PromisedValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for PromisedValue {}

impl fmt::Debug for PromisedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PromisedValue")
            .field("id", &self.id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Atom {
    // Atom is optimized tagged data `[Key]:()`
    // use Intern for fast comparison and hash
    key: Intern<Key>,
}

impl Atom {
    pub fn from_key(key: &Key) -> Self {
        Self {
            key: Intern::new(key.clone()),
        }
    }

    pub fn key(&self) -> &Key {
        self.key.as_ref()
    }
}

impl fmt::Debug for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Atom").field(self.key()).finish()
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Atom(Atom),
    Number(Number),
    Binary(Bytes),
    AbstractGlobalPath(Arc<[String]>),
    List(Arc<[Key]>),
    Dict(Arc<[(Key, Key)]>),
}

impl Key {
    pub fn atom_from_text(text: impl AsRef<str>) -> Self {
        Self::atom_from_key(&Self::binary_from_text(text))
    }

    pub fn atom_from_key(key: &Key) -> Self {
        Self::Atom(Atom::from_key(key))
    }

    pub fn binary_from_text(text: impl AsRef<str>) -> Self {
        Self::Binary(Bytes::copy_from_slice(text.as_ref().as_bytes()))
    }

    pub fn abstract_global_path<I, S>(parts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::AbstractGlobalPath(Arc::from(
            parts.into_iter().map(Into::into).collect::<Vec<_>>(),
        ))
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Atom(atom) => Some(Self::Atom(*atom)),
            Value::Number(number) => Some(Self::Number(number.clone())),
            Value::Binary(bytes) => Some(Self::Binary(bytes.clone())),
            Value::List(list) => Some(Self::List(list_to_key_items(list)?)),
            Value::Dict(dict) => Some(Self::Dict(Arc::from(
                dict.iter()
                    .map(|(key, value)| {
                        let value = Self::from_value(value)?;
                        if matches!(&value, Key::Dict(entries) if entries.is_empty()) {
                            return Some(None);
                        }
                        Some(Some((key.clone(), value)))
                    })
                    .collect::<Option<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
            ))),
            Value::Builtin(_)
            | Value::PartialBuiltin(_)
            | Value::Function(_)
            | Value::Net(_)
            | Value::Lazy(_)
            | Value::Promised(_)
            | Value::Metadata(_)
            | Value::Opaque(_) => None,
        }
    }

    pub(crate) fn to_value_with(&self, values: &CoreValueFactory) -> Value {
        match self {
            Self::Atom(atom) => values.atom(*atom),
            Self::Number(number) => Value::Number(number.clone()),
            Self::Binary(bytes) => Value::Binary(bytes.clone()),
            Self::AbstractGlobalPath(parts) => {
                values.atom(Atom::from_key(&Self::AbstractGlobalPath(parts.clone())))
            }
            Self::List(items) => Value::List(List::from_values(
                items
                    .iter()
                    .map(|item| item.to_value_with(values))
                    .collect(),
            )),
            Self::Dict(entries) => {
                Value::Dict(entries.iter().fold(Dict::new_sync(), |dict, (key, value)| {
                    dict.insert(key.clone(), value.to_value_with(values))
                }))
            }
        }
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Atom(atom) => f.debug_tuple("Atom").field(atom).finish(),
            Key::Number(number) => f.debug_tuple("Number").field(number).finish(),
            Key::Binary(bytes) => f.debug_tuple("Binary").field(bytes).finish(),
            Key::AbstractGlobalPath(parts) => {
                f.debug_tuple("AbstractGlobalPath").field(parts).finish()
            }
            Key::List(items) => f.debug_tuple("List").field(items).finish(),
            Key::Dict(entries) => f.debug_tuple("Dict").field(entries).finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Value {
    Atom(Atom),
    Number(Number),
    Binary(Bytes),
    List(List),
    Dict(Dict),
    Builtin(Builtin),
    PartialBuiltin(BuiltinCall),
    /// An ordinary observable function value backed by a shared curried net
    /// stage. Unlike `Net`, this never exposes structural binders as values.
    Function(FunctionValue),
    /// A closed interaction net with one designated exposed port.
    Net(NetValue),
    /// A closed suspended computation or memoized failure.
    Lazy(LazyValue),
    /// A named one-write hole whose assignment may itself be deferred.
    Promised(PromisedValue),
    /// A sealed unit carrier whose associated Glam metadata is available only
    /// to privileged reflection operations.
    Metadata(MetadataCarrier),
    /// Host-owned identity whose representation is deliberately unavailable to
    /// Glam programs. Clones retain the payload and compare by identity.
    Opaque(OpaqueValue),
}

impl fmt::Debug for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Atom(value) => formatter.debug_tuple("Atom").field(value).finish(),
            Self::Number(value) => formatter.debug_tuple("Number").field(value).finish(),
            Self::Binary(value) => formatter.debug_tuple("Binary").field(value).finish(),
            Self::List(value) => formatter.debug_tuple("List").field(value).finish(),
            Self::Dict(value) => formatter.debug_tuple("Dict").field(value).finish(),
            Self::Builtin(value) => formatter.debug_tuple("Builtin").field(value).finish(),
            Self::PartialBuiltin(value) => formatter
                .debug_tuple("PartialBuiltin")
                .field(value)
                .finish(),
            Self::Function(value) => formatter.debug_tuple("Function").field(value).finish(),
            Self::Net(value) => formatter.debug_tuple("Net").field(value).finish(),
            Self::Lazy(value) => formatter.debug_tuple("Lazy").field(value).finish(),
            Self::Promised(value) => formatter.debug_tuple("Promised").field(value).finish(),
            Self::Metadata(_) => formatter.write_str("Sealed(..)"),
            Self::Opaque(value) => formatter.debug_tuple("Opaque").field(value).finish(),
        }
    }
}

/// A sealed unit carrier with reflection-only associated Glam metadata.
///
/// Pointer equality exists only to support Rust's internal value containers.
/// Ordinary Glam comparison rejects the carrier rather than exposing identity.
#[derive(Clone)]
pub struct MetadataCarrier {
    metadata: Arc<Value>,
}

impl MetadataCarrier {
    fn new(metadata: Value) -> Self {
        Self {
            metadata: Arc::new(metadata),
        }
    }

    fn associated_metadata(&self) -> Value {
        self.metadata.as_ref().clone()
    }
}

impl fmt::Debug for MetadataCarrier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MetadataCarrier(..)")
    }
}

impl PartialEq for MetadataCarrier {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.metadata, &other.metadata)
    }
}

impl Eq for MetadataCarrier {}

/// Type-erased storage for internal handles that must participate in ordinary
/// [`Value`] ownership without exposing forgeable identifiers to Glam code.
#[derive(Clone)]
pub struct OpaqueValue {
    payload: Arc<dyn Any + Send + Sync>,
}

impl OpaqueValue {
    pub(crate) fn new<T: Any + Send + Sync>(payload: Arc<T>) -> Self {
        Self { payload }
    }

    pub(crate) fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.payload.clone().downcast().ok()
    }
}

impl fmt::Debug for OpaqueValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueValue(..)")
    }
}

impl PartialEq for OpaqueValue {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.payload, &other.payload)
    }
}

impl Eq for OpaqueValue {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetValue {
    runtime: CoreRuntimeNet,
}

impl NetValue {
    pub fn new(runtime: CoreRuntimeNet) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &CoreRuntimeNet {
        &self.runtime
    }

    pub fn into_runtime(self) -> CoreRuntimeNet {
        self.runtime
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionCode {
    runtime: CoreRuntimeNet,
    arity: usize,
    capture_count: usize,
}

impl FunctionCode {
    pub(crate) fn new(runtime: CoreRuntimeNet, arity: usize, capture_count: usize) -> Self {
        Self {
            runtime,
            arity,
            capture_count,
        }
    }

    pub(crate) fn runtime(&self) -> &CoreRuntimeNet {
        &self.runtime
    }

    pub fn arity(&self) -> usize {
        self.arity
    }

    pub fn capture_count(&self) -> usize {
        self.capture_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionValue {
    stage: NetValue,
    remaining_arity: usize,
}

impl FunctionValue {
    pub(crate) fn new(stage: NetValue, remaining_arity: usize) -> Self {
        assert!(
            remaining_arity > 0,
            "a function stage must accept an argument"
        );
        Self {
            stage,
            remaining_arity,
        }
    }

    pub(crate) fn stage(&self) -> &NetValue {
        &self.stage
    }

    pub fn remaining_arity(&self) -> usize {
        self.remaining_arity
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinCall {
    pub builtin: Builtin,
    pub arguments: Arc<[Value]>,
}

impl BuiltinCall {
    pub fn new(builtin: Builtin) -> Self {
        Self {
            builtin,
            arguments: Arc::from([]),
        }
    }
}

#[derive(Clone)]
pub(crate) enum LazySource {
    Error,
    ComputedFixpoint(Arc<FixpointComputation>),
    Deferred(Arc<DeferredComputation>),
    ReflectionTask(Arc<ReflectionComputation>),
    Access {
        path: Arc<[CoreDataKey]>,
        arguments: Arc<[Value]>,
    },
    Application(Arc<LazyApplication>),
    Builtin(BuiltinCall),
    /// A closed freer-effect program that constructs one interaction net.
    /// Mutable interpreter state belongs to the observing evaluation task.
    NetConstruction(Arc<Value>),
    NetComputation(NetValue),
    FunctionCall {
        function: FunctionValue,
        arguments: Arc<[Value]>,
    },
}

pub(crate) struct LazyApplication {
    function: Value,
    arguments: Arc<[Value]>,
}

impl LazyApplication {
    pub(crate) fn function(&self) -> &Value {
        &self.function
    }

    pub(crate) fn arguments(&self) -> &[Value] {
        &self.arguments
    }
}

#[derive(Clone)]
pub(crate) enum FixpointComputation {
    Function(Value),
    ObjectInstance(Value),
}

impl TaskPromise {
    pub(crate) fn owner(&self) -> EvaluationTaskId {
        self.owner
    }

    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }
}

pub(crate) type DeferredComputation =
    dyn Fn(&EvalContext) -> Result<Value, EvaluationHalt> + Send + Sync;

/// A lazy reflection task which either gates a target or returns its result.
///
/// The payload is boxed so adding reflection does not enlarge every
/// `LazySource`. Task execution state remains in `EvaluationSession`; this
/// cell only remembers which task the first observer started.
pub(crate) struct ReflectionComputation {
    effect: Value,
    completion: ReflectionCompletion,
    task: OnceLock<Result<EvaluationTaskHandle, Arc<EvaluationFailure>>>,
}

pub(crate) enum ReflectionCompletion {
    Gate { target: Value },
    ReturnValue,
}

impl ReflectionComputation {
    pub(crate) fn task(
        &self,
        context: &EvalContext,
    ) -> Result<&EvaluationTaskHandle, &Arc<EvaluationFailure>> {
        self.task
            .get_or_init(|| {
                context
                    .start_reflection_task(self.effect.clone(), self.result_policy())
                    .map_err(|error| Arc::new(EvaluationFailure::message(error)))
            })
            .as_ref()
    }

    pub(crate) fn completion(&self) -> &ReflectionCompletion {
        &self.completion
    }

    fn result_policy(&self) -> ReflectionTaskResultPolicy {
        match self.completion {
            ReflectionCompletion::Gate { .. } => ReflectionTaskResultPolicy::RequireUnit,
            ReflectionCompletion::ReturnValue => ReflectionTaskResultPolicy::ReturnValue,
        }
    }
}

impl LazyValue {
    pub(crate) fn from_access(
        values: &CoreValueFactory,
        path: Arc<[CoreDataKey]>,
        arguments: Arc<[Value]>,
    ) -> Self {
        Self::with_source(values, "access", LazySource::Access { path, arguments })
    }

    pub(crate) fn from_application(
        values: &CoreValueFactory,
        function: Value,
        arguments: Arc<[Value]>,
    ) -> Self {
        assert!(
            !arguments.is_empty(),
            "lazy application requires an argument"
        );
        Self::with_source(
            values,
            "application",
            LazySource::Application(Arc::new(LazyApplication {
                function,
                arguments,
            })),
        )
    }

    pub(crate) fn from_builtin(values: &CoreValueFactory, call: BuiltinCall) -> Self {
        Self::with_source(values, "builtin call", LazySource::Builtin(call))
    }

    pub(crate) fn from_net_construction(values: &CoreValueFactory, effect: Value) -> Self {
        Self::with_source(
            values,
            "interaction-net construction",
            LazySource::NetConstruction(Arc::new(effect)),
        )
    }

    pub(crate) fn from_function_call(
        values: &CoreValueFactory,
        function: FunctionValue,
        arguments: Arc<[Value]>,
    ) -> Self {
        Self::with_source(
            values,
            "function call",
            LazySource::FunctionCall {
                function,
                arguments,
            },
        )
    }

    pub(crate) fn from_net_computation(values: &CoreValueFactory, net: NetValue) -> Self {
        Self::with_source(values, "net computation", LazySource::NetComputation(net))
    }

    pub(crate) fn from_reflection_gate(
        values: &CoreValueFactory,
        effect: Value,
        target: Value,
    ) -> Self {
        Self::with_source(
            values,
            "reflection annotation",
            LazySource::ReflectionTask(Arc::new(ReflectionComputation {
                effect,
                completion: ReflectionCompletion::Gate { target },
                task: OnceLock::new(),
            })),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Builtin {
    Append,
    Add,
    Subtract,
    Multiply,
    Divide,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    LessEqual,
    Less,
    Fixpoint,
    Anno,
    /// Compiler-private assertion gate. Accepts a diagnostic context, the
    /// value that must evaluate to unit, and the target returned on success.
    AssertUnit,
    Seq,
    Spark,
    InteractionNet,
    NetArity,
    /// Host-provided capability for inspecting opaque compilation origins.
    /// The constructor is exposed only through the reflection environment.
    InspectOrigin,
    MergeDuplicate,
    Floor,
    Mod,
    Slice,
    Map,
    ListConcat,
    ListLen,
    ListSplit,
    ListSplitEnd,
    ListAt,
    ListHead,
    ListTail,
    /// Compiler-private pass/fail observations used by pattern lowering.
    PatternIsList,
    PatternListTryUncons,
    PatternListTryUnsnoc,
    PatternListIsEmpty,
    PatternEqual,
    PatternPathEqual,
    PatternIsDict,
    PatternDictTryTake,
    PatternDictTryTakeOptional,
    PatternDictIsEmpty,
    /// Splits binary-compatible text into shared line segments without their
    /// newline delimiters. Internal support for closed formatting functions.
    TextLines,
    ListEffect,
    ListEffectReturn,
    ListEffectSeq,
    ListEffectAlt,
    ListEffectCut,
    ListEffectFix,
    /// Compiler-private selectors for pure conditional search results.
    IfResult,
    MatchResult,
    DictSingleton,
    DictUnion,
    DictUpdate,
    ObjectSpec,
    ObjectFromDict,
    ObjectLocalName,
    ObjectInstanceFromParts,
    ObjectInstance,
    /// Internal protocol adapters used while object/effect construction is
    /// still implemented by the bootstrap evaluator.
    EffectApply,
    EffectCall,
    EffectMap,
    EffectMapRun,
    EffectMapContinue,
    ObjectDefaultDefs,
    ObjectDictDefs,
    ObjectWithDefs,
    ObjectComposedDefs,
    ObjectOverrideDefs,
}

impl Builtin {
    pub fn arity(self) -> usize {
        match self {
            Self::Append => 2,
            Self::Add => 2,
            Self::Subtract => 2,
            Self::Multiply => 2,
            Self::Divide => 2,
            Self::Greater => 2,
            Self::GreaterEqual => 2,
            Self::Equal => 2,
            Self::NotEqual => 2,
            Self::LessEqual => 2,
            Self::Less => 2,
            Self::Fixpoint => 1,
            Self::Anno => 2,
            Self::AssertUnit => 3,
            Self::Seq => 2,
            Self::Spark => 2,
            Self::InteractionNet => 1,
            Self::NetArity => 2,
            Self::InspectOrigin => 1,
            Self::MergeDuplicate => 3,
            Self::Floor => 1,
            Self::Mod => 2,
            Self::Slice => 3,
            Self::Map => 2,
            Self::ListConcat => 1,
            Self::ListLen => 1,
            Self::ListSplit => 2,
            Self::ListSplitEnd => 2,
            Self::ListAt => 2,
            Self::ListHead => 1,
            Self::ListTail => 1,
            Self::PatternIsList => 1,
            Self::PatternListTryUncons => 1,
            Self::PatternListTryUnsnoc => 1,
            Self::PatternListIsEmpty => 1,
            Self::PatternEqual => 2,
            Self::PatternPathEqual => 2,
            Self::PatternIsDict => 1,
            Self::PatternDictTryTake => 2,
            Self::PatternDictTryTakeOptional => 2,
            Self::PatternDictIsEmpty => 1,
            Self::TextLines => 1,
            Self::ListEffect => 1,
            Self::ListEffectReturn => 1,
            Self::ListEffectSeq => 2,
            Self::ListEffectAlt => 2,
            Self::ListEffectCut => 1,
            Self::ListEffectFix => 1,
            Self::IfResult => 1,
            Self::MatchResult => 1,
            Self::DictSingleton => 2,
            Self::DictUnion => 2,
            Self::DictUpdate => 3,
            Self::ObjectSpec => 1,
            Self::ObjectFromDict => 1,
            Self::ObjectLocalName => 2,
            Self::ObjectInstanceFromParts => 3,
            Self::ObjectInstance => 1,
            Self::EffectApply => 3,
            Self::EffectCall => 3,
            Self::EffectMap => 2,
            Self::EffectMapRun => 4,
            Self::EffectMapContinue => 4,
            Self::ObjectDefaultDefs => 2,
            Self::ObjectDictDefs => 3,
            Self::ObjectWithDefs => 2,
            Self::ObjectComposedDefs => 4,
            Self::ObjectOverrideDefs => 3,
        }
    }
}

pub type Dict = RedBlackTreeMapSync<Key, Value>;

/// An opaque deferred tail in a persistent list.
///
/// Lists preserve the distinction between computed lazy chunks and named
/// assignment holes without depending on evaluator state. Only evaluator-owned
/// list operations decide when to force either kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListThunk {
    Lazy(LazyValue),
    Promised(PromisedValue),
}

impl From<LazyValue> for ListThunk {
    fn from(lazy: LazyValue) -> Self {
        Self::Lazy(lazy)
    }
}

impl From<PromisedValue> for ListThunk {
    fn from(promise: PromisedValue) -> Self {
        Self::Promised(promise)
    }
}

pub type List = crate::list::List<Value, ListThunk>;

fn list_to_key_items(list: &List) -> Option<Arc<[Key]>> {
    let items = std::cell::RefCell::new(Vec::new());
    list.for_each_segment(
        &mut |bytes| {
            items
                .borrow_mut()
                .extend(bytes.iter().map(|byte| Key::Number(Number::from_u8(*byte))));
            Ok::<_, ()>(())
        },
        &mut |values| {
            for value in values {
                items.borrow_mut().push(Key::from_value(value).ok_or(())?);
            }
            Ok(())
        },
    )
    .ok()?;
    Some(Arc::from(items.into_inner()))
}

impl Value {
    /// Names the most useful semantic category for diagnostics without
    /// changing the representation-oriented public value kind.
    pub(crate) fn diagnostic_kind_name(&self) -> &'static str {
        match self {
            Self::Atom(atom) if atom.key() == &*keys::UNIT => "Unit",
            Self::Atom(_) => "Atom",
            Self::Number(_) => "Number",
            Self::Binary(_) => "Binary",
            Self::List(_) => "List",
            Self::Dict(dict) if dict.is_empty() => "Undefined",
            Self::Dict(_) => "Dict",
            Self::Builtin(_) | Self::PartialBuiltin(_) | Self::Function(_) => "Function",
            Self::Net(_) => "Net",
            Self::Lazy(_) | Self::Promised(_) => "Lazy",
            Self::Metadata(_) => "Sealed",
            Self::Opaque(_) => "Opaque",
        }
    }

    pub fn binary_from_text(text: &str) -> Self {
        Self::Binary(Bytes::copy_from_slice(text.as_bytes()))
    }

    pub(crate) fn deferred(
        values: &CoreValueFactory,
        label: impl Into<Arc<str>>,
        thunk: impl Fn(&EvalContext) -> Result<Value, EvaluationHalt> + Send + Sync + 'static,
    ) -> Self {
        Self::Lazy(LazyValue::deferred(values, label, thunk))
    }

    pub(crate) fn error(values: &CoreValueFactory, message: impl Into<Arc<str>>) -> Self {
        Self::Lazy(LazyValue::error(values, message))
    }

    pub(crate) fn reflection_gate(values: &CoreValueFactory, effect: Value, target: Value) -> Self {
        Self::Lazy(LazyValue::from_reflection_gate(values, effect, target))
    }

    pub(crate) fn reflection_task_result(values: &CoreValueFactory, effect: Value) -> Self {
        Self::Lazy(LazyValue::with_source(
            values,
            "reflection task result",
            LazySource::ReflectionTask(Arc::new(ReflectionComputation {
                effect,
                completion: ReflectionCompletion::ReturnValue,
                task: OnceLock::new(),
            })),
        ))
    }

    /// Constructs a sealed unit carrier with reflection-only metadata.
    pub(crate) fn metadata_carrier(metadata: Value) -> Self {
        Self::Metadata(MetadataCarrier::new(metadata))
    }

    /// Returns the canonical carrier whose associated metadata is `{}`.
    pub(crate) fn initial_metadata_carrier(values: &CoreValueFactory) -> Self {
        values.initial_metadata()
    }

    /// Returns a sealed carrier's associated metadata for privileged clients.
    pub(crate) fn associated_metadata(&self) -> Option<Value> {
        match self {
            Self::Metadata(carrier) => Some(carrier.associated_metadata()),
            _ => None,
        }
    }

    /// Constructs a builtin value at a specific curried stage without
    /// evaluating a saturated call.
    pub(crate) fn builtin_call(
        values: &CoreValueFactory,
        builtin: Builtin,
        arguments: Vec<Value>,
    ) -> Self {
        assert!(
            arguments.len() <= builtin.arity(),
            "builtin call contains too many arguments"
        );
        match arguments.len() {
            0 => Self::Builtin(builtin),
            supplied if supplied < builtin.arity() => Self::PartialBuiltin(BuiltinCall {
                builtin,
                arguments: Arc::from(arguments),
            }),
            _ => Self::Lazy(LazyValue::from_builtin(
                values,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )),
        }
    }

    pub fn singleton_list(value: Value) -> List {
        List::from_values(vec![value])
    }
}

impl Value {
    #[cfg(test)]
    pub fn get_key_path(&self, path: &[Key]) -> Option<&Value> {
        match path {
            [] => Some(self),
            [head, rest @ ..] => match self {
                Value::Dict(dict) => dict.get(head)?.get_key_path(rest),
                Value::Atom(_)
                | Value::Number(_)
                | Value::Binary(_)
                | Value::List(_)
                | Value::Function(_)
                | Value::Net(_)
                | Value::Builtin(_)
                | Value::PartialBuiltin(_)
                | Value::Lazy(_)
                | Value::Promised(_)
                | Value::Metadata(_)
                | Value::Opaque(_) => None,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn get_atom_path(&self, path: &[Atom]) -> Option<&Value> {
        let path = path.iter().cloned().map(Key::Atom).collect::<Vec<_>>();
        self.get_key_path(&path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    fn values() -> CoreValueFactory {
        test_value_factory()
    }

    struct DropSignal(Arc<AtomicBool>);

    struct CachedProbe;

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn runtime_cache_installs_one_complete_winner_after_racing_construction() {
        let factory = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        );
        let barrier = Arc::new(Barrier::new(2));
        let builds = Arc::new(AtomicUsize::new(0));
        let handles = (0..2)
            .map(|_| {
                let factory = factory.clone();
                let barrier = barrier.clone();
                let builds = builds.clone();
                std::thread::spawn(move || {
                    factory.cached(|| {
                        builds.fetch_add(1, Ordering::Relaxed);
                        barrier.wait();
                        CachedProbe
                    })
                })
            })
            .collect::<Vec<_>>();
        let mut winners = handles
            .into_iter()
            .map(|handle| handle.join().expect("cache race should finish"));
        let left = winners.next().expect("first cache winner");
        let right = winners.next().expect("second cache winner");
        assert_eq!(builds.load(Ordering::Relaxed), 2);
        assert!(Arc::ptr_eq(&left, &right));
    }

    #[test]
    fn distinct_runtime_caches_do_not_share_constructed_extensions() {
        let first = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        )
        .cached(|| CachedProbe);
        let second = CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        )
        .cached(|| CachedProbe);
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn terminal_lazy_cache_releases_its_shared_source_after_active_snapshots() {
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = DropSignal(dropped.clone());
        let lazy = LazyValue::deferred(&values(), "source release", move |_| {
            let _keep_signal_captured = &signal;
            Ok(values().unit())
        });
        let observer = lazy.clone();
        let active_snapshot = lazy
            .source_snapshot()
            .expect("an unresolved lazy should expose a source snapshot");

        let result = EvaluatedValue::try_from(values().unit()).expect("unit is already evaluated");
        assert!(observer.cache(Ok(result)).is_ok());
        assert!(
            lazy.source_snapshot().is_none(),
            "all clones should observe the released shared source"
        );
        assert!(
            !dropped.load(Ordering::Acquire),
            "an active worker snapshot should keep its producer alive"
        );

        drop(active_snapshot);
        assert!(
            dropped.load(Ordering::Acquire),
            "the producer should drop after its final active snapshot"
        );
    }

    #[test]
    fn boxed_reflection_computation_does_not_enlarge_lazy_source() {
        #[allow(dead_code)]
        enum LazySourceWithoutReflection {
            Error,
            ComputedFixpoint(Arc<FixpointComputation>),
            Deferred(Arc<DeferredComputation>),
            Access {
                path: Arc<[CoreDataKey]>,
                arguments: Arc<[Value]>,
            },
            Application(Arc<LazyApplication>),
            Builtin(BuiltinCall),
            NetComputation(NetValue),
            FunctionCall {
                function: FunctionValue,
                arguments: Arc<[Value]>,
            },
        }

        assert_eq!(
            std::mem::size_of::<LazySource>(),
            std::mem::size_of::<LazySourceWithoutReflection>()
        );
        assert_eq!(
            std::mem::size_of::<Arc<ReflectionComputation>>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn atoms_and_binary_keys_are_distinct() {
        let asm = Atom::from_key(&Key::binary_from_text("asm"));
        let dict = Dict::new_sync()
            .insert(Key::Atom(asm), Value::binary_from_text("atom"))
            .insert(
                Key::binary_from_text("asm"),
                Value::binary_from_text("binary"),
            );
        let value = Value::Dict(dict);

        assert_eq!(
            value.get_atom_path(&[asm]),
            Some(&Value::binary_from_text("atom"))
        );
    }

    #[test]
    fn atom_keys_are_canonical_by_key() {
        assert_eq!(
            Atom::from_key(&Key::binary_from_text("asm")),
            Atom::from_key(&Key::binary_from_text("asm"))
        );
        assert_eq!(
            Atom::from_key(&Key::binary_from_text("asm")).key(),
            &Key::binary_from_text("asm")
        );
    }

    #[test]
    fn atom_keys_from_equal_keys_are_canonical() {
        let binary_key = Key::binary_from_text("tag");
        let atom_key_1 = Key::atom_from_key(&binary_key);
        let atom_key_2 = Key::atom_from_key(&Key::binary_from_text("tag"));

        assert!(matches!(atom_key_1, Key::Atom(_)));
        assert_eq!(atom_key_1, atom_key_2);
        assert_ne!(atom_key_1, binary_key);
    }

    #[test]
    fn values_can_store_lists_and_numbers() {
        let value = Value::List(List::from_values(vec![
            Value::Number(1.into()),
            Value::Number(2.into()),
            Value::Number(3.into()),
        ]));

        assert!(matches!(value, Value::List(_)));
    }

    #[test]
    fn semantic_values_can_hold_atoms() {
        let value = Value::Atom(Atom::from_key(&Key::binary_from_text("greeting")));

        assert!(matches!(value, Value::Atom(_)));
    }

    #[test]
    fn metadata_carriers_hide_unit_and_associated_metadata() {
        let first = Value::initial_metadata_carrier(&values());
        let second = Value::initial_metadata_carrier(&values());
        let Value::Metadata(first_carrier) = &first else {
            panic!("initial metadata value should be a sealed carrier");
        };
        let Value::Metadata(second_carrier) = &second else {
            panic!("initial metadata value should be a sealed carrier");
        };

        assert!(
            Arc::ptr_eq(&first_carrier.metadata, &second_carrier.metadata),
            "initial metadata carriers should share one allocation"
        );
        assert_ne!(first, values().unit());
        assert_eq!(
            first.associated_metadata(),
            Some(Value::Dict(Dict::new_sync()))
        );
        assert_eq!(Key::from_value(&first), None);
        assert_eq!(first.diagnostic_kind_name(), "Sealed");

        let private = Value::metadata_carrier(Value::binary_from_text("hidden metadata"));
        assert_eq!(format!("{private:?}"), "Sealed(..)");
        assert!(!format!("{private:?}").contains("hidden metadata"));
    }

    #[test]
    fn metadata_carriers_transport_through_ordinary_containers() {
        let carrier = Value::initial_metadata_carrier(&values());
        let list = Value::List(List::from_values(vec![carrier.clone()]));
        let dict =
            Value::Dict(Dict::new_sync().insert(Key::atom_from_text("trace"), carrier.clone()));

        assert_eq!(list, Value::List(List::from_values(vec![carrier.clone()])));
        assert_eq!(
            dict.get_key_path(&[Key::atom_from_text("trace")]),
            Some(&carrier)
        );
    }

    #[test]
    fn semantic_values_can_hold_lazy_errors() {
        let value = Value::error(&values(), "ambiguous key");

        assert!(
            matches!(value, Value::Lazy(lazy) if lazy.cached().is_some_and(|value| value.is_err()))
        );
    }

    #[test]
    fn evaluated_values_reject_deferred_outer_shells_only() {
        let field = Value::deferred(&values(), "lazy field", |_| Ok(Value::Number(1.into())));
        let promise = PromisedValue::new(&values(), "promised field");
        let container =
            Value::Dict(Dict::new_sync().insert(Key::atom_from_text("field"), field.clone()));
        let sealed = Value::initial_metadata_carrier(&values());

        let evaluated = EvaluatedValue::try_from(container.clone())
            .expect("a container with a lazy field is in outer WHNF");
        assert_eq!(evaluated.into_value(), container);
        assert_eq!(
            EvaluatedValue::try_from(sealed.clone())
                .expect("a sealed carrier is already in outer WHNF")
                .into_value(),
            sealed
        );
        assert!(matches!(
            EvaluatedValue::try_from(field),
            Err(Value::Lazy(_))
        ));
        assert!(matches!(
            EvaluatedValue::try_from(Value::Promised(promise)),
            Err(Value::Promised(_))
        ));
    }

    #[test]
    fn promised_assignments_retain_deferred_aliases() {
        let target = PromisedValue::new(&values(), "target");
        let forwarding = PromisedValue::new(&values(), "forwarding");
        forwarding
            .set(Value::Promised(target))
            .expect("new promise should accept its target");

        assert!(matches!(
            forwarding.assignment(),
            Some(Ok(Value::Promised(_)))
        ));

        let ready = PromisedValue::new(&values(), "ready");
        ready
            .set(Value::Number(42.into()))
            .expect("new promise should accept its value");
        assert_eq!(ready.assignment(), Some(Ok(Value::Number(42.into()))));
    }

    #[test]
    fn lazy_cycle_failures_retain_member_identity_and_labels() {
        let first = LazyValue::error(&values(), "first failure");
        let second = LazyValue::error(&values(), "second failure");
        let cycle = EvaluationFailure::dependency_cycle(Arc::new(LazyCycle {
            members: vec![
                LazyCycleMember {
                    id: first.id(),
                    label: Arc::from("first"),
                },
                LazyCycleMember {
                    id: second.id(),
                    label: Arc::from("second"),
                },
            ]
            .into_boxed_slice(),
        }));

        assert_eq!(
            cycle.to_string(),
            format!(
                "lazy dependency cycle -> {} (first) -> {} (second) -> {}",
                first.id().get(),
                second.id().get(),
                first.id().get()
            )
        );
    }

    #[test]
    fn keys_can_represent_nested_value_data() {
        let value = Value::Dict(Dict::new_sync().insert(
            Key::atom_from_text("payload"),
            Value::List(List::concat(
                List::from_values(vec![Value::Number(1.into())]),
                List::from_bytes(Bytes::from_static(b"Hi")),
            )),
        ));

        assert_eq!(
            Key::from_value(&value),
            Some(Key::Dict(Arc::from([(
                Key::atom_from_text("payload"),
                Key::List(Arc::from([
                    Key::Number(1.into()),
                    Key::Number(Number::from_u8(b'H')),
                    Key::Number(Number::from_u8(b'i')),
                ])),
            )])))
        );
    }

    #[test]
    fn empty_dict_values_are_elided_from_dict_keys() {
        let empty = Value::Dict(Dict::new_sync());
        let with_empty_field = Value::Dict(
            Dict::new_sync().insert(Key::atom_from_text("key"), Value::Dict(Dict::new_sync())),
        );

        assert_eq!(Key::from_value(&empty), Some(Key::Dict(Arc::from([]))));
        assert_eq!(
            Key::from_value(&with_empty_field),
            Some(Key::Dict(Arc::from([])))
        );
    }

    #[test]
    fn keys_reject_deferred_values() {
        assert_eq!(
            Key::from_value(&Value::deferred(&values(), "number", |_| {
                Ok(Value::Number(1.into()))
            })),
            None
        );
        assert_eq!(
            Key::from_value(&Value::Promised(PromisedValue::new(&values(), "number"))),
            None
        );
    }

    #[test]
    fn abstract_global_path_keys_are_distinct_from_list_keys() {
        let abstract_path = Key::abstract_global_path(["builtin", "unit"]);
        let list_path = Key::List(Arc::from([
            Key::binary_from_text("builtin"),
            Key::binary_from_text("unit"),
        ]));

        assert_ne!(abstract_path, list_path);
    }

    #[test]
    fn values_support_non_atom_key_paths() {
        let list_key = Key::List(Arc::from([Key::Number(1.into()), Key::Number(2.into())]));
        let dict = Dict::new_sync().insert(list_key.clone(), Value::Number(7.into()));
        let value = Value::Dict(dict);

        assert_eq!(
            value.get_key_path(&[list_key]),
            Some(&Value::Number(7.into()))
        );
    }

    #[test]
    fn list_concat_shares_segments() {
        let bytes = List::from_bytes(Bytes::from_static(b"Hello"));
        let values = List::from_values(vec![Value::Number(33.into())]);
        let list = List::concat(bytes, values);

        assert!(!list.is_empty());
    }

    #[test]
    fn balanced_lists_use_finger_tree_and_preserve_segments() {
        let list = List::concat(
            List::concat(
                List::from_bytes(Bytes::from_static(b"He")),
                List::from_bytes(Bytes::from_static(b"ll")),
            ),
            List::from_values(vec![Value::Number(111.into()), Value::Number(33.into())]),
        );

        let balanced = list.balanced();

        assert_eq!(balanced.len(), 6);
        let bytes = std::cell::RefCell::new(Vec::new());
        let values = std::cell::RefCell::new(Vec::new());
        balanced
            .for_each_segment(
                &mut |segment| {
                    bytes.borrow_mut().extend_from_slice(segment);
                    Ok::<_, ()>(())
                },
                &mut |segment| {
                    values.borrow_mut().extend(segment.iter().cloned());
                    Ok(())
                },
            )
            .expect("balanced list should walk");
        assert_eq!(bytes.into_inner(), b"Hell");
        assert_eq!(
            values.into_inner(),
            vec![Value::Number(111.into()), Value::Number(33.into())]
        );
    }

    #[test]
    fn list_slice_uses_rope_segments() {
        let list = List::concat(
            List::from_bytes(Bytes::from_static(b"Hello")),
            List::from_values(vec![Value::Number(44.into()), Value::Number(32.into())]),
        )
        .balanced();

        let sliced = list.slice(1, 6);

        let bytes = std::cell::RefCell::new(Vec::new());
        let values = std::cell::RefCell::new(Vec::new());
        sliced
            .for_each_segment(
                &mut |segment| {
                    bytes.borrow_mut().extend_from_slice(segment);
                    Ok::<_, ()>(())
                },
                &mut |segment| {
                    values.borrow_mut().extend(segment.iter().cloned());
                    Ok(())
                },
            )
            .expect("sliced list should walk");
        assert_eq!(bytes.into_inner(), b"ello");
        assert_eq!(values.into_inner(), vec![Value::Number(44.into())]);
    }

    #[test]
    fn list_slice_shares_partial_byte_and_value_leaves() {
        let bytes = Bytes::from_static(b"Hello");
        let value_leaf =
            List::from_values(vec![Value::Number(44.into()), Value::Number(32.into())]);
        let original_value_ptr = std::cell::Cell::new(std::ptr::null());
        value_leaf
            .for_each_segment(&mut |_| Ok::<_, ()>(()), &mut |values| {
                original_value_ptr.set(values.as_ptr());
                Ok(())
            })
            .unwrap();
        let list = List::concat(List::from_bytes(bytes.clone()), value_leaf).balanced();

        let sliced = list.slice(1, 6);

        let byte_ptrs = std::cell::RefCell::new(Vec::new());
        let value_ptrs = std::cell::RefCell::new(Vec::new());
        sliced
            .for_each_segment(
                &mut |segment| {
                    byte_ptrs.borrow_mut().push(segment.as_ptr());
                    Ok::<_, ()>(())
                },
                &mut |segment| {
                    value_ptrs.borrow_mut().push(segment.as_ptr());
                    Ok(())
                },
            )
            .expect("sliced list should walk");

        assert_eq!(byte_ptrs.into_inner(), vec![bytes[1..].as_ptr()]);
        assert_eq!(value_ptrs.into_inner(), vec![original_value_ptr.get()]);
    }

    #[test]
    fn list_split_from_end_preserves_lazy_concat_when_split_is_in_right_branch() {
        let left = List::from_values(vec![Value::Number(1.into()), Value::Number(2.into())]);
        let list = List::concat(left.clone(), List::from_bytes(Bytes::from_static(b"abc")));

        let (prefix, suffix) = list
            .split_from_end(1)
            .expect("suffix count should be in bounds");

        assert_eq!(prefix.len(), left.len() + 2);

        let bytes = std::cell::RefCell::new(Vec::new());
        suffix
            .for_each_segment(
                &mut |segment| {
                    bytes.borrow_mut().extend_from_slice(segment);
                    Ok::<_, ()>(())
                },
                &mut |_| Ok(()),
            )
            .expect("suffix should walk");
        assert_eq!(bytes.into_inner(), b"c");
    }
}

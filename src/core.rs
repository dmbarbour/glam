use std::any::Any;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use internment::Intern;
use rpds::RedBlackTreeMapSync;

use crate::core_net::{CoreDataKey, CoreRuntimeNet};
use crate::evaluation::{EvalContext, EvaluationTaskHandle, EvaluationTaskId, EvaluationWaitToken};
use crate::number::Number;

pub(crate) mod keys;

#[derive(Clone)]
pub struct LazyValue {
    id: u64,
    label: Arc<str>,
    source: LazySource,
    result: Arc<OnceLock<Result<Value, Arc<str>>>>,
}

static NEXT_LAZY_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_FIXPOINT_ID: AtomicU64 = AtomicU64::new(1);

impl LazyValue {
    fn with_source(label: impl Into<Arc<str>>, source: LazySource) -> Self {
        Self {
            id: NEXT_LAZY_ID.fetch_add(1, Ordering::Relaxed),
            label: label.into(),
            source,
            result: Arc::new(OnceLock::new()),
        }
    }

    pub fn promised(label: impl Into<Arc<str>>) -> Self {
        Self::with_source(label, LazySource::Promised)
    }

    pub(crate) fn fixpoint(
        context: &EvalContext,
        label: impl Into<Arc<str>>,
    ) -> Result<Self, Arc<str>> {
        let result = Arc::new(OnceLock::new());
        let (owner, wait) = context.register_promise(&result)?;
        let id = allocate_fixpoint_id()?;
        Ok(Self {
            id: NEXT_LAZY_ID.fetch_add(1, Ordering::Relaxed),
            label: label.into(),
            source: LazySource::Fixpoint(Arc::new(FixpointCell { id, owner, wait })),
            result,
        })
    }

    pub(crate) fn computed_fixpoint(
        label: impl Into<Arc<str>>,
        computation: FixpointComputation,
    ) -> Result<Self, Arc<str>> {
        let id = allocate_fixpoint_id()?;
        Ok(Self {
            id: NEXT_LAZY_ID.fetch_add(1, Ordering::Relaxed),
            label: label.into(),
            source: LazySource::ComputedFixpoint(Arc::new(ComputedFixpointCell {
                id,
                computation,
                state: Mutex::new(ComputedFixpointState::Unclaimed),
            })),
            result: Arc::new(OnceLock::new()),
        })
    }

    pub(crate) fn deferred(
        label: impl Into<Arc<str>>,
        thunk: impl Fn(&EvalContext) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(label, LazySource::Deferred(Arc::new(thunk)))
    }

    pub fn error(message: impl Into<Arc<str>>) -> Self {
        let value = Self::with_source("error", LazySource::Promised);
        value
            .result
            .set(Err(message.into()))
            .expect("new lazy error must be uninitialized");
        value
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub fn set(&self, value: Value) -> Result<(), Value> {
        self.result.set(Ok(value)).map_err(|result| {
            result.expect("setting a lazy value always supplies a successful value")
        })
    }

    pub fn cached(&self) -> Option<Result<Value, Arc<str>>> {
        self.result.get().cloned()
    }

    pub fn cache(&self, value: Result<Value, Arc<str>>) -> Result<Value, Arc<str>> {
        let _ = self.result.set(value);
        self.result
            .get()
            .expect("lazy cache should contain a value after set")
            .clone()
    }

    pub(crate) fn is_promised(&self) -> bool {
        matches!(self.source, LazySource::Promised) && self.result.get().is_none()
    }

    pub(crate) fn force_deferred(&self, context: &EvalContext) -> Option<Result<Value, Arc<str>>> {
        match &self.source {
            LazySource::Deferred(thunk) => Some(
                self.result
                    .get_or_init(|| thunk(context).map_err(Arc::from))
                    .clone(),
            ),
            _ => None,
        }
    }
}

fn allocate_fixpoint_id() -> Result<FixpointId, Arc<str>> {
    NEXT_FIXPOINT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map(|id| FixpointId(NonZeroU64::new(id).expect("fixpoint IDs start at one")))
        .map_err(|_| Arc::<str>::from("fixpoint IDs exhausted"))
}

impl PartialEq for LazyValue {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for LazyValue {}

impl fmt::Debug for LazyValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyValue")
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
            | Value::Opaque(_) => None,
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// A closed suspended computation, promised value, or memoized failure.
    Lazy(LazyValue),
    /// Host-owned identity whose representation is deliberately unavailable to
    /// Glam programs. Clones retain the payload and compare by identity.
    Opaque(OpaqueValue),
}

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
enum LazySource {
    Promised,
    Fixpoint(Arc<FixpointCell>),
    ComputedFixpoint(Arc<ComputedFixpointCell>),
    Deferred(Arc<DeferredComputation>),
    ReflectionGate(Arc<ReflectionGate>),
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

struct LazyApplication {
    function: Value,
    arguments: Arc<[Value]>,
}

pub(crate) struct FixpointCell {
    id: FixpointId,
    owner: EvaluationTaskId,
    wait: EvaluationWaitToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FixpointId(NonZeroU64);

#[derive(Clone)]
pub(crate) enum FixpointComputation {
    Function(Value),
    ObjectInstance(Value),
}

pub(crate) struct ComputedFixpointCell {
    id: FixpointId,
    computation: FixpointComputation,
    state: Mutex<ComputedFixpointState>,
}

enum ComputedFixpointState {
    Unclaimed,
    Running(FixpointClaim),
    Suspended(FixpointClaim),
    Complete(FixpointClaim),
}

#[derive(Clone)]
struct FixpointClaim {
    owner: EvaluationTaskId,
    wait: EvaluationWaitToken,
}

pub(crate) enum ComputedFixpointAction {
    Produce {
        owner: EvaluationTaskId,
        computation: FixpointComputation,
    },
    Recursive {
        id: u64,
        owner: EvaluationTaskId,
    },
    Wait(EvaluationWaitToken),
}

impl FixpointCell {
    pub(crate) fn id(&self) -> u64 {
        self.id.0.get()
    }

    pub(crate) fn owner(&self) -> EvaluationTaskId {
        self.owner
    }

    pub(crate) fn wait(&self) -> &EvaluationWaitToken {
        &self.wait
    }
}

impl ComputedFixpointCell {
    pub(crate) fn begin(
        &self,
        context: &EvalContext,
        result: &Arc<OnceLock<Result<Value, Arc<str>>>>,
    ) -> Result<ComputedFixpointAction, Arc<str>> {
        let observer = context.task_id()?;
        let mut state = self
            .state
            .lock()
            .expect("computed fixpoint state was poisoned");
        match &*state {
            ComputedFixpointState::Unclaimed => {
                let (owner, wait) = context.register_promise(result)?;
                let claim = FixpointClaim { owner, wait };
                *state = ComputedFixpointState::Running(claim);
                Ok(ComputedFixpointAction::Produce {
                    owner,
                    computation: self.computation.clone(),
                })
            }
            ComputedFixpointState::Running(claim) if claim.owner == observer => {
                Ok(ComputedFixpointAction::Recursive {
                    id: self.id.0.get(),
                    owner: claim.owner,
                })
            }
            ComputedFixpointState::Suspended(claim) if claim.owner == observer => {
                let claim = claim.clone();
                *state = ComputedFixpointState::Running(claim.clone());
                Ok(ComputedFixpointAction::Produce {
                    owner: claim.owner,
                    computation: self.computation.clone(),
                })
            }
            ComputedFixpointState::Running(claim)
            | ComputedFixpointState::Suspended(claim)
            | ComputedFixpointState::Complete(claim) => {
                Ok(ComputedFixpointAction::Wait(claim.wait.clone()))
            }
        }
    }

    pub(crate) fn suspend(&self, owner: EvaluationTaskId) {
        let mut state = self
            .state
            .lock()
            .expect("computed fixpoint state was poisoned");
        let ComputedFixpointState::Running(claim) = &*state else {
            return;
        };
        assert_eq!(
            claim.owner, owner,
            "only the producer may suspend a fixpoint"
        );
        *state = ComputedFixpointState::Suspended(claim.clone());
    }

    pub(crate) fn complete(&self, context: &EvalContext, owner: EvaluationTaskId) {
        let wait = {
            let mut state = self
                .state
                .lock()
                .expect("computed fixpoint state was poisoned");
            let ComputedFixpointState::Running(claim) = &*state else {
                return;
            };
            assert_eq!(
                claim.owner, owner,
                "only the producer may complete a fixpoint"
            );
            let claim = claim.clone();
            let wait = claim.wait.clone();
            *state = ComputedFixpointState::Complete(claim);
            wait
        };
        context.release_owned_promise(owner, &wait);
    }
}

type DeferredComputation = dyn Fn(&EvalContext) -> Result<Value, String> + Send + Sync;

/// A lazy sequencing boundary for `anno refl:Effect Target`.
///
/// The payload is boxed so adding reflection does not enlarge every
/// `LazySource`. Task execution state remains in `EvaluationSession`; this
/// cell only remembers which task the first observer started.
pub(crate) struct ReflectionGate {
    effect: Value,
    target: Value,
    task: OnceLock<Result<EvaluationTaskHandle, Arc<str>>>,
}

impl ReflectionGate {
    pub(crate) fn task(&self, context: &EvalContext) -> Result<&EvaluationTaskHandle, &Arc<str>> {
        self.task
            .get_or_init(|| context.start_reflection_task(self.effect.clone()))
            .as_ref()
    }

    pub(crate) fn target(&self) -> &Value {
        &self.target
    }
}

impl LazyValue {
    pub(crate) fn from_access(path: Arc<[CoreDataKey]>, arguments: Arc<[Value]>) -> Self {
        Self::with_source("access", LazySource::Access { path, arguments })
    }

    pub(crate) fn from_application(function: Value, arguments: Arc<[Value]>) -> Self {
        assert!(
            !arguments.is_empty(),
            "lazy application requires an argument"
        );
        Self::with_source(
            "application",
            LazySource::Application(Arc::new(LazyApplication {
                function,
                arguments,
            })),
        )
    }

    pub(crate) fn from_builtin(call: BuiltinCall) -> Self {
        Self::with_source("builtin call", LazySource::Builtin(call))
    }

    pub(crate) fn from_function_call(function: FunctionValue, arguments: Arc<[Value]>) -> Self {
        Self::with_source(
            "function call",
            LazySource::FunctionCall {
                function,
                arguments,
            },
        )
    }

    pub(crate) fn from_net_computation(net: NetValue) -> Self {
        Self::with_source("net computation", LazySource::NetComputation(net))
    }

    pub(crate) fn from_reflection_gate(effect: Value, target: Value) -> Self {
        Self::with_source(
            "reflection annotation",
            LazySource::ReflectionGate(Arc::new(ReflectionGate {
                effect,
                target,
                task: OnceLock::new(),
            })),
        )
    }

    pub(crate) fn access(&self) -> Option<(&[CoreDataKey], &[Value])> {
        match &self.source {
            LazySource::Access { path, arguments } => Some((path, arguments)),
            _ => None,
        }
    }

    pub(crate) fn application(&self) -> Option<(&Value, &[Value])> {
        match &self.source {
            LazySource::Application(application) => {
                Some((&application.function, &application.arguments))
            }
            _ => None,
        }
    }

    pub(crate) fn builtin(&self) -> Option<&BuiltinCall> {
        match &self.source {
            LazySource::Builtin(call) => Some(call),
            _ => None,
        }
    }

    pub(crate) fn function_call(&self) -> Option<(&FunctionValue, &[Value])> {
        match &self.source {
            LazySource::FunctionCall {
                function,
                arguments,
            } => Some((function, arguments)),
            _ => None,
        }
    }

    pub(crate) fn net_computation(&self) -> Option<&NetValue> {
        match &self.source {
            LazySource::NetComputation(net) => Some(net),
            _ => None,
        }
    }

    pub(crate) fn reflection_gate(&self) -> Option<&ReflectionGate> {
        match &self.source {
            LazySource::ReflectionGate(gate) => Some(gate),
            _ => None,
        }
    }

    pub(crate) fn fixpoint_cell(&self) -> Option<&FixpointCell> {
        match &self.source {
            LazySource::Fixpoint(cell) => Some(cell),
            _ => None,
        }
    }

    pub(crate) fn computed_fixpoint_cell(&self) -> Option<&ComputedFixpointCell> {
        match &self.source {
            LazySource::ComputedFixpoint(cell) => Some(cell),
            _ => None,
        }
    }

    pub(crate) fn result_cell(&self) -> &Arc<OnceLock<Result<Value, Arc<str>>>> {
        &self.result
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
    Seq,
    Spark,
    MergeDuplicate,
    Floor,
    Mod,
    Slice,
    Map,
    ListConcat,
    ListLen,
    ListSplit,
    ListSplitEnd,
    ListHead,
    ListTail,
    /// Splits binary-compatible text into shared line segments without their
    /// newline delimiters. Internal support for closed formatting functions.
    TextLines,
    ListEffect,
    ListEffectReturn,
    ListEffectSeq,
    ListEffectAlt,
    ListEffectCut,
    ListEffectFix,
    DictSingleton,
    DictUnion,
    DictUpdate,
    ObjectSpec,
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
            Self::Seq => 2,
            Self::Spark => 2,
            Self::MergeDuplicate => 3,
            Self::Floor => 1,
            Self::Mod => 2,
            Self::Slice => 3,
            Self::Map => 2,
            Self::ListConcat => 1,
            Self::ListLen => 1,
            Self::ListSplit => 2,
            Self::ListSplitEnd => 2,
            Self::ListHead => 1,
            Self::ListTail => 1,
            Self::TextLines => 1,
            Self::ListEffect => 1,
            Self::ListEffectReturn => 1,
            Self::ListEffectSeq => 2,
            Self::ListEffectAlt => 2,
            Self::ListEffectCut => 1,
            Self::ListEffectFix => 1,
            Self::DictSingleton => 2,
            Self::DictUnion => 2,
            Self::DictUpdate => 3,
            Self::ObjectSpec => 1,
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

pub type List = crate::list::List<Value, LazyValue>;

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
    pub fn binary_from_text(text: &str) -> Self {
        Self::Binary(Bytes::copy_from_slice(text.as_bytes()))
    }

    pub(crate) fn deferred(
        label: impl Into<Arc<str>>,
        thunk: impl Fn(&EvalContext) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self::Lazy(LazyValue::deferred(label, thunk))
    }

    pub fn error(message: impl Into<Arc<str>>) -> Self {
        Self::Lazy(LazyValue::error(message))
    }

    pub(crate) fn reflection_gate(effect: Value, target: Value) -> Self {
        Self::Lazy(LazyValue::from_reflection_gate(effect, target))
    }

    /// Constructs a builtin value at a specific curried stage without
    /// evaluating a saturated call.
    pub fn builtin_call(builtin: Builtin, arguments: Vec<Value>) -> Self {
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
            _ => Self::Lazy(LazyValue::from_builtin(BuiltinCall {
                builtin,
                arguments: Arc::from(arguments),
            })),
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

    #[test]
    fn boxed_reflection_gate_does_not_enlarge_lazy_source() {
        #[allow(dead_code)]
        enum LazySourceWithoutReflection {
            Promised,
            Fixpoint(Arc<FixpointCell>),
            ComputedFixpoint(Arc<ComputedFixpointCell>),
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
            std::mem::size_of::<Arc<ReflectionGate>>(),
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
    fn semantic_values_can_hold_lazy_errors() {
        let value = Value::error("ambiguous key");

        assert!(
            matches!(value, Value::Lazy(lazy) if lazy.cached().is_some_and(|value| value.is_err()))
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
    fn keys_reject_lazy_values() {
        assert_eq!(
            Key::from_value(&Value::deferred("number", |_| Ok(Value::Number(1.into())))),
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

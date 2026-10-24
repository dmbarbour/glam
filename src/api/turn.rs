//! An affine runtime whose holding thread takes turns on a tree of
//! machines (`runtime-api-turns-on-machines`, `machines-form-a-tree`).
//!
//! A machine is named by its path of [`Key`]s from the root, the empty path,
//! and holds glam values: a stack, a stash beside it, local state by key,
//! and its children. It starts empty, and an empty machine stores nothing. A
//! turn is not atomic: what it changes before an error stays, and an error
//! or panic that ends it leaves its machine in error, which only a
//! recovering entry enters (`machine-errors-mark-and-recover`). Each action on
//! the shared heap is atomic by itself, and a cut groups actions into one
//! transaction that also bounds a backtracking search
//! (`turn-cuts-bound-transactions-and-search`).
//!
//! Only the thread that owns the runtime takes turns on it. A turn reaches
//! its machine's descendants alone: it takes synchronous turns on them,
//! moves values to and from them, and forks them onto threads that join by
//! the turn's end. So no thread waits for a machine, and waits form no
//! cycle.
//!
//! This first form is a facade over the assembler and its evaluation
//! runtime, which the runtime holds privately; each operation enters the
//! value domain on its own, never across the user's code.

use std::any::Any;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, Scope, ScopedJoinHandle};

use bytes::Bytes;

use super::runtime::{
    DIAGNOSTICS, PortAddress, PortJournal, RuntimeEventJournal, RuntimeSharedResources,
    ShareJournal, Wire, WireEnd,
};
use super::{
    Assembler, Diagnostic, Error, ErrorKind, EvaluationRuntime, ModuleInput, Value as HandleValue,
    ValueKind,
};
use crate::compiler::{import_source_in, source_record_in};
use crate::core::{Builtin, Dict, Key, Value as CoreValue};
use crate::evaluation::{EvaluationPumpOutcome, ExactDemandRoute, WorkDependency};

/// How many reductions a cut whose evaluation waited lets each pump of the
/// work it waits on make.
const AWAIT_PUMP_ALLOWANCE: usize = 4_096;
use crate::core::{ManagedResolverRoot, RuntimeValueAccess, VolumeCellView};
use crate::reflection::{
    ConflictAnalysisStrategy, Promises, Resolution, StoreCommitResult, StoreJournal,
    contents_at_path,
};

/// What opening a runtime takes. It configures the runtime alone:
/// compilation's settings travel with what is compiled.
#[derive(Clone, Default)]
pub struct RuntimeConfig {
    worker_threads: usize,
    conflict_analysis: Option<Arc<dyn ConflictAnalysisStrategy>>,
    diagnostics_wiring: DiagnosticsWiring,
}

/// How a runtime wires machines' `diagnostics` ports by default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticsWiring {
    /// Each child's diagnostics output to its parent's diagnostics input,
    /// and each machine's diagnostics input, the root's aside, through to its
    /// own output, so diagnostics gather at the root's input. The root
    /// passes its `imports` input, where the runtime asks for sources
    /// (`imports-are-source-requests`), through to its output, for the host
    /// to wire to its server. A machine intervenes by unwiring a default
    /// wire it owns.
    #[default]
    ToRoot,
    /// None: the diagnostics and imports ports are wired as any other.
    None,
}

impl RuntimeConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Background worker threads, besides the threads that take turns.
    pub fn worker_threads(mut self, worker_threads: usize) -> Self {
        self.worker_threads = worker_threads;
        self
    }

    /// How transactions summarize what they read.
    pub fn conflict_analysis(mut self, strategy: Arc<dyn ConflictAnalysisStrategy>) -> Self {
        self.conflict_analysis = Some(strategy);
        self
    }

    /// How machines' diagnostics ports are wired by default.
    pub fn diagnostics_wiring(mut self, wiring: DiagnosticsWiring) -> Self {
        self.diagnostics_wiring = wiring;
        self
    }
}

/// A glam runtime. One thread at a time holds it: it may move between
/// threads, but no two share it. The thread that holds it takes turns on
/// it, and dropping it retires everything. Concurrency comes from forks
/// within turns, which may borrow what outlives the turn:
///
/// ```
/// let mut rt = glam::Runtime::open(glam::RuntimeConfig::new())?;
/// let words = ["forks", "may", "borrow"];
/// let total = rt.run("parent", |m| {
///     let left = m.fork("left", |c| {
///         c.int_push(words[..1].len() as i64);
///         Ok(())
///     })?;
///     let right = m.fork("right", |c| {
///         c.int_push(words[1..].len() as i64);
///         Ok(())
///     })?;
///     m.join(left)?;
///     m.join(right)?;
///     // What a fork leaves is on its machine's stack.
///     m.take("left", 1)?;
///     m.take("right", 1)?;
///     Ok(m.int_pop()? + m.int_pop()?)
/// })?;
/// assert_eq!(total, 3);
/// # Ok::<(), glam::Error>(())
/// ```
///
/// A turn takes the runtime mutably, so no turn takes another on it; a turn
/// reaches other machines through its own descendants:
///
/// ```compile_fail
/// let mut rt = glam::Runtime::open(glam::RuntimeConfig::new()).unwrap();
/// let _ = rt.run("a", |_| rt.run("b", |_| Ok(())));
/// ```
///
/// It is `Send`, so a thread may hand it to another:
///
/// ```
/// fn send<T: Send>() {}
/// send::<glam::Runtime>();
/// ```
///
/// It is not `Sync`, so no two threads hold it at once:
///
/// ```compile_fail
/// fn sync<T: Sync>() {}
/// sync::<glam::Runtime>();
/// ```
pub struct Runtime {
    root: MachineState,
    shared: Shared,
    /// `Send` but not `Sync`, as a `Cell` is: one thread at a time.
    _affine: PhantomData<Cell<()>>,
}

/// What a turn's machines share, on every thread of its forks.
struct Shared {
    assembler: Assembler,
}

/// A machine's place in the tree of machines: the keys from the root, which
/// the empty path, `()`, names. A key or text names a child of the root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MachinePath(Vec<Key>);

impl MachinePath {
    /// The root machine's path, which is empty.
    pub fn root() -> Self {
        Self::default()
    }

    pub fn keys(&self) -> &[Key] {
        &self.0
    }
}

impl From<()> for MachinePath {
    fn from((): ()) -> Self {
        Self::root()
    }
}

impl From<Key> for MachinePath {
    fn from(key: Key) -> Self {
        Self(vec![key])
    }
}

impl From<&str> for MachinePath {
    fn from(key: &str) -> Self {
        Self(vec![key.into()])
    }
}

impl From<String> for MachinePath {
    fn from(key: String) -> Self {
        Self(vec![key.into()])
    }
}

impl<K: Into<Key>, const N: usize> From<[K; N]> for MachinePath {
    fn from(keys: [K; N]) -> Self {
        Self(keys.into_iter().map(Into::into).collect())
    }
}

impl<K: Into<Key>> From<Vec<K>> for MachinePath {
    fn from(keys: Vec<K>) -> Self {
        Self(keys.into_iter().map(Into::into).collect())
    }
}

/// One end of a wire, as the machine that owns the wire names it: its own
/// input or output port, or a direct child's port. A child's port is the
/// child's output as a wire's source, and its input as a wire's destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint(WireEnd);

impl Endpoint {
    /// The machine's own input `port`. As a source, what arrives there
    /// passes through; as a destination, the wire delivers there.
    pub fn input(port: impl Into<Key>) -> Self {
        Self(WireEnd::In(port.into()))
    }

    /// The machine's own output `port`. As a source, what reaches it also
    /// flows inward; as a destination, the wire's values leave by it.
    pub fn output(port: impl Into<Key>) -> Self {
        Self(WireEnd::Out(port.into()))
    }

    /// The `port` of the direct child at `child`.
    pub fn child(child: impl Into<Key>, port: impl Into<Key>) -> Self {
        Self(WireEnd::Child(child.into(), port.into()))
    }
}

/// The wiring a machine owns, as [`Machine::wiring`] reports it: its own
/// wires, the default rule that applies to it, and the default wires it
/// removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineWiring {
    wires: Vec<(Endpoint, Endpoint)>,
    default_wiring: DiagnosticsWiring,
    unwired_defaults: Vec<(Endpoint, Endpoint)>,
}

impl MachineWiring {
    /// The machine's own wires, each from its source to its destination, in
    /// a fixed order.
    pub fn wires(&self) -> &[(Endpoint, Endpoint)] {
        &self.wires
    }

    /// The default rule that applies to the wires the machine owns, as
    /// [`DiagnosticsWiring`] states it: the runtime's configured rule, or
    /// [`DiagnosticsWiring::None`] where the machine turned it off.
    pub fn default_wiring(&self) -> DiagnosticsWiring {
        self.default_wiring
    }

    /// The wires of the default rule that the machine removed.
    pub fn unwired_defaults(&self) -> &[(Endpoint, Endpoint)] {
        &self.unwired_defaults
    }
}

#[derive(Clone, Default)]
struct MachineState {
    stack: Vec<HandleValue>,
    stash: Vec<HandleValue>,
    locals: HashMap<Key, HandleValue>,
    children: HashMap<Key, Child>,
    /// Whether the machine is in error: a turn on it ended in an error or a
    /// panic. Until a recovering entry clears it, nothing enters the machine
    /// or its state.
    failed: bool,
}

/// How an operation reaches a descendant. Only the descendant's own error
/// matters: a machine between in error bars nothing below it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Into its state, which must not be in error.
    Enter,
    /// To recover it, in error or not.
    Recover,
    /// The runtime's own: to return a machine to its place, or to run a hook
    /// where a cut registered it, whatever errors came after.
    Internal,
}

#[derive(Clone)]
enum Child {
    Idle(MachineState),
    /// Running on a fork's thread, with its descendants, until its join.
    Forked,
}

impl MachineState {
    fn is_empty(&self) -> bool {
        self.stack.is_empty()
            && self.stash.is_empty()
            && self.locals.is_empty()
            && self.children.is_empty()
            && !self.failed
    }

    /// Appends the machines below this one, at `path`, with their statuses,
    /// depth first, in key order.
    fn walk(&self, path: &mut Vec<Key>, found: &mut Vec<(MachinePath, MachineStatus)>) {
        let mut children: Vec<_> = self.children.iter().collect();
        children.sort_by_key(|&(key, _)| key);
        for (key, child) in children {
            path.push(key.clone());
            match child {
                Child::Idle(state) => {
                    let status = match state.failed {
                        true => MachineStatus::Failed,
                        false => MachineStatus::Idle,
                    };
                    found.push((MachinePath(path.clone()), status));
                    state.walk(path, found);
                }
                Child::Forked => found.push((MachinePath(path.clone()), MachineStatus::Forked)),
            }
            path.pop();
        }
    }

    /// The descendant at `path`, if present, without creating it. It fails
    /// when the descendant, or a machine between, is forked.
    fn find(&self, path: &[Key]) -> Result<Option<&MachineState>, Error> {
        let mut state = self;
        for (depth, key) in path.iter().enumerate() {
            match state.children.get(key) {
                None => return Ok(None),
                Some(Child::Idle(child)) => state = child,
                Some(Child::Forked) => return Err(forked(&path[..=depth])),
            }
        }
        Ok(Some(state))
    }

    /// The descendant at `path`, created empty if absent. It fails when the
    /// descendant, or a machine between, is forked, and when the descendant
    /// is in error where `reach` forbids it.
    fn descendant(&mut self, path: &[Key], reach: Reach) -> Result<&mut MachineState, Error> {
        let mut state = self;
        for (depth, key) in path.iter().enumerate() {
            match state
                .children
                .entry(key.clone())
                .or_insert_with(|| Child::Idle(MachineState::default()))
            {
                Child::Idle(child) => {
                    let target = depth + 1 == path.len();
                    if target && reach == Reach::Enter && child.failed {
                        return Err(failed(path));
                    }
                    state = child;
                }
                Child::Forked => return Err(forked(&path[..=depth])),
            }
        }
        Ok(state)
    }

    /// Removes the empty machines along `path`, deepest first.
    fn prune(&mut self, path: &[Key]) {
        let Some((key, rest)) = path.split_first() else {
            return;
        };
        if let Some(Child::Idle(child)) = self.children.get_mut(key) {
            child.prune(rest);
            if child.is_empty() {
                self.children.remove(key);
            }
        }
    }

    /// Takes the descendant at `path` to a fork, leaving it marked as
    /// forked until its join returns it.
    fn fork_out(&mut self, path: &[Key]) -> Result<MachineState, Error> {
        let (key, parent) = path
            .split_last()
            .ok_or_else(|| Error::new("a machine forks its descendants, not itself"))?;
        // Only the forked machine's own error bars it, checked below.
        let parent = self.descendant(parent, Reach::Internal)?;
        match parent.children.insert(key.clone(), Child::Forked) {
            Some(Child::Idle(state)) if state.failed => {
                parent.children.insert(key.clone(), Child::Idle(state));
                Err(failed(path))
            }
            Some(Child::Idle(state)) if state.has_fork() => {
                parent.children.insert(key.clone(), Child::Idle(state));
                Err(Error::new(format!(
                    "machine {path:?} cannot fork while a machine below it is forked"
                )))
            }
            Some(Child::Idle(state)) => Ok(state),
            None => Ok(MachineState::default()),
            Some(Child::Forked) => Err(forked(path)),
        }
    }

    /// Returns a fork's machine to its place.
    fn join_in(&mut self, path: &[Key], state: MachineState) {
        let (key, parent) = path.split_last().expect("a fork's path is not empty");
        let parent = self
            .descendant(parent, Reach::Internal)
            .expect("the machines above a fork stay until its join");
        parent.children.insert(key.clone(), Child::Idle(state));
        self.prune(path);
    }

    fn has_fork(&self) -> bool {
        self.children.values().any(|child| match child {
            Child::Idle(state) => state.has_fork(),
            Child::Forked => true,
        })
    }

    /// Fails when the descendant at `path`, or a machine between, is
    /// forked, and when the descendant is in error. An absent descendant is
    /// an empty one, and stays absent.
    fn reachable(&self, path: &[Key]) -> Result<(), Error> {
        let mut state = self;
        for (depth, key) in path.iter().enumerate() {
            match state.children.get(key) {
                None => return Ok(()),
                Some(Child::Idle(child)) => state = child,
                Some(Child::Forked) => return Err(forked(&path[..=depth])),
            }
        }
        if state.failed {
            return Err(failed(path));
        }
        Ok(())
    }
}

fn forked(path: &[Key]) -> Error {
    Error::new(format!(
        "machine {path:?} is running on a fork until its join"
    ))
}

fn failed(path: &[Key]) -> Error {
    Error::new(format!(
        "machine {path:?} is in error until a recovering entry enters it"
    ))
}

impl Runtime {
    pub fn open(config: RuntimeConfig) -> Result<Self, Error> {
        let runtime = match config.conflict_analysis {
            Some(strategy) => {
                EvaluationRuntime::with_conflict_analysis(config.worker_threads, strategy)?
            }
            None => EvaluationRuntime::new(config.worker_threads)?,
        };
        // The host serves sources: imports ask for them on the root's
        // `imports` port (`imports-are-source-requests`).
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime)
            .served_sources()
            .build()?;
        Self::around(assembler, config.diagnostics_wiring)
    }

    /// A runtime around an assembler, which `open` builds and the CLI's
    /// transition (`crate::transition`) brings.
    pub(crate) fn around(
        assembler: Assembler,
        diagnostics_wiring: DiagnosticsWiring,
    ) -> Result<Self, Error> {
        let evaluation_runtime = assembler.evaluation_runtime();
        let resources = evaluation_runtime.shared_resources();
        resources.set_diagnostics_wiring(diagnostics_wiring == DiagnosticsWiring::ToRoot);
        resources.enable_machine_environment();
        // Until reflection tasks are machines, the runtime's own diagnostics
        // enter the `refl` machine's diagnostics input, as if they were its
        // children's, and pass through it to the root's by default.
        if diagnostics_wiring == DiagnosticsWiring::ToRoot {
            assembler.diagnostic_bus().route_to_port(
                &evaluation_runtime,
                [environment::REFL.clone()],
                DIAGNOSTICS.clone(),
            )?;
        }
        Ok(Self {
            root: MachineState::default(),
            shared: Shared { assembler },
            _affine: PhantomData,
        })
    }

    /// The assembler the runtime holds, for the CLI's transition
    /// (`crate::transition`).
    pub(crate) fn assembler(&self) -> &Assembler {
        &self.shared.assembler
    }

    /// Starts the background workers, once, in a runtime opened with none:
    /// for a host that learns how many it wants after opening the runtime.
    pub fn activate_workers(&self, worker_threads: usize) -> Result<(), Error> {
        self.shared
            .assembler
            .evaluation_runtime()
            .activate_workers(worker_threads)
    }

    /// Every profiling counter and phase timer of the runtime, in a
    /// `glam-prof` build.
    #[cfg(feature = "glam-prof")]
    pub fn profile(&self) -> crate::profiling::RuntimeProfileSnapshot {
        self.shared.assembler.evaluation_runtime().profile()
    }

    /// Takes a turn on the machine at `path`, which starts empty if it holds
    /// nothing: `()` is the root, and a key names a child of the root.
    ///
    /// The turn is a scope for threads, as [`std::thread::scope`] is: a
    /// fork's closure may borrow what outlives this call, and every fork
    /// joins before it returns. A panic in the turn, or in a fork it left
    /// unjoined, resumes here once every fork has joined; what the turn
    /// changed stays.
    ///
    /// An error or panic that ends the turn exits it early, and leaves the
    /// machine in error. Returning the error is no recovery: until
    /// [`Self::recover`] enters it, this fails on it without entering.
    /// Machines below it stay reachable. The forks the turn left unjoined
    /// join once its body has returned, when there is nothing of it left to
    /// spoil: their errors leave only their own machines in error, and their
    /// panics resume here without one.
    pub fn run<'env, R>(
        &'env mut self,
        path: impl Into<MachinePath>,
        turn: impl for<'scope> FnOnce(&mut Machine<'scope, 'env>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.enter(path.into(), Reach::Enter, turn)
    }

    /// As [`Self::run`], for a machine that may be in error: it clears the
    /// error, and the turn finds the machine as the failed turn left it, to
    /// inspect and repair by its own conventions.
    pub fn recover<'env, R>(
        &'env mut self,
        path: impl Into<MachinePath>,
        turn: impl for<'scope> FnOnce(&mut Machine<'scope, 'env>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.enter(path.into(), Reach::Recover, turn)
    }

    fn enter<'env, R>(
        &'env mut self,
        path: MachinePath,
        reach: Reach,
        turn: impl for<'scope> FnOnce(&mut Machine<'scope, 'env>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let Runtime { root, shared, .. } = self;
        if path.keys().is_empty() && reach == Reach::Enter && root.failed {
            return Err(failed(&[]));
        }
        let mut state = std::mem::take(root.descendant(path.keys(), reach)?);
        state.failed &= reach != Reach::Recover;
        let shared: &'env Shared = shared;
        let (state, outcome) = thread::scope(|scope| {
            let mut machine = Machine::new(shared, scope, path.keys().to_vec(), state);
            let outcome = machine.take_turn(turn);
            (machine.state, outcome)
        });
        *root
            .descendant(path.keys(), Reach::Internal)
            .expect("no machine is forked between turns") = state;
        root.prune(path.keys());
        outcome.unwrap_or_else(|panic| resume_unwind(panic))
    }

    /// The machines below the one at `path`, each with its path from the
    /// root and its status, depth first, in key order. Only machines that
    /// hold something are present: state, or values waiting in an input, or
    /// wiring they own.
    pub fn machines(
        &self,
        path: impl Into<MachinePath>,
    ) -> std::vec::IntoIter<(MachinePath, MachineStatus)> {
        let path = path.into();
        let mut found = Vec::new();
        if let Some(state) = self
            .root
            .find(path.keys())
            .expect("no machine is forked between turns")
        {
            state.walk(&mut path.0.clone(), &mut found);
        }
        let (_, _, events) = self
            .shared
            .assembler
            .evaluation_runtime()
            .shared_resources()
            .transaction_snapshot();
        let present = RuntimeEventJournal::new(events).present_below(path.keys());
        with_port_presence(found, present, path.keys().len(), 0).into_iter()
    }

    /// Whether the machine at `path` is in error, which only
    /// [`Self::recover`] clears. An absent machine is an empty one, in no
    /// error.
    pub fn failed(&self, path: impl Into<MachinePath>) -> bool {
        self.root
            .find(path.into().keys())
            .expect("no machine is forked between turns")
            .is_some_and(|state| state.failed)
    }
}

/// A glam value that a turn popped from a machine's stack: a handle rooted
/// for as long as the turn's scope, `'scope`, which it cannot outlive. A
/// turn may push it again, as may the turns within it.
///
/// It cannot leave its turn:
///
/// ```compile_fail
/// let mut rt = glam::Runtime::open(glam::RuntimeConfig::new()).unwrap();
/// let escaped = rt.run("m", |m| {
///     m.int_push(1);
///     m.value_pop()
/// });
/// ```
#[derive(Clone)]
pub struct Value<'scope> {
    value: HandleValue,
    _scope: PhantomData<&'scope ()>,
}

impl std::fmt::Debug for Value<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Value(..)")
    }
}

/// A machine during a turn: its stack, its stash, its local state and its
/// children. It is also a thread scope, whose lifetime `'scope` brands the
/// forks made in it: each turn, each attempt of a cut and each
/// [`Machine::scope`] opens a fresh one, so a fork joins only on the machine,
/// and in the cut or scope, that made it.
pub struct Machine<'scope, 'env: 'scope> {
    runtime: &'scope Shared,
    scope: &'scope Scope<'scope, 'env>,
    state: MachineState,
    /// The transaction of the outermost cut in progress, which nested cuts
    /// share, and which a synchronous turn on a descendant joins.
    cut: Option<CutTransaction>,
    /// This machine's path from the root, which names its ports.
    path: Vec<Key>,
    /// This machine's path from the machine whose turn opened the cut.
    cut_offset: Vec<Key>,
    /// Forks not yet joined, oldest first.
    forks: Vec<PendingFork<'scope>>,
}

/// A fork's thread, and where its machine returns.
struct PendingFork<'scope> {
    id: u64,
    path: Vec<Key>,
    thread: ScopedJoinHandle<'scope, ForkReturn>,
}

/// A machine's status, as [`Runtime::machines`] and [`Machine::machines`]
/// report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineStatus {
    Idle,
    /// In error, until a recovering entry.
    Failed,
    /// Running on a fork; what lies below it is on the fork's thread.
    Forked,
}

/// What a fork's thread returns at its join: its machine, how its turn
/// ended, and, for a fork within a cut, its branch of the cut's transaction.
struct ForkReturn {
    state: MachineState,
    outcome: Result<(), Error>,
    panic: Option<Box<dyn Any + Send>>,
    branch: Option<CutTransaction>,
}

/// What joining a fork found.
struct Joined {
    outcome: Result<(), Error>,
    panic: Option<Box<dyn Any + Send>>,
    /// Whether the fork's branch merged in a serial order.
    serial: bool,
}

/// How a fork's branch merged into its cut.
enum Merged {
    Serial,
    /// No serial order explains it, so the outermost cut runs again.
    Conflicted,
    /// It resolved a promise that the cut resolved too since the fork: an
    /// error, as resolving it again in sequence is, since running again
    /// would only race again.
    ResolvedTwice,
}

/// A fork of a machine, from [`Machine::fork`], which [`Machine::join`]
/// joins. A fork returns nothing to Rust: what it leaves is on its machine's
/// stack, for its owner to take. Its lifetime is the scope it was made in,
/// so only the machine that made it joins it, and only in the cut or scope
/// that made it:
///
/// ```compile_fail
/// let mut rt = glam::Runtime::open(glam::RuntimeConfig::new()).unwrap();
/// let _ = rt.run("m", |m| {
///     let fork = m.fork("child", |_| Ok(()))?;
///     m.scope(|m| m.join(fork))
/// });
/// ```
///
/// ```compile_fail
/// let mut rt = glam::Runtime::open(glam::RuntimeConfig::new()).unwrap();
/// let _ = rt.run("m", |m| {
///     let fork = m.fork("child", |_| Ok(()))?;
///     m.run("other", |other| other.join(fork))
/// });
/// ```
///
/// Nor does a fork leave the scope that made it:
///
/// ```compile_fail
/// let mut rt = glam::Runtime::open(glam::RuntimeConfig::new()).unwrap();
/// let _ = rt.run("m", |m| {
///     let fork = m.scope(|m| m.fork("child", |_| Ok(())))?;
///     m.join(fork)
/// });
/// ```
#[must_use = "a fork's outcome is observed only by joining it: one left \
              unjoined to its turn's end leaves its error only in its machine"]
pub struct Fork<'scope> {
    id: u64,
    /// Invariant, so that no fork passes for one of a narrower scope.
    _scope: PhantomData<fn(&'scope ()) -> &'scope ()>,
}

static NEXT_FORK: AtomicU64 = AtomicU64::new(0);

impl<'scope, 'env> Machine<'scope, 'env> {
    fn new(
        runtime: &'scope Shared,
        scope: &'scope Scope<'scope, 'env>,
        path: Vec<Key>,
        state: MachineState,
    ) -> Self {
        Self {
            runtime,
            scope,
            state,
            path,
            cut: None,
            cut_offset: Vec::new(),
            forks: Vec::new(),
        }
    }

    /// Runs `turn` on this machine, then joins the forks it left. An error
    /// or panic that ends `turn` exits it early, and leaves the machine in
    /// error. Once `turn` has returned, nothing of it is left to spoil, so
    /// the forks' errors are only theirs: each leaves its own machine in
    /// error, and goes no further. A panic, in the turn or in such a fork,
    /// returns once every fork has joined.
    fn take_turn<R>(
        &mut self,
        turn: impl FnOnce(&mut Self) -> Result<R, Error>,
    ) -> thread::Result<Result<R, Error>> {
        let outcome = catch_unwind(AssertUnwindSafe(|| turn(self)));
        if matches!(outcome, Err(_) | Ok(Err(_))) {
            self.state.failed = true;
        }
        let (fork_panic, _) = self.join_forks();
        match (outcome, fork_panic) {
            (Err(panic), _) | (Ok(_), Some(panic)) => Err(panic),
            (Ok(outcome), None) => Ok(outcome),
        }
    }

    /// Runs `body` on this machine within a fresh thread scope, whose
    /// lifetime brands the forks made in it, then joins those it left, as
    /// a turn does: the outcome is `body`'s error, or else the first of
    /// theirs. A panic, in `body` or in such a fork, resumes once they have
    /// joined.
    fn within_scope<'b, R>(
        &mut self,
        body: impl for<'inner> FnOnce(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        let runtime: &'b Shared = self.runtime;
        thread::scope(|scope| {
            let mut inner = Machine {
                runtime,
                scope,
                state: std::mem::take(&mut self.state),
                path: self.path.clone(),
                cut: self.cut.take(),
                cut_offset: std::mem::take(&mut self.cut_offset),
                forks: Vec::new(),
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| body(&mut inner)));
            let (fork_panic, fork_error) = inner.join_forks();
            self.state = std::mem::take(&mut inner.state);
            self.cut = inner.cut.take();
            self.cut_offset = std::mem::take(&mut inner.cut_offset);
            match (outcome, fork_panic) {
                (Err(panic), _) | (Ok(_), Some(panic)) => resume_unwind(panic),
                (Ok(Ok(result)), None) => fork_error.map_or(Ok(result), Err),
                (Ok(Err(error)), None) => Err(error),
            }
        })
    }

    /// Takes a synchronous turn on the descendant at `path`, relative to
    /// this machine, which starts empty if it holds nothing. Within a cut,
    /// the turn joins it: its actions on shared state join the cut's
    /// transaction, and the descendant's state, part of this machine's,
    /// returns with it when the cut undoes what it did.
    ///
    /// As [`Runtime::run`] does, it fails on a descendant in error without
    /// entering, and an error or panic that ends the turn leaves the
    /// descendant in error.
    pub fn run<'b, R>(
        &mut self,
        path: impl Into<MachinePath>,
        turn: impl for<'inner> FnOnce(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        self.descend(descendant_path(path)?, Reach::Enter, turn)
    }

    /// As [`Self::run`], for a descendant that may be in error, as
    /// [`Runtime::recover`] is: it clears the error, and the turn finds the
    /// descendant as the failed turn left it.
    pub fn recover<'b, R>(
        &mut self,
        path: impl Into<MachinePath>,
        turn: impl for<'inner> FnOnce(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        self.descend(descendant_path(path)?, Reach::Recover, turn)
    }

    fn descend<'b, R>(
        &mut self,
        path: Vec<Key>,
        reach: Reach,
        turn: impl for<'inner> FnOnce(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        let mut state = std::mem::take(self.state.descendant(&path, reach)?);
        state.failed &= reach != Reach::Recover;
        let runtime: &'b Shared = self.runtime;
        let cut = self.cut.take();
        let cut_offset = match cut {
            Some(_) => self.cut_offset.iter().chain(&path).cloned().collect(),
            None => Vec::new(),
        };
        let child_path = self.path.iter().chain(&path).cloned().collect();
        let (state, cut, outcome) = thread::scope(|scope| {
            let mut child = Machine::new(runtime, scope, child_path, state);
            child.cut = cut;
            child.cut_offset = cut_offset;
            let outcome = child.take_turn(turn);
            (child.state, child.cut, outcome)
        });
        self.cut = cut;
        *self
            .state
            .descendant(&path, Reach::Internal)
            .expect("a descendant's place stays during its turn") = state;
        self.state.prune(&path);
        outcome.unwrap_or_else(|panic| resume_unwind(panic))
    }

    /// The machines below the one at `path`, relative to this machine, which
    /// `()` names, each with its path from this machine and its status,
    /// depth first, in key order. Only machines that hold something are
    /// present, and the walk does not descend into a forked one. It fails
    /// when the machine at `path`, or one between, is forked.
    pub fn machines(
        &self,
        path: impl Into<MachinePath>,
    ) -> Result<std::vec::IntoIter<(MachinePath, MachineStatus)>, Error> {
        let path = path.into();
        let mut found = Vec::new();
        if let Some(state) = self.state.find(path.keys())? {
            state.walk(&mut path.0.clone(), &mut found);
        }
        let absolute: Vec<Key> = self.path.iter().chain(path.keys()).cloned().collect();
        let present = match &self.cut {
            Some(cut) => cut.events.present_below(&absolute),
            None => {
                let (_, _, events) = self.resources().transaction_snapshot();
                RuntimeEventJournal::new(events).present_below(&absolute)
            }
        };
        Ok(with_port_presence(found, present, absolute.len(), self.path.len()).into_iter())
    }

    /// Whether the descendant at `path` is in error, which only
    /// [`Self::recover`] clears. An absent descendant is an empty one, in no
    /// error. It fails when the descendant, or a machine between, is forked.
    pub fn failed(&self, path: impl Into<MachinePath>) -> Result<bool, Error> {
        let path = descendant_path(path)?;
        Ok(self.state.find(&path)?.is_some_and(|state| state.failed))
    }

    /// Moves the top `n` values, keeping their order, onto the stack of the
    /// descendant at `path`. Moving none only checks that the descendant is
    /// neither forked nor in error.
    pub fn give(&mut self, path: impl Into<MachinePath>, n: usize) -> Result<(), Error> {
        let path = descendant_path(path)?;
        if n == 0 {
            return self.state.reachable(&path);
        }
        let depth = self.require(n)?;
        self.state.descendant(&path, Reach::Enter)?;
        let items = self.state.stack.split_off(depth - n);
        self.state
            .descendant(&path, Reach::Enter)
            .expect("the descendant was just reached")
            .stack
            .extend(items);
        Ok(())
    }

    /// Moves the top `n` values of the descendant at `path`, keeping their
    /// order, onto this machine's stack. Moving none only checks that the
    /// descendant is neither forked nor in error.
    pub fn take(&mut self, path: impl Into<MachinePath>, n: usize) -> Result<(), Error> {
        let path = descendant_path(path)?;
        if n == 0 {
            return self.state.reachable(&path);
        }
        let descendant = self.state.descendant(&path, Reach::Enter)?;
        let held = descendant.stack.len();
        let items = (held >= n).then(|| descendant.stack.split_off(held - n));
        self.state.prune(&path);
        let items = items.ok_or_else(|| underflow("descendant's stack", n, held))?;
        self.state.stack.extend(items);
        Ok(())
    }

    /// Forks the descendant at `path`: takes it, with its own descendants,
    /// to a new thread that takes a turn on it. It fails on a descendant
    /// already forked, or in error. The fork's closure may borrow what
    /// outlives this scope, and returns nothing: what the fork leaves is on
    /// its machine's stack, for [`Self::take`] after the join. It joins at
    /// [`Self::join`], or at the end of the turn, [`Self::scope`] or cut that
    /// made it. Until then, neither it nor a machine below it is reachable.
    /// A scope's or cut's end observes its outcome, as a join does; a turn's
    /// end, after its body has returned, does not, and its error stays in its
    /// machine (see [`Runtime::run`]).
    ///
    /// Within a cut, the fork joins the cut's transaction: it works on a
    /// branch of it, which sees what the cut wrote before the fork and
    /// merges at the join, in join order. Should the fork have read what the
    /// cut wrote after the fork, no serial order explains it, and the
    /// outermost cut runs again.
    pub fn fork(
        &mut self,
        path: impl Into<MachinePath>,
        turn: impl for<'inner> FnOnce(&mut Machine<'inner, 'scope>) -> Result<(), Error> + Send + 'scope,
    ) -> Result<Fork<'scope>, Error> {
        self.fork_with(thread::Builder::new(), path, turn)
    }

    /// As [`Self::fork`], with the thread configured by `builder`, such as
    /// its name and stack size.
    pub fn fork_with(
        &mut self,
        builder: thread::Builder,
        path: impl Into<MachinePath>,
        turn: impl for<'inner> FnOnce(&mut Machine<'inner, 'scope>) -> Result<(), Error> + Send + 'scope,
    ) -> Result<Fork<'scope>, Error> {
        let path = path.into().0;
        let state = self.state.fork_out(&path)?;
        let branch = self.cut.as_ref().map(|cut| CutTransaction {
            journal: cut.journal.branch(),
            events: {
                let mut events = cut.events.clone();
                events.ports = cut.events.ports.branch();
                events.shares = cut.events.shares.branch();
                events
            },
            observed: false,
            conflicted: false,
            awaits_evaluation: false,
            awaited: Vec::new(),
            commit_hooks: Vec::new(),
            abort_hooks: Vec::new(),
        });
        let cut_offset: Vec<Key> = match branch {
            Some(_) => self.cut_offset.iter().chain(&path).cloned().collect(),
            None => Vec::new(),
        };
        // The thread takes its machine from here, so a thread that does not
        // start leaves it to return.
        let start = Arc::new(Mutex::new(Some((state, branch))));
        let taken = Arc::clone(&start);
        let id = NEXT_FORK.fetch_add(1, Ordering::Relaxed);
        let runtime = self.runtime;
        let fork_path: Vec<Key> = self.path.iter().chain(&path).cloned().collect();
        let thread = builder.spawn_scoped(self.scope, move || {
            let (state, branch) = taken
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .expect("a fork's thread takes its machine once");
            // The fork's turn is a scope of its own, as every turn is.
            let fork_return = thread::scope(|scope| {
                let mut machine = Machine::new(runtime, scope, fork_path, state);
                machine.cut = branch;
                machine.cut_offset = cut_offset;
                let (outcome, panic) = match machine.take_turn(turn) {
                    Ok(outcome) => (outcome, None),
                    Err(panic) => (Ok(()), Some(panic)),
                };
                ForkReturn {
                    state: machine.state,
                    outcome,
                    panic,
                    branch: machine.cut,
                }
            });
            // The thread ends with its turn, so, as a worker does, it gives
            // back the collector caches it held.
            glam_gc::Heap::release_current_thread_caches();
            fork_return
        });
        match thread {
            Ok(thread) => {
                self.forks.push(PendingFork { id, path, thread });
                Ok(Fork {
                    id,
                    _scope: PhantomData,
                })
            }
            Err(error) => {
                let (state, _) = start
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .expect("a thread that did not start took nothing");
                self.state.join_in(&path, state);
                Err(Error::new(format!(
                    "a fork's thread did not start: {error}"
                )))
            }
        }
    }

    /// Waits for `fork`'s turn to end, returns its machine to its place, and
    /// returns how the turn ended: success, a failure ([`ErrorKind::Backtrack`])
    /// or another error. An error leaves the forked machine in error whether
    /// or not it is observed here, and a panic in the fork resumes here.
    /// Within a cut, the fork's branch merges; should the fork have read
    /// what the cut wrote after the fork, this fails, and the outermost cut
    /// runs again whatever its body returns.
    pub fn join(&mut self, fork: Fork<'scope>) -> Result<(), Error> {
        let index = self
            .forks
            .iter()
            .position(|pending| pending.id == fork.id)
            .expect("a fork's scope is its machine's, and it joins once");
        let pending = self.forks.remove(index);
        let joined = self.join_pending(pending);
        if let Some(panic) = joined.panic {
            resume_unwind(panic);
        }
        if !joined.serial {
            return Err(Error::new(
                "the fork read what its cut wrote after the fork, so the cut runs again",
            ));
        }
        joined.outcome
    }

    /// Runs `body` as a scope of this machine: the forks made in it join by
    /// its end, in the order they were made, as a turn's do, and none leaves
    /// it. Its outcome is `body`'s error, or else the first of theirs. A
    /// panic in `body` or in such a fork resumes once they have joined.
    pub fn scope<'b, R>(
        &mut self,
        body: impl for<'inner> FnOnce(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        self.within_scope(body)
    }

    /// Joins the unjoined forks, in the order they were made, returning the
    /// first panic and the first error among their turns.
    fn join_forks(&mut self) -> (Option<Box<dyn Any + Send>>, Option<Error>) {
        let mut first_panic = None;
        let mut first_error = None;
        for pending in std::mem::take(&mut self.forks) {
            let joined = self.join_pending(pending);
            if let Some(panic) = joined.panic {
                first_panic.get_or_insert(panic);
            }
            if let Err(error) = joined.outcome {
                first_error.get_or_insert(error);
            }
        }
        (first_panic, first_error)
    }

    /// Joins a fork's thread and returns its machine to its place, merging
    /// its branch into this machine's cut.
    fn join_pending(&mut self, pending: PendingFork<'scope>) -> Joined {
        let ForkReturn {
            state,
            outcome,
            panic,
            branch,
        } = pending
            .thread
            .join()
            .unwrap_or_else(|panic| resume_unwind(panic));
        self.state.join_in(&pending.path, state);
        // A panic may have abandoned the cut a branch came from.
        let merged = match (branch, &mut self.cut) {
            (Some(branch), Some(cut)) => cut.merge(branch),
            _ => Merged::Serial,
        };
        let outcome = match merged {
            Merged::ResolvedTwice => outcome.and(Err(Error::new(
                "the fork resolved a promise that its cut resolved too",
            ))),
            Merged::Serial | Merged::Conflicted => outcome,
        };
        Joined {
            outcome,
            panic,
            serial: !matches!(merged, Merged::Conflicted),
        }
    }
}

fn descendant_path(path: impl Into<MachinePath>) -> Result<Vec<Key>, Error> {
    let path = path.into().0;
    if path.is_empty() {
        return Err(Error::new("the operation needs a descendant's path"));
    }
    Ok(path)
}

/// A cut's transaction: reads and writes staged against one snapshot of the
/// shared heap, and the hooks its outcome runs.
struct CutTransaction {
    journal: StoreJournal,
    /// Its port actions, which commit with its store journal.
    events: RuntimeEventJournal,
    /// Whether a read observed shared state, so a change could wake the cut
    /// after a failure.
    observed: bool,
    /// Whether a fork's branch read what the cut wrote after the fork, so
    /// the outermost cut must run again.
    conflicted: bool,
    /// Whether an evaluation waited on what is not known yet, such as an
    /// unresolved promise, so any change may wake the cut.
    awaits_evaluation: bool,
    /// What its evaluations waited on, whose progress wakes the cut as any
    /// change does.
    awaited: Vec<WorkDependency>,
    commit_hooks: Vec<Hook>,
    abort_hooks: Vec<Hook>,
}

impl CutTransaction {
    /// Merges a fork's branch of this transaction, returning how it merged.
    /// A branch that did not merge still brings its hooks, so that its abort
    /// hooks run as the attempt aborts.
    fn merge(&mut self, branch: CutTransaction) -> Merged {
        let merged = if self.journal.resolves_as_well(&branch.journal) {
            Merged::ResolvedTwice
        // Both journals merge, or neither: the ports' check changes nothing.
        } else if !self.events.ports.conflicts_with(&branch.events.ports)
            && self.journal.merge_branch(&branch.journal)
        {
            self.events.ports.merge(&branch.events.ports);
            self.events.shares.merge(&branch.events.shares);
            Merged::Serial
        } else {
            Merged::Conflicted
        };
        self.observed |= branch.observed;
        self.conflicted |= branch.conflicted || matches!(merged, Merged::Conflicted);
        self.awaits_evaluation |= branch.awaits_evaluation;
        self.awaited.extend(branch.awaited);
        self.commit_hooks.extend(branch.commit_hooks);
        self.abort_hooks.extend(branch.abort_hooks);
        merged
    }
}

/// A hook, the stack items it captured when registered, and the path of
/// the machine that registered it, from the machine whose turn opened the
/// cut.
struct Hook {
    machine: Vec<Key>,
    items: Vec<HandleValue>,
    run: Box<HookFn>,
}

type HookFn = dyn FnOnce(&mut Machine<'_, '_>) + Send;

/// When an aborted outermost cut runs again.
/// The start of an outermost transaction: the machine's state, and the
/// observation epoch the transaction began at.
struct TransactionStart {
    state: MachineState,
    generation: u64,
}

/// A point within a cut that a machine can return to: its state, and its
/// transaction's journals and hooks as they stood.
#[derive(Clone)]
struct CutCheckpoint {
    state: MachineState,
    journal: StoreJournal,
    events: RuntimeEventJournal,
    commit_hooks: usize,
    abort_hooks: usize,
}

enum CutRetry {
    /// At once: its commit conflicted.
    Now,
    /// Once something its journals observed has changed: it failed, or
    /// met pushback.
    AfterChange(Box<(StoreJournal, RuntimeEventJournal)>),
    /// Once anything has changed since the observation epoch it began at,
    /// or what its evaluations waited on can progress: an evaluation waited
    /// on what was not known yet.
    AfterEvaluation(u64, Vec<WorkDependency>),
    /// Never: an error ended it.
    Never(Error),
}

impl<'scope, 'env> Machine<'scope, 'env> {
    /// How many values the stack holds.
    pub fn depth(&self) -> usize {
        self.state.stack.len()
    }

    /// How many values the stash holds.
    pub fn stash_depth(&self) -> usize {
        self.state.stash.len()
    }

    /// Pushes a value that a turn popped, in this turn or one enclosing it.
    pub fn value_push(&mut self, value: &Value<'scope>) -> Result<(), Error> {
        self.runtime.assembler.values().require(&value.value)?;
        self.push(value.value.clone());
        Ok(())
    }

    /// Pops the top value, without evaluating it, as a handle rooted for
    /// as long as this turn's scope.
    pub fn value_pop(&mut self) -> Result<Value<'scope>, Error> {
        Ok(Value {
            value: self.pop()?,
            _scope: PhantomData,
        })
    }

    pub fn unit_push(&mut self) {
        self.push(self.runtime.assembler.values().unit());
    }

    pub fn int_push(&mut self, value: i64) {
        self.push(self.runtime.assembler.values().integer(value));
    }

    /// Evaluates the top value to an integer and pops it.
    pub fn int_pop(&mut self) -> Result<i64, Error> {
        self.pop_evaluated(Ok, |evaluated| {
            evaluated
                .as_i64()?
                .ok_or_else(|| Error::new("the top value is not an integer within 64 bits"))
        })
    }

    /// Pushes text, which glam holds as UTF-8 bytes.
    pub fn text_push(&mut self, text: impl AsRef<str>) {
        self.push(self.runtime.assembler.values().text(text));
    }

    /// Evaluates the top value to UTF-8 text and pops it.
    pub fn text_pop(&mut self) -> Result<String, Error> {
        let values = self.runtime.assembler.values();
        self.pop_evaluated(
            |value| values.anno_binary(value),
            |evaluated| {
                String::from_utf8(binary(evaluated)?.to_vec())
                    .map_err(|_| Error::new("the top value is not UTF-8 text"))
            },
        )
    }

    pub fn bytes_push(&mut self, bytes: impl Into<Bytes>) {
        self.push(self.runtime.assembler.values().bytes(bytes));
    }

    /// Evaluates the top value to bytes and pops them.
    pub fn bytes_pop(&mut self) -> Result<Bytes, Error> {
        let values = self.runtime.assembler.values();
        self.pop_evaluated(|value| values.anno_binary(value), binary)
    }

    /// Pushes the atom `'name`.
    pub fn atom_push(&mut self, name: impl AsRef<str>) {
        self.push(self.runtime.assembler.values().atom_from_text(name));
    }

    /// Evaluates the top value to an atom named by text, such as `'name`,
    /// and pops its name.
    pub fn atom_pop(&mut self) -> Result<String, Error> {
        let reflection = self.runtime.assembler.reflection();
        self.pop_evaluated(
            |value| reflection.atom_key(&value),
            |name| {
                name.as_bytes()?
                    .and_then(|name| String::from_utf8(name.to_vec()).ok())
                    .ok_or_else(|| Error::new("the atom is not named by text"))
            },
        )
    }

    /// Pushes the abstract global path with these parts, an atom that glam
    /// source names only as `abstract_global_path`.
    pub fn abstract_path_push<I, S>(&mut self, parts: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.push(self.runtime.assembler.values().abstract_global_path(parts));
    }

    /// Evaluates the top value to an abstract global path and pops its
    /// parts.
    pub fn abstract_path_pop(&mut self) -> Result<Vec<String>, Error> {
        let values = self.runtime.assembler.values();
        self.pop_evaluated(Ok, |evaluated| {
            let path = evaluated.into_value();
            values.with_access(|scoped| match scoped.clone_core(&path)? {
                CoreValue::Atom(atom) => match atom.key() {
                    Key::AbstractGlobalPath(parts) => Ok(parts.to_vec()),
                    _ => Err(Error::new("the atom is not an abstract global path")),
                },
                _ => Err(Error::new("the top value is not an abstract global path")),
            })
        })
    }

    /// Replaces the top `n` values with a list of them, the deepest first:
    /// `a1 … an -- [a1 … an]`. Nothing is evaluated.
    pub fn list_pack(&mut self, n: usize) -> Result<(), Error> {
        let values = self.runtime.assembler.values();
        self.replace_top(n, |items| values.list(items))
    }

    /// Evaluates the top value to a list of exactly `n` items and replaces
    /// it with them, the first deepest: `[a1 … an] -- a1 … an`, the twin of
    /// [`Self::list_pack`]. Any other list fails, changing nothing.
    pub fn list_unpack(&mut self, n: usize) -> Result<(), Error> {
        let values = self.runtime.assembler.values();
        let items = self.peek_evaluated(
            |value| values.anno_array(value),
            |evaluated| {
                let items = evaluated
                    .array_items()?
                    .ok_or_else(|| Error::new("the top value is not a list"))?;
                if items.len() != n {
                    return Err(Error::new(format!(
                        "the list holds {} items, not {n}",
                        items.len()
                    )));
                }
                Ok(items)
            },
        )?;
        self.pop()?;
        self.state.stack.extend(items);
        Ok(())
    }

    /// `xs ys -- xs ++ ys`, lazily.
    pub fn list_append(&mut self) -> Result<(), Error> {
        self.rewrite_top(2, |access, items| {
            let [xs, ys] = <[CoreValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            Ok(vec![builtin_call_in(access, Builtin::Append, [xs, ys])])
        })
    }

    /// `xs i -- front back`, lazily: `front` holds the first `i` items, and
    /// `front ++ back` is `xs`.
    pub fn list_split(&mut self) -> Result<(), Error> {
        self.split(Builtin::ListSplit)
    }

    /// As [`Self::list_split`], counting `i` from the end: `back` holds the
    /// last `i` items.
    pub fn list_split_end(&mut self) -> Result<(), Error> {
        self.split(Builtin::ListSplitEnd)
    }

    fn split(&mut self, builtin: Builtin) -> Result<(), Error> {
        self.rewrite_top(2, |access, items| {
            let [xs, index] = <[CoreValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            let split = builtin_call_in(access, builtin, [index, xs]);
            let half = |key: &Key| {
                CoreValue::Lazy(crate::core::LazyValue::from_access_in(
                    access,
                    Arc::from([crate::core_net::CoreDataKey::Key(key.clone())]),
                    Arc::from([access.duplicate_value(&split)]),
                ))
            };
            Ok(vec![
                half(&crate::core::keys::LEFT),
                half(&crate::core::keys::RIGHT),
            ])
        })
    }

    /// `xs -- n`, lazily: how many items `xs` holds.
    pub fn list_len(&mut self) -> Result<(), Error> {
        self.rewrite_top(1, |access, items| {
            let [xs] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            Ok(vec![builtin_call_in(access, Builtin::ListLen, [xs])])
        })
    }

    /// `xs -- ys`, lazily: the items of `xs` in reverse order, or a binary's
    /// bytes reversed. Reversing twice is the identity on lists.
    pub fn list_reverse(&mut self) -> Result<(), Error> {
        self.rewrite_top(1, |access, items| {
            let [xs] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            Ok(vec![builtin_call_in(access, Builtin::ListReverse, [xs])])
        })
    }

    /// Replaces the top values, one for each key, with a dict from the keys
    /// to them: `v1 … vn -- {k1:v1 … kn:vn}`, the deepest value under the
    /// first key. Nothing is evaluated.
    pub fn dict_pack<K: Into<Key>>(
        &mut self,
        keys: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let keys = distinct_keys(keys)?;
        self.rewrite_top(keys.len(), |_, items| {
            Ok(vec![CoreValue::Dict(
                keys.iter()
                    .cloned()
                    .zip(items)
                    .fold(Dict::new_sync(), |dict, (key, item)| dict.insert(key, item)),
            )])
        })
    }

    /// Evaluates the top value to a dict holding no keys but these, and
    /// replaces it with their values, `{}` where absent:
    /// `{k1:v1 … kn:vn} -- v1 … vn`, the twin of [`Self::dict_pack`]. A dict
    /// with any other key fails, changing nothing, as would dropping it.
    pub fn dict_unpack<K: Into<Key>>(
        &mut self,
        keys: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let keys = distinct_keys(keys)?;
        let items = self.peek_core_evaluated(|access, scoped, value| {
            let CoreValue::Dict(dict) = value else {
                return Err(Error::new("the top value is not a dict"));
            };
            if let Some((extra, _)) = dict.iter().find(|(key, _)| !keys.contains(key)) {
                return Err(Error::new(format!(
                    "the dict holds `{extra:?}`, which unpacking would drop"
                )));
            }
            Ok(keys
                .iter()
                .map(|key| {
                    scoped.wrap(
                        dict.get(key)
                            .map(|value| access.duplicate_value(value))
                            .unwrap_or_else(|| undefined(access)),
                    )
                })
                .collect::<Vec<_>>())
        })?;
        self.pop()?;
        self.state.stack.extend(items);
        Ok(())
    }

    /// `i1 … in d -- d'`, lazily: `d` with the items under these keys, the
    /// deepest under the first.
    pub fn dict_insert_keys<K: Into<Key>>(
        &mut self,
        keys: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let keys = distinct_keys(keys)?;
        self.rewrite_top(keys.len() + 1, |access, mut items| {
            let dict = items.pop().expect("the dict is on top");
            Ok(vec![
                keys.iter().zip(items).fold(dict, |dict, (key, item)| {
                    dict_insert_in(access, std::slice::from_ref(key), item, dict)
                }),
            ])
        })
    }

    /// `d -- i1 … in d'`, lazily: the items under these keys, `{}` where
    /// absent, and `d` without them, the twin of [`Self::dict_insert_keys`].
    pub fn dict_extract_keys<K: Into<Key>>(
        &mut self,
        keys: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let keys = distinct_keys(keys)?;
        self.rewrite_top(1, |access, items| {
            let [dict] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            let mut extracted = keys
                .iter()
                .map(|key| path_access_in(access, &dict, std::slice::from_ref(key)))
                .collect::<Vec<_>>();
            extracted.push(keys.iter().fold(dict, |dict, key| {
                dict_remove_in(access, std::slice::from_ref(key), dict)
            }));
            Ok(extracted)
        })
    }

    /// `item dict key -- dict'`, lazily: `dict` with `item` under `key`.
    pub fn dict_insert(&mut self) -> Result<(), Error> {
        self.rewrite_top(3, |access, items| {
            let [item, dict, key] = <[CoreValue; 3]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top three values"));
            let path = CoreValue::List(crate::core::List::from_values(vec![key]));
            Ok(vec![builtin_call_in(
                access,
                Builtin::DictInsert,
                [path, item, dict],
            )])
        })
    }

    /// `dict key -- item dict'`, lazily: the item under `key`, `{}` where
    /// absent, and `dict` without it, the twin of [`Self::dict_insert`].
    pub fn dict_extract(&mut self) -> Result<(), Error> {
        self.rewrite_top(2, |access, items| {
            let [dict, key] = <[CoreValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            let item = CoreValue::Lazy(crate::core::LazyValue::from_access_in(
                access,
                Arc::from([crate::core_net::CoreDataKey::Index]),
                Arc::from([access.duplicate_value(&dict), access.duplicate_value(&key)]),
            ));
            let path = CoreValue::List(crate::core::List::from_values(vec![key]));
            let rest = builtin_call_in(access, Builtin::DictRemove, [path, dict]);
            Ok(vec![item, rest])
        })
    }

    /// `a b -- a ∪ b`, lazily: the shallow union, a dict with both's
    /// entries, as glam's `{a, b}` is. A key both hold is an error when that
    /// entry is forced (`dict-path-fusion`).
    pub fn dict_union(&mut self) -> Result<(), Error> {
        self.rewrite_top(2, |access, items| {
            let [left, right] = <[CoreValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            Ok(vec![builtin_call_in(
                access,
                Builtin::DictUnion,
                [left, right],
            )])
        })
    }

    /// `d -- pairs`, lazily: the members of `d` as `[key, value]` lists, in
    /// key order.
    pub fn dict_pairs(&mut self) -> Result<(), Error> {
        self.rewrite_top(1, |access, items| {
            let [dict] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            Ok(vec![builtin_call_in(access, Builtin::DictPairs, [dict])])
        })
    }

    /// `pairs -- d`, lazily: the dict of a list of `[key, value]` lists, the
    /// twin of [`Self::dict_pairs`]. A pair of another length, or a key
    /// given twice, fails where forced; a member whose value is `{}` is left
    /// out.
    pub fn dict_from_pairs(&mut self) -> Result<(), Error> {
        self.rewrite_top(1, |access, items| {
            let [pairs] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            Ok(vec![builtin_call_in(
                access,
                Builtin::DictFromPairs,
                [pairs],
            )])
        })
    }

    /// `d -- keys values`, lazily: the keys of `d` in order, and its values
    /// in the same order.
    pub fn dict_entries(&mut self) -> Result<(), Error> {
        self.rewrite_top(1, |access, items| {
            let [dict] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            let pairs = builtin_call_in(access, Builtin::DictPairs, [dict]);
            let second = CoreValue::Lazy(crate::core::LazyValue::from_application_in(
                access,
                CoreValue::Builtin(Builtin::ListAt),
                Arc::from([CoreValue::Number(1.into())]),
            ));
            let keys = builtin_call_in(
                access,
                Builtin::Map,
                [
                    CoreValue::Builtin(Builtin::ListHead),
                    access.duplicate_value(&pairs),
                ],
            );
            let values = builtin_call_in(access, Builtin::Map, [second, pairs]);
            Ok(vec![keys, values])
        })
    }

    /// `keys values -- d`, lazily: the dict pairing keys with values by
    /// position, the twin of [`Self::dict_entries`]. Lists of unequal length,
    /// or a key given twice, fail where forced; a member whose value is `{}`
    /// is left out.
    pub fn dict_from_entries(&mut self) -> Result<(), Error> {
        self.rewrite_top(2, |access, items| {
            let [keys, values] = <[CoreValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            Ok(vec![builtin_call_in(
                access,
                Builtin::DictFromEntries,
                [keys, values],
            )])
        })
    }

    /// `d -- v`, lazily: the member at `key`, `{}` where absent.
    pub fn dict_get(&mut self, key: impl Into<Key>) -> Result<(), Error> {
        self.dict_get_path([key])
    }

    /// `d -- v`, lazily: the member at `path`, a path of keys.
    pub fn dict_get_path<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = nonempty_path(path)?;
        self.rewrite_top(1, |access, items| {
            let [dict] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            Ok(vec![path_access_in(access, &dict, &path)])
        })
    }

    /// `item d -- d'`, lazily: `d` with `item` at `path`.
    pub fn dict_insert_path<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = nonempty_path(path)?;
        self.rewrite_top(2, |access, items| {
            let [item, dict] = <[CoreValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            Ok(vec![dict_insert_in(access, &path, item, dict)])
        })
    }

    /// `d -- item d'`, lazily: the item at `path`, `{}` where absent, and `d`
    /// without it, the twin of [`Self::dict_insert_path`].
    pub fn dict_extract_path<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = nonempty_path(path)?;
        self.rewrite_top(1, |access, items| {
            let [dict] =
                <[CoreValue; 1]>::try_from(items).unwrap_or_else(|_| unreachable!("the top value"));
            let item = path_access_in(access, &dict, &path);
            Ok(vec![item, dict_remove_in(access, &path, dict)])
        })
    }

    /// `v -- {key:v}`: a singleton tag.
    pub fn tag_pack(&mut self, key: impl Into<Key>) -> Result<(), Error> {
        self.dict_pack([key])
    }

    /// Evaluates the top value to the tag `{key:v}` and replaces it with
    /// `v`, the twin of [`Self::tag_pack`]. Any other dict fails, changing
    /// nothing.
    pub fn tag_unpack(&mut self, key: impl Into<Key>) -> Result<(), Error> {
        self.dict_unpack([key])
    }

    /// The kind of the top value as it stands, without popping or evaluating
    /// it: a value not yet evaluated is [`ValueKind::Lazy`].
    pub fn kind(&self) -> Result<ValueKind, Error> {
        self.runtime.assembler.reflection().kind(self.peek()?)
    }

    /// Whether the top two values have the same representation, without
    /// popping or evaluating them. This is reflection rather than glam's
    /// equality: values not yet evaluated compare by identity.
    pub fn same_representation(&self) -> Result<bool, Error> {
        let depth = self.require(2)?;
        let stack = &self.state.stack;
        self.runtime
            .assembler
            .reflection()
            .same_representation(&stack[depth - 2], &stack[depth - 1])
    }

    /// Copies the top `n` values: `a1 … an -- a1 … an a1 … an`.
    pub fn dup(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.require(n)?;
        let copies = self.state.stack[depth - n..].to_vec();
        self.state.stack.extend(copies);
        Ok(())
    }

    /// Exchanges the top two values.
    pub fn swap(&mut self) -> Result<(), Error> {
        let depth = self.require(2)?;
        self.state.stack.swap(depth - 1, depth - 2);
        Ok(())
    }

    /// Discards the top `n` values.
    pub fn discard(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.require(n)?;
        self.state.stack.truncate(depth - n);
        Ok(())
    }

    /// Reverses the order of the top `n` values: `flip(2)` is `swap`.
    pub fn flip(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.require(n)?;
        self.state.stack[depth - n..].reverse();
        Ok(())
    }

    /// Rearranges the top values as `pattern` says, a stack effect whose
    /// input and output arities it fixes: `"abc-cabbca"` is `3 -- 6`. The
    /// names left of `-` bind the top values, `c` the top, and the names
    /// right of it, which may repeat a name or leave one out, replace them,
    /// the last on top. Names are single ASCII letters, each bound once.
    pub fn shuffle(&mut self, pattern: &str) -> Result<(), Error> {
        let (inputs, outputs) = shuffle_pattern(pattern)?;
        let depth = self.require(inputs.len())?;
        let bound = self.state.stack.split_off(depth - inputs.len());
        self.state
            .stack
            .extend(outputs.iter().map(|&index| bound[index].clone()));
        Ok(())
    }

    /// Moves the top `n` values to the stash one at a time, as `stash(1)`
    /// does `n` times. The stash is the stack's other side, as in a zipper:
    /// the top value moves first, so it ends deepest, and `stash_pop(n)`
    /// returns the values as they were.
    pub fn stash(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.require(n)?;
        let moved = self.state.stack.split_off(depth - n);
        self.state.stash.extend(moved.into_iter().rev());
        Ok(())
    }

    /// Moves the top `n` values of the stash back to the stack one at a
    /// time, as `stash_pop(1)` does `n` times.
    pub fn stash_pop(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.state.stash.len();
        if depth < n {
            return Err(underflow("stash", n, depth));
        }
        let moved = self.state.stash.split_off(depth - n);
        self.state.stack.extend(moved.into_iter().rev());
        Ok(())
    }

    /// Annotates a value: the annotation is pushed first, then its target,
    /// and the annotated value replaces them, as `anno Annotation Target`
    /// reads. Nothing is evaluated.
    pub fn anno(&mut self) -> Result<(), Error> {
        let values = self.runtime.assembler.values();
        self.replace_top(2, |items| {
            let [annotation, target] = <[HandleValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            values.anno(annotation, target)
        })
    }

    /// Applies a function to `n` arguments: the function is pushed first,
    /// then the arguments in order, and the application replaces them.
    /// Nothing is evaluated.
    pub fn apply(&mut self, n: usize) -> Result<(), Error> {
        let values = self.runtime.assembler.values();
        self.replace_top(n + 1, |items| {
            let (function, arguments) = items
                .split_first()
                .unwrap_or_else(|| unreachable!("a function, then its arguments"));
            values.apply(function, arguments.to_vec())
        })
    }

    /// As [`Self::eval`], spending at most `steps` of this thread's
    /// evaluation, which it reduces by what it spent: waiting on work other
    /// threads run costs nothing. Once the steps are spent it returns
    /// [`ErrorKind::Retry`], leaving the value in place, so a caller that
    /// finds `steps` at zero knows why; what it evaluated stays, so another
    /// call continues. Only reductions cost steps; reaching the work again is
    /// free, but each call spends a step to resume it, so a budget of one
    /// step may make no progress. Within a cut, returning that pushback
    /// waits for a change, as any pushback does.
    pub fn eval_budget(&mut self, steps: &mut usize) -> Result<(), Error> {
        let value = self.peek()?.clone();
        let evaluated = self
            .runtime
            .assembler
            .evaluator()
            .eval_within(&value, steps);
        let evaluated = self.note_evaluation(evaluated)?.into_value();
        self.pop()?;
        self.push(evaluated);
        Ok(())
    }

    /// Evaluates the top value to weak-head normal form, in place.
    pub fn eval(&mut self) -> Result<(), Error> {
        let evaluated = self.pop_evaluated(Ok, |evaluated| Ok(evaluated.into_value()))?;
        self.push(evaluated);
        Ok(())
    }

    /// Replaces the top `n` values with what `build` makes of them, the
    /// deepest first. Where it fails, they stay: a machine's operation that
    /// fails does nothing (reflection aside).
    fn replace_top(
        &mut self,
        n: usize,
        build: impl FnOnce(Vec<HandleValue>) -> Result<HandleValue, Error>,
    ) -> Result<(), Error> {
        let depth = self.require(n)?;
        let built = build(self.state.stack[depth - n..].to_vec())?;
        self.state.stack.truncate(depth - n);
        self.push(built);
        Ok(())
    }

    /// Replaces the top `n` values with what `build` makes of them, as core
    /// values within one access region, the deepest first, and pushes what
    /// it returns in order. Where it fails, they stay. Nothing is evaluated.
    fn rewrite_top(
        &mut self,
        n: usize,
        build: impl FnOnce(&RuntimeValueAccess<'_>, Vec<CoreValue>) -> Result<Vec<CoreValue>, Error>,
    ) -> Result<(), Error> {
        let depth = self.require(n)?;
        let built = self.runtime.assembler.values().with_access(|scoped| {
            let items = self.state.stack[depth - n..]
                .iter()
                .map(|item| scoped.clone_core(item))
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, Error>(
                build(scoped.runtime_access(), items)?
                    .into_iter()
                    .map(|value| scoped.wrap(value))
                    .collect::<Vec<_>>(),
            )
        })?;
        self.state.stack.truncate(depth - n);
        self.state.stack.extend(built);
        Ok(())
    }

    /// Evaluates the top value to weak-head normal form and reads it as a
    /// core value, leaving the stack as it was.
    fn peek_core_evaluated<T>(
        &mut self,
        read: impl FnOnce(
            &RuntimeValueAccess<'_>,
            &super::value::ScopedValues<'_>,
            &CoreValue,
        ) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let value = self.peek()?.clone();
        let evaluated = self.runtime.assembler.evaluator().eval(&value);
        let evaluated = self.note_evaluation(evaluated)?.into_value();
        self.runtime.assembler.values().with_access(|scoped| {
            let value = scoped.clone_core(&evaluated)?;
            read(scoped.runtime_access(), &scoped, &value)
        })
    }

    /// As [`Self::pop_evaluated`], leaving the top value in place.
    fn peek_evaluated<T>(
        &mut self,
        prepare: impl FnOnce(HandleValue) -> Result<HandleValue, Error>,
        convert: impl FnOnce(crate::api::EvaluatedValue) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let value = prepare(self.peek()?.clone())?;
        let evaluated = self.runtime.assembler.evaluator().eval(&value);
        convert(self.note_evaluation(evaluated)?)
    }

    /// Evaluates the top value, as `prepare` presents it, to weak-head
    /// normal form, and converts it, popping it once both have succeeded:
    /// where either fails, the value stays. An evaluation that waits on what
    /// is not known yet, such as an unresolved promise, is pushback,
    /// [`ErrorKind::Retry`], which the outermost cut waits out.
    fn pop_evaluated<T>(
        &mut self,
        prepare: impl FnOnce(HandleValue) -> Result<HandleValue, Error>,
        convert: impl FnOnce(crate::api::EvaluatedValue) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let value = prepare(self.peek()?.clone())?;
        let evaluated = self.runtime.assembler.evaluator().eval(&value);
        let converted = convert(self.note_evaluation(evaluated)?)?;
        self.pop()?;
        Ok(converted)
    }

    /// Notes, within a cut, an evaluation that waited on what is not known
    /// yet, so the outermost cut runs again after any change.
    fn note_evaluation<T>(&mut self, result: Result<T, Error>) -> Result<T, Error> {
        if let (Err(error), Some(cut)) = (&result, &mut self.cut)
            && error.kind() == ErrorKind::Retry
        {
            // Only the cut's commit would settle a promise it resolves, so
            // waiting on its value would wait forever
            // (`promise-resolution-commits-with-its-cut`).
            if cut.journal.resolves_any()
                && error
                    .awaited_promise(self.runtime.assembler.values().core())
                    .is_some_and(|cell| cut.journal.resolves_cell(cell))
            {
                return Err(Error::new(crate::reflection::SELF_AWAITED));
            }
            cut.observed = true;
            cut.awaits_evaluation = true;
            cut.awaited.extend(error.awaited().cloned());
        }
        result
    }

    /// Pops a source's key, `{package, path}`, and pushes the source's
    /// binary, lazily: forcing it asks the host for it on the root's
    /// `imports` port (`imports-are-source-requests`). Nothing is
    /// evaluated, the key included.
    pub fn source_import(&mut self) -> Result<(), Error> {
        let requester = self.runtime.assembler.source_requester();
        let values = self.runtime.assembler.values();
        self.replace_top(1, |items| {
            let [key] = <[HandleValue; 1]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top value"));
            values.with_access(|scoped| {
                let key = scoped.clone_core(&key)?;
                Ok(scoped.wrap(import_source_in(scoped.runtime_access(), requester, key)))
            })
        })
    }

    /// Pops a record, `{scope, lang, source}`, and pushes its open mixin: a
    /// function of a base and a self that compiles the source when applied,
    /// on the thread that forces it (`compile-yields-an-open-mixin`). The
    /// scope is `{location, namespace}`: the source's key, for its relative
    /// imports, absent for a script, and the namespace that seeds its
    /// global paths. `lang` names the front end, and `source` is a binary.
    /// Nothing is evaluated; a compile that fails, fails when forced, and
    /// its diagnostics go where the runtime's own go.
    pub fn compile(&mut self) -> Result<(), Error> {
        let compiler = self.runtime.assembler.source_compiler(None);
        let values = self.runtime.assembler.values();
        self.replace_top(1, |items| {
            let [record] = <[HandleValue; 1]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top value"));
            values.with_access(|scoped| {
                let record = scoped.clone_core(&record)?;
                let compiler = CoreValue::Opaque(crate::core::OpaqueValue::new(
                    scoped.runtime_access().values(),
                    Arc::new(compiler),
                ));
                Ok(
                    scoped.wrap(CoreValue::PartialBuiltin(crate::core::BuiltinCall {
                        builtin: crate::core::Builtin::CompileSource,
                        arguments: Arc::from([
                            compiler,
                            record,
                            CoreValue::Dict(crate::core::Dict::new_sync()),
                        ]),
                    })),
                )
            })
        })
    }

    /// Pops a mixin, then the base below it, and pushes the object the mixin
    /// makes of the base: its fixpoint, whose self is that object. Nothing
    /// is evaluated.
    pub fn instantiate(&mut self) -> Result<(), Error> {
        let values = self.runtime.assembler.values();
        self.replace_top(2, |items| {
            let [base, mixin] = <[HandleValue; 2]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top two values"));
            values.with_access(|scoped| {
                let access = scoped.runtime_access();
                let over_base = CoreValue::Lazy(crate::core::LazyValue::from_application_in(
                    access,
                    scoped.clone_core(&mixin)?,
                    Arc::from([scoped.clone_core(&base)?]),
                ));
                Ok(scoped.wrap(CoreValue::Lazy(
                    crate::core::LazyValue::computed_fixpoint_in(
                        access,
                        crate::core::FixpointComputation::Function(over_base),
                    ),
                )))
            })
        })
    }

    /// Pops `n` mixins and pushes their composition, one mixin: each applies
    /// over those below it, so the top one's definitions override the rest.
    pub fn compose(&mut self, n: usize) -> Result<(), Error> {
        if n == 0 {
            return Err(Error::new("a composition takes at least one mixin"));
        }
        let values = self.runtime.assembler.values();
        self.replace_top(n, |items| {
            values.with_access(|scoped| {
                let mut items = items.iter();
                let first = items.next().expect("at least one mixin");
                items
                    .try_fold(scoped.clone_core(first)?, |inner, outer| {
                        Ok(CoreValue::PartialBuiltin(crate::core::BuiltinCall {
                            builtin: crate::core::Builtin::MixinCompose,
                            arguments: Arc::from([scoped.clone_core(outer)?, inner]),
                        }))
                    })
                    .map(|composed| scoped.wrap(composed))
            })
        })
    }

    /// Pushes the record that compiles `text` as a script in the language of
    /// `extension`, under `namespace`. A script has no location, so it
    /// imports nothing local.
    pub fn script_record<I, S>(&mut self, namespace: I, extension: &str, text: impl Into<String>)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let namespace = namespace.into_iter().map(Into::into).collect::<Vec<_>>();
        let text = text.into();
        let record = self.runtime.assembler.values().with_access(|scoped| {
            scoped.wrap(source_record_in(
                scoped.runtime_access(),
                None,
                &namespace,
                extension,
                CoreValue::binary_from_text(&text),
            ))
        });
        self.push(record);
    }

    /// Pops a base and pushes the object that a script makes of it: the
    /// record of `text` in the language of `extension`, under `namespace`,
    /// compiled and instantiated over the base. Nothing is evaluated.
    pub fn script_object<I, S>(
        &mut self,
        namespace: I,
        extension: &str,
        text: impl Into<String>,
    ) -> Result<(), Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.require(1)?;
        self.script_record(namespace, extension, text);
        self.compile()?;
        self.instantiate()
    }

    /// Pops a package and pushes the record that compiles the source at
    /// `path` in it, under `namespace`: its language is the path's
    /// extension, and its binary is asked for when forced
    /// ([`Self::source_import`]).
    pub fn source_record<I, S, P, T>(&mut self, namespace: I, path: P) -> Result<(), Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        P: IntoIterator<Item = T>,
        T: Into<String>,
    {
        let namespace = namespace.into_iter().map(Into::into).collect::<Vec<_>>();
        let path = path.into_iter().map(Into::into).collect::<Vec<_>>();
        let lang = path
            .last()
            .and_then(|name| crate::compiler::extension(name))
            .ok_or_else(|| {
                Error::new(format!(
                    "source `{}` has no extension to select a front-end compiler",
                    path.join("/")
                ))
            })?
            .to_owned();
        let requester = self.runtime.assembler.source_requester();
        let values = self.runtime.assembler.values();
        self.replace_top(1, |items| {
            let [package] = <[HandleValue; 1]>::try_from(items)
                .unwrap_or_else(|_| unreachable!("the top value"));
            values.with_access(|scoped| {
                let access = scoped.runtime_access();
                let key = || -> Result<CoreValue, Error> {
                    Ok(CoreValue::Dict(
                        crate::core::Dict::new_sync()
                            .insert(
                                crate::core::keys::PACKAGE.clone(),
                                scoped.clone_core(&package)?,
                            )
                            .insert(
                                crate::core::keys::PATH.clone(),
                                CoreValue::List(crate::core::List::from_values(
                                    path.iter()
                                        .map(|name| CoreValue::binary_from_text(name))
                                        .collect(),
                                )),
                            ),
                    ))
                };
                let source = import_source_in(access, requester, key()?);
                Ok(scoped.wrap(source_record_in(
                    access,
                    Some(key()?),
                    &namespace,
                    &lang,
                    source,
                )))
            })
        })
    }

    /// Eagerly compiles `inputs` through the handle API's assembler, as the
    /// CLI's transition does (`crate::transition`), over initial definitions
    /// where given, returning the module and its warnings.
    pub(crate) fn compile_inputs<I, S>(
        &mut self,
        module_path: I,
        inputs: impl IntoIterator<Item = ModuleInput>,
        over: bool,
    ) -> Result<Vec<Diagnostic>, Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let definitions = if over {
            Some(self.peek()?.clone())
        } else {
            None
        };
        let (module, diagnostics) = self.compile_module(module_path, inputs, definitions)?;
        if over {
            self.pop()?;
        }
        self.push(module);
        Ok(diagnostics)
    }

    fn compile_module<I, S>(
        &mut self,
        module_path: I,
        inputs: impl IntoIterator<Item = ModuleInput>,
        definitions: Option<HandleValue>,
    ) -> Result<(HandleValue, Vec<Diagnostic>), Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut builder = self.runtime.assembler.module(module_path).inputs(inputs);
        if let Some(definitions) = definitions {
            builder = builder.initial_definitions(definitions);
        }
        let module = builder.build()?;
        let diagnostics = module.diagnostics().to_vec();
        Ok((module.into_value(), diagnostics))
    }

    /// Pushes this machine's environment: the mixins that it and its
    /// ancestors set, composed from the root down as one object, built
    /// lazily, so nothing is evaluated (`machines-inherit-an-environment`):
    /// as `object _ extends` the mixins, its own first, so a machine's mixin
    /// overrides what its ancestors' set. Within a cut, the read joins the
    /// cut's transaction and sees its changes, and the cut runs again when a
    /// mixin above changes; outside one, it reads the mixins as they stand.
    pub fn env_get(&mut self) {
        let machines = environment::environment_chain(&self.path);
        let mixins = match &mut self.cut {
            Some(cut) => machines
                .iter()
                .filter_map(|machine| environment::observed_mixin(cut, machine))
                .collect::<Vec<_>>(),
            None => {
                let (_, store, events) = self.resources().transaction_snapshot();
                let shares = ShareJournal::new(&events.shares);
                machines
                    .iter()
                    .filter(|machine| shares.is_set(machine))
                    .map(|machine| {
                        store.environment_value(&environment::environment_share(machine))
                    })
                    .collect()
            }
        };
        let values = self.runtime.assembler.values();
        let composed = crate::g_syntax::composed_environment_root(
            values.core(),
            mixins
                .into_iter()
                .map(HandleValue::into_runtime_root)
                .collect(),
            None,
        );
        self.push(HandleValue::from_runtime_root(composed));
    }

    /// Sets the top value, an object, as this machine's mixin of the
    /// environment, popping it: the mixin that the environments of the
    /// machine and its descendants extend, in place of one set before. Only
    /// the machine sets its mixin. Outside a cut, it commits at once;
    /// within one, with the outermost cut.
    pub fn env_mixin_set(&mut self) -> Result<(), Error> {
        let mixin = self.peek()?.clone();
        self.runtime.assembler.values().require(&mixin)?;
        let machine = self.path.clone();
        let path = environment::environment_share(&machine);
        self.transact(|store, events| {
            store.write_environment(path.clone(), mixin.clone());
            events.shares.set(machine.clone());
            Ok(())
        })?;
        self.pop()?;
        Ok(())
    }

    /// Pushes this machine's own mixin of the environment, to extend and
    /// set again, or an empty object where it has none. Within a cut, the
    /// read joins the cut's transaction, as [`Self::env_get`]'s does.
    pub fn env_mixin_get(&mut self) {
        let machine = self.path.clone();
        let mixin = match &mut self.cut {
            Some(cut) => environment::observed_mixin(cut, &machine),
            None => {
                let (_, store, events) = self.resources().transaction_snapshot();
                ShareJournal::new(&events.shares)
                    .is_set(&machine)
                    .then(|| store.environment_value(&environment::environment_share(&machine)))
            }
        };
        let mixin = mixin.unwrap_or_else(|| {
            let values = self.runtime.assembler.values();
            HandleValue::from_runtime_root(crate::g_syntax::composed_environment_root(
                values.core(),
                Vec::new(),
                None,
            ))
        });
        self.push(mixin);
    }

    /// Clears this machine's mixin of the environment, so the environments
    /// of the machine and its descendants extend its ancestors' alone.
    /// Outside a cut, it commits at once; within one, with the outermost
    /// cut.
    pub fn env_mixin_clear(&mut self) -> Result<(), Error> {
        let machine = self.path.clone();
        let path = environment::environment_share(&machine);
        self.transact(|store, events| {
            store.remove_environment(path.clone());
            events.shares.clear(machine.clone());
            Ok(())
        })
    }

    /// The reflection environment of `role` as this machine sees it, its
    /// shares as they stand over the role's sealed environment, for the
    /// CLI's transition (`crate::transition`).
    pub(crate) fn reflection_environment(&self, role: &str) -> HandleValue {
        let base = self.runtime.assembler.reflection_environment_for_role(role);
        HandleValue::from_runtime_root(environment::committed_environment(
            &self.resources(),
            &self.path,
            &base.into_runtime_root(),
        ))
    }

    /// Pushes a promised value, which waits until it is resolved, then its
    /// resolver: an opaque glam value that [`Self::promise_fulfill`] or
    /// [`Self::promise_break`] resolves, once.
    pub fn promise_new(&mut self) {
        let promises = self.promises();
        let (promised, resolver) = self.runtime.assembler.values().with_access(|scoped| {
            let (_, promised, resolver) = promises.allocate(scoped.runtime_access());
            (scoped.wrap(promised), scoped.wrap(resolver))
        });
        self.push(promised);
        self.push(resolver);
    }

    /// The status of the promise that the resolver on top, which stays and
    /// is evaluated, resolves. A promised value has no status of its own: it
    /// stands for whatever it evaluates to. Within a cut, the read joins the
    /// cut's transaction, so the promise's resolution, committed first,
    /// conflicts with the cut, and wakes it once it has failed.
    pub fn promise_status(&mut self) -> Result<PromiseStatus, Error> {
        let top = self.peek()?.clone();
        let evaluated = self.runtime.assembler.evaluator().eval(&top);
        let resolver = self.note_evaluation(evaluated)?.into_value();
        let resolver = self.resolver_root(&resolver)?;
        let promise = resolver.id();
        if let Some(cut) = &mut self.cut {
            // The cut sees its own resolution, as its own writes.
            if let Some(staged) = cut.journal.staged_status(promise) {
                return Ok(staged);
            }
            cut.observed |= cut.journal.observe_promise_read(promise);
        }
        Ok(resolver.status())
    }

    /// A hold on the breaker of `resolver`, evaluated.
    fn resolver_root(&self, resolver: &HandleValue) -> Result<ManagedResolverRoot, Error> {
        self.runtime
            .assembler
            .values()
            .with_access(|scoped| {
                Ok::<_, Error>(
                    scoped
                        .runtime_access()
                        .resolver_root(&scoped.clone_core(resolver)?),
                )
            })?
            .ok_or_else(|| Error::new("the value is not a resolver of this runtime's promise"))
    }

    /// The runtime's promises, which its store holds.
    fn promises(&self) -> Arc<Promises> {
        self.resources().reflection_snapshot().1.promises().clone()
    }

    /// Pops a value, then a resolver, and fulfills the resolver's promise
    /// with the value, waking what waits on it. It evaluates the resolver,
    /// never the value. A promise resolves once, so resolving it again is an
    /// error, which pops nothing.
    ///
    /// Within a cut, the resolution is part of the cut's transaction
    /// (`promise-resolution-commits-with-its-cut`): the promise resolves
    /// when the outermost cut commits, and a failed cut resolves nothing.
    /// The cut sees its own resolution in [`Self::promise_status`], but not
    /// the promised value, which waits for the commit, so a cut that reads
    /// the value of a promise it resolves fails, as soon as its evaluation
    /// would wait on it, directly or through lazy values. Where another
    /// resolution commits first, the cut's commit conflicts, and running
    /// again it finds the promise resolved.
    pub fn promise_fulfill(&mut self) -> Result<(), Error> {
        self.resolve_promise(Resolution::Fulfill)
    }

    /// Pops a value, then a resolver, and breaks the resolver's promise with
    /// the value, a message or a diagnostic, so what reads the promised
    /// value fails with it. As [`Self::promise_fulfill`], it evaluates the resolver,
    /// resolves once, and within a cut resolves when the cut commits.
    pub fn promise_break(&mut self) -> Result<(), Error> {
        self.resolve_promise(Resolution::Break)
    }

    /// Resolves the promise of the resolver below the top value, with that
    /// value, popping both once it has.
    fn resolve_promise(
        &mut self,
        resolution: impl FnOnce(crate::runtime::RuntimeValueRoot) -> Resolution,
    ) -> Result<(), Error> {
        let depth = self.require(2)?;
        let resolver = self.state.stack[depth - 2].clone();
        let value = self.state.stack[depth - 1].clone();
        self.runtime.assembler.values().require(&value)?;
        let evaluated = self.runtime.assembler.evaluator().eval(&resolver);
        let resolver = self.note_evaluation(evaluated)?.into_value();
        let resolver = self.resolver_root(&resolver)?;
        let resolution = resolution(value.into_runtime_root());
        self.transact(|store, _| {
            store
                .resolve(&resolver, resolution.clone())
                .map_err(|error| Error::new(error.as_ref()))
        })?;
        self.state.stack.truncate(depth - 2);
        Ok(())
    }

    /// Pushes the value at `path` in the shared heap, without evaluating
    /// it: a missing key reads `{}`. Within a cut, the read joins the cut's
    /// transaction and sees its writes; outside one, it reads the heap as it
    /// stands.
    pub fn heap_get<K: Into<Key>>(&mut self, path: impl IntoIterator<Item = K>) {
        let path = key_path(path);
        let value = match &mut self.cut {
            Some(cut) => {
                cut.observed |= cut.journal.observe_read(&path);
                cut.journal.heap_value(&path)
            }
            None => self.resources().reflection_snapshot().1.heap_value(&path),
        };
        self.push(value);
    }

    /// Pops a value and writes it at `path` in the shared heap. Outside a
    /// cut, the write commits at once; within one, with the outermost cut.
    pub fn heap_set<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        let value = self.peek()?.clone();
        self.runtime.assembler.values().require(&value)?;
        self.store_edit(|journal| journal.write(path.clone(), value.clone()))?;
        self.pop()?;
        Ok(())
    }

    /// Pushes the value at `path` in the shared heap, without evaluating
    /// it, where `path` is defined, and fails where it is not, a
    /// [`Self::fail`] error (`dict-presence-ops`). It forces the dicts on
    /// the way, never the value. Within a cut, the read joins the cut's
    /// transaction, as [`Self::heap_get`]'s does, so a cut that fails on it
    /// waits for the path's definition.
    pub fn heap_try_get<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        let (root, value) = match &mut self.cut {
            Some(cut) => {
                cut.observed |= cut.journal.observe_read(&path);
                (cut.journal.heap_value(&[]), cut.journal.heap_value(&path))
            }
            None => {
                let snapshot = self.resources().reflection_snapshot().1;
                (snapshot.heap_value(&[]), snapshot.heap_value(&path))
            }
        };
        if !self.defines(&root, &path)? {
            return Err(self.fail());
        }
        self.push(value);
        Ok(())
    }

    /// Whether `root` defines `path`, forcing the dicts on the way and
    /// nothing else (`dict-presence-ops`).
    fn defines(&mut self, root: &HandleValue, path: &[Key]) -> Result<bool, Error> {
        let values = self.runtime.assembler.values().clone();
        let defines = values.with_access(|scoped| {
            let access = scoped.runtime_access();
            let root = scoped.clone_core(root)?;
            let path = CoreValue::List(crate::core::List::from_values(
                path.iter().map(|key| access.value_from_key(key)).collect(),
            ));
            Ok::<_, Error>(scoped.wrap(CoreValue::builtin_call_in(
                access,
                crate::core::Builtin::DictDefines,
                vec![path, root],
            )))
        })?;
        let evaluated = self.runtime.assembler.evaluator().eval(&defines);
        let answer = self.note_evaluation(evaluated)?.into_value();
        values.with_access(|scoped| {
            let answer = scoped.clone_core(&answer)?;
            crate::eval::predicate_effect_holds(scoped.runtime_access(), &answer)
                .ok_or_else(|| Error::new("a definedness test answers `.r ()` or `.fail`"))
        })
    }

    /// Removes what is at `path` in the shared heap, pruning the dicts the
    /// removal empties (`dict-presence-ops`). Outside a cut, the removal
    /// commits at once; within one, with the outermost cut. The path is not
    /// empty.
    pub fn heap_del<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        if path.is_empty() {
            return Err(Error::new("a heap removal needs a non-empty path"));
        }
        self.store_edit(|journal| journal.remove(path.clone()))
    }

    /// Pops a function and rewrites the value at `path` in the shared heap
    /// with it. The function applies lazily to the value the write finds
    /// when it commits, so rewrites compose rather than conflict.
    pub fn heap_rewrite<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        let updater = self.peek()?.clone();
        self.runtime.assembler.values().require(&updater)?;
        self.store_edit(|journal| journal.rewrite(path.clone(), updater.clone()))?;
        self.pop()?;
        Ok(())
    }

    /// Pushes the handle of the shared heap's volume, as `.heap.vol` gives
    /// it, so the volume operations reach the heap too
    /// (`requests-travel-on-ports`).
    pub fn heap_volume(&mut self) {
        let store = self.resources().reflection_snapshot().1;
        let handle = self
            .runtime
            .assembler
            .values()
            .with_access(|scoped| scoped.wrap(store.heap_handle(scoped.runtime_access())));
        self.push(handle);
    }

    /// Allocates a volume whose root is the top value, replacing it with the
    /// volume's handle: an opaque glam value whose holder reads and writes
    /// the volume. Allocation is transactional: outside a cut, the volume
    /// exists at once; within one, from the outermost cut's commit, and a
    /// failed cut allocates nothing. A volume lives while its handle does:
    /// once no copy is reachable, a collection collects it with its
    /// contents, even where they reach its handle (`api-volume-cells`).
    pub fn volume_new(&mut self) -> Result<(), Error> {
        let initial = self.peek()?.clone();
        let values = self.runtime.assembler.values();
        values.require(&initial)?;
        let handle = self.transact(|store, _| {
            store
                .create_volume(initial.clone())
                .map_err(|error| Error::new(error.as_ref()))
        })?;
        self.pop()?;
        self.push(handle);
        Ok(())
    }

    /// Replaces the top value, a volume's handle, with the value at `path`
    /// in the volume, without evaluating it: a missing key reads `{}`. It
    /// evaluates the handle, never the value. Within a cut, the read joins
    /// the cut's transaction, as [`Self::heap_get`]'s does.
    pub fn volume_get<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        let handle = self.volume_handle_at(1)?;
        let snapshot = self
            .cut
            .is_none()
            .then(|| self.resources().reflection_snapshot().1);
        let runtime = self.runtime;
        let cut = &mut self.cut;
        let value = runtime.assembler.values().with_access(|scoped| {
            let access = scoped.runtime_access();
            let handle = scoped.clone_core(&handle)?;
            let volume = access
                .volume_cell(&handle)
                .expect("the handle names a volume");
            let contents = match (cut.as_mut(), &snapshot) {
                (Some(cut), _) => {
                    cut.observed |= cut.journal.observe_volume_read(volume, &path);
                    // The cut's journal keeps the volume it read until the
                    // cut ends (`api-volume-cells`).
                    cut.journal.read_volume_in(access, volume)
                }
                (None, Some(snapshot)) => snapshot.read_volume_in(access, volume),
                (None, None) => unreachable!("outside a cut, a turn reads a snapshot"),
            };
            contents
                .map(|contents| scoped.wrap(contents_at_path(access, contents, &path)))
                .ok_or_else(|| revoked(volume.id()))
        })?;
        self.pop()?;
        self.push(value);
        Ok(())
    }

    /// Pops a value, then a volume's handle, and writes the value at `path`
    /// in the volume, as [`Self::heap_set`] writes the heap's.
    pub fn volume_set<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        self.volume_edit(|journal, access, volume, value| {
            journal.write_volume_in(access, volume, path.clone(), value)
        })
    }

    /// Replaces the top value, a volume's handle, with the value at `path`
    /// in the volume where `path` is defined, and fails where it is not, as
    /// [`Self::heap_try_get`] reads the heap.
    pub fn volume_try_get<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        let handle = self.volume_handle_at(1)?;
        let snapshot = self
            .cut
            .is_none()
            .then(|| self.resources().reflection_snapshot().1);
        let runtime = self.runtime;
        let cut = &mut self.cut;
        let (root, value) = runtime.assembler.values().with_access(|scoped| {
            let access = scoped.runtime_access();
            let handle = scoped.clone_core(&handle)?;
            let volume = access
                .volume_cell(&handle)
                .expect("the handle names a volume");
            let contents = match (cut.as_mut(), &snapshot) {
                (Some(cut), _) => {
                    cut.observed |= cut.journal.observe_volume_read(volume, &path);
                    cut.journal.read_volume_in(access, volume)
                }
                (None, Some(snapshot)) => snapshot.read_volume_in(access, volume),
                (None, None) => unreachable!("outside a cut, a turn reads a snapshot"),
            };
            contents
                .map(|contents| {
                    (
                        scoped.wrap(contents_at_path(access, contents, &[])),
                        scoped.wrap(contents_at_path(access, contents, &path)),
                    )
                })
                .ok_or_else(|| revoked(volume.id()))
        })?;
        if !self.defines(&root, &path)? {
            return Err(self.fail());
        }
        self.pop()?;
        self.push(value);
        Ok(())
    }

    /// Pops a volume's handle and removes what is at `path` in the volume,
    /// as [`Self::heap_del`] removes from the heap.
    pub fn volume_del<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        if path.is_empty() {
            return Err(Error::new("a volume removal needs a non-empty path"));
        }
        let depth = self.require(1)?;
        let handle = self.volume_handle_at(1)?;
        let values = self.runtime.assembler.values().clone();
        self.store_edit(|journal| {
            values.with_access(|scoped| {
                let handle = scoped
                    .clone_core(&handle)
                    .expect("the handle belongs to this runtime");
                let access = scoped.runtime_access();
                let volume = access
                    .volume_cell(&handle)
                    .expect("the handle names a volume");
                journal.remove_volume_in(access, volume, path.clone());
            })
        })?;
        self.state.stack.truncate(depth - 1);
        Ok(())
    }

    /// Pops a function, then a volume's handle, and rewrites the value at
    /// `path` in the volume with it, as [`Self::heap_rewrite`] rewrites the
    /// heap's.
    pub fn volume_rewrite<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        self.volume_edit(|journal, access, volume, updater| {
            journal.rewrite_volume_in(access, volume, path.clone(), updater)
        })
    }

    /// Writes with `edit` the top value to the volume whose handle lies
    /// below it, popping both once it has.
    fn volume_edit(
        &mut self,
        edit: impl Fn(&mut StoreJournal, &RuntimeValueAccess<'_>, VolumeCellView<'_>, HandleValue),
    ) -> Result<(), Error> {
        let depth = self.require(2)?;
        let value = self.state.stack[depth - 1].clone();
        self.runtime.assembler.values().require(&value)?;
        let handle = self.volume_handle_at(2)?;
        let values = self.runtime.assembler.values().clone();
        self.store_edit(|journal| {
            values.with_access(|scoped| {
                let handle = scoped
                    .clone_core(&handle)
                    .expect("the handle belongs to this runtime");
                let access = scoped.runtime_access();
                let volume = access
                    .volume_cell(&handle)
                    .expect("the handle names a volume");
                edit(journal, access, volume, value.clone());
            })
        })?;
        self.state.stack.truncate(depth - 2);
        Ok(())
    }

    /// The handle `n` values down, evaluated, where it names a volume; the
    /// handle stays. Volume operations reach the volume through a view of
    /// its cell, which the handle gives (`api-volume-cells`).
    fn volume_handle_at(&mut self, n: usize) -> Result<HandleValue, Error> {
        let depth = self.require(n)?;
        let handle = self.state.stack[depth - n].clone();
        let evaluated = self.runtime.assembler.evaluator().eval(&handle);
        let handle = self.note_evaluation(evaluated)?.into_value();
        let names_volume = self.runtime.assembler.values().with_access(|scoped| {
            Ok::<_, Error>(
                scoped
                    .runtime_access()
                    .volume_cell(&scoped.clone_core(&handle)?)
                    .is_some(),
            )
        })?;
        if !names_volume {
            return Err(Error::new(
                "the value is not a handle of this runtime's volume",
            ));
        }
        Ok(handle)
    }

    /// The volume that the handle `n` values down names.
    #[cfg(test)]
    fn volume_at(&mut self, n: usize) -> Result<crate::reflection::VolumeId, Error> {
        let handle = self.volume_handle_at(n)?;
        self.runtime.assembler.values().with_access(|scoped| {
            Ok(crate::reflection::handle_volume(
                scoped.runtime_access(),
                &scoped.clone_core(&handle)?,
            )
            .expect("the handle names a volume"))
        })
    }

    /// Pops `n` values and writes them, in order, to this machine's output
    /// `port`. Each value follows every wire from it, outward by its
    /// parent's wiring and inward by its own, through pass-throughs, to every
    /// buffer it reaches, and arrives when the write commits: at once
    /// outside a cut, with the outermost cut within one. An output with no wire is an
    /// error, which pops nothing. Buffers have no bound yet, so a write
    /// never waits.
    pub fn port_write(&mut self, port: impl Into<Key>, n: usize) -> Result<(), Error> {
        let depth = self.require(n)?;
        let values = self.state.stack[depth - n..].to_vec();
        self.send(port.into(), &values)?;
        self.state.stack.truncate(depth - n);
        Ok(())
    }

    /// Posts a request on the output `port` (`requests-travel-on-ports`):
    /// replaces the top value with the promise of its reply, and writes
    /// `{request:value, reply:resolver}`, as [`Self::port_write`] writes, for a
    /// server to read and resolve with [`Self::promise_fulfill`] or
    /// [`Self::promise_break`]. Where several servers receive it, the first
    /// resolution wins. Where the write fails, it does nothing.
    pub fn port_ask(&mut self, port: impl Into<Key>) -> Result<(), Error> {
        let value = self.peek()?.clone();
        let values = self.runtime.assembler.values();
        values.require(&value)?;
        let promises = self.promises();
        let (promise, promised, resolver) = values.with_access(|scoped| {
            let (promise, promised, resolver) = promises.allocate(scoped.runtime_access());
            (promise, scoped.wrap(promised), scoped.wrap(resolver))
        });
        let sent = values
            .record([("request", value), ("reply", resolver)])
            .and_then(|request| self.send(port.into(), &[request]));
        if let Err(error) = sent {
            promises.forget(promise);
            return Err(error);
        }
        self.pop()?;
        self.push(promised);
        Ok(())
    }

    /// Sends `values`, in order, from this machine's output `port` to every
    /// buffer its wiring reaches.
    fn send(&mut self, port: Key, values: &[HandleValue]) -> Result<(), Error> {
        for value in values {
            self.runtime.assembler.values().require(value)?;
        }
        let machine = self.path.clone();
        self.port_action(|ports| {
            let buffers = ports.route(&machine, &port).map_err(Error::new)?;
            for value in values {
                for buffer in &buffers {
                    ports.append(buffer.clone(), value.clone());
                }
            }
            Ok(())
        })
    }

    /// Pushes the next `n` values of this machine's input `port`, in the
    /// order they arrived, taking them. It reads what is committed, never
    /// this transaction's own writes. When fewer than `n` are there, it is
    /// pushback: within a cut it returns [`ErrorKind::Retry`], which the
    /// outermost cut waits out, and outside one it waits until they are.
    pub fn port_read(&mut self, port: impl Into<Key>, n: usize) -> Result<(), Error> {
        self.read_port(port.into(), n, true)
    }

    /// As [`Self::port_read`], but returns [`ErrorKind::Retry`] outside a cut too,
    /// rather than waiting.
    pub fn port_try_read(&mut self, port: impl Into<Key>, n: usize) -> Result<(), Error> {
        self.read_port(port.into(), n, false)
    }

    fn read_port(&mut self, port: Key, n: usize, wait: bool) -> Result<(), Error> {
        let address = PortAddress {
            machine: self.path.clone(),
            port,
        };
        if let Some(cut) = &mut self.cut {
            cut.observed = true;
            let values = cut
                .events
                .ports
                .claim(&address, n)
                .ok_or_else(|| short(&address, n))?;
            self.state.stack.extend(values);
            return Ok(());
        }
        let resources = self.resources();
        loop {
            let (_, store, events) = resources.transaction_snapshot();
            let store = StoreJournal::new(store);
            let mut events = RuntimeEventJournal::new(events);
            match events.ports.claim(&address, n) {
                Some(values) => match resources.try_commit_transaction(&store, &events) {
                    StoreCommitResult::Committed => {
                        self.state.stack.extend(values);
                        return Ok(());
                    }
                    _ => continue,
                },
                None if wait => self.await_change(&resources, &store, &events),
                None => return Err(short(&address, n)),
            }
        }
    }

    /// How many values this machine's input `port` holds beyond what this
    /// transaction took. Within a cut the count is a read of the port, so a
    /// cut that fails after counting waits for it to change.
    pub fn port_available(&mut self, port: impl Into<Key>) -> usize {
        let address = PortAddress {
            machine: self.path.clone(),
            port: port.into(),
        };
        match &mut self.cut {
            Some(cut) => {
                cut.observed = true;
                cut.events.ports.available(&address)
            }
            None => {
                let (_, _, events) = self.resources().transaction_snapshot();
                RuntimeEventJournal::new(events).ports.available(&address)
            }
        }
    }

    /// Wires `source` to `destination`, both named from this machine: its
    /// own ports and its direct children's, never deeper, so each machine
    /// states what it exposes. A wire from its own input to its own output
    /// passes the input through; from its own output to a child's input,
    /// feeds the child; from a child's output to its own input, collects
    /// from the child. The wire is this machine's, and takes effect as a
    /// write does: at once outside a cut, with the outermost cut within one.
    pub fn wire_add(&mut self, source: Endpoint, destination: Endpoint) -> Result<(), Error> {
        let wire = Wire {
            source: source.0,
            destination: destination.0,
        };
        let owner = self.path.clone();
        self.port_action(|ports| {
            ports.wire(owner.clone(), wire.clone());
            Ok(())
        })
    }

    /// Removes the wire from `source` to `destination` that this machine
    /// owns, if there is one, as [`Self::wire_add`] adds it. Removing a wire of
    /// the default diagnostics wiring, such as this machine's pass-through
    /// of its diagnostics input, keeps it removed until it is wired again.
    pub fn wire_del(&mut self, source: Endpoint, destination: Endpoint) -> Result<(), Error> {
        let wire = Wire {
            source: source.0,
            destination: destination.0,
        };
        let owner = self.path.clone();
        self.port_action(|ports| {
            ports.unwire(owner.clone(), wire.clone());
            Ok(())
        })
    }

    /// This machine's input ports that hold values beyond what this
    /// transaction took, with how many, in port order. Within a cut it reads
    /// all of the machine's inputs at once, so a cut that finds nothing and
    /// retries wakes when any input receives a value.
    pub fn port_pending(&mut self) -> Vec<(Key, usize)> {
        let machine = self.path.clone();
        self.port_view(|ports| ports.pending(&machine))
    }

    /// Whether a wire leads into this machine's input `port`.
    pub fn port_input_connected(&mut self, port: impl Into<Key>) -> bool {
        let (machine, port) = (self.path.clone(), port.into());
        self.port_view(|ports| ports.input_connected(&machine, &port))
    }

    /// Whether a wire leads from this machine's output `port`.
    pub fn port_output_connected(&mut self, port: impl Into<Key>) -> bool {
        let (machine, port) = (self.path.clone(), port.into());
        self.port_view(|ports| ports.output_connected(&machine, &port))
    }

    /// The wiring this machine owns: its own wires, with the default rule
    /// that applies to it and the default wires it removed. The wires its
    /// parent owns to and from it are the parent's.
    pub fn wire_list(&mut self) -> MachineWiring {
        let machine = self.path.clone();
        let owned = self.port_view(|ports| ports.owned_wiring(&machine));
        let pairs = |wires: Vec<Wire>| {
            wires
                .into_iter()
                .map(|wire| (Endpoint(wire.source), Endpoint(wire.destination)))
                .collect()
        };
        MachineWiring {
            wires: pairs(owned.wires),
            default_wiring: if owned.defaults {
                DiagnosticsWiring::ToRoot
            } else {
                DiagnosticsWiring::None
            },
            unwired_defaults: pairs(owned.suppressed),
        }
    }

    /// Turns the default rule for the wires this machine owns on or off:
    /// its children's diagnostics outputs to its diagnostics input, and,
    /// below the root, that input on to its own output. Off, the machine
    /// starts from its own wires alone, and a wire of the rule's shape that
    /// it adds is its own. [`Self::clear`] turns the rule back on.
    pub fn wire_defaults(&mut self, enabled: bool) -> Result<(), Error> {
        let owner = self.path.clone();
        self.port_action(|ports| {
            ports.set_defaults(owner.clone(), enabled);
            Ok(())
        })
    }

    /// Pushback, which `body` returns as its error, `return Err(m.retry())`,
    /// to say that what it needs is not there yet: a divergence that no
    /// alternative takes, which the outermost cut waits out by running again
    /// once what it observed has changed. See [`Self::cut`].
    pub fn retry(&self) -> Error {
        Error::retry("a retry reached no cut")
    }

    /// Runs a port action within this machine's cut, or, outside one, in a
    /// transaction of its own that commits at once, again on a conflict.
    fn port_action<T>(
        &mut self,
        mut action: impl FnMut(&mut PortJournal) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if let Some(cut) = &mut self.cut {
            cut.observed = true;
        }
        self.transact(|_, events| action(&mut events.ports))
    }

    /// Runs `action` on this machine's cut's journals, or, outside a cut,
    /// on fresh ones that commit at once, as one transaction, again where
    /// the commit conflicts. Where `action` fails, nothing it did commits.
    fn transact<T>(
        &mut self,
        mut action: impl FnMut(&mut StoreJournal, &mut RuntimeEventJournal) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if let Some(cut) = &mut self.cut {
            let (mut journal, mut events) = (cut.journal.clone(), cut.events.clone());
            let result = action(&mut journal, &mut events)?;
            (cut.journal, cut.events) = (journal, events);
            return Ok(result);
        }
        let resources = self.resources();
        loop {
            let (_, store, events) = resources.transaction_snapshot();
            let mut store = StoreJournal::new(store);
            let mut events = RuntimeEventJournal::new(events);
            let result = action(&mut store, &mut events)?;
            match resources.try_commit_transaction(&store, &events) {
                StoreCommitResult::Committed => return Ok(result),
                StoreCommitResult::Conflict => continue,
                StoreCommitResult::MissingVolume(volume) => {
                    return Err(Error::new(format!(
                        "reflection volume {} was revoked before the action committed",
                        volume.get()
                    )));
                }
            }
        }
    }

    /// Observes the ports within this machine's cut, or, outside one, as
    /// they stand.
    fn port_view<T>(&mut self, view: impl FnOnce(&mut PortJournal) -> T) -> T {
        match &mut self.cut {
            Some(cut) => {
                cut.observed = true;
                view(&mut cut.events.ports)
            }
            None => {
                let (_, _, events) = self.resources().transaction_snapshot();
                view(&mut RuntimeEventJournal::new(events).ports)
            }
        }
    }

    /// Reads the virtual resource at `path` within a cut: a proxy, named by
    /// a path of keys, for host state that the runtime does not see. It
    /// holds no value, so the read pushes nothing, but it joins the cut's
    /// transaction as [`Self::heap_get`]'s read does: should another cut
    /// set or rewrite the resource, or a path above or below it, and commit
    /// first, this cut's commit conflicts, and once this cut has failed,
    /// that write wakes it. As on the heap, the cut's own earlier set of
    /// the path covers the read, which then records nothing. Outside a cut,
    /// a read joins no transaction, so this does nothing.
    pub fn virtual_get<K: Into<Key>>(&mut self, path: impl IntoIterator<Item = K>) {
        if let Some(cut) = &mut self.cut {
            cut.observed |= cut.journal.observe_virtual_read(&key_path(path));
        }
    }

    /// Sets the virtual resource at `path`, as Rust code does once it
    /// changes, or sees a change in, the host state that the resource
    /// stands for, and so knows that state. Outside a cut, the write
    /// commits at once; within one, with the outermost cut, so a failed cut
    /// writes nothing.
    pub fn virtual_set<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        self.store_edit(|journal| journal.write_virtual(&path))
    }

    /// Rewrites the virtual resource at `path`: a write, as
    /// [`Self::virtual_set`] is, for a change that depends on the state
    /// before it, such as an append. As on the heap, it does not cover a
    /// later read of the path, which still depends on what other cuts
    /// write.
    pub fn virtual_rewrite<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        self.store_edit(|journal| journal.rewrite_virtual(&path))
    }

    /// Runs `body` as a cut: its actions on shared state, the heap and
    /// virtual resources, form one atomic, isolated transaction that commits
    /// when it returns `Ok`, and cuts within it nest. When `body` returns an
    /// error, the cut undoes what it did, the machine's stack, stash and
    /// state included, and returns the error; reads stay recorded.
    ///
    /// An explicit failure, from [`Self::fail`], is the intent to
    /// backtrack: a nested cut returns it, so its caller may try another
    /// alternative, and the outermost cut runs `body` again, on the state it
    /// started from, once something it observed has changed. Pushback, an
    /// evaluation that waited on what is not known yet, runs it again once
    /// anything has changed or what the evaluation waited on can progress,
    /// a promise fulfilled or a lazy value's work runnable or done. The
    /// outermost cut also runs `body` again when its commit conflicts with
    /// another's.
    /// Other errors end the outermost cut. Rust effects within `body` must
    /// tolerate a rerun, or wait for a commit or undo an abort with
    /// [`Self::on_commit`] and [`Self::on_abort`].
    pub fn cut<'b, R>(
        &mut self,
        mut body: impl for<'inner> FnMut(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        if self.cut.is_some() {
            return self.nested_cut(&mut body);
        }
        loop {
            let start = self.transaction_begin();
            let outcome = self.within_scope(|m| body(m));
            match self.transaction_end(start, outcome) {
                Ok(result) => return Ok(result),
                Err(retry) => self.transaction_await(retry)?,
            }
        }
    }

    /// Runs `body` once as a transaction: as [`Self::cut`], without its
    /// retry (`api-atomic`). Outermost, it commits when `body` returns `Ok`.
    /// When `body` returns an error, explicit failure and pushback included,
    /// it undoes what `body` did and returns the error at once, without
    /// waiting for a change; when its commit conflicts with another's, it
    /// undoes it and returns [`ErrorKind::Conflict`], so the caller tells
    /// "run again now" from "run again after a change". Within a cut, it is
    /// a nested cut.
    pub fn atomic<'b, R>(
        &mut self,
        body: impl for<'inner> FnOnce(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        if self.cut.is_some() {
            let mut body = Some(body);
            return self
                .nested_cut(&mut |m| body.take().expect("a nested cut runs its body once")(m));
        }
        let start = self.transaction_begin();
        let outcome = self.within_scope(body);
        let conflicted = self.cut.as_ref().is_some_and(|cut| cut.conflicted);
        match outcome {
            // An attempt whose forks merged in no serial order is
            // inconsistent, whatever it returned: that is the conflict.
            Err(error) if !conflicted => {
                self.transaction_abort(start);
                Err(error)
            }
            outcome => match self.transaction_end(start, outcome) {
                Ok(result) => Ok(result),
                Err(CutRetry::Now) => Err(Error::conflict()),
                Err(CutRetry::Never(error)) => Err(error),
                Err(CutRetry::AfterChange(_) | CutRetry::AfterEvaluation(..)) => {
                    unreachable!("only an error waits for a change, and it returned above")
                }
            },
        }
    }

    /// Begins the outermost transaction, which [`Self::transaction_end`]
    /// or [`Self::transaction_abort`] ends.
    fn transaction_begin(&mut self) -> TransactionStart {
        debug_assert!(self.cut.is_none(), "one outermost transaction at a time");
        let state = self.state.clone();
        let (generation, store, events) = self.resources().transaction_snapshot();
        self.cut = Some(CutTransaction {
            journal: StoreJournal::new(store),
            events: RuntimeEventJournal::new(events),
            observed: false,
            conflicted: false,
            awaits_evaluation: false,
            awaited: Vec::new(),
            commit_hooks: Vec::new(),
            abort_hooks: Vec::new(),
        });
        TransactionStart { state, generation }
    }

    /// Ends the outermost transaction as the `outcome` of its body says: it
    /// commits after a success, or returns the machine to its start and
    /// says when to run the body again. A failure or pushback, with no
    /// alternative left, diverges: it waits for a change in what it
    /// observed.
    fn transaction_end<R>(
        &mut self,
        start: TransactionStart,
        outcome: Result<R, Error>,
    ) -> Result<R, CutRetry> {
        let resources = self.resources();
        let cut = self
            .cut
            .take()
            .expect("the outermost cut keeps its transaction");
        let next = match outcome {
            // An attempt whose forks merged in no serial order is
            // inconsistent, whatever it returned.
            _ if cut.conflicted => CutRetry::Now,
            Ok(result) => match resources.try_commit_transaction(&cut.journal, &cut.events) {
                StoreCommitResult::Committed => {
                    self.run_hooks(cut.commit_hooks);
                    return Ok(result);
                }
                StoreCommitResult::Conflict => CutRetry::Now,
                StoreCommitResult::MissingVolume(volume) => CutRetry::Never(Error::new(format!(
                    "reflection volume {} was revoked before the cut committed",
                    volume.get()
                ))),
            },
            Err(error)
                if matches!(error.kind(), ErrorKind::Backtrack | ErrorKind::Retry)
                    && cut.awaits_evaluation =>
            {
                CutRetry::AfterEvaluation(start.generation, cut.awaited.clone())
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::Backtrack | ErrorKind::Retry)
                    && cut.observed =>
            {
                CutRetry::AfterChange(Box::new((cut.journal.clone(), cut.events.clone())))
            }
            Err(error) if matches!(error.kind(), ErrorKind::Backtrack | ErrorKind::Retry) => {
                CutRetry::Never(Error::new(
                    "a cut failed or retried without observing shared state, so no \
                     change could wake it",
                ))
            }
            Err(error) => CutRetry::Never(error),
        };
        self.state = start.state;
        self.run_hooks(cut.abort_hooks.into_iter().rev());
        Err(next)
    }

    /// Abandons the outermost transaction, returning the machine to its
    /// start, for a failure that something else handles.
    fn transaction_abort(&mut self, start: TransactionStart) {
        let cut = self
            .cut
            .take()
            .expect("the outermost cut keeps its transaction");
        self.state = start.state;
        self.run_hooks(cut.abort_hooks.into_iter().rev());
    }

    /// Waits as `retry` says before the outermost cut runs again, or
    /// returns the error that ended it.
    fn transaction_await(&mut self, retry: CutRetry) -> Result<(), Error> {
        let resources = self.resources();
        match retry {
            CutRetry::Now => {}
            CutRetry::AfterChange(journals) => {
                let (journal, events) = &*journals;
                self.await_change(&resources, journal, events)
            }
            CutRetry::AfterEvaluation(generation, awaited) => {
                self.await_evaluation(&resources, generation, &awaited);
            }
            CutRetry::Never(error) => return Err(error),
        }
        Ok(())
    }

    /// Waits, for a cut whose evaluation waited, until anything has changed
    /// since the observation epoch `generation`, or until what its
    /// evaluations waited on can progress: a promise fulfilled, or a lazy
    /// value's work runnable or done. That progress needs no change in shared
    /// state, so the cut subscribes to it as evaluation does, pumping the
    /// work it waits on, and counts itself an observer of the epoch, whose
    /// advances then move the work generation it waits on too.
    fn await_evaluation(
        &self,
        resources: &RuntimeSharedResources,
        generation: u64,
        awaited: &[WorkDependency],
    ) {
        let runtime = self.runtime.assembler.evaluation_runtime();
        let context = self.runtime.assembler.eval_context();
        let waits = awaited
            .iter()
            .filter_map(|dependency| match dependency {
                WorkDependency::Wait(wait) => Some(wait.clone()),
                WorkDependency::Promise(promise) => {
                    crate::eval::promise_root_wait(&context, promise).ok()
                }
                #[cfg(test)]
                WorkDependency::Test(_) => None,
            })
            .collect::<Vec<_>>();
        let coordinator = self.runtime.assembler.values().core().work_coordinator();
        let Some(coordinator) = coordinator.filter(|_| !waits.is_empty()) else {
            runtime.pump_until_stable();
            resources.wait_for_change(generation);
            return;
        };
        let _observation = coordinator.observe_epoch_as_client();
        let mut route = ExactDemandRoute::default();
        loop {
            runtime.pump_until_stable();
            let work = coordinator.client_work_generation();
            if resources.observations.current().get() != generation {
                return;
            }
            let mut progressed = false;
            let mut busy = None;
            for wait in &waits {
                match context.pump_wait_on_route(wait, AWAIT_PUMP_ALLOWANCE, &mut route) {
                    EvaluationPumpOutcome::TargetReady => return,
                    EvaluationPumpOutcome::BudgetExhausted => progressed = true,
                    EvaluationPumpOutcome::Busy => busy = Some(wait),
                    EvaluationPumpOutcome::NoProgress => {}
                }
            }
            if progressed {
                continue;
            }
            match busy {
                Some(wait) => context.wait_for_claimed_task_on_route(wait, &mut route),
                None => coordinator.wait_for_client_change(work),
            }
        }
    }

    /// An explicit failure, the intent to backtrack, which `body` returns
    /// as its error: `return Err(m.fail())`. See [`Self::cut`].
    pub fn fail(&self) -> Error {
        Error::fail()
    }

    /// Pops `items` values and registers `hook` to run once the outermost
    /// cut commits, after the hooks registered before it. Outside a cut it
    /// runs at once, since each action there commits by itself.
    ///
    /// A hook runs on this machine, with its local state as it then stands,
    /// but on a stack holding only the captured values, in their order, and
    /// an empty stash; the machine's own stack and stash return after. It
    /// runs even on a machine a later error left in error, since it sees
    /// only what it captured. Hooks are best effort, and the host's: a hook
    /// handles its own failures, by logging through the machine or
    /// otherwise, and a cut's outcome is only its own. A panic within a cut
    /// abandons it without running them.
    pub fn on_commit(
        &mut self,
        items: usize,
        hook: impl FnOnce(&mut Machine<'_, '_>) + Send + 'static,
    ) -> Result<(), Error> {
        let hook = self.hook(items, hook)?;
        match &mut self.cut {
            Some(cut) => cut.commit_hooks.push(hook),
            None => self.run_hooks([hook]),
        }
        Ok(())
    }

    /// Pops `items` values and registers `hook` to run if a failure
    /// backtracks through it: when the cut it was registered in, or one
    /// enclosing it, aborts, before the hooks registered before it. It runs
    /// as [`Self::on_commit`]'s hooks do, once the cut has undone what it
    /// did, and within the enclosing cut when there is one. Outside a cut
    /// nothing aborts, and the hook is dropped.
    pub fn on_abort(
        &mut self,
        items: usize,
        hook: impl FnOnce(&mut Machine<'_, '_>) + Send + 'static,
    ) -> Result<(), Error> {
        let hook = self.hook(items, hook)?;
        if let Some(cut) = &mut self.cut {
            cut.abort_hooks.push(hook);
        }
        Ok(())
    }

    fn hook(
        &mut self,
        items: usize,
        run: impl FnOnce(&mut Machine<'_, '_>) + Send + 'static,
    ) -> Result<Hook, Error> {
        let depth = self.require(items)?;
        Ok(Hook {
            machine: self.cut_offset.clone(),
            items: self.state.stack.split_off(depth - items),
            run: Box::new(run),
        })
    }

    /// Runs hooks in turn, each on the machine that registered it, which is
    /// this one or a descendant.
    fn run_hooks(&mut self, hooks: impl IntoIterator<Item = Hook>) {
        for Hook {
            machine,
            items,
            run,
        } in hooks
        {
            debug_assert!(machine.starts_with(&self.cut_offset));
            let below = machine[self.cut_offset.len()..].to_vec();
            if below.is_empty() {
                self.run_hook(items, run);
            } else {
                self.descend(below, Reach::Internal, move |m| {
                    m.run_hook(items, run);
                    Ok(())
                })
                .expect("a hook's machine stays reachable until the hook runs");
            }
        }
    }

    /// Runs a hook on a stack of its captured values and an empty stash.
    fn run_hook(&mut self, items: Vec<HandleValue>, run: Box<HookFn>) {
        let stack = std::mem::replace(&mut self.state.stack, items);
        let stash = std::mem::take(&mut self.state.stash);
        run(self);
        self.state.stack = stack;
        self.state.stash = stash;
    }

    fn nested_cut<'b, R>(
        &mut self,
        body: &mut impl for<'inner> FnMut(&mut Machine<'inner, 'b>) -> Result<R, Error>,
    ) -> Result<R, Error>
    where
        'scope: 'b,
    {
        let checkpoint = self.checkpoint();
        let outcome = self.within_scope(|m| body(m));
        if outcome.is_err() {
            self.rollback(checkpoint);
        }
        outcome
    }

    /// A point within a cut that this machine can return to.
    fn checkpoint(&self) -> CutCheckpoint {
        let cut = self.cut.as_ref().expect("a checkpoint lies within a cut");
        CutCheckpoint {
            state: self.state.clone(),
            journal: cut.journal.clone(),
            events: cut.events.clone(),
            commit_hooks: cut.commit_hooks.len(),
            abort_hooks: cut.abort_hooks.len(),
        }
    }

    /// Returns the machine to `checkpoint`, as a failed nested cut does,
    /// running the abort hooks registered since.
    fn rollback(&mut self, checkpoint: CutCheckpoint) {
        self.state = checkpoint.state;
        let cut = self
            .cut
            .as_mut()
            .expect("a checkpoint's transaction outlives it");
        // The journals' clones share their reads, which stay recorded.
        cut.journal = checkpoint.journal;
        cut.events = checkpoint.events;
        cut.commit_hooks.truncate(checkpoint.commit_hooks);
        let aborted = cut.abort_hooks.split_off(checkpoint.abort_hooks);
        self.run_hooks(aborted.into_iter().rev());
    }

    /// Waits until something a failed cut observed has changed. The thread
    /// first runs the runtime's background work, which, without workers,
    /// nothing else may run. Today a wake follows any change in the
    /// runtime, and validation decides whether the cut's observations
    /// changed; a finer wake keeps the same contract.
    fn await_change(
        &self,
        resources: &RuntimeSharedResources,
        journal: &StoreJournal,
        events: &RuntimeEventJournal,
    ) {
        loop {
            self.runtime
                .assembler
                .evaluation_runtime()
                .pump_until_stable();
            match resources.validate_transaction(journal, events) {
                (StoreCommitResult::Committed, generation) => {
                    resources.wait_for_change(generation);
                }
                _ => return,
            }
        }
    }

    /// Stages an edit within a cut, or commits it at once outside one.
    fn store_edit(&mut self, edit: impl Fn(&mut StoreJournal)) -> Result<(), Error> {
        if let Some(cut) = &mut self.cut {
            edit(&mut cut.journal);
            return Ok(());
        }
        let resources = self.resources();
        loop {
            let mut journal = StoreJournal::new(resources.reflection_snapshot().1);
            edit(&mut journal);
            match resources.commit_reflection(&journal) {
                StoreCommitResult::Committed => return Ok(()),
                // A write that reads nothing conflicts with nothing, but a
                // conflict analysis may be coarse: write again, afresh.
                StoreCommitResult::Conflict => continue,
                StoreCommitResult::MissingVolume(volume) => {
                    return Err(Error::new(format!(
                        "reflection volume {} was revoked before the write committed",
                        volume.get()
                    )));
                }
            }
        }
    }

    fn resources(&self) -> std::sync::Arc<RuntimeSharedResources> {
        self.runtime
            .assembler
            .evaluation_runtime()
            .shared_resources()
    }

    /// Pops a value, checking that it belongs to this runtime before the
    /// store takes it.
    /// Pops the top value into local state at `key`, replacing what was
    /// there.
    pub fn local_set(&mut self, key: impl Into<Key>) -> Result<(), Error> {
        let value = self.pop()?;
        self.state.locals.insert(key.into(), value);
        Ok(())
    }

    /// Pushes the local state at `key`, or `{}` where the machine holds
    /// none there.
    pub fn local_get(&mut self, key: impl Into<Key>) {
        let value = match self.state.locals.get(&key.into()) {
            Some(value) => value.clone(),
            None => self.runtime.assembler.values().empty_dict(),
        };
        self.push(value);
    }

    /// Moves the local state at `key` to the stack, or pushes `{}` where the
    /// machine holds none there: the twin of [`Self::local_set`].
    pub fn local_take(&mut self, key: impl Into<Key>) {
        let value = match self.state.locals.remove(&key.into()) {
            Some(value) => value,
            None => self.runtime.assembler.values().empty_dict(),
        };
        self.push(value);
    }

    /// Returns the machine to its initial state: an empty stack, stash,
    /// local state and children, and the initial ports of the machine and
    /// its descendants: nothing waiting in their inputs, and the wiring they
    /// own back to the defaults. What arrives at their inputs later stays,
    /// and the wires its parent owns to and from it stay. The shares of the
    /// environment that the machine and its descendants bind go too. The
    /// ports and shares change as a write does: at once outside a cut, with
    /// the outermost cut within one. It fails, changing nothing, while a
    /// descendant is forked.
    pub fn clear(&mut self) -> Result<(), Error> {
        if self.state.has_fork() {
            return Err(Error::new(
                "a machine clears once its forked descendants have joined",
            ));
        }
        let path = self.path.clone();
        let cleared = if path.is_empty() {
            vec![
                vec![environment::SHARE.clone()],
                vec![environment::CHILDREN.clone()],
            ]
        } else {
            vec![environment::environment_node(&path)]
        };
        if let Some(cut) = &mut self.cut {
            // Clearing the ports claims what the inputs hold.
            cut.observed = true;
        }
        self.transact(|store, events| {
            events.ports.clear_below(&path);
            for node in &cleared {
                store.remove_environment(node.clone());
            }
            events.shares.clear_below(path.clone());
            Ok(())
        })?;
        self.state = MachineState::default();
        Ok(())
    }

    fn push(&mut self, value: HandleValue) {
        self.state.stack.push(value);
    }

    pub(crate) fn pop(&mut self) -> Result<HandleValue, Error> {
        self.state
            .stack
            .pop()
            .ok_or_else(|| underflow("stack", 1, 0))
    }

    fn peek(&self) -> Result<&HandleValue, Error> {
        self.state
            .stack
            .last()
            .ok_or_else(|| underflow("stack", 1, 0))
    }

    /// The stack's depth, once it holds at least `n` values.
    fn require(&self, n: usize) -> Result<usize, Error> {
        let depth = self.state.stack.len();
        if depth < n {
            return Err(underflow("stack", n, depth));
        }
        Ok(depth)
    }
}

/// Adds to a walk's machines those below its start, at depth `start` from
/// the root, that hold only port state, as idle, with their paths from
/// depth `base`, in path order, which is the walk's depth-first order.
/// Nothing below a forked machine joins: it lies on the fork's thread.
fn with_port_presence(
    found: Vec<(MachinePath, MachineStatus)>,
    present: BTreeSet<Vec<Key>>,
    start: usize,
    base: usize,
) -> Vec<(MachinePath, MachineStatus)> {
    let mut machines: BTreeMap<Vec<Key>, MachineStatus> = found
        .into_iter()
        .map(|(path, status)| (path.0, status))
        .collect();
    let forked: Vec<Vec<Key>> = machines
        .iter()
        .filter(|(_, status)| **status == MachineStatus::Forked)
        .map(|(path, _)| path.clone())
        .collect();
    for path in present {
        if path.len() <= start {
            continue;
        }
        let path = path[base..].to_vec();
        let below_fork = forked
            .iter()
            .any(|fork| path.len() > fork.len() && path.starts_with(fork));
        if !below_fork {
            machines.entry(path).or_insert(MachineStatus::Idle);
        }
    }
    machines
        .into_iter()
        .map(|(path, status)| (MachinePath(path), status))
        .collect()
}

/// Pushback from an input port that holds fewer than a read needs.
fn short(address: &PortAddress, n: usize) -> Error {
    Error::retry(format!(
        "input {:?} of machine {:?} holds fewer than {n} values",
        address.port, address.machine
    ))
}

fn underflow(what: &str, needed: usize, held: usize) -> Error {
    Error::new(format!(
        "the machine's {what} holds {held} values, and the operation needs {needed}"
    ))
}

/// `keys`, which must differ.
fn distinct_keys<K: Into<Key>>(keys: impl IntoIterator<Item = K>) -> Result<Vec<Key>, Error> {
    let keys = keys.into_iter().map(Into::into).collect::<Vec<Key>>();
    for (index, key) in keys.iter().enumerate() {
        if keys[..index].contains(key) {
            return Err(Error::new(format!(
                "a dict's keys must differ, and `{key:?}` repeats"
            )));
        }
    }
    Ok(keys)
}

/// `path`, which must not be empty.
fn nonempty_path<K: Into<Key>>(path: impl IntoIterator<Item = K>) -> Result<Vec<Key>, Error> {
    let path = key_path(path);
    if path.is_empty() {
        return Err(Error::new("a path names at least one key"));
    }
    Ok(path)
}

/// Glam's undefined value, `{}`.
fn undefined(_access: &RuntimeValueAccess<'_>) -> CoreValue {
    CoreValue::Dict(Dict::new_sync())
}

/// The lazy application of `builtin` to `arguments`.
fn builtin_call_in<const N: usize>(
    access: &RuntimeValueAccess<'_>,
    builtin: Builtin,
    arguments: [CoreValue; N],
) -> CoreValue {
    CoreValue::Lazy(crate::core::LazyValue::from_application_in(
        access,
        CoreValue::Builtin(builtin),
        Arc::from(arguments),
    ))
}

/// The lazy member of `dict` at `path`.
fn path_access_in(access: &RuntimeValueAccess<'_>, dict: &CoreValue, path: &[Key]) -> CoreValue {
    CoreValue::Lazy(crate::core::LazyValue::from_access_in(
        access,
        path.iter()
            .cloned()
            .map(crate::core_net::CoreDataKey::Key)
            .collect::<Vec<_>>()
            .into(),
        Arc::from([access.duplicate_value(dict)]),
    ))
}

/// `dict` with `item` at `path`, lazily, replacing what is there.
fn dict_insert_in(
    access: &RuntimeValueAccess<'_>,
    path: &[Key],
    item: CoreValue,
    dict: CoreValue,
) -> CoreValue {
    builtin_call_in(
        access,
        Builtin::DictInsert,
        [path_value_in(access, path), item, dict],
    )
}

/// `dict` without what is at `path`, lazily, pruning the dicts that empties.
fn dict_remove_in(access: &RuntimeValueAccess<'_>, path: &[Key], dict: CoreValue) -> CoreValue {
    builtin_call_in(
        access,
        Builtin::DictRemove,
        [path_value_in(access, path), dict],
    )
}

fn path_value_in(access: &RuntimeValueAccess<'_>, path: &[Key]) -> CoreValue {
    CoreValue::List(crate::core::List::from_values(
        path.iter().map(|key| key.to_value_in(access)).collect(),
    ))
}

/// A shuffle's pattern, `"abc-cba"`: the input names' count, and for each
/// output the index of the input it copies.
fn shuffle_pattern(pattern: &str) -> Result<(Vec<u8>, Vec<usize>), Error> {
    let malformed = |reason: &str| Error::new(format!("shuffle pattern `{pattern}` {reason}"));
    let (inputs, outputs) = pattern
        .split_once('-')
        .ok_or_else(|| malformed("separates inputs from outputs with `-`"))?;
    let inputs = inputs.bytes().collect::<Vec<_>>();
    for (index, name) in inputs.iter().enumerate() {
        if !name.is_ascii_alphabetic() {
            return Err(malformed("names its inputs with ASCII letters"));
        }
        if inputs[..index].contains(name) {
            return Err(malformed("binds each name once"));
        }
    }
    let outputs = outputs
        .bytes()
        .map(|name| {
            inputs
                .iter()
                .position(|input| *input == name)
                .ok_or_else(|| malformed("uses only names its inputs bind"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((inputs, outputs))
}

fn key_path<K: Into<Key>>(path: impl IntoIterator<Item = K>) -> Vec<Key> {
    path.into_iter().map(Into::into).collect()
}

mod effects;
pub(in crate::api) mod environment;

pub use effects::{Answer, EffectFamilies, EffectProvider};

pub use crate::reflection::PromiseStatus;

#[cfg(test)]
mod tests;

/// The bytes an evaluated binary holds.
fn binary(evaluated: crate::api::EvaluatedValue) -> Result<Bytes, Error> {
    evaluated
        .as_bytes()?
        .ok_or_else(|| Error::new("the top value is not binary data"))
}

/// The error for a volume that no longer exists.
fn revoked(volume: crate::reflection::VolumeId) -> Error {
    Error::new(format!("reflection volume {} was revoked", volume.get()))
}

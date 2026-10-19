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
    DIAGNOSTICS, PortAddress, PortJournal, RuntimeEventJournal, RuntimeSharedResources, Wire,
    WireEnd,
};
use super::{
    Assembler, Diagnostic, Error, ErrorKind, EvaluationRuntime, ModuleInput, Value, ValueKind,
};
use crate::core::Key;
use crate::reflection::{ConflictAnalysisStrategy, StoreCommitResult, StoreJournal};

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
    /// own output, so diagnostics gather at the root's input. A machine
    /// intervenes by unwiring a default wire it owns.
    #[default]
    ToRoot,
    /// None: the diagnostics ports are wired as any other.
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
///         c.push_int(words[..1].len() as i64);
///         Ok(())
///     })?;
///     let right = m.fork("right", |c| {
///         c.push_int(words[1..].len() as i64);
///         Ok(())
///     })?;
///     m.join(left)?;
///     m.join(right)?;
///     // What a fork leaves is on its machine's stack.
///     m.take("left", 1)?;
///     m.take("right", 1)?;
///     Ok(m.pop_int()? + m.pop_int()?)
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

#[derive(Clone, Default)]
struct MachineState {
    stack: Vec<Value>,
    stash: Vec<Value>,
    locals: HashMap<Key, Value>,
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
        let assembler = Assembler::builder().evaluation_runtime(runtime).build()?;
        Self::around(assembler, config.diagnostics_wiring)
    }

    /// A runtime around an assembler, which `open` builds and the CLI's
    /// transition (`crate::transition`) brings.
    pub(crate) fn around(
        assembler: Assembler,
        diagnostics_wiring: DiagnosticsWiring,
    ) -> Result<Self, Error> {
        let evaluation_runtime = assembler.evaluation_runtime();
        evaluation_runtime
            .shared_resources()
            .set_diagnostics_wiring(diagnostics_wiring == DiagnosticsWiring::ToRoot);
        // Until reflection tasks are machines, the runtime's own diagnostics
        // join the root's diagnostics input as if a child had sent them.
        if diagnostics_wiring == DiagnosticsWiring::ToRoot {
            assembler.diagnostic_bus().route_to_port(
                &evaluation_runtime,
                (),
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
        let present = PortJournal::new(events.ports).present_below(path.keys());
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
            Some(cut) => cut.events.ports.present_below(&absolute),
            None => {
                let (_, _, events) = self.resources().transaction_snapshot();
                PortJournal::new(events.ports).present_below(&absolute)
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
                events
            },
            observed: false,
            conflicted: false,
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
        let serial = match (branch, &mut self.cut) {
            (Some(branch), Some(cut)) => cut.merge(branch),
            _ => true,
        };
        Joined {
            outcome,
            panic,
            serial,
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
    commit_hooks: Vec<Hook>,
    abort_hooks: Vec<Hook>,
}

impl CutTransaction {
    /// Merges a fork's branch of this transaction, returning whether it
    /// merged in a serial order. A branch that did not still brings its
    /// hooks, so that its abort hooks run as the attempt aborts.
    fn merge(&mut self, branch: CutTransaction) -> bool {
        // Both journals merge, or neither: the ports' check changes nothing.
        let serial = !self.events.ports.conflicts_with(&branch.events.ports)
            && self.journal.merge_branch(&branch.journal);
        if serial {
            self.events.ports.merge(&branch.events.ports);
        }
        self.observed |= branch.observed;
        self.conflicted |= branch.conflicted || !serial;
        self.commit_hooks.extend(branch.commit_hooks);
        self.abort_hooks.extend(branch.abort_hooks);
        serial
    }
}

/// A hook, the stack items it captured when registered, and the path of
/// the machine that registered it, from the machine whose turn opened the
/// cut.
struct Hook {
    machine: Vec<Key>,
    items: Vec<Value>,
    run: Box<HookFn>,
}

type HookFn = dyn FnOnce(&mut Machine<'_, '_>) + Send;

/// When an aborted outermost cut runs again.
enum CutRetry {
    /// At once: its commit conflicted.
    Now,
    /// Once something its journals observed has changed: it failed, or
    /// met pushback.
    AfterChange(Box<(StoreJournal, RuntimeEventJournal)>),
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

    pub fn push_unit(&mut self) {
        self.push(self.runtime.assembler.values().unit());
    }

    pub fn push_int(&mut self, value: i64) {
        self.push(self.runtime.assembler.values().integer(value));
    }

    /// Pushes text, which glam holds as UTF-8 bytes.
    pub fn push_text(&mut self, text: impl AsRef<str>) {
        self.push(self.runtime.assembler.values().text(text));
    }

    pub fn push_bytes(&mut self, bytes: impl Into<Bytes>) {
        self.push(self.runtime.assembler.values().bytes(bytes));
    }

    /// Pushes the atom `'name`.
    pub fn push_atom(&mut self, name: impl AsRef<str>) {
        self.push(self.runtime.assembler.values().atom_from_text(name));
    }

    /// Pushes the abstract global path with these parts, an atom that glam
    /// source names only as `abstract_global_path`.
    pub fn push_abstract_path<I, S>(&mut self, parts: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.push(self.runtime.assembler.values().abstract_global_path(parts));
    }

    /// Replaces the top `n` values with a list of them, the deepest first.
    pub fn make_list(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.require(n)?;
        let items = self.state.stack.split_off(depth - n);
        let list = self.runtime.assembler.values().list(items)?;
        self.push(list);
        Ok(())
    }

    /// Replaces the top values, one for each name, with a dict from the
    /// names to them: the deepest value takes the first name.
    pub fn make_dict<I, S>(&mut self, names: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let names = names.into_iter().collect::<Vec<_>>();
        for (index, name) in names.iter().enumerate() {
            if names[..index]
                .iter()
                .any(|earlier| earlier.as_ref() == name.as_ref())
            {
                return Err(Error::new(format!(
                    "a dict's names must differ, and `{}` repeats",
                    name.as_ref()
                )));
            }
        }
        let depth = self.require(names.len())?;
        let items = self.state.stack.split_off(depth - names.len());
        let dict = self
            .runtime
            .assembler
            .values()
            .record(names.into_iter().zip(items))?;
        self.push(dict);
        Ok(())
    }

    /// Evaluates the top value to an integer and pops it.
    pub fn pop_int(&mut self) -> Result<i64, Error> {
        let value = self.pop()?;
        let evaluated = self.runtime.assembler.evaluator().eval(&value)?;
        evaluated
            .as_i64()?
            .ok_or_else(|| Error::new("the top value is not an integer within 64 bits"))
    }

    /// Evaluates the top value to bytes and pops them.
    pub fn pop_bytes(&mut self) -> Result<Bytes, Error> {
        let value = self.pop()?;
        let binary = self.runtime.assembler.values().anno_binary(value)?;
        let evaluated = self.runtime.assembler.evaluator().eval(&binary)?;
        evaluated
            .as_bytes()?
            .ok_or_else(|| Error::new("the top value is not binary data"))
    }

    /// Evaluates the top value to UTF-8 text and pops it.
    pub fn pop_text(&mut self) -> Result<String, Error> {
        let bytes = self.pop_bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::new("the top value is not UTF-8 text"))
    }

    /// Evaluates the top value to an atom named by text, such as `'name`,
    /// and pops its name.
    pub fn pop_atom_name(&mut self) -> Result<String, Error> {
        let value = self.pop()?;
        let assembler = &self.runtime.assembler;
        let key = assembler.reflection().atom_key(&value)?;
        let name = assembler.evaluator().eval(&key)?.as_bytes()?;
        name.and_then(|name| String::from_utf8(name.to_vec()).ok())
            .ok_or_else(|| Error::new("the atom is not named by text"))
    }

    /// Evaluates the top value to a list and replaces it with its items, the
    /// first deepest, returning how many there are.
    pub fn unpack_list(&mut self) -> Result<usize, Error> {
        let value = self.pop()?;
        let assembler = &self.runtime.assembler;
        let array = assembler.values().anno_array(value)?;
        let items = assembler
            .evaluator()
            .eval(&array)?
            .array_items()?
            .ok_or_else(|| Error::new("the top value is not a list"))?;
        let count = items.len();
        self.state.stack.extend(items);
        Ok(count)
    }

    /// Evaluates the top value to a dict and replaces it with its entries,
    /// each key pushed before its value, returning how many there are.
    ///
    /// This is reflection: glam offers no iteration of a dict's keys, and the
    /// entries come in the runtime's canonical order of keys.
    pub fn unpack_dict(&mut self) -> Result<usize, Error> {
        let value = self.pop()?;
        let entries = self
            .runtime
            .assembler
            .reflection()
            .dictionary_items(&value)?;
        let count = entries.len();
        for (key, value) in entries {
            self.push(key);
            self.push(value);
        }
        Ok(count)
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

    /// Copies the top value.
    pub fn dup(&mut self) -> Result<(), Error> {
        let top = self.peek()?.clone();
        self.push(top);
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

    /// Replaces the top value with its member at `path`, a dotted path of
    /// names: `asm.result`. Nothing is evaluated.
    pub fn access(&mut self, path: &str) -> Result<(), Error> {
        let values = self.runtime.assembler.values();
        let base = self.pop()?;
        let accessed = values.access_path(
            &base,
            path.split('.').map(|name| values.atom_from_text(name)),
        )?;
        self.push(accessed);
        Ok(())
    }

    /// Annotates a value: the annotation is pushed first, then its target,
    /// and the annotated value replaces them, as `anno Annotation Target`
    /// reads. Nothing is evaluated.
    pub fn anno(&mut self) -> Result<(), Error> {
        let target = self.pop()?;
        let annotation = self.pop()?;
        let annotated = self.runtime.assembler.values().anno(annotation, target)?;
        self.push(annotated);
        Ok(())
    }

    /// Applies a function to `n` arguments: the function is pushed first,
    /// then the arguments in order, and the application replaces them.
    /// Nothing is evaluated.
    pub fn apply(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.require(n + 1)?;
        let arguments = self.state.stack.split_off(depth - n);
        let function = self.pop()?;
        let applied = self
            .runtime
            .assembler
            .values()
            .apply(&function, arguments)?;
        self.push(applied);
        Ok(())
    }

    /// Evaluates the top value to weak-head normal form, in place.
    pub fn eval(&mut self) -> Result<(), Error> {
        let value = self.pop()?;
        let evaluated = self.runtime.assembler.evaluator().eval(&value)?;
        self.push(evaluated.into_value());
        Ok(())
    }

    /// Compiles a module at `module_path` from `inputs`, in order, and
    /// pushes it, returning its warnings.
    pub fn compile<I, S>(
        &mut self,
        module_path: I,
        inputs: impl IntoIterator<Item = ModuleInput>,
    ) -> Result<Vec<Diagnostic>, Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.compile_module(module_path, inputs, None)
    }

    /// As [`Self::compile`], over initial definitions popped from the
    /// stack: a dict whose names the module starts with, as if an input
    /// before the first had defined them.
    pub fn compile_over<I, S>(
        &mut self,
        module_path: I,
        inputs: impl IntoIterator<Item = ModuleInput>,
    ) -> Result<Vec<Diagnostic>, Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let definitions = self.pop()?;
        self.compile_module(module_path, inputs, Some(definitions))
    }

    fn compile_module<I, S>(
        &mut self,
        module_path: I,
        inputs: impl IntoIterator<Item = ModuleInput>,
        definitions: Option<Value>,
    ) -> Result<Vec<Diagnostic>, Error>
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
        self.push(module.into_value());
        Ok(diagnostics)
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
        let value = self.pop_own()?;
        self.store_edit(|journal| journal.write(path.clone(), value.clone()))
    }

    /// Pops a function and rewrites the value at `path` in the shared heap
    /// with it. The function applies lazily to the value the write finds
    /// when it commits, so rewrites compose rather than conflict.
    pub fn heap_rewrite<K: Into<Key>>(
        &mut self,
        path: impl IntoIterator<Item = K>,
    ) -> Result<(), Error> {
        let path = key_path(path);
        let updater = self.pop_own()?;
        self.store_edit(|journal| journal.rewrite(path.clone(), updater.clone()))
    }

    /// Pops `n` values and writes them, in order, to this machine's output
    /// `port`. Each value follows every wire from it, outward by its
    /// parent's wiring and inward by its own, through pass-throughs, to every
    /// buffer it reaches, and arrives when the write commits: at once
    /// outside a cut, with the outermost cut within one. An output with no wire is an
    /// error, which pops nothing. Buffers have no bound yet, so a write
    /// never waits.
    pub fn write(&mut self, port: impl Into<Key>, n: usize) -> Result<(), Error> {
        let port = port.into();
        let depth = self.require(n)?;
        let values = self.state.stack[depth - n..].to_vec();
        for value in &values {
            self.runtime.assembler.values().require(value)?;
        }
        let machine = self.path.clone();
        self.port_action(|ports| {
            let buffers = ports.route(&machine, &port).map_err(Error::new)?;
            for value in &values {
                for buffer in &buffers {
                    ports.append(buffer.clone(), value.clone());
                }
            }
            Ok(())
        })?;
        self.state.stack.truncate(depth - n);
        Ok(())
    }

    /// Pushes the next `n` values of this machine's input `port`, in the
    /// order they arrived, taking them. It reads what is committed, never
    /// this transaction's own writes. When fewer than `n` are there, it is
    /// pushback: within a cut it returns [`ErrorKind::Retry`], which the
    /// outermost cut waits out, and outside one it waits until they are.
    pub fn read(&mut self, port: impl Into<Key>, n: usize) -> Result<(), Error> {
        self.read_port(port.into(), n, true)
    }

    /// As [`Self::read`], but returns [`ErrorKind::Retry`] outside a cut too,
    /// rather than waiting.
    pub fn try_read(&mut self, port: impl Into<Key>, n: usize) -> Result<(), Error> {
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
    pub fn available(&mut self, port: impl Into<Key>) -> usize {
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
    pub fn wire(&mut self, source: Endpoint, destination: Endpoint) -> Result<(), Error> {
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
    /// owns, if there is one, as [`Self::wire`] adds it. Removing a wire of
    /// the default diagnostics wiring, such as this machine's pass-through
    /// of its diagnostics input, keeps it removed until it is wired again.
    pub fn unwire(&mut self, source: Endpoint, destination: Endpoint) -> Result<(), Error> {
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
    pub fn pending(&mut self) -> Vec<(Key, usize)> {
        let machine = self.path.clone();
        self.port_view(|ports| ports.pending(&machine))
    }

    /// Whether a wire leads into this machine's input `port`.
    pub fn input_connected(&mut self, port: impl Into<Key>) -> bool {
        let (machine, port) = (self.path.clone(), port.into());
        self.port_view(|ports| ports.input_connected(&machine, &port))
    }

    /// Whether a wire leads from this machine's output `port`.
    pub fn output_connected(&mut self, port: impl Into<Key>) -> bool {
        let (machine, port) = (self.path.clone(), port.into());
        self.port_view(|ports| ports.output_connected(&machine, &port))
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
            return action(&mut cut.events.ports);
        }
        let resources = self.resources();
        loop {
            let (_, store, events) = resources.transaction_snapshot();
            let store = StoreJournal::new(store);
            let mut events = RuntimeEventJournal::new(events);
            let result = action(&mut events.ports)?;
            match resources.try_commit_transaction(&store, &events) {
                StoreCommitResult::Committed => return Ok(result),
                StoreCommitResult::Conflict => continue,
                StoreCommitResult::MissingVolume(volume) => {
                    return Err(Error::new(format!(
                        "reflection volume {} was revoked before the port action committed",
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
    /// started from, once something it observed has changed. The outermost
    /// cut also runs `body` again when its commit conflicts with another's.
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
        let resources = self.resources();
        loop {
            let start = self.state.clone();
            let (_, store, events) = resources.transaction_snapshot();
            self.cut = Some(CutTransaction {
                journal: StoreJournal::new(store),
                events: RuntimeEventJournal::new(events),
                observed: false,
                conflicted: false,
                commit_hooks: Vec::new(),
                abort_hooks: Vec::new(),
            });
            let outcome = self.within_scope(|m| body(m));
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
                    StoreCommitResult::MissingVolume(volume) => {
                        CutRetry::Never(Error::new(format!(
                            "reflection volume {} was revoked before the cut committed",
                            volume.get()
                        )))
                    }
                },
                // With no alternative left, a failure diverges as pushback
                // does: the cut waits for a change in what it observed.
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
            self.state = start;
            self.run_hooks(cut.abort_hooks.into_iter().rev());
            match next {
                CutRetry::Now => {}
                CutRetry::AfterChange(journals) => {
                    let (journal, events) = &*journals;
                    self.await_change(&resources, journal, events)
                }
                CutRetry::Never(error) => return Err(error),
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
    fn run_hook(&mut self, items: Vec<Value>, run: Box<HookFn>) {
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
        let start = self.state.clone();
        let cut = self.cut.as_ref().expect("a nested cut runs within a cut");
        let journal = cut.journal.clone();
        let events = cut.events.clone();
        let (commit_hooks, abort_hooks) = (cut.commit_hooks.len(), cut.abort_hooks.len());
        let outcome = self.within_scope(|m| body(m));
        if outcome.is_err() {
            self.state = start;
            let cut = self
                .cut
                .as_mut()
                .expect("a nested cut's transaction outlives it");
            // The journals' clones share their reads, which stay recorded.
            cut.journal = journal;
            cut.events = events;
            cut.commit_hooks.truncate(commit_hooks);
            let aborted = cut.abort_hooks.split_off(abort_hooks);
            self.run_hooks(aborted.into_iter().rev());
        }
        outcome
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
    fn pop_own(&mut self) -> Result<Value, Error> {
        let value = self.pop()?;
        self.runtime.assembler.values().require(&value)?;
        Ok(value)
    }

    /// Pops the top value into local state at `key`, replacing what was
    /// there.
    pub fn set(&mut self, key: impl Into<Key>) -> Result<(), Error> {
        let value = self.pop()?;
        self.state.locals.insert(key.into(), value);
        Ok(())
    }

    /// Pushes the local state at `key`.
    pub fn get(&mut self, key: impl Into<Key>) -> Result<(), Error> {
        let key = key.into();
        let value = self
            .state
            .locals
            .get(&key)
            .ok_or_else(|| Error::new(format!("the machine holds no state at {key:?}")))?
            .clone();
        self.push(value);
        Ok(())
    }

    /// Returns the machine to its initial state: an empty stack, stash,
    /// local state and children, and the initial ports of the machine and
    /// its descendants: nothing waiting in their inputs, and the wiring they
    /// own back to the defaults. What arrives at their inputs later stays,
    /// and the wires its parent owns to and from it stay. The ports change
    /// as a write does: at once outside a cut, with the outermost cut within
    /// one. It fails, changing nothing, while a descendant is forked.
    pub fn clear(&mut self) -> Result<(), Error> {
        if self.state.has_fork() {
            return Err(Error::new(
                "a machine clears once its forked descendants have joined",
            ));
        }
        let path = self.path.clone();
        self.port_action(|ports| {
            ports.clear_below(&path);
            Ok(())
        })?;
        self.state = MachineState::default();
        Ok(())
    }

    fn push(&mut self, value: Value) {
        self.state.stack.push(value);
    }

    pub(crate) fn pop(&mut self) -> Result<Value, Error> {
        self.state
            .stack
            .pop()
            .ok_or_else(|| underflow("stack", 1, 0))
    }

    fn peek(&self) -> Result<&Value, Error> {
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

fn key_path<K: Into<Key>>(path: impl IntoIterator<Item = K>) -> Vec<Key> {
    path.into_iter().map(Into::into).collect()
}

#[cfg(test)]
mod tests;

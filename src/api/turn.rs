//! A thread-affine runtime in which Rust threads take turns on keyed machines
//! (`runtime-api-turns-on-machines`).
//!
//! A machine is named by a user-chosen [`Key`] and holds glam values: a
//! stack, a stash beside it, and local state by key. It starts empty, and an
//! empty machine stores nothing. A turn is not atomic: what it changes before
//! an error stays.
//!
//! This first form is a facade over the assembler and its evaluation
//! runtime, which the runtime holds privately; each operation enters the
//! value domain on its own, never across the user's code.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;

use super::{Assembler, Diagnostic, Error, EvaluationRuntime, ModuleInput, Value, ValueKind};
use crate::core::Key;
use crate::reflection::ConflictAnalysisStrategy;

/// What opening a runtime takes. It configures the runtime alone:
/// compilation's settings travel with what is compiled.
#[derive(Clone, Default)]
pub struct RuntimeConfig {
    worker_threads: usize,
    conflict_analysis: Option<Arc<dyn ConflictAnalysisStrategy>>,
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
}

/// A glam runtime. It stays on the thread that opened it, where dropping it
/// retires everything, but threads scoped within that thread's lifetime of
/// it may share it, each taking turns on machines.
///
/// It is not `Send`:
///
/// ```compile_fail
/// fn send<T: Send>() {}
/// send::<glam::Runtime>();
/// ```
///
/// It is `Sync`, so scoped threads may borrow it:
///
/// ```
/// fn sync<T: Sync>() {}
/// sync::<glam::Runtime>();
/// ```
pub struct Runtime {
    machines: Mutex<HashMap<Key, Slot>>,
    assembler: Assembler,
    /// `!Send` but `Sync`, as a mutex guard is.
    _thread: PhantomData<MutexGuard<'static, ()>>,
}

enum Slot {
    Idle(MachineState),
    InTurn,
}

#[derive(Default)]
struct MachineState {
    stack: Vec<Value>,
    stash: Vec<Value>,
    locals: HashMap<Key, Value>,
}

impl MachineState {
    fn is_empty(&self) -> bool {
        self.stack.is_empty() && self.stash.is_empty() && self.locals.is_empty()
    }
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
        Ok(Self::around(assembler))
    }

    /// A runtime around an assembler its caller built, for the CLI's
    /// transition (`crate::transition`).
    pub(crate) fn around(assembler: Assembler) -> Self {
        Self {
            machines: Mutex::new(HashMap::new()),
            assembler,
            _thread: PhantomData,
        }
    }

    /// The assembler the runtime holds, for the CLI's transition
    /// (`crate::transition`).
    pub(crate) fn assembler(&self) -> &Assembler {
        &self.assembler
    }

    /// Starts the background workers, once, in a runtime opened with none:
    /// for a host that learns how many it wants after opening the runtime.
    pub fn activate_workers(&self, worker_threads: usize) -> Result<(), Error> {
        self.assembler
            .evaluation_runtime()
            .activate_workers(worker_threads)
    }

    /// Every profiling counter and phase timer of the runtime, in a
    /// `glam-prof` build.
    #[cfg(feature = "glam-prof")]
    pub fn profile(&self) -> crate::profiling::RuntimeProfileSnapshot {
        self.assembler.evaluation_runtime().profile()
    }

    /// Takes a turn on the machine named `key`, which starts empty if it
    /// holds nothing. A machine takes one turn at a time.
    pub fn run<R>(
        &self,
        key: impl Into<Key>,
        turn: impl FnOnce(&mut Machine<'_>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let key = key.into();
        let state = {
            let mut machines = self.machines();
            match machines.insert(key.clone(), Slot::InTurn) {
                Some(Slot::Idle(state)) => state,
                None => MachineState::default(),
                Some(Slot::InTurn) => {
                    return Err(Error::new(format!(
                        "machine {key:?} is already taking a turn"
                    )));
                }
            }
        };
        let mut machine = Machine {
            runtime: self,
            key: Some(key),
            state,
        };
        turn(&mut machine)
    }

    /// The machines, under a leaf lock: no user code runs while it is held.
    fn machines(&self) -> MutexGuard<'_, HashMap<Key, Slot>> {
        self.machines.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn end_turn(&self, key: Key, state: MachineState) {
        let mut machines = self.machines();
        if state.is_empty() {
            machines.remove(&key);
        } else {
            machines.insert(key, Slot::Idle(state));
        }
    }
}

/// A machine during a turn: its stack, its stash and its local state. The
/// turn ends when it drops, returning what it holds to the runtime.
pub struct Machine<'rt> {
    runtime: &'rt Runtime,
    key: Option<Key>,
    state: MachineState,
}

impl Drop for Machine<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.runtime.end_turn(key, std::mem::take(&mut self.state));
        }
    }
}

impl Machine<'_> {
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

    /// Moves the top `n` values to the stash, keeping their order, so
    /// `stash_pop(n)` returns them as they were.
    pub fn stash(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.require(n)?;
        let moved = self.state.stack.split_off(depth - n);
        self.state.stash.extend(moved);
        Ok(())
    }

    /// Moves the top `n` values of the stash back to the stack, keeping
    /// their order.
    pub fn stash_pop(&mut self, n: usize) -> Result<(), Error> {
        let depth = self.state.stash.len();
        if depth < n {
            return Err(underflow("stash", n, depth));
        }
        let moved = self.state.stash.split_off(depth - n);
        self.state.stack.extend(moved);
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

    /// Returns the machine to its initial state: an empty stack, stash and
    /// local state.
    pub fn clear(&mut self) {
        self.state = MachineState::default();
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

fn underflow(what: &str, needed: usize, held: usize) -> Error {
    Error::new(format!(
        "the machine's {what} holds {held} values, and the operation needs {needed}"
    ))
}

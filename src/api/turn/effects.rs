//! Effects a turn runs (`api-effect-providers`,
//! `effect-families-pass-through`): a monadic effect evaluates on the turn's
//! thread, keeping only the pure core, and each other request it makes
//! returns to the turn. The machine serves the standard families that the
//! provider passes through, and the provider answers the rest outside any
//! evaluation, so it may evaluate too. Branching through cuts follows.

use std::collections::HashMap;
use std::ops::BitOr;
use std::sync::{Arc, Mutex, PoisonError};

use super::{DIAGNOSTICS, Machine};
use crate::api::{Diagnostic, Error, ErrorKind, PromiseResolver, Value as HandleValue};
use crate::core::{Dict, EvaluationHalt, Key, Value as CoreValue};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{EvaluationPumpOutcome, EvaluationWaitToken, ExactDemandRoute};
use crate::reflection::{
    EffectRequestSpec, IsolatedEffectSearch, IsolatedSearchPoll, IsolatedTaskHost,
    KeyListRequestWork, PreparationPoll, RequestContext, RequestResult, SpecializationRequestInput,
    SpecializationRequestPoll, SpecializationRequestWork, TaskHalt, TaskSpecialization,
    parse_evaluated_severity,
};

/// Steps the effect's evaluation takes between looks at what it asked for.
const STEP_BUDGET: usize = 4_096;

/// Families of the standard effects, which an [`EffectProvider`] passes
/// through to the machine, or leaves for itself to define
/// (`effect-families-pass-through`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EffectFamilies(u8);

impl EffectFamilies {
    /// No family.
    pub const NONE: Self = Self(0);
    /// `.get path` and `.set path value`, on the machine's local state: a
    /// path's first key names a local, and the empty path names them all,
    /// as a dict.
    pub const STATE: Self = Self(1);
    /// `.heap.*` and `.vol.*`, on the shared heap and its volumes.
    pub const HEAP: Self = Self(1 << 1);
    /// `.promise.*`, on the runtime's promises.
    pub const PROMISES: Self = Self(1 << 2);
    /// `.log severity message`, to the machine's diagnostics output.
    pub const LOG: Self = Self(1 << 3);
    /// `.env path`, from the machine's environment.
    pub const ENV: Self = Self(1 << 4);
    /// Every family.
    pub const ALL: Self =
        Self(Self::STATE.0 | Self::HEAP.0 | Self::PROMISES.0 | Self::LOG.0 | Self::ENV.0);

    /// Whether these families include all of `families`.
    pub const fn contains(self, families: Self) -> bool {
        self.0 & families.0 == families.0
    }
}

impl BitOr for EffectFamilies {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Rust code that answers what an effect a turn runs asks of it
/// ([`Machine::effect_run`]). It is borrowed for one run and receives the
/// machine for each answer, so it holds no runtime owner.
pub trait EffectProvider {
    /// What the provider calls one of its requests.
    type Request: Clone;

    /// The standard families the machine serves; the provider may define
    /// the others' requests itself. All of them, by default.
    fn passes_through(&self) -> EffectFamilies {
        EffectFamilies::ALL
    }

    /// The requests the provider answers: each a name in the effect's API,
    /// its tag, and its arity.
    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>>;

    /// Answers `request`, whose arguments are on top of `m`'s stack, the
    /// last on top: it pops them and leaves what [`Answer`] says. Failing,
    /// `Err(m.fail())`, fails the request within the effect; any other error
    /// ends the run.
    fn answer(&mut self, m: &mut Machine<'_, '_>, request: &Self::Request)
    -> Result<Answer, Error>;
}

/// What a provider's answer leaves on the stack in place of the request's
/// arguments.
#[non_exhaustive]
pub enum Answer {
    /// `args -- result`: the request returns `result`.
    Return,
}

/// A standard request, which the machine serves where its family passes
/// through.
#[derive(Clone, Copy)]
enum Standard {
    Get,
    Set,
    HeapGet,
    HeapSet,
    HeapRewrite,
    HeapVolume,
    VolumeNew,
    VolumeGet,
    VolumeSet,
    VolumeRewrite,
    PromiseNew,
    PromiseStatus,
    PromiseFulfill,
    PromiseBreak,
    Log,
    Env,
}

/// A standard request's family, its name in the effect's API, its arity,
/// and which of its arguments, if any, is a path of keys.
struct Shape {
    family: EffectFamilies,
    name: &'static [&'static str],
    arity: usize,
    path: Option<usize>,
}

impl Standard {
    const ALL: [Self; 16] = [
        Self::Get,
        Self::Set,
        Self::HeapGet,
        Self::HeapSet,
        Self::HeapRewrite,
        Self::HeapVolume,
        Self::VolumeNew,
        Self::VolumeGet,
        Self::VolumeSet,
        Self::VolumeRewrite,
        Self::PromiseNew,
        Self::PromiseStatus,
        Self::PromiseFulfill,
        Self::PromiseBreak,
        Self::Log,
        Self::Env,
    ];

    fn shape(self) -> Shape {
        let shape = |family, name, arity, path| Shape {
            family,
            name,
            arity,
            path,
        };
        use EffectFamilies as F;
        match self {
            Self::Get => shape(F::STATE, &["get"], 1, Some(0)),
            Self::Set => shape(F::STATE, &["set"], 2, Some(0)),
            Self::HeapGet => shape(F::HEAP, &["heap", "get"], 1, Some(0)),
            Self::HeapSet => shape(F::HEAP, &["heap", "set"], 2, Some(0)),
            Self::HeapRewrite => shape(F::HEAP, &["heap", "rewrite"], 2, Some(0)),
            Self::HeapVolume => shape(F::HEAP, &["heap", "vol"], 0, None),
            Self::VolumeNew => shape(F::HEAP, &["vol", "new"], 1, None),
            Self::VolumeGet => shape(F::HEAP, &["vol", "get"], 2, Some(1)),
            Self::VolumeSet => shape(F::HEAP, &["vol", "set"], 3, Some(1)),
            Self::VolumeRewrite => shape(F::HEAP, &["vol", "rewrite"], 3, Some(1)),
            Self::PromiseNew => shape(F::PROMISES, &["promise", "new"], 0, None),
            Self::PromiseStatus => shape(F::PROMISES, &["promise", "status"], 1, None),
            Self::PromiseFulfill => shape(F::PROMISES, &["promise", "fulfill"], 2, None),
            Self::PromiseBreak => shape(F::PROMISES, &["promise", "break"], 2, None),
            Self::Log => shape(F::LOG, &["log"], 2, None),
            Self::Env => shape(F::ENV, &["env"], 1, Some(0)),
        }
    }

    /// The specs of the standard requests in `families`.
    fn specs(families: EffectFamilies) -> impl Iterator<Item = EffectRequestSpec<TurnRequest>> {
        Self::ALL.into_iter().filter_map(move |standard| {
            let shape = standard.shape();
            families.contains(shape.family).then(|| {
                EffectRequestSpec::at_path(
                    shape.name.iter().copied(),
                    ["turn_effects", "request"]
                        .into_iter()
                        .chain(shape.name.iter().copied()),
                    shape.arity,
                    TurnRequest::Standard(standard),
                )
            })
        })
    }
}

/// A request of an effect a turn runs: a standard one, or the provider's,
/// by its index.
#[derive(Clone, Copy)]
enum TurnRequest {
    Standard(Standard),
    Provider(usize),
}

impl TurnRequest {
    /// Which of the request's arguments is a path of keys, which the effect
    /// machine converts before the request parks.
    fn path(self) -> Option<usize> {
        match self {
            Self::Standard(standard) => standard.shape().path,
            Self::Provider(_) => None,
        }
    }
}

/// A request the effect made, which waits for the turn's answer: its
/// arguments, with a path argument converted to `path`.
struct Parked {
    request: TurnRequest,
    path: Vec<Key>,
    arguments: Vec<HandleValue>,
    resolver: PromiseResolver,
}

/// What the turn answered a parked request.
enum Answered {
    Return(HandleValue),
    Fail,
}

/// Where the effect's requests meet the turn: the request parked, and the
/// answer the turn gave it.
#[derive(Default)]
struct Slot {
    parked: Option<Parked>,
    answered: Option<Answered>,
}

type SharedSlot = Arc<Mutex<Slot>>;

/// The effect machine's view of a turn: the standard requests that pass
/// through and the provider's, each of which parks in the slot for the turn
/// to answer.
#[derive(Clone)]
struct TurnEffects {
    requests: Arc<[EffectRequestSpec<TurnRequest>]>,
    slot: SharedSlot,
}

impl TaskSpecialization for TurnEffects {
    type Host = IsolatedTaskHost<()>;
    type Request = TurnRequest;
    type RequestWork = TurnRequestWork;
    type Snapshot = ();
    type Journal = ();

    fn serves_standard_families(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<TurnRequest>> {
        self.requests.to_vec()
    }

    fn start_request(&self, request: TurnRequest, arguments: Vec<HandleValue>) -> TurnRequestWork {
        TurnRequestWork::Start {
            request,
            arguments,
            slot: self.slot.clone(),
        }
    }
}

enum TurnRequestWork {
    Start {
        request: TurnRequest,
        arguments: Vec<HandleValue>,
        slot: SharedSlot,
    },
    /// Converting the request's path argument to keys.
    Path {
        request: TurnRequest,
        arguments: Vec<HandleValue>,
        path: KeyListRequestWork,
        slot: SharedSlot,
    },
    /// Waiting on the promise the turn fulfills once it has answered.
    Parked(SharedSlot),
    Done,
}

impl TurnRequestWork {
    /// Parks the request for the turn, demanding the promise it fulfills
    /// once it has answered.
    fn park(
        &mut self,
        context: &RequestContext<'_, TurnEffects>,
        request: TurnRequest,
        path: Vec<Key>,
        arguments: Vec<HandleValue>,
        slot: SharedSlot,
    ) -> SpecializationRequestPoll {
        let (promised, resolver) = context.values().promise("effect request");
        slot.lock().unwrap_or_else(PoisonError::into_inner).parked = Some(Parked {
            request,
            path,
            arguments,
            resolver,
        });
        *self = Self::Parked(slot);
        SpecializationRequestPoll::Demand(promised)
    }
}

impl SpecializationRequestWork<TurnEffects> for TurnRequestWork {
    fn poll(
        &mut self,
        _specialization: &TurnEffects,
        input: Option<SpecializationRequestInput>,
        context: &mut RequestContext<'_, TurnEffects>,
    ) -> Result<SpecializationRequestPoll, TaskHalt> {
        match std::mem::replace(self, Self::Done) {
            Self::Start {
                request,
                mut arguments,
                slot,
            } => match request.path() {
                Some(index) => {
                    let path = KeyListRequestWork::new(arguments.remove(index));
                    *self = Self::Path {
                        request,
                        arguments,
                        path,
                        slot,
                    };
                    Ok(SpecializationRequestPoll::Continue)
                }
                None => Ok(self.park(context, request, Vec::new(), arguments, slot)),
            },
            Self::Path {
                request,
                arguments,
                mut path,
                slot,
            } => match path.poll(input, context)? {
                PreparationPoll::Progress => {
                    *self = Self::Path {
                        request,
                        arguments,
                        path,
                        slot,
                    };
                    Ok(SpecializationRequestPoll::Continue)
                }
                PreparationPoll::Demand(value) => {
                    *self = Self::Path {
                        request,
                        arguments,
                        path,
                        slot,
                    };
                    Ok(SpecializationRequestPoll::Demand(value))
                }
                PreparationPoll::Ready(path) => {
                    Ok(self.park(context, request, path, arguments, slot))
                }
            },
            Self::Parked(slot) => {
                if let Some(SpecializationRequestInput::Failed(halt)) = input {
                    return Err(halt);
                }
                let answered = slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .answered
                    .take()
                    .expect("the turn answers a request before fulfilling its promise");
                Ok(SpecializationRequestPoll::Complete(match answered {
                    Answered::Return(value) => RequestResult::Return(value),
                    Answered::Fail => RequestResult::Fail,
                }))
            }
            Self::Done => panic!("an answered effect request was polled again"),
        }
    }
}

impl Machine<'_, '_> {
    /// `effect -- result`: runs the effect on top of the stack on this turn's
    /// thread (`effect-families-pass-through`). The machine serves the
    /// standard families that `provider` passes through, as its own methods
    /// do, and `provider` answers its own requests. An effect that fails
    /// fails as [`Self::fail`] does; one whose evaluation waits is pushback.
    ///
    /// Branching, the search family, comes with a later slice of
    /// `api-effect-providers`.
    pub fn effect_run<P: EffectProvider>(&mut self, provider: &mut P) -> Result<(), Error> {
        let effect = self.pop()?;
        let (names, provided): (Vec<_>, Vec<_>) = provider
            .requests()
            .into_iter()
            .enumerate()
            .map(|(index, spec)| {
                let mut name = None;
                let spec = spec.map_request(|request| {
                    name = Some(request);
                    TurnRequest::Provider(index)
                });
                (name.expect("a request spec holds its request"), spec)
            })
            .unzip();
        let requests = Standard::specs(provider.passes_through())
            .chain(provided)
            .collect::<Vec<_>>();
        let slot = SharedSlot::default();
        let effects = TurnEffects {
            requests: requests.into(),
            slot: slot.clone(),
        };
        let values = self.runtime.assembler.values();
        let environment = values.with_access(|scoped| {
            scoped.wrap(crate::core::Value::Dict(crate::core::Dict::new_sync()))
        });
        let host = Arc::new(IsolatedTaskHost::new(&values, environment, ())?);
        let context = self.runtime.assembler.eval_context();
        let mut search =
            IsolatedEffectSearch::new_in_context(&effect, effects, host, context.clone())
                .map_err(|halt| Error::new(halt.to_string()))?;
        let mut route = ExactDemandRoute::default();
        let branches = loop {
            match search.poll(STEP_BUDGET) {
                IsolatedSearchPoll::BudgetExhausted => {}
                IsolatedSearchPoll::Blocked(blocked) => {
                    let parked = slot
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .parked
                        .take();
                    if let Some(Parked {
                        request,
                        path,
                        arguments,
                        resolver,
                    }) = parked
                    {
                        let answered =
                            self.answer_parked(provider, &names, request, path, arguments)?;
                        slot.lock().unwrap_or_else(PoisonError::into_inner).answered =
                            Some(answered);
                        resolver.resolve(values.unit())?;
                        continue;
                    }
                    let Some(wait) = blocked.dependency().cloned() else {
                        return Err(match blocked.error() {
                            Some(halt) => Error::new(halt.to_string()),
                            None => Error::new("the effect blocked on nothing it could wait for"),
                        });
                    };
                    match context.pump_wait_on_route(&wait, STEP_BUDGET, &mut route) {
                        EvaluationPumpOutcome::TargetReady
                        | EvaluationPumpOutcome::Busy
                        | EvaluationPumpOutcome::BudgetExhausted => {}
                        EvaluationPumpOutcome::NoProgress => {
                            return self.note_evaluation(Err(self.awaiting(wait)));
                        }
                    }
                }
                IsolatedSearchPoll::Complete(branches) => break branches,
                IsolatedSearchPoll::Failed(halt) => return Err(Error::new(halt.to_string())),
                IsolatedSearchPoll::Cancelled => {
                    return Err(Error::new("the effect was cancelled"));
                }
            }
        };
        match branches.iter().find_map(|branch| branch.value()) {
            Some(result) => {
                self.push(result.clone());
                Ok(())
            }
            None => Err(self.fail()),
        }
    }

    /// Answers `request` with `arguments` on the stack, serving a standard
    /// request or handing it to `provider`, and reads the answer off the
    /// stack.
    fn answer_parked<P: EffectProvider>(
        &mut self,
        provider: &mut P,
        names: &[P::Request],
        request: TurnRequest,
        path: Vec<Key>,
        arguments: Vec<HandleValue>,
    ) -> Result<Answered, Error> {
        let depth = self.depth();
        let arity = arguments.len();
        for argument in arguments {
            self.push(argument);
        }
        let answer = match request {
            TurnRequest::Standard(standard) => self
                .serve_standard(standard, &path)
                .map(|()| Answer::Return),
            TurnRequest::Provider(index) => provider.answer(self, &names[index]),
        };
        match answer {
            Ok(Answer::Return) => {
                if self.depth() != depth + 1 {
                    self.state.stack.truncate(depth);
                    return Err(Error::new(format!(
                        "an effect provider's return must replace its request's {arity} \
                         arguments with one result"
                    )));
                }
                Ok(Answered::Return(self.pop()?))
            }
            Err(error) if error.kind() == ErrorKind::Backtrack => {
                self.state.stack.truncate(depth);
                Ok(Answered::Fail)
            }
            Err(error) => {
                self.state.stack.truncate(depth);
                Err(error)
            }
        }
    }

    /// Serves a standard request, whose arguments but its path are on the
    /// stack, as this machine's own methods do, leaving its result.
    fn serve_standard(&mut self, standard: Standard, path: &[Key]) -> Result<(), Error> {
        match standard {
            Standard::Get => match path.split_first() {
                None => self.locals_push()?,
                Some((local, rest)) => {
                    self.local_get(local.clone());
                    if !rest.is_empty() {
                        self.dict_get_path(rest.iter().cloned())?;
                    }
                }
            },
            Standard::Set => {
                match path.split_first() {
                    None => self.locals_set()?,
                    Some((local, [])) => self.local_set(local.clone())?,
                    Some((local, rest)) => {
                        self.local_get(local.clone());
                        self.dict_insert_path(rest.iter().cloned())?;
                        self.local_set(local.clone())?;
                    }
                }
                self.unit_push();
            }
            Standard::HeapGet => self.heap_get(path.iter().cloned()),
            Standard::HeapSet => {
                self.heap_set(path.iter().cloned())?;
                self.unit_push();
            }
            Standard::HeapRewrite => {
                self.heap_rewrite(path.iter().cloned())?;
                self.unit_push();
            }
            Standard::HeapVolume => self.heap_volume(),
            Standard::VolumeNew => self.volume_new()?,
            Standard::VolumeGet => self.volume_get(path.iter().cloned())?,
            Standard::VolumeSet => {
                self.volume_set(path.iter().cloned())?;
                self.unit_push();
            }
            Standard::VolumeRewrite => {
                self.volume_rewrite(path.iter().cloned())?;
                self.unit_push();
            }
            Standard::PromiseNew => {
                self.promise_new();
                self.dict_pack(["promise", "resolver"])?;
            }
            Standard::PromiseStatus => {
                let status = self.promise_status()?;
                self.discard(1)?;
                self.atom_push(status.name());
            }
            Standard::PromiseFulfill => {
                self.promise_fulfill()?;
                self.unit_push();
            }
            Standard::PromiseBreak => {
                self.promise_break()?;
                self.unit_push();
            }
            Standard::Log => {
                self.swap()?;
                let severity = self.pop_evaluated(Ok, |severity| {
                    parse_evaluated_severity(&severity).map_err(|halt| Error::new(halt.to_string()))
                })?;
                let message = self.pop()?;
                let values = self.runtime.assembler.values();
                let diagnostic = Diagnostic::from_emission(&values, severity, message)?;
                self.push(diagnostic.transport_value(&values)?);
                self.port_write(DIAGNOSTICS.clone(), 1)?;
                self.unit_push();
            }
            Standard::Env => {
                self.env_get();
                if !path.is_empty() {
                    self.dict_get_path(path.iter().cloned())?;
                }
            }
        }
        Ok(())
    }

    /// Pushes the machine's local state as a dict, each local under its key.
    fn locals_push(&mut self) -> Result<(), Error> {
        let locals = &self.state.locals;
        let dict = self.runtime.assembler.values().with_access(|scoped| {
            let dict = locals
                .iter()
                .try_fold(Dict::new_sync(), |dict, (key, value)| {
                    Ok::<_, Error>(dict.insert(key.clone(), scoped.clone_core(value)?))
                })?;
            Ok::<_, Error>(scoped.wrap(CoreValue::Dict(dict)))
        })?;
        self.push(dict);
        Ok(())
    }

    /// Evaluates the top value to a dict and pops it as the machine's local
    /// state, each of its members a local under its key.
    fn locals_set(&mut self) -> Result<(), Error> {
        let locals = self.peek_core_evaluated(|access, scoped, value| {
            let CoreValue::Dict(dict) = value else {
                return Err(Error::new("the whole of the state must be a dict"));
            };
            Ok(dict
                .iter()
                .map(|(key, value)| (key.clone(), scoped.wrap(access.duplicate_value(value))))
                .collect::<HashMap<_, _>>())
        })?;
        self.pop()?;
        self.state.locals = locals;
        Ok(())
    }

    /// Pushback for an effect whose evaluation waits on `wait`.
    fn awaiting(&self, wait: EvaluationWaitToken) -> Error {
        self.runtime
            .assembler
            .evaluation_error(EvaluationHalt::blocked(CoreWaitToken(wait)))
    }
}

//! Effects a turn runs (`api-effect-providers`,
//! `effect-families-pass-through`): a monadic effect evaluates on the turn's
//! thread, serving its pure effects itself, state and delimited control
//! among them, and each host request it makes returns to the turn. The
//! machine serves the host families that the provider passes through, and
//! the provider answers the rest outside any evaluation, so it may evaluate
//! too. The effect searches as machines do (`alt-cuts-its-first-operand`),
//! and the turn keeps a way back to its state for each alternative the
//! effect sets aside: its cuts are the turn's transactions.

use std::ops::BitOr;
use std::sync::{Arc, Mutex, PoisonError};

use super::{CutCheckpoint, CutRetry, DIAGNOSTICS, Machine, TransactionStart};
use crate::api::{Diagnostic, Error, ErrorKind, PromiseResolver, Value as HandleValue};
use crate::core::{EvaluationHalt, Key};
use crate::core_net::CoreWaitToken;
use crate::evaluation::{EvaluationPumpOutcome, EvaluationWaitToken, ExactDemandRoute};
use crate::reflection::{
    EffectRequestSpec, HostControl, HostedEffectRun, HostedPoll, IsolatedTaskHost,
    KeyListRequestWork, PreparationPoll, RequestContext, RequestResult, SpecializationRequestInput,
    SpecializationRequestPoll, SpecializationRequestWork, TaskHalt, TaskSpecialization,
    parse_evaluated_severity,
};

/// Steps the effect's evaluation takes between looks at what it asked for.
const STEP_BUDGET: usize = 4_096;

/// What the turn keeps for an effect's cuts: the transaction the effect's
/// outermost cut opened, if the turn was in none, with the version it began
/// at; where that cut began; and a way back to each version a branch was set
/// aside at, before the turn answered past it.
#[derive(Default)]
struct EffectCuts {
    owned: Option<(u64, TransactionStart)>,
    entry: Option<u64>,
    checkpoints: Vec<(u64, CutCheckpoint)>,
}

/// Families of the standard host effects, which an [`EffectProvider`] passes
/// through to the machine, or leaves for itself to define
/// (`effect-families-pass-through`). The pure effects, state among them,
/// belong to no family: the effect serves them itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EffectFamilies(u8);

impl EffectFamilies {
    /// No family.
    pub const NONE: Self = Self(0);
    /// `.heap.*` and `.vol.*`, on the shared heap and its volumes.
    pub const HEAP: Self = Self(1);
    /// `.promise.*`, on the runtime's promises.
    pub const PROMISES: Self = Self(1 << 1);
    /// `.log severity message`, to the machine's diagnostics output.
    pub const LOG: Self = Self(1 << 2);
    /// `.env path`, from the machine's environment.
    pub const ENV: Self = Self(1 << 3);
    /// Every family.
    pub const ALL: Self = Self(Self::HEAP.0 | Self::PROMISES.0 | Self::LOG.0 | Self::ENV.0);

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

    /// The host families the machine serves; the provider may define the
    /// others' requests itself. All of them, by default.
    fn passes_through(&self) -> EffectFamilies {
        EffectFamilies::ALL
    }

    /// The requests the provider answers: each a name in the effect's API,
    /// its tag, and its arity.
    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>>;

    /// Answers `request` on a stack of its own, as a commit hook runs:
    /// the effect's state, then the request's arguments, the last on top,
    /// and an empty stash. It leaves `state' result`, as [`Answer`] says, and
    /// the stash empty; the turn's own stack and stash return after. The
    /// state is the effect's, which `.get []` reads, so what the provider
    /// keeps there each branch keeps for itself. Failing, `Err(m.fail())`,
    /// fails the request within the effect; any other error ends the run.
    fn answer(&mut self, m: &mut Machine<'_, '_>, request: &Self::Request)
    -> Result<Answer, Error>;
}

/// What a provider's answer leaves on the stack in place of the effect's
/// state and the request's arguments.
#[non_exhaustive]
pub enum Answer {
    /// `state args -- state' result`: the request returns `result`, and the
    /// effect continues from `state'`.
    Return,
    /// `state args -- state' r1 … rn`: the request returns each result in
    /// turn, as a choice of returns does, the effect continuing from
    /// `state'`. None at all is a failure.
    Alternatives(usize),
}

/// A standard host request, which the machine serves where its family
/// passes through.
#[derive(Clone, Copy)]
enum Standard {
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
    const ALL: [Self; 14] = [
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
/// arguments, with a path argument converted to `path`, and for the
/// provider's own requests, the branch's state.
struct Parked {
    request: TurnRequest,
    state: Option<HandleValue>,
    path: Vec<Key>,
    arguments: Vec<HandleValue>,
    resolver: PromiseResolver,
}

/// What the turn answered a parked request: what the request returns, and
/// for the provider's own requests, the branch's state after it.
enum Answered {
    Return {
        state: Option<HandleValue>,
        result: RequestResult,
    },
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

    fn serves_host_families(&self) -> bool {
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
    /// Evaluating the state the provider answered with, which the branch
    /// continues from once it is a dict.
    Settling(RequestResult),
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
        let state = matches!(request, TurnRequest::Provider(_)).then(|| context.state());
        slot.lock().unwrap_or_else(PoisonError::into_inner).parked = Some(Parked {
            request,
            state,
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
                match answered {
                    Answered::Return {
                        state: Some(state),
                        result,
                    } => {
                        *self = Self::Settling(result);
                        Ok(SpecializationRequestPoll::Demand(state))
                    }
                    Answered::Return {
                        state: None,
                        result,
                    } => Ok(SpecializationRequestPoll::Complete(result)),
                    Answered::Fail => Ok(SpecializationRequestPoll::Complete(RequestResult::Fail)),
                }
            }
            Self::Settling(result) => match input {
                Some(SpecializationRequestInput::Value(state)) => {
                    context.set_state(state)?;
                    Ok(SpecializationRequestPoll::Complete(result))
                }
                Some(SpecializationRequestInput::Failed(halt)) => Err(halt),
                None => panic!("an effect's state settles once it is evaluated"),
            },
            Self::Done => panic!("an answered effect request was polled again"),
        }
    }
}

impl Machine<'_, '_> {
    /// `state effect -- state' result`: runs the effect on top of the stack
    /// on this turn's thread, from `state`, a dict
    /// (`effect-families-pass-through`). The effect serves its pure effects
    /// itself, `.get` and `.set` on its state among them; the machine serves
    /// the host families that `provider` passes through, as its own methods
    /// do; and `provider` answers its own requests.
    ///
    /// The effect searches as machines do (`alt-cuts-its-first-operand`):
    /// `.alt a b` means `.alt (.cut a) b`, and its own cuts scope its
    /// choices. An effect's cut is a transaction of this machine's: the
    /// outermost, which commits or retries as [`Self::cut`]'s does, outside
    /// a cut, and nested within one. A failed alternative leaves nothing of
    /// what the turn did for it. An effect that fails fails as
    /// [`Self::fail`] does; one whose evaluation waits outside a cut of its
    /// own is pushback.
    pub fn effect_run<P: EffectProvider>(&mut self, provider: &mut P) -> Result<(), Error> {
        self.require(2)?;
        let effect = self.pop()?;
        let state = self.pop()?;
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
        let mut run = HostedEffectRun::new_with_state_in_context(
            &effect,
            &state,
            effects,
            host,
            self.runtime.assembler.eval_context(),
        )
        .map_err(|halt| Error::new(halt.to_string()))?;
        let mut cuts = EffectCuts::default();
        let ran = self.drive_effect(&mut run, &mut cuts, provider, &names, &slot);
        if let (Err(_), Some((_, start))) = (&ran, cuts.owned.take()) {
            self.transaction_abort(start);
        }
        let (state, result) = ran?;
        self.push(state);
        self.push(result);
        Ok(())
    }

    /// Runs the effect to its end, answering what it asks and handling
    /// what it tells the turn of its cuts.
    fn drive_effect<P: EffectProvider>(
        &mut self,
        run: &mut HostedEffectRun<TurnEffects>,
        cuts: &mut EffectCuts,
        provider: &mut P,
        names: &[P::Request],
        slot: &SharedSlot,
    ) -> Result<(HandleValue, HandleValue), Error> {
        let values = self.runtime.assembler.values();
        let context = self.runtime.assembler.eval_context();
        let mut route = ExactDemandRoute::default();
        loop {
            match run.poll(STEP_BUDGET) {
                HostedPoll::BudgetExhausted => {}
                HostedPoll::Paused(event) => self.effect_cut_event(run, cuts, event)?,
                HostedPoll::Blocked(blocked) => {
                    let parked = slot
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .parked
                        .take();
                    if let Some(Parked {
                        request,
                        state,
                        path,
                        arguments,
                        resolver,
                    }) = parked
                    {
                        // A way back to the state before the answer, for
                        // the branches set aside at it.
                        if run.needs_checkpoint() && self.cut.is_some() {
                            cuts.checkpoints.push((run.version(), self.checkpoint()));
                        }
                        match self.answer_parked(provider, names, request, state, path, arguments) {
                            Ok(answered) => {
                                slot.lock().unwrap_or_else(PoisonError::into_inner).answered =
                                    Some(answered);
                                resolver.resolve(values.unit())?;
                            }
                            Err(error) if error.kind() == ErrorKind::Retry => {
                                self.effect_pushback(run, cuts, error)?;
                            }
                            Err(error) => return Err(error),
                        }
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
                        // A `.try` within the effect answers a wait
                        // outside the effect's control as its outcome.
                        EvaluationPumpOutcome::NoProgress if run.answer_try_wait() => {}
                        EvaluationPumpOutcome::NoProgress => {
                            let pushback = self.awaiting(wait);
                            let pushback = self
                                .note_evaluation::<()>(Err(pushback))
                                .expect_err("pushback stays an error");
                            self.effect_pushback(run, cuts, pushback)?;
                        }
                    }
                }
                HostedPoll::Complete { value, state } => return Ok((state, value)),
                HostedPoll::Backtracked => return Err(self.fail()),
                HostedPoll::Failed(halt) => return Err(Error::new(halt.to_string())),
                HostedPoll::Cancelled => return Err(Error::new("the effect was cancelled")),
            }
        }
    }

    /// Handles what the effect tells the turn of its cuts.
    fn effect_cut_event(
        &mut self,
        run: &mut HostedEffectRun<TurnEffects>,
        cuts: &mut EffectCuts,
        event: HostControl,
    ) -> Result<(), Error> {
        match event {
            HostControl::CutBegin(entry) => {
                cuts.entry = Some(entry);
                // Outside a cut of the turn's, the effect's opens one.
                if self.cut.is_none() {
                    cuts.owned = Some((entry, self.transaction_begin()));
                }
            }
            HostControl::Restore(version) => self.effect_restore(cuts, version),
            HostControl::CutAbort(entry) => {
                cuts.entry = None;
                match cuts.owned.take() {
                    Some((_, start)) => {
                        cuts.checkpoints.clear();
                        self.transaction_abort(start);
                    }
                    None => self.effect_restore(cuts, entry),
                }
            }
            HostControl::CutCommit { retries } => {
                let entry = cuts.entry.take().expect("a committing cut began before it");
                cuts.checkpoints.retain(|(version, _)| *version < entry);
                match cuts.owned.take() {
                    Some((entry, start)) => match self.transaction_end(start, Ok(())) {
                        Ok(()) => run.resume_commit(true),
                        // A `.try` answers with the conflict, without
                        // running again.
                        Err(CutRetry::Now) if !retries => run.resume_commit(false),
                        Err(CutRetry::Now) => {
                            cuts.entry = Some(entry);
                            cuts.owned = Some((entry, self.transaction_begin()));
                            run.resume_commit(false);
                        }
                        Err(CutRetry::Never(error)) => return Err(error),
                        Err(CutRetry::AfterChange(_) | CutRetry::AfterEvaluation(..)) => {
                            unreachable!("a commit runs again at once or never")
                        }
                    },
                    None => run.resume_commit(true),
                }
            }
        }
        Ok(())
    }

    /// Returns the turn to its state at `version`, as a failed nested cut
    /// returns, where it kept a way back. Outside a cut, what the turn did
    /// stands.
    fn effect_restore(&mut self, cuts: &mut EffectCuts, version: u64) {
        cuts.checkpoints.retain(|(kept, _)| *kept <= version);
        if let Some((kept, checkpoint)) = cuts.checkpoints.last()
            && *kept == version
        {
            self.rollback(checkpoint.clone());
        }
    }

    /// Handles pushback within the effect: within the transaction its own
    /// cut opened, the cut runs again once what it waited on can progress,
    /// as [`Self::cut`] does; otherwise the pushback is the turn's.
    fn effect_pushback(
        &mut self,
        run: &mut HostedEffectRun<TurnEffects>,
        cuts: &mut EffectCuts,
        pushback: Error,
    ) -> Result<(), Error> {
        let Some((entry, start)) = cuts.owned.take() else {
            return Err(pushback);
        };
        cuts.checkpoints.clear();
        if let Err(retry) = self.transaction_end::<()>(start, Err(pushback)) {
            self.transaction_await(retry)?;
        }
        cuts.entry = Some(entry);
        cuts.owned = Some((entry, self.transaction_begin()));
        run.restart_outermost_cut();
        Ok(())
    }

    /// Answers `request` on a stack of its own, as a commit hook runs:
    /// `arguments`, above `state` for the provider's own requests, and an
    /// empty stash. It serves a standard request or hands it to `provider`,
    /// and reads the answer off that stack before the turn's returns.
    fn answer_parked<P: EffectProvider>(
        &mut self,
        provider: &mut P,
        names: &[P::Request],
        request: TurnRequest,
        state: Option<HandleValue>,
        path: Vec<Key>,
        arguments: Vec<HandleValue>,
    ) -> Result<Answered, Error> {
        let arity = arguments.len();
        let stateful = state.is_some();
        let frame = state.into_iter().chain(arguments).collect();
        let stack = std::mem::replace(&mut self.state.stack, frame);
        let stash = std::mem::take(&mut self.state.stash);
        let answer = match request {
            TurnRequest::Standard(standard) => self
                .serve_standard(standard, &path)
                .map(|()| Answer::Return),
            // An answer joins the forks it made, as a cut's body does, so
            // their branches of a cut's transaction merge before it ends.
            TurnRequest::Provider(index) => {
                self.within_scope(|m| provider.answer(m, &names[index]))
            }
        };
        let mut frame = std::mem::replace(&mut self.state.stack, stack);
        let frame_stash = std::mem::replace(&mut self.state.stash, stash);
        let results = match answer {
            Ok(Answer::Return) => 1,
            Ok(Answer::Alternatives(count)) => count,
            Err(error) if error.kind() == ErrorKind::Backtrack => return Ok(Answered::Fail),
            Err(error) => return Err(error),
        };
        if frame.len() != usize::from(stateful) + results || !frame_stash.is_empty() {
            return Err(Error::new(format!(
                "an effect provider's answer must replace the effect's state and its \
                 request's {arity} arguments with a state and its {results} results, and \
                 leave its stash empty"
            )));
        }
        let mut results = frame.split_off(usize::from(stateful));
        let state = frame.pop();
        let result = match answer {
            Ok(Answer::Alternatives(_)) => RequestResult::Alternatives(results),
            _ => RequestResult::Return(results.pop().expect("one result")),
        };
        Ok(Answered::Return { state, result })
    }

    /// Serves a standard request, whose arguments but its path are on the
    /// stack, as this machine's own methods do, leaving its result.
    fn serve_standard(&mut self, standard: Standard, path: &[Key]) -> Result<(), Error> {
        match standard {
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

    /// Pushback for an effect whose evaluation waits on `wait`.
    fn awaiting(&self, wait: EvaluationWaitToken) -> Error {
        self.runtime
            .assembler
            .evaluation_error(EvaluationHalt::blocked(CoreWaitToken(wait)))
    }
}

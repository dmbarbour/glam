//! Effects a turn runs (`api-effect-providers`,
//! `effect-families-pass-through`): a monadic effect evaluates on the turn's
//! thread, and each request it makes returns to the turn, which a provider
//! answers outside any evaluation, so the provider may evaluate too. This
//! first slice serves the provider's own requests, with returns and failure;
//! the standard families and branching through cuts follow.

use std::sync::{Arc, Mutex, PoisonError};

use super::Machine;
use crate::api::{Error, ErrorKind, PromiseResolver, Value as HandleValue};
use crate::core::EvaluationHalt;
use crate::core_net::CoreWaitToken;
use crate::evaluation::{EvaluationPumpOutcome, EvaluationWaitToken, ExactDemandRoute};
use crate::reflection::{
    EffectRequestSpec, IsolatedEffectSearch, IsolatedSearchPoll, IsolatedTaskHost, RequestContext,
    RequestResult, SpecializationRequestInput, SpecializationRequestPoll,
    SpecializationRequestWork, TaskHalt, TaskSpecialization,
};

/// Steps the effect's evaluation takes between looks at what it asked for.
const STEP_BUDGET: usize = 4_096;

/// Rust code that answers what an effect a turn runs asks of it
/// ([`Machine::effect_run`]). It is borrowed for one run and receives the
/// machine for each answer, so it holds no runtime owner.
pub trait EffectProvider {
    /// What the provider calls one of its requests.
    type Request: Clone;

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

/// A request the effect made, which waits for the turn's answer.
struct Parked {
    request: usize,
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

/// The effect machine's view of a provider: its requests by index, each of
/// which parks in the slot for the turn to answer.
#[derive(Clone)]
struct TurnEffects {
    requests: Arc<[EffectRequestSpec<usize>]>,
    slot: SharedSlot,
}

impl TaskSpecialization for TurnEffects {
    type Host = IsolatedTaskHost<()>;
    type Request = usize;
    type RequestWork = TurnRequestWork;
    type Snapshot = ();
    type Journal = ();

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<usize>> {
        self.requests.to_vec()
    }

    fn start_request(&self, request: usize, arguments: Vec<HandleValue>) -> TurnRequestWork {
        TurnRequestWork::Start {
            request,
            arguments,
            slot: self.slot.clone(),
        }
    }
}

enum TurnRequestWork {
    Start {
        request: usize,
        arguments: Vec<HandleValue>,
        slot: SharedSlot,
    },
    /// Waiting on the promise the turn fulfills once it has answered.
    Parked(SharedSlot),
    Done,
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
                arguments,
                slot,
            } => {
                let (promised, resolver) = context.values().promise("effect request");
                slot.lock().unwrap_or_else(PoisonError::into_inner).parked = Some(Parked {
                    request,
                    arguments,
                    resolver,
                });
                *self = Self::Parked(slot);
                Ok(SpecializationRequestPoll::Demand(promised))
            }
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
    /// thread, answering the requests it makes of `provider`
    /// (`effect-families-pass-through`). An effect that fails fails as
    /// [`Self::fail`] does; one whose evaluation waits is pushback.
    ///
    /// This first slice serves the provider's requests and the effects'
    /// pure core; the standard families, and branching through cuts, come
    /// with later slices of `api-effect-providers`.
    pub fn effect_run<P: EffectProvider>(&mut self, provider: &mut P) -> Result<(), Error> {
        let effect = self.pop()?;
        let (names, requests): (Vec<_>, Vec<_>) = provider
            .requests()
            .into_iter()
            .enumerate()
            .map(|(index, spec)| {
                let mut name = None;
                let spec = spec.map_request(|request| {
                    name = Some(request);
                    index
                });
                (name.expect("a request spec holds its request"), spec)
            })
            .unzip();
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
                    if let Some(parked) = parked {
                        let answered =
                            self.answer_parked(provider, &names, parked.request, parked.arguments)?;
                        slot.lock().unwrap_or_else(PoisonError::into_inner).answered =
                            Some(answered);
                        parked.resolver.resolve(values.unit())?;
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

    /// Hands `arguments` to `provider` for its `request` and reads its
    /// answer off the stack.
    fn answer_parked<P: EffectProvider>(
        &mut self,
        provider: &mut P,
        names: &[P::Request],
        request: usize,
        arguments: Vec<HandleValue>,
    ) -> Result<Answered, Error> {
        let depth = self.depth();
        let arity = arguments.len();
        for argument in arguments {
            self.push(argument);
        }
        match provider.answer(self, &names[request]) {
            Ok(Answer::Return) => {
                if self.depth() != depth + 1 {
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
            Err(error) => Err(error),
        }
    }

    /// Pushback for an effect whose evaluation waits on `wait`.
    fn awaiting(&self, wait: EvaluationWaitToken) -> Error {
        self.runtime
            .assembler
            .evaluation_error(EvaluationHalt::blocked(CoreWaitToken(wait)))
    }
}

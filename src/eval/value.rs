use std::sync::Arc;

#[cfg(test)]
use crate::core::ListThunk;
use crate::core::{
    Dict, EvaluatedValue, EvaluationFailure, EvaluationHalt, FixpointComputation, Key, LazySource,
    LazyValue, List, ManagedLazyRoot, ManagedPromiseRoot, PromisedValue, RuntimeValueAccess, Value,
    keys,
};
use crate::core_net::CoreDataKey;
use crate::core_net::CoreWaitToken;
use crate::evaluation::{
    EvalContext, EvaluationMachinePoll, EvaluationPumpOutcome, EvaluationTaskBlock,
    EvaluationTaskMachine, EvaluationWaitPoll, EvaluatorStepContext, WhnfOwnerPoll, WorkDependency,
    poll_lazy_checkpoint, poll_whnf_computation,
};
#[cfg(test)]
use crate::list::ListItem;
use crate::number::Number;

use super::access_machine::{AccessMachine, AccessMachinePoll};
use super::builtin_machine::{BuiltinTaskMachine, BuiltinTaskPoll};
use super::builtins::{NetConstructionMachine, NetConstructionPoll, apply_builtin_in};
use super::lazy_checkpoint::{
    HostCallCheckpointObservation, ManagedLazyCheckpointEdge, ManagedLazyCheckpointKindTag,
};
use super::list_effect_machine::{ListEffectSourceMachine, ListEffectSourcePoll};
use super::net::*;
use super::object_machine::{ObjectFixpointMachine, ObjectFixpointPoll};

pub(crate) fn failure_diagnostic_value_in(
    access: &RuntimeValueAccess<'_>,
    failure: &EvaluationFailure,
) -> Value {
    let emission = match failure.emission_value_in(access) {
        Some(Value::Binary(text)) => {
            crate::diagnostic::text_message(None, String::from_utf8_lossy(text))
        }
        Some(emission @ Value::Dict(_)) => access.duplicate_value(emission),
        Some(other) => {
            return fallback_failure_diagnostic(
                access,
                failure,
                Some(access.duplicate_value(other)),
                failure_contexts_value(access, failure),
            );
        }
        None => crate::diagnostic::text_message(None, failure.to_string()),
    };

    crate::diagnostic::prepend_contexts(
        access.duplicate_value(&emission),
        failure.contexts_in(access),
    )
    .unwrap_or_else(|_| {
        fallback_failure_diagnostic(
            access,
            failure,
            Some(emission),
            failure_contexts_value(access, failure),
        )
    })
}

#[cfg(test)]
pub(crate) fn failure_diagnostic_value(failure: &EvaluationFailure) -> Value {
    let emission = match failure.emission_value() {
        Some(Value::Binary(text)) => {
            crate::diagnostic::text_message(None, String::from_utf8_lossy(text))
        }
        Some(Value::Dict(_)) => failure
            .emission_value()
            .expect("matched failure emission")
            .clone(),
        Some(other) => {
            return fallback_failure_diagnostic_for_test(
                failure,
                Some(other.clone()),
                Value::List(List::from_values(failure.contexts().to_vec())),
            );
        }
        None => crate::diagnostic::text_message(None, failure.to_string()),
    };

    crate::diagnostic::prepend_contexts(emission.clone(), failure.contexts()).unwrap_or_else(|_| {
        fallback_failure_diagnostic_for_test(
            failure,
            Some(emission),
            Value::List(List::from_values(failure.contexts().to_vec())),
        )
    })
}

#[cfg(test)]
pub(crate) fn halt_diagnostic_value(halt: &EvaluationHalt) -> Option<Value> {
    halt.permanent_failure()
        .map(|failure| failure_diagnostic_value(failure))
}

pub(crate) fn evaluation_context_frame_in(
    access: &RuntimeValueAccess<'_>,
    operation: &str,
) -> Value {
    evaluation_context_frame_with_args_in(access, operation, Dict::new_sync())
}

pub(crate) fn evaluation_context_frame_with_args_in(
    _access: &RuntimeValueAccess<'_>,
    operation: &str,
    args: Dict,
) -> Value {
    let operation = Value::Atom(crate::core::Atom::from_key(&Key::binary_from_text(
        operation,
    )));
    let mut detail = Dict::new_sync().insert((*keys::OP).clone(), operation);
    if !args.is_empty() {
        detail = detail.insert((*keys::ARGS).clone(), Value::Dict(args));
    }
    Value::Dict(Dict::new_sync().insert((*keys::EVAL).clone(), Value::Dict(detail)))
}

#[cfg(test)]
pub(crate) fn evaluation_context_frame(operation: &str) -> Value {
    crate::diagnostic::evaluation_context_frame(operation)
}

#[cfg(test)]
pub(crate) fn evaluation_context_frame_with_args(operation: &str, args: Dict) -> Value {
    crate::diagnostic::evaluation_context_frame_with_args(operation, args)
}

fn fallback_failure_diagnostic(
    _access: &RuntimeValueAccess<'_>,
    failure: &EvaluationFailure,
    emission: Option<Value>,
    contexts: Value,
) -> Value {
    let mut message = Dict::new_sync()
        .insert(
            (*keys::TEXT).clone(),
            Value::binary_from_text(&failure.to_string()),
        )
        .insert((*keys::CONTEXT).clone(), contexts);
    if let Some(emission) = emission {
        message = message.insert((*keys::VALUE).clone(), emission);
    }
    Value::Dict(Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message)))
}

#[cfg(test)]
fn fallback_failure_diagnostic_for_test(
    failure: &EvaluationFailure,
    emission: Option<Value>,
    contexts: Value,
) -> Value {
    let mut message = Dict::new_sync()
        .insert(
            (*keys::TEXT).clone(),
            Value::binary_from_text(&failure.to_string()),
        )
        .insert((*keys::CONTEXT).clone(), contexts);
    if let Some(emission) = emission {
        message = message.insert((*keys::VALUE).clone(), emission);
    }
    Value::Dict(Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message)))
}

fn failure_contexts_value(access: &RuntimeValueAccess<'_>, failure: &EvaluationFailure) -> Value {
    Value::List(List::from_values(
        failure
            .contexts_in(access)
            .iter()
            .map(|context| access.duplicate_value(context))
            .collect(),
    ))
}

#[allow(
    dead_code,
    reason = "W8 retains the direct compatibility evaluator for tests until the family migration closes"
)]
pub fn eval_value(context: &EvalContext, value: &Value) -> Result<Value, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| eval_value_in(evaluator, value))
}

pub(crate) fn eval_value_in(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
) -> Result<Value, EvaluationHalt> {
    match value {
        Value::Lazy(lazy) => eval_lazy_in(context, lazy),
        Value::Promised(promise) => eval_promised_in(context, promise),
        other => Ok(other.clone()),
    }
}

enum LazyTaskWork {
    Produce,
    Whnf(super::whnf::WhnfComputation),
    WhnfCheckpoint,
    NetWhnf {
        machine: Box<NetWhnfMachine>,
        failure_context: Option<&'static str>,
    },
    Access(Box<AccessMachine>),
    Builtin(Box<BuiltinTaskMachine>),
    ObjectFixpoint(Box<ObjectFixpointMachine>),
    ListEffect(Box<ListEffectSourceMachine>),
    /// Transient one-shot authority held only by the route which installed an
    /// `Invoking` host-call checkpoint.
    HostCallInvoke,
    HostCallCheckpoint,
    NetConstruction(Box<NetConstructionMachine>),
}

struct LazyTaskMachine {
    context: EvalContext,
    lazy: ManagedLazyRoot,
    work: LazyTaskWork,
}

impl LazyTaskMachine {
    fn work_for_checkpoint_kind(kind: ManagedLazyCheckpointKindTag) -> LazyTaskWork {
        match kind {
            ManagedLazyCheckpointKindTag::Whnf => LazyTaskWork::WhnfCheckpoint,
            ManagedLazyCheckpointKindTag::HostCall => LazyTaskWork::HostCallCheckpoint,
        }
    }

    fn checkpoint_work(&self, context: &EvaluatorStepContext<'_>) -> Option<LazyTaskWork> {
        context.with_value_access(|access| {
            access
                .lazy_root(&self.lazy)
                .checkpoint_snapshot()
                .map(|checkpoint| Self::work_for_checkpoint_kind(checkpoint.kind()))
        })
    }

    fn complete(
        &self,
        context: &EvaluatorStepContext<'_>,
        value: EvaluatedValue,
    ) -> EvaluationMachinePoll {
        let result =
            context.with_value_access(|access| self.lazy.cache(access.values(), Ok(value)));
        match result {
            Ok(value) => EvaluationMachinePoll::Complete(context.root_value(value.into_value())),
            Err(error) => EvaluationMachinePoll::Failed(context.root_failure(error)),
        }
    }

    fn complete_root(
        &self,
        context: &EvaluatorStepContext<'_>,
        value: &crate::runtime::RuntimeValueRoot,
    ) -> EvaluationMachinePoll {
        self.complete(
            context,
            EvaluatedValue::try_from(context.project_root(value))
                .expect("WHNF owner completion must eliminate the outer deferred variant"),
        )
    }

    fn cached_poll(&self, context: &EvaluatorStepContext<'_>) -> EvaluationMachinePoll {
        let result = context.with_value_access(|access| access.lazy_root(&self.lazy).cached());
        match result.expect("a released lazy source must have a terminal cache") {
            Ok(value) => EvaluationMachinePoll::Complete(context.root_value(value.into_value())),
            Err(error) => EvaluationMachinePoll::Failed(context.root_failure(error)),
        }
    }

    fn follow_value(&mut self, value: crate::runtime::RuntimeValueRoot) -> EvaluationMachinePoll {
        self.work = LazyTaskWork::Whnf(super::whnf::WhnfComputation::from_root(value));
        EvaluationMachinePoll::Yielded
    }

    /// Moves an ordinary rooted WHNF computation into the exact checkpoint
    /// edge retained by this lazy. The old registered root remains live until
    /// the managed producer transition has published the same allocation.
    fn publish_whnf_checkpoint(
        &mut self,
        context: &EvaluatorStepContext<'_>,
    ) -> Option<EvaluationMachinePoll> {
        let ready = matches!(
            &self.work,
            LazyTaskWork::Whnf(computation) if computation.source_root().is_none()
        );
        if !ready {
            return None;
        }
        let LazyTaskWork::Whnf(mut computation) =
            std::mem::replace(&mut self.work, LazyTaskWork::Produce)
        else {
            unreachable!("a ready WHNF producer must retain its computation")
        };
        let installed = context.with_value_access(|access| {
            let checkpoint = computation
                .checkpoint_edge_in(&access)
                .expect("a ready WHNF computation must expose its managed checkpoint");
            let lazy = access.lazy_root(&self.lazy);
            match lazy.install_checkpoint(checkpoint) {
                Ok(()) => true,
                Err(_checkpoint) => {
                    if lazy.checkpoint_snapshot().is_some() {
                        true
                    } else {
                        assert!(
                            lazy.cached().is_some(),
                            "a rejected checkpoint must find another checkpoint or a terminal cache"
                        );
                        false
                    }
                }
            }
        });
        drop(computation);
        self.work = LazyTaskWork::WhnfCheckpoint;
        (!installed).then(|| self.cached_poll(context))
    }

    fn poll_whnf_checkpoint(
        &self,
        context: &EvaluatorStepContext<'_>,
        poll_context: &crate::evaluation::EvaluationPollContext,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        match poll_lazy_checkpoint(&self.lazy, poll_context, durable_context, step_budget) {
            WhnfOwnerPoll::Ready(value) => self.complete_root(context, &value),
            WhnfOwnerPoll::Pending(dependency) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: Some(dependency),
                    observed_epoch: None,
                    error: None,
                })
            }
            WhnfOwnerPoll::Yielded => EvaluationMachinePoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => {
                self.fail(context, EvaluationHalt::failure(failure.into_failure()))
            }
            WhnfOwnerPoll::External(_) => {
                unreachable!("W4 external sources retain explicit lazy-task modes")
            }
        }
    }

    fn poll_host_call_checkpoint(
        &mut self,
        context: &EvaluatorStepContext<'_>,
    ) -> EvaluationMachinePoll {
        enum Transition {
            Interrupted,
            Whnf,
            Terminal,
        }

        let transition = context.with_value_access(|access| {
            let lazy = access.lazy_root(&self.lazy);
            let checkpoint = lazy
                .checkpoint_snapshot()
                .expect("host-call route must retain its managed checkpoint");
            match checkpoint.observe_host_call_in(access.values()) {
                HostCallCheckpointObservation::Invoking => Transition::Interrupted,
                HostCallCheckpointObservation::After(Ok(value)) => {
                    let work = super::whnf::RegionalWhnfWork::from_focus(&access, value);
                    let next = ManagedLazyCheckpointEdge::allocate_regional_in(&access, work)
                        .expect("canonical WHNF state must fit its reviewed managed slot");
                    match lazy.replace_checkpoint(next) {
                        Ok(()) => Transition::Whnf,
                        Err(_) => {
                            assert!(
                                lazy.cached().is_some(),
                                "a rejected host-to-WHNF transition must find a terminal cache"
                            );
                            Transition::Terminal
                        }
                    }
                }
                HostCallCheckpointObservation::After(Err(failure)) => {
                    let _ = self.lazy.cache(access.values(), Err(failure));
                    Transition::Terminal
                }
            }
        });

        match transition {
            Transition::Interrupted => self.fail(
                context,
                EvaluationHalt::new(
                    "host callback was interrupted after invocation began; refusing to replay it",
                ),
            ),
            Transition::Whnf => {
                self.work = LazyTaskWork::WhnfCheckpoint;
                EvaluationMachinePoll::Yielded
            }
            Transition::Terminal => self.cached_poll(context),
        }
    }
}

impl EvaluationTaskMachine for LazyTaskMachine {
    fn poll(
        &mut self,
        poll_context: &crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        let durable_context = self.context.clone();
        if matches!(self.work, LazyTaskWork::HostCallInvoke) {
            // Retire the sole invocation permit before entering arbitrary
            // Rust. If the callback unwinds, this route and every later route
            // observe only the non-replayable `Invoking` checkpoint.
            self.work = LazyTaskWork::HostCallCheckpoint;
            let producer = durable_context
                .values()
                .with_runtime_value_access(|access| {
                    let lazy = self
                        .lazy
                        .access(&access)
                        .expect("host-call route and lazy must share one runtime");
                    lazy.checkpoint_snapshot()
                        .and_then(|checkpoint| checkpoint.host_call_producer_in(&access))
                });
            let Some(producer) = producer else {
                // Another terminal publication won before invocation. The
                // ordinary evaluator path below observes that cache.
                return EvaluationMachinePoll::Yielded;
            };
            let outcome = match producer.invoke(durable_context.values()) {
                Ok(value) if value.runtime_id() != durable_context.values().runtime_id() => {
                    Err(Arc::new(EvaluationFailure::message(format!(
                        "host call returned a value from evaluation runtime {}, expected evaluation runtime {}",
                        value.runtime_id().get(),
                        durable_context.values().runtime_id().get()
                    ))))
                }
                Ok(value) => Ok(value),
                Err(failure) => Err(failure),
            };
            durable_context
                .values()
                .with_runtime_value_access(|access| {
                    let lazy = self
                        .lazy
                        .access(&access)
                        .expect("host-call route and lazy must share one runtime");
                    let checkpoint = lazy
                        .checkpoint_snapshot()
                        .expect("invoked host call must retain its checkpoint");
                    let outcome = outcome.map(|value| value.clone_core_with(&access));
                    checkpoint.complete_host_call_in(&access, outcome);
                });
            // The rooted callback result has become a traced managed edge.
            // A later poll performs the host-to-WHNF family handoff.
            return EvaluationMachinePoll::Yielded;
        }
        poll_context.evaluate(&durable_context, |context| {
            if let Some(result) =
                context.with_value_access(|access| access.lazy_root(&self.lazy).cached())
            {
                return match result {
                    Ok(value) => {
                        EvaluationMachinePoll::Complete(context.root_value(value.into_value()))
                    }
                    Err(error) => EvaluationMachinePoll::Failed(context.root_failure(error)),
                };
            }

            if matches!(self.work, LazyTaskWork::Produce) {
                let source = context
                    .with_value_access(|access| access.lazy_root(&self.lazy).source_snapshot());
                if source.is_none() {
                    let Some(work) = self.checkpoint_work(context) else {
                        return self.cached_poll(context);
                    };
                    self.work = work;
                }
                if let Some(source) = source {
                    self.work = match source {
                        LazySource::NetConstruction(effect) => {
                            let effect = context.with_value_access(|access| {
                                access.values().root_runtime_value(
                                    access.values().duplicate_value(effect.as_ref()),
                                )
                            });
                            let machine = match NetConstructionMachine::new(
                                durable_context.clone(),
                                effect,
                            ) {
                                Ok(machine) => machine,
                                Err(error) => return self.fail(context, error),
                            };
                            LazyTaskWork::NetConstruction(Box::new(machine))
                        }
                        LazySource::HostCall(producer) => {
                            let installed = context.with_value_access(|access| {
                                let checkpoint = ManagedLazyCheckpointEdge::allocate_host_call_in(
                                    access.values(),
                                    producer,
                                )
                                .expect("managed host-call state must fit its reviewed slot");
                                access
                                    .lazy_root(&self.lazy)
                                    .install_checkpoint(checkpoint)
                                    .is_ok()
                            });
                            if installed {
                                LazyTaskWork::HostCallInvoke
                            } else if let Some(work) = self.checkpoint_work(context) {
                                work
                            } else {
                                return self.cached_poll(context);
                            }
                        }
                        LazySource::ReflectionTask(computation) => {
                            let (effect, target, completion) = context
                                .with_value_access(|access| {
                                    computation.handoff_roots_in(access.values())
                                });
                            let already_started =
                                completion.producer().is_some() || completion.is_terminal();
                            let reservation = if already_started {
                                None
                            } else {
                                let background = match durable_context.for_runtime_background() {
                                    Ok(background) => background,
                                    Err(error) => {
                                        return self.fail(
                                            context,
                                            EvaluationHalt::new(error.to_string()),
                                        );
                                    }
                                };
                                match background.reserve_reflection_completion_activation(
                                    effect,
                                    target,
                                    completion.clone(),
                                    computation.result_policy(),
                                ) {
                                    Ok(reservation) => Some(reservation),
                                    Err(error) => {
                                        return self.fail(
                                            context,
                                            EvaluationHalt::new(error.to_string()),
                                        );
                                    }
                                }
                            };
                            let installed = context.with_value_access(|access| {
                                let focus = Value::Promised(PromisedValue::from_root(
                                    &completion,
                                    access.values(),
                                ));
                                let work = super::whnf::RegionalWhnfWork::from_focus(&access, focus);
                                let checkpoint =
                                    ManagedLazyCheckpointEdge::allocate_regional_in(&access, work)
                                        .expect(
                                            "canonical reflection WHNF state must fit its reviewed managed slot",
                                        );
                                let lazy = access.lazy_root(&self.lazy);
                                match lazy.install_checkpoint(checkpoint) {
                                    Ok(()) => true,
                                    Err(_checkpoint) => {
                                        if lazy.checkpoint_snapshot().is_some() {
                                            true
                                        } else {
                                            assert!(
                                                lazy.cached().is_some(),
                                                "a rejected reflection checkpoint must find another checkpoint or a terminal cache"
                                            );
                                            false
                                        }
                                    }
                                }
                            });
                            if !installed {
                                drop(reservation);
                                return self.cached_poll(context);
                            }
                            if let Some(reservation) = reservation {
                                context.defer_reflection_activation(reservation);
                            }
                            LazyTaskWork::WhnfCheckpoint
                        }
                        LazySource::Application(application) => {
                            let computation = context.with_value_access(|access| {
                                super::whnf::WhnfComputation::from_application_checkpoint_in(
                                    &access,
                                    application.function().clone(),
                                    application.arguments(),
                                    None,
                                )
                            });
                            LazyTaskWork::Whnf(computation)
                        }
                        LazySource::ComputedFixpoint(fixpoint) => {
                            match fixpoint.as_ref() {
                                FixpointComputation::Function(function) => {
                                    let computation = context.with_value_access(|access| {
                                let marker =
                                    Value::Lazy(LazyValue::from_root(&self.lazy, access.values()));
                                super::whnf::WhnfComputation::from_application_checkpoint_in(
                                    &access,
                                    function.clone(),
                                    std::slice::from_ref(&marker),
                                    None,
                                )
                            });
                                    LazyTaskWork::Whnf(computation)
                                }
                                FixpointComputation::ObjectInstance(spec) => {
                                    let (spec, marker) = context.with_value_access(|access| {
                                        let spec = access.values().root_runtime_value(
                                            access.values().duplicate_value(spec),
                                        );
                                        let marker =
                                            access.values().root_runtime_value(Value::Lazy(
                                                LazyValue::from_root(&self.lazy, access.values()),
                                            ));
                                        (spec, marker)
                                    });
                                    LazyTaskWork::ObjectFixpoint(Box::new(
                                        ObjectFixpointMachine::new(self.lazy.id(), spec, marker),
                                    ))
                                }
                            }
                        }
                        LazySource::FunctionCall {
                            function,
                            arguments,
                        } => LazyTaskWork::NetWhnf {
                            machine: Box::new(context.with_value_access(|access| {
                                NetWhnfMachine::from_function_call(&access, &function, &arguments)
                            })),
                            failure_context: None,
                        },
                        LazySource::NetComputation(net) => {
                            let runtime = context.with_value_access(|access| {
                                net.runtime().duplicate_in(access.values())
                            });
                            let exposed = context.with_value_access(|access| {
                                access.net(&runtime).with(|runtime| runtime.exposed())
                            });
                            LazyTaskWork::NetWhnf {
                                machine: Box::new(NetWhnfMachine::new(
                                    context,
                                    runtime,
                                    exposed,
                                    "lazy net computation",
                                )),
                                failure_context: Some("net_computation"),
                            }
                        }
                        LazySource::ListEffectComputation(recipe) => {
                            LazyTaskWork::ListEffect(Box::new(ListEffectSourceMachine::new(
                                context,
                                self.lazy.id(),
                                &recipe,
                            )))
                        }
                        LazySource::Access { path, arguments }
                            if path.iter().all(|part| matches!(part, CoreDataKey::Key(_))) =>
                        {
                            let keys = path
                                .iter()
                                .map(|part| match part {
                                    CoreDataKey::Key(key) => key.clone(),
                                    CoreDataKey::Index | CoreDataKey::PathIndex => unreachable!(),
                                })
                                .collect::<Vec<_>>();
                            let base = arguments
                                .first()
                                .cloned()
                                .expect("value access must retain its base value");
                            let computation = context.with_value_access(|access| {
                                super::whnf::WhnfComputation::from_static_access_checkpoint_in(
                                    &access,
                                    base,
                                    Arc::from(keys),
                                    Some(self.lazy.id()),
                                )
                            });
                            LazyTaskWork::Whnf(computation)
                        }
                        LazySource::Access { path, arguments } => {
                            let arguments = context.with_value_access(|access| {
                                arguments
                                    .iter()
                                    .map(|value| {
                                        access.values().root_runtime_value(
                                            access.values().duplicate_value(value),
                                        )
                                    })
                                    .collect()
                            });
                            LazyTaskWork::Access(Box::new(AccessMachine::new(
                                self.lazy.id(),
                                path,
                                arguments,
                            )))
                        }
                        LazySource::Builtin(call) if BuiltinTaskMachine::supports(call.builtin) => {
                            let arguments = context.with_value_access(|access| {
                                call.arguments
                                    .iter()
                                    .map(|value| {
                                        access.values().root_runtime_value(
                                            access.values().duplicate_value(value),
                                        )
                                    })
                                    .collect()
                            });
                            LazyTaskWork::Builtin(Box::new(BuiltinTaskMachine::new(
                                call.builtin,
                                arguments,
                            )))
                        }
                        LazySource::Builtin(call) => {
                            let result = context.with_value_access(|access| {
                                let mut arguments =
                                    call.arguments.iter().cloned().collect::<Vec<_>>();
                                let argument = arguments
                                    .pop()
                                    .expect("saturated builtin source must contain an argument");
                                apply_builtin_in(&access, call.builtin, arguments, argument)
                                    .map(|value| access.values().root_runtime_value(value))
                            });
                            match result {
                                Ok(value) => LazyTaskWork::Whnf(
                                    super::whnf::WhnfComputation::from_root(value),
                                ),
                                Err(error) => return self.fail(context, error),
                            }
                        }
                        _ => LazyTaskWork::Whnf(super::whnf::WhnfComputation::from_lazy_source(
                            self.lazy.clone(),
                            durable_context.values().runtime_id(),
                        )),
                    };
                }
                if matches!(
                    self.work,
                    LazyTaskWork::NetConstruction(_) | LazyTaskWork::HostCallInvoke
                ) {
                    return EvaluationMachinePoll::Yielded;
                }
                if let Some(poll) = self.publish_whnf_checkpoint(context) {
                    return poll;
                }
            }

            if matches!(self.work, LazyTaskWork::HostCallCheckpoint) {
                return self.poll_host_call_checkpoint(context);
            }

            if let LazyTaskWork::NetConstruction(machine) = &mut self.work {
                return match machine.poll(poll_context, context, &durable_context, step_budget) {
                    NetConstructionPoll::Ready(value) => self.complete_root(context, &value),
                    NetConstructionPoll::Pending(dependency) => {
                        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                            dependency: Some(dependency),
                            observed_epoch: None,
                            error: None,
                        })
                    }
                    NetConstructionPoll::Yielded => EvaluationMachinePoll::Yielded,
                    NetConstructionPoll::Failed(failure) => {
                        self.fail(context, EvaluationHalt::failure(failure.into_failure()))
                    }
                };
            }

            if let LazyTaskWork::NetWhnf {
                machine,
                failure_context,
            } = &mut self.work
            {
                return match machine.poll(context, step_budget) {
                    Ok(NetWhnfPoll::Ready(value)) => self.follow_value(context.root_value(value)),
                    Ok(NetWhnfPoll::Yielded) => EvaluationMachinePoll::Yielded,
                    Err(error) => {
                        let error = if let Some(operation) = failure_context {
                            context.with_value_access(|access| {
                                error.with_context(
                                    access.values(),
                                    evaluation_context_frame_in(access.values(), operation),
                                )
                            })
                        } else {
                            error
                        };
                        self.fail(context, error)
                    }
                };
            }

            if let LazyTaskWork::Access(machine) = &mut self.work {
                return match machine.poll(poll_context, context, &durable_context, step_budget) {
                    AccessMachinePoll::Ready(value) => self.complete_root(context, &value),
                    AccessMachinePoll::Pending(dependency) => {
                        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                            dependency: Some(dependency),
                            observed_epoch: None,
                            error: None,
                        })
                    }
                    AccessMachinePoll::Yielded => EvaluationMachinePoll::Yielded,
                    AccessMachinePoll::Failed(failure) => {
                        self.fail(context, EvaluationHalt::failure(failure.into_failure()))
                    }
                };
            }

            if let LazyTaskWork::Builtin(machine) = &mut self.work {
                return match machine.poll(poll_context, context, &durable_context, step_budget) {
                    BuiltinTaskPoll::Ready(value) => self.follow_value(value),
                    BuiltinTaskPoll::ScheduleSpark(value) => {
                        EvaluationMachinePoll::ScheduleSpark(value)
                    }
                    BuiltinTaskPoll::Pending(dependency) => {
                        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                            dependency: Some(dependency),
                            observed_epoch: None,
                            error: None,
                        })
                    }
                    BuiltinTaskPoll::Yielded => EvaluationMachinePoll::Yielded,
                    BuiltinTaskPoll::Failed(failure) => {
                        self.fail(context, EvaluationHalt::failure(failure.into_failure()))
                    }
                };
            }

            if let LazyTaskWork::ObjectFixpoint(machine) = &mut self.work {
                return match machine.poll(poll_context, context, &durable_context, step_budget) {
                    ObjectFixpointPoll::Ready(value) => self.complete_root(context, &value),
                    ObjectFixpointPoll::Pending(dependency) => {
                        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                            dependency: Some(dependency),
                            observed_epoch: None,
                            error: None,
                        })
                    }
                    ObjectFixpointPoll::Yielded => EvaluationMachinePoll::Yielded,
                    ObjectFixpointPoll::Failed(failure) => {
                        self.fail(context, EvaluationHalt::failure(failure.into_failure()))
                    }
                };
            }

            if let LazyTaskWork::ListEffect(machine) = &mut self.work {
                return match machine.poll(poll_context, context, &durable_context, step_budget) {
                    ListEffectSourcePoll::Ready(value) => self.complete_root(context, &value),
                    ListEffectSourcePoll::Pending(dependency) => {
                        EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                            dependency: Some(dependency),
                            observed_epoch: None,
                            error: None,
                        })
                    }
                    ListEffectSourcePoll::Yielded => EvaluationMachinePoll::Yielded,
                    ListEffectSourcePoll::Failed(failure) => {
                        self.fail(context, EvaluationHalt::failure(failure.into_failure()))
                    }
                };
            }

            let source_pending = matches!(
                &self.work,
                LazyTaskWork::Whnf(computation) if computation.source_root().is_some()
            );
            if source_pending {
                let source = context
                    .with_value_access(|access| access.lazy_root(&self.lazy).source_snapshot());
                let Some(source) = source else {
                    return self.cached_poll(context);
                };
                match produce_lazy_source_in(context, &source) {
                    Ok(value) => {
                        let LazyTaskWork::Whnf(computation) = &mut self.work else {
                            unreachable!("source work must retain its WHNF computation")
                        };
                        computation.install_source_result(context.root_value(value));
                    }
                    Err(error) => return self.fail(context, error),
                }
                if let Some(poll) = self.publish_whnf_checkpoint(context) {
                    return poll;
                }
            }

            if let Some(poll) = self.publish_whnf_checkpoint(context) {
                return poll;
            }
            let LazyTaskWork::WhnfCheckpoint = self.work else {
                unreachable!("non-producing lazy work must demand a value or construct a net")
            };
            self.poll_whnf_checkpoint(context, poll_context, &durable_context, step_budget)
        })
    }
}

impl LazyTaskMachine {
    fn fail(
        &self,
        context: &EvaluatorStepContext<'_>,
        error: EvaluationHalt,
    ) -> EvaluationMachinePoll {
        if let Some(wait) = error.blocked_on() {
            return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(wait.0)),
                observed_epoch: None,
                error: None,
            });
        }
        if let Some(promise) = error.unassigned_promise_root() {
            let wait = match promise_root_wait(context.context(), promise) {
                Ok(wait) => wait,
                Err(error) => {
                    return EvaluationMachinePoll::Failed(
                        context.root_failure(Arc::new(EvaluationFailure::message(error.as_ref()))),
                    );
                }
            };
            return EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                dependency: Some(WorkDependency::Wait(wait)),
                observed_epoch: None,
                error: None,
            });
        }
        let failure = error.into_permanent_failure();
        let result =
            context.with_value_access(|access| self.lazy.cache(access.values(), Err(failure)));
        match result {
            Ok(value) => EvaluationMachinePoll::Complete(context.root_value(value.into_value())),
            Err(error) => EvaluationMachinePoll::Failed(context.root_failure(error)),
        }
    }
}

struct PromiseFollower {
    context: EvalContext,
    computation: super::whnf::WhnfComputation,
}

impl EvaluationTaskMachine for PromiseFollower {
    fn poll(
        &mut self,
        poll_context: &crate::evaluation::EvaluationPollContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> EvaluationMachinePoll {
        let durable_context = self.context.clone();
        match poll_whnf_computation(
            &mut self.computation,
            poll_context,
            &durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(value) => EvaluationMachinePoll::Complete(value),
            WhnfOwnerPoll::Pending(dependency) => {
                EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
                    dependency: Some(dependency),
                    observed_epoch: None,
                    error: None,
                })
            }
            WhnfOwnerPoll::Yielded => EvaluationMachinePoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => EvaluationMachinePoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("W2 promise follower produced an external {boundary:?} boundary")
            }
        }
    }
}

pub(super) fn promise_wait(
    context: &EvalContext,
    promise: &PromisedValue,
) -> Result<crate::evaluation::EvaluationWaitToken, Arc<str>> {
    context.promise_task(promise, |task_context, promise| {
        Box::new(PromiseFollower {
            computation: crate::eval::whnf::WhnfComputation::from_promise_root(
                task_context.values(),
                &promise,
            ),
            context: task_context,
        })
    })
}

pub(crate) fn promise_root_wait(
    context: &EvalContext,
    promise: &ManagedPromiseRoot,
) -> Result<crate::evaluation::EvaluationWaitToken, Arc<str>> {
    context.promise_root_task(promise, |task_context, promise| {
        Box::new(PromiseFollower {
            computation: super::whnf::WhnfComputation::from_promise_root(
                task_context.values(),
                &promise,
            ),
            context: task_context,
        })
    })
}

#[cfg(test)]
pub(super) fn eval_lazy(context: &EvalContext, lazy: &LazyValue) -> Result<Value, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| eval_lazy_in(evaluator, lazy))
}

pub(super) fn eval_lazy_in(
    context: &EvaluatorStepContext<'_>,
    lazy: &LazyValue,
) -> Result<Value, EvaluationHalt> {
    loop {
        if let Some(result) = context.with_value_access(|access| access.lazy(lazy).cached()) {
            return result
                .map(EvaluatedValue::into_value)
                .map_err(EvaluationHalt::failure);
        }
        let wait = context
            .context()
            .lazy_task(lazy, |task_context, lazy| {
                Box::new(LazyTaskMachine {
                    context: task_context,
                    lazy,
                    work: LazyTaskWork::Produce,
                })
            })
            .map_err(|error| EvaluationHalt::new(error.as_ref()))?;
        if let Some(value) = await_deferred_task(context, wait, "lazy value")? {
            return Ok(value);
        }
    }
}

pub(crate) fn lazy_root_wait(
    context: &EvalContext,
    lazy: &ManagedLazyRoot,
) -> Result<crate::evaluation::EvaluationWaitToken, Arc<str>> {
    context.lazy_root_task(lazy, |task_context, lazy| {
        Box::new(LazyTaskMachine {
            context: task_context,
            lazy,
            work: LazyTaskWork::Produce,
        })
    })
}

fn await_deferred_task(
    context: &EvaluatorStepContext<'_>,
    wait: crate::evaluation::EvaluationWaitToken,
    kind: &str,
) -> Result<Option<Value>, EvaluationHalt> {
    let poll = context.context().poll_wait(&wait);
    if !matches!(&poll, EvaluationWaitPoll::Pending(_)) {
        return deferred_wait_result(context, &wait, kind, poll);
    }
    #[cfg(test)]
    if context.context().pause_deferred_pump(&wait) {
        return Err(EvaluationHalt::blocked(CoreWaitToken(wait)));
    }
    if context.context().runs_scheduled_task() {
        return match context.context().pump_wait(&wait, 256) {
            EvaluationPumpOutcome::TargetReady => {
                deferred_wait_result(context, &wait, kind, context.context().poll_wait(&wait))
            }
            EvaluationPumpOutcome::Busy
            | EvaluationPumpOutcome::NoProgress
            | EvaluationPumpOutcome::BudgetExhausted => {
                Err(EvaluationHalt::blocked(CoreWaitToken(wait)))
            }
        };
    }
    loop {
        match context.context().pump_wait(&wait, 256) {
            EvaluationPumpOutcome::TargetReady => break,
            EvaluationPumpOutcome::Busy if context.context().waits_for_claimed_tasks() => {
                context.context().wait_for_claimed_task(&wait);
            }
            EvaluationPumpOutcome::Busy => {
                return Err(EvaluationHalt::blocked(CoreWaitToken(wait)));
            }
            EvaluationPumpOutcome::NoProgress => {
                if context.context().waits_for_claimed_tasks()
                    && context.context().retry_after_no_progress(&wait)
                {
                    continue;
                }
                return Err(EvaluationHalt::blocked(CoreWaitToken(wait)));
            }
            EvaluationPumpOutcome::BudgetExhausted => {}
        }
    }
    deferred_wait_result(context, &wait, kind, context.context().poll_wait(&wait))
}

fn deferred_wait_result(
    context: &EvaluatorStepContext<'_>,
    wait: &crate::evaluation::EvaluationWaitToken,
    kind: &str,
    poll: EvaluationWaitPoll,
) -> Result<Option<Value>, EvaluationHalt> {
    match poll {
        EvaluationWaitPoll::Complete(value) => Ok(Some(context.project_root(&value))),
        EvaluationWaitPoll::Failed(error) => {
            Err(deferred_task_failure(context.context(), wait, error))
        }
        EvaluationWaitPoll::Pending(wait) => Err(EvaluationHalt::blocked(CoreWaitToken(wait))),
        EvaluationWaitPoll::Cancelled => Err(EvaluationHalt::new(format!(
            "{kind} evaluation was cancelled"
        ))),
        EvaluationWaitPoll::Abandoned => Ok(None),
        EvaluationWaitPoll::Exited => Err(EvaluationHalt::new(format!(
            "{kind} producer exited without a result"
        ))),
        EvaluationWaitPoll::Killed(error) => Err(EvaluationHalt::failure(error.into_failure())),
    }
}

fn deferred_task_failure(
    context: &EvalContext,
    wait: &crate::evaluation::EvaluationWaitToken,
    failure: crate::runtime::RuntimeFailureRoot,
) -> EvaluationHalt {
    context
        .lazy_failure_for_wait(wait)
        .map(EvaluationHalt::failure)
        .unwrap_or_else(|| EvaluationHalt::failure(failure.into_failure()))
}

fn produce_lazy_source_in(
    _context: &EvaluatorStepContext<'_>,
    source: &LazySource,
) -> Result<Value, EvaluationHalt> {
    match source {
        LazySource::Error => Err(EvaluationHalt::new(
            "initialized lazy errors must be returned from their result cache",
        )),
        LazySource::ComputedFixpoint(fixpoint) => match fixpoint.as_ref() {
            FixpointComputation::ObjectInstance(_) => {
                unreachable!("object fixpoints retain one pollable construction owner")
            }
            FixpointComputation::Function(_) => {
                unreachable!("function fixpoints retain typed WHNF application work")
            }
        },
        #[cfg(test)]
        LazySource::SemanticComputation(computation) => computation.evaluate(_context),
        LazySource::ListEffectComputation(_) => {
            unreachable!("list effects retain one pollable source owner")
        }
        #[cfg(test)]
        LazySource::SemanticThunk(thunk) => thunk(_context),
        LazySource::HostCall(_) => {
            unreachable!("a host call must execute outside the evaluator step")
        }
        LazySource::ReflectionTask(_) => {
            unreachable!("reflection tasks retain one pollable source owner")
        }
        LazySource::Access { .. } => {
            unreachable!("access sources retain one pollable access owner")
        }
        LazySource::Application(_) => {
            unreachable!("applications retain typed WHNF application work")
        }
        LazySource::Builtin(_) => {
            unreachable!("builtin sources retain a regional machine or immediate rooted result")
        }
        LazySource::NetConstruction(_) => {
            unreachable!("net construction must retain its pollable effect machine")
        }
        LazySource::NetComputation(_) => {
            unreachable!("net computations retain one pollable net-WHNF owner")
        }
        LazySource::FunctionCall { .. } => {
            unreachable!("function calls retain one pollable net-WHNF owner")
        }
    }
}

fn eval_promised_in(
    context: &EvaluatorStepContext<'_>,
    promise: &PromisedValue,
) -> Result<Value, EvaluationHalt> {
    loop {
        let (assignment, producer, id) = context.with_value_access(|access| {
            let promise = access.promise(promise);
            (promise.assignment(), promise.producer(), promise.id())
        });
        if let Some(assignment) = assignment {
            if assignment.is_err()
                && let Some(producer) = &producer
            {
                producer.acknowledge_propagated_failure();
            }
            let value = assignment.map_err(EvaluationHalt::failure)?;
            if !context.with_value_access(|access| is_deferred_value(access.values(), &value)) {
                return Ok(value);
            }
            let wait = promise_wait(context.context(), promise)
                .map_err(|error| EvaluationHalt::new(error.as_ref()))?;
            if let Some(value) = await_deferred_task(context, wait, "promised value")? {
                return Ok(value);
            }
            continue;
        }
        if let Some(task) = producer {
            if context.context().observes_as_task(task.owner()) {
                return Err(EvaluationHalt::new(format!(
                    "reflection promise {} recursively observed itself in task {}",
                    id.get(),
                    task.owner().get()
                )));
            }
            let wait = promise_wait(context.context(), promise)
                .map_err(|error| EvaluationHalt::new(error.as_ref()))?;
            if let Some(value) = await_deferred_task(context, wait, "promised value")? {
                return Ok(value);
            }
            continue;
        }
        let root = context.with_value_access(|access| promise.root_in(access.values()));
        return Err(EvaluationHalt::unassigned_root(root));
    }
}

pub(super) fn format_name_part(key: &Key) -> String {
    match key {
        Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Key::AbstractGlobalPath(parts) => parts.join("."),
        Key::Atom(atom) => match atom.key() {
            Key::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Key::AbstractGlobalPath(parts) => parts.join("."),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
pub(super) fn force_list_thunk_in(
    context: &EvaluatorStepContext<'_>,
    thunk: &ListThunk,
) -> Result<List, EvaluationHalt> {
    let thunk = context.with_value_access(|access| thunk.duplicate_as_value_in(access.values()));
    match eval_value_in(context, &thunk)? {
        Value::Binary(bytes) => Ok(List::from_bytes(bytes)),
        Value::List(list) => Ok(list),
        other => Err(EvaluationHalt::new(format!(
            "lazy list chunk must evaluate to a list or binary value, got {other:?}"
        ))),
    }
}

#[cfg(test)]
pub(crate) fn pop_list_front(
    context: &EvalContext,
    list: &List,
) -> Result<Option<(Value, List)>, EvaluationHalt> {
    super::with_direct_evaluator(context, |evaluator| pop_list_front_in(evaluator, list))
}

#[cfg(test)]
pub(crate) fn pop_list_front_in(
    context: &EvaluatorStepContext<'_>,
    list: &List,
) -> Result<Option<(Value, List)>, EvaluationHalt> {
    Ok(list
        .try_pop_front_by(
            &mut |value| context.with_value_access(|access| access.values().duplicate_value(value)),
            &mut |thunk| force_list_thunk_in(context, thunk),
        )?
        .map(|(item, tail)| {
            let value = match item {
                ListItem::Byte(byte) => Value::Number(Number::from_u8(byte)),
                ListItem::Value(value) => value,
            };
            (value, tail)
        }))
}

pub(super) fn split_result_value(
    _access: &RuntimeValueAccess<'_>,
    left: Value,
    right: Value,
) -> Value {
    Value::Dict(
        crate::core::Dict::new_sync()
            .insert((*keys::LEFT).clone(), left)
            .insert((*keys::RIGHT).clone(), right),
    )
}

pub(super) fn number_from_evaluated(
    value: EvaluatedValue,
    builtin_name: &str,
) -> Result<Number, EvaluationHalt> {
    let value = value.into_value();
    let Value::Number(number) = value else {
        return Err(EvaluationHalt::new(format!(
            "{builtin_name} builtin requires number values"
        )));
    };
    Ok(number)
}

pub(super) fn index_from_evaluated(
    value: EvaluatedValue,
    builtin_name: &str,
) -> Result<usize, EvaluationHalt> {
    let value = value.into_value();
    let Value::Number(number) = value else {
        return Err(EvaluationHalt::new(format!(
            "{builtin_name} builtin requires number values"
        )));
    };
    number.to_usize_if_integer().ok_or_else(|| {
        EvaluationHalt::new(format!(
            "{builtin_name} builtin requires non-negative integer indices"
        ))
    })
}

pub(super) fn is_deferred_value(_access: &RuntimeValueAccess<'_>, value: &Value) -> bool {
    matches!(value, Value::Lazy(_) | Value::Promised(_))
}

pub(super) fn is_error_lazy_value(access: &RuntimeValueAccess<'_>, value: &Value) -> bool {
    matches!(value, Value::Lazy(lazy)
        if lazy.access(access).cached()
            .is_some_and(|result| result.is_err()))
}

pub(super) fn is_undefined_dict_value(_access: &RuntimeValueAccess<'_>, value: &Value) -> bool {
    matches!(value, Value::Dict(dict) if dict.is_empty())
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    fn assert_poll_spanning_owner_inventory(
        work: &LazyTaskWork,
        lazy: &LazyTaskMachine,
        promise: &PromiseFollower,
    ) {
        match work {
            LazyTaskWork::Produce => {}
            LazyTaskWork::Whnf(computation) => {
                let _: &crate::eval::whnf::WhnfComputation = computation;
            }
            LazyTaskWork::WhnfCheckpoint => {}
            LazyTaskWork::NetWhnf { machine, .. } => {
                let _: &NetWhnfMachine = machine;
            }
            LazyTaskWork::Access(machine) => {
                let _: &AccessMachine = machine;
            }
            LazyTaskWork::Builtin(machine) => {
                let _: &BuiltinTaskMachine = machine;
            }
            LazyTaskWork::ObjectFixpoint(machine) => {
                let _: &ObjectFixpointMachine = machine;
            }
            LazyTaskWork::ListEffect(machine) => {
                let _: &ListEffectSourceMachine = machine;
            }
            LazyTaskWork::HostCallInvoke | LazyTaskWork::HostCallCheckpoint => {}
            LazyTaskWork::NetConstruction(machine) => {
                let _: &NetConstructionMachine = machine;
            }
        }

        let LazyTaskMachine {
            context,
            lazy: lazy_root,
            work,
        } = lazy;
        let _: &EvalContext = context;
        let _: &ManagedLazyRoot = lazy_root;
        let _: &LazyTaskWork = work;

        let PromiseFollower {
            context,
            computation,
        } = promise;
        let _: &EvalContext = context;
        let _: &crate::eval::whnf::WhnfComputation = computation;
    }

    #[test]
    fn poll_spanning_evaluator_state_uses_canonical_owners() {
        let _: fn(&LazyTaskWork, &LazyTaskMachine, &PromiseFollower) =
            assert_poll_spanning_owner_inventory;
    }

    #[test]
    fn promise_follower_yields_from_its_retained_whnf_checkpoint() {
        let context = EvalContext::standalone();
        let promise = PromisedValue::new(context.values(), "resumable promise follower");
        let promise_root = context
            .values()
            .with_runtime_value_access(|access| promise.root_in(&access));
        let mut follower = PromiseFollower {
            context: (*context).clone(),
            computation: crate::eval::whnf::WhnfComputation::from_promise_root(
                context.values(),
                &promise_root,
            ),
        };
        let poll_context = crate::evaluation::EvaluationPollContext::for_context(&context);

        let pending = follower.poll(
            &poll_context,
            &mut crate::evaluation::EvaluationStepBudget::new(1),
        );
        let EvaluationMachinePoll::Blocked(EvaluationTaskBlock {
            dependency: Some(WorkDependency::Promise(dependency)),
            ..
        }) = pending
        else {
            panic!("an unassigned follower must retain the exact promise dependency")
        };
        assert_eq!(dependency.runtime_id(), promise_root.runtime_id());
        assert_eq!(dependency.id(), promise_root.id());

        crate::core::set_test_promise(context.values(), &promise, Value::Number(73.into()))
            .expect("the follower promise should accept one assignment");
        assert!(matches!(
            follower.poll(
                &poll_context,
                &mut crate::evaluation::EvaluationStepBudget::new(1)
            ),
            EvaluationMachinePoll::Yielded
        ));
        let EvaluationMachinePoll::Complete(value) = follower.poll(
            &poll_context,
            &mut crate::evaluation::EvaluationStepBudget::new(1),
        ) else {
            panic!("the yielded follower must resume from the assigned value")
        };
        assert_eq!(value.clone_core_for_test(), Value::Number(73.into()));
    }
}

#[cfg(test)]
#[path = "value/tests/w3a.rs"]
mod w3a_tests;

#[cfg(test)]
#[path = "value/tests/w4.rs"]
mod w4_tests;

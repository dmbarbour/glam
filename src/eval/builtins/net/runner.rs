//! Pure interpretation of construction effects against builder state.
//!
//! This is the state-threading counterpart of `ListEffectComputation::Run`:
//! demand an `eff` value, apply its handler to the private builder API and the
//! current state, then require the ordinary list of builder outcomes.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{EvaluationFailure, LazyId, Value};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};

use super::super::super::effect_machine::effect_function_in;
use super::super::super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::super::super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place,
    reduce_semantic_shell,
};
use super::builder::{decode_outcome, initial_builder_state, private_builder_api};
use super::identity::{ConstructionBrand, decode_construction_port};
use super::netlist::{encode_selected_netlist, interaction_net_from_netlist_in};

pub(in crate::eval) enum RegionalBuilderEffectPoll {
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

enum RegionalBuilderEffectPhase {
    Effect(RegionalWhnfWork),
    Application(RegionalWhnfWork),
}

/// Retained interpretation of one source effect at one builder state.
pub(in crate::eval) struct RegionalBuilderEffectRunner {
    state: Option<Value>,
    source_owner: LazyId,
    phase: RegionalBuilderEffectPhase,
}

impl RegionalBuilderEffectRunner {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        effect: &Value,
        state: &Value,
    ) -> Self {
        Self {
            state: Some(access.values().duplicate_value(state)),
            source_owner,
            phase: RegionalBuilderEffectPhase::Effect(
                RegionalWhnfWork::from_focus(access, access.values().duplicate_value(effect))
                    .with_source_owner(source_owner),
            ),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuilderEffectPoll {
        match &mut self.phase {
            RegionalBuilderEffectPhase::Effect(demand) => {
                let effect = match drive(demand, access, step_budget) {
                    Drive::Ready(value) => value,
                    Drive::Boundary(request) => {
                        return RegionalBuilderEffectPoll::Boundary(request);
                    }
                    Drive::Yielded => return RegionalBuilderEffectPoll::Yielded,
                    Drive::Failed(failure) => {
                        return RegionalBuilderEffectPoll::Failed(failure);
                    }
                };
                let function =
                    match effect_function_in(access, &effect, "interaction-net builder effect") {
                        Ok(function) => function,
                        Err(error) => {
                            return RegionalBuilderEffectPoll::Failed(
                                error.into_permanent_failure(),
                            );
                        }
                    };
                let state = self
                    .state
                    .take()
                    .expect("builder effect runner must retain its input state");
                self.phase = RegionalBuilderEffectPhase::Application(
                    RegionalWhnfWork::from_application_checkpoint_in(
                        access,
                        function,
                        &[private_builder_api(access.values()), state],
                        Some(self.source_owner),
                    ),
                );
                RegionalBuilderEffectPoll::Yielded
            }
            RegionalBuilderEffectPhase::Application(demand) => {
                match drive(demand, access, step_budget) {
                    Drive::Ready(value) if matches!(value, Value::List(_)) => {
                        RegionalBuilderEffectPoll::Ready(value)
                    }
                    Drive::Ready(value) => RegionalBuilderEffectPoll::Failed(Arc::new(
                        EvaluationFailure::message(format!(
                            "interaction-net builder effect expected a result list, got {value:?}"
                        )),
                    )),
                    Drive::Boundary(request) => RegionalBuilderEffectPoll::Boundary(request),
                    Drive::Yielded => RegionalBuilderEffectPoll::Yielded,
                    Drive::Failed(failure) => RegionalBuilderEffectPoll::Failed(failure),
                }
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(state) = &self.state {
            crate::core::trace_compatibility_value_managed_edges(state, visitor);
        }
        match &self.phase {
            RegionalBuilderEffectPhase::Effect(demand)
            | RegionalBuilderEffectPhase::Application(demand) => {
                demand.trace_managed_edges(visitor);
            }
        }
    }
}

enum Drive {
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

fn drive(
    demand: &mut RegionalWhnfWork,
    access: &EvaluationValueAccess<'_>,
    step_budget: &mut EvaluationStepBudget,
) -> Drive {
    match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => Drive::Ready(value),
        RegionalWhnfStatus::Boundary(request) => Drive::Boundary(request),
        RegionalWhnfStatus::Yielded => Drive::Yielded,
        RegionalWhnfStatus::Failed(failure) => Drive::Failed(failure),
    }
}

pub(in crate::eval) enum RegionalNetConstructionPoll {
    Ready(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

enum ConstructionPhase {
    Build(RegionalBuilderEffectRunner),
    First(RegionalListFront),
    Second {
        first: Value,
        front: RegionalListFront,
    },
    Exposed {
        state: Value,
        demand: RegionalWhnfWork,
    },
}

/// The single retained path from a source effect to one replayed net.
pub(in crate::eval) struct RegionalNetConstruction {
    brand: Arc<ConstructionBrand>,
    source_owner: LazyId,
    phase: ConstructionPhase,
}

impl RegionalNetConstruction {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        effect: &Value,
    ) -> Self {
        let brand = Arc::new(ConstructionBrand::default());
        let state = initial_builder_state(access.values(), &brand);
        Self {
            brand,
            source_owner,
            phase: ConstructionPhase::Build(RegionalBuilderEffectRunner::new_in(
                access,
                source_owner,
                effect,
                &state,
            )),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalNetConstructionPoll {
        match &mut self.phase {
            ConstructionPhase::Build(runner) => match runner.poll_in(access, step_budget) {
                RegionalBuilderEffectPoll::Ready(results) => {
                    self.phase = ConstructionPhase::First(RegionalListFront::new_in(
                        access,
                        results,
                        Some(self.source_owner),
                    ));
                    RegionalNetConstructionPoll::Yielded
                }
                RegionalBuilderEffectPoll::Boundary(request) => {
                    RegionalNetConstructionPoll::Boundary(request)
                }
                RegionalBuilderEffectPoll::Yielded => RegionalNetConstructionPoll::Yielded,
                RegionalBuilderEffectPoll::Failed(failure) => {
                    RegionalNetConstructionPoll::Failed(failure)
                }
            },
            ConstructionPhase::First(front) => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some((first, tail))) => {
                    self.phase = ConstructionPhase::Second {
                        first,
                        front: RegionalListFront::new_in(access, tail, Some(self.source_owner)),
                    };
                    RegionalNetConstructionPoll::Yielded
                }
                RegionalListFrontPoll::Ready(None) => {
                    RegionalNetConstructionPoll::Failed(Arc::new(EvaluationFailure::message(
                        "interaction-net construction produced no successful result",
                    )))
                }
                RegionalListFrontPoll::Boundary(request) => {
                    RegionalNetConstructionPoll::Boundary(request)
                }
                RegionalListFrontPoll::Yielded => RegionalNetConstructionPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => {
                    RegionalNetConstructionPoll::Failed(failure)
                }
            },
            ConstructionPhase::Second { first, front } => {
                match front.poll_in(access, step_budget) {
                    RegionalListFrontPoll::Ready(Some(_)) => {
                        RegionalNetConstructionPoll::Failed(Arc::new(EvaluationFailure::message(
                            "interaction-net construction produced multiple results; use `.cut` to select one",
                        )))
                    }
                    RegionalListFrontPoll::Ready(None) => {
                        let [exposed, state] = match decode_outcome(access.values(), first) {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                return RegionalNetConstructionPoll::Failed(
                                    error.into_permanent_failure(),
                                );
                            }
                        };
                        self.phase = ConstructionPhase::Exposed {
                            state,
                            demand: RegionalWhnfWork::from_focus(access, exposed)
                                .with_source_owner(self.source_owner),
                        };
                        RegionalNetConstructionPoll::Yielded
                    }
                    RegionalListFrontPoll::Boundary(request) => {
                        RegionalNetConstructionPoll::Boundary(request)
                    }
                    RegionalListFrontPoll::Yielded => RegionalNetConstructionPoll::Yielded,
                    RegionalListFrontPoll::Failed(failure) => {
                        RegionalNetConstructionPoll::Failed(failure)
                    }
                }
            }
            ConstructionPhase::Exposed { state, demand } => {
                let exposed = match drive(demand, access, step_budget) {
                    Drive::Ready(value) => value,
                    Drive::Boundary(request) => {
                        return RegionalNetConstructionPoll::Boundary(request);
                    }
                    Drive::Yielded => return RegionalNetConstructionPoll::Yielded,
                    Drive::Failed(failure) => {
                        return RegionalNetConstructionPoll::Failed(failure);
                    }
                };
                let Value::Opaque(port) = exposed else {
                    return RegionalNetConstructionPoll::Failed(Arc::new(
                        EvaluationFailure::message(
                            "interaction-net operation requires a construction port",
                        ),
                    ));
                };
                let port =
                    match decode_construction_port(access.values().values(), &port, &self.brand) {
                        Ok(port) => port,
                        Err(error) => {
                            return RegionalNetConstructionPoll::Failed(
                                error.into_permanent_failure(),
                            );
                        }
                    };
                let selected = encode_selected_netlist(
                    access.values(),
                    access.values().duplicate_value(state),
                    &self.brand,
                    port,
                );
                match interaction_net_from_netlist_in(access.values(), &selected) {
                    Ok(net) => RegionalNetConstructionPoll::Ready(net),
                    Err(error) => {
                        RegionalNetConstructionPoll::Failed(error.into_permanent_failure())
                    }
                }
            }
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.phase {
            ConstructionPhase::Build(runner) => runner.trace_managed_edges(visitor),
            ConstructionPhase::First(front) => front.trace_managed_edges(visitor),
            ConstructionPhase::Second { first, front } => {
                crate::core::trace_compatibility_value_managed_edges(first, visitor);
                front.trace_managed_edges(visitor);
            }
            ConstructionPhase::Exposed { state, demand } => {
                crate::core::trace_compatibility_value_managed_edges(state, visitor);
                demand.trace_managed_edges(visitor);
            }
        }
    }
}

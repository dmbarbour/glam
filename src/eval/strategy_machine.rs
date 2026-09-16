//! Resumable demand shared by `seq` and best-effort spark workers.

use crate::core::{EvaluatedValue, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluationStepBudget, EvaluatorStepContext, WhnfOwnerPoll,
    WorkDependency, poll_whnf_computation,
};
use crate::runtime::{EvaluationRuntimeId, RuntimeFailureRoot, RuntimeValueRoot};

use super::whnf::WhnfComputation;

pub(crate) enum StrategyDemandPoll {
    Ready,
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

enum StrategyDemandPhase {
    Value,
    Metadata,
    Complete,
}

/// Demands one ordinary value and at most one hidden metadata value.
pub(crate) struct StrategyDemandMachine {
    source: RuntimeValueRoot,
    demand: WhnfComputation,
    phase: StrategyDemandPhase,
}

impl StrategyDemandMachine {
    pub(crate) fn new(source: RuntimeValueRoot) -> Self {
        Self {
            demand: WhnfComputation::from_root(source.clone()),
            source,
            phase: StrategyDemandPhase::Value,
        }
    }

    pub(crate) fn runtime_id(&self) -> EvaluationRuntimeId {
        self.source.runtime_id()
    }

    pub(crate) fn source(&self) -> &RuntimeValueRoot {
        &self.source
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut EvaluationStepBudget,
    ) -> StrategyDemandPoll {
        if matches!(self.phase, StrategyDemandPhase::Complete) {
            return StrategyDemandPoll::Ready;
        }
        let ready = match poll_whnf_computation(
            &mut self.demand,
            poll_context,
            durable_context,
            step_budget,
        ) {
            WhnfOwnerPoll::Ready(value) => value,
            WhnfOwnerPoll::Pending(dependency) => return StrategyDemandPoll::Pending(dependency),
            WhnfOwnerPoll::Yielded => return StrategyDemandPoll::Yielded,
            WhnfOwnerPoll::Failed(failure) => return StrategyDemandPoll::Failed(failure),
            WhnfOwnerPoll::External(boundary) => {
                unreachable!("strategy demand produced an external {boundary:?} boundary")
            }
        };

        if matches!(self.phase, StrategyDemandPhase::Value) {
            let metadata = context.with_value_access(|access| {
                EvaluatedValue::try_from(access.clone_root(&ready))
                    .expect("strategy demand must produce WHNF")
                    .into_value()
                    .associated_metadata()
                    .map(|value| access.values().root_runtime_value(value))
            });
            if let Some(metadata) = metadata {
                self.demand = WhnfComputation::from_root(metadata);
                self.phase = StrategyDemandPhase::Metadata;
                return StrategyDemandPoll::Yielded;
            }
        }

        self.phase = StrategyDemandPhase::Complete;
        StrategyDemandPoll::Ready
    }

    pub(crate) fn is_useful_spark(&self, context: &EvaluatorStepContext<'_>) -> bool {
        context.with_value_access(|access| {
            matches!(
                access.clone_root(&self.source),
                Value::Lazy(_) | Value::Promised(_) | Value::Metadata(_)
            )
        })
    }
}

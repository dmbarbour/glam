//! Lazy interpretation and checked replay of the `interaction_net` effect.

use std::num::NonZeroU64;
use std::sync::Arc;

use crate::api::Value as PublicValue;
use crate::core::{CoreValueFactory, Dict, List, NetValue, OpaqueValue, Value};
use crate::core_net::{CoreSpecialization, CoreWaitToken};
use crate::evaluation::EvalContext;
use crate::interaction_net::{NetBuilder, Port};
use crate::reflection::{
    EffectRequestSpec, IsolatedEffectSearch, IsolatedSearchPoll, IsolatedTaskHost, RequestContext,
    RequestResult, TaskHalt, TaskSpecialization, task_eval_error,
};

use super::super::super::{EvaluationHalt, eval_index_number, eval_value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ConstructionPortId(NonZeroU64);

impl ConstructionPortId {
    fn index(self) -> Result<usize, EvaluationHalt> {
        usize::try_from(self.0.get() - 1)
            .map_err(|_| EvaluationHalt::new("interaction-net port index exceeds this target"))
    }
}

struct ConstructionBrand;

struct ConstructionPort {
    brand: Arc<ConstructionBrand>,
    id: ConstructionPortId,
}

enum ConstructionOp {
    Bind {
        ports: [ConstructionPortId; 3],
    },
    Copy {
        ports: Arc<[ConstructionPortId]>,
    },
    Data {
        port: ConstructionPortId,
        value: Value,
    },
    Wire {
        left: ConstructionPortId,
        right: ConstructionPortId,
    },
}

struct ConstructionLog {
    previous: Option<Arc<ConstructionLog>>,
    operation: ConstructionOp,
}

/// A persistent write-only journal. Alternative branches share their complete
/// prefix and allocate subsequent logical ports independently.
#[derive(Clone)]
struct ConstructionJournal {
    tail: Option<Arc<ConstructionLog>>,
    next_port: u64,
}

impl Default for ConstructionJournal {
    fn default() -> Self {
        Self {
            tail: None,
            next_port: 1,
        }
    }
}

impl ConstructionJournal {
    fn append(&mut self, operation: ConstructionOp) {
        self.tail = Some(Arc::new(ConstructionLog {
            previous: self.tail.clone(),
            operation,
        }));
    }

    fn allocate_ports(&mut self, count: usize) -> Result<Vec<ConstructionPortId>, TaskHalt> {
        let count = u64::try_from(count)
            .map_err(|_| TaskHalt::new("interaction-net port count exceeds u64"))?;
        let end = self
            .next_port
            .checked_add(count)
            .ok_or_else(|| TaskHalt::new("interaction-net port IDs exhausted"))?;
        let capacity = usize::try_from(count)
            .map_err(|_| TaskHalt::new("interaction-net port count exceeds this target"))?;
        let mut ports = Vec::new();
        ports
            .try_reserve_exact(capacity)
            .map_err(|_| TaskHalt::new("interaction-net port allocation is too large"))?;
        for id in self.next_port..end {
            ports.push(ConstructionPortId(
                NonZeroU64::new(id).expect("construction port IDs start at one"),
            ));
        }
        self.next_port = end;
        Ok(ports)
    }

    fn operations(&self) -> Vec<&ConstructionOp> {
        let mut operations = Vec::new();
        let mut current = self.tail.as_deref();
        while let Some(entry) = current {
            operations.push(&entry.operation);
            current = entry.previous.as_deref();
        }
        operations.reverse();
        operations
    }
}

#[derive(Clone, Copy)]
enum InteractionNetRequest {
    Bind,
    Copy,
    Data,
    Wire,
}

#[derive(Clone)]
struct InteractionNetEffects {
    brand: Arc<ConstructionBrand>,
}

impl TaskSpecialization for InteractionNetEffects {
    type Host = ConstructionHost;
    type Request = InteractionNetRequest;
    type Snapshot = ();
    type Journal = ConstructionJournal;

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        [
            EffectRequestSpec::new(
                "bind",
                ["interaction_net_runtime", "v0", "request", "bind"],
                0,
                InteractionNetRequest::Bind,
            ),
            EffectRequestSpec::new(
                "copy",
                ["interaction_net_runtime", "v0", "request", "copy"],
                1,
                InteractionNetRequest::Copy,
            ),
            EffectRequestSpec::new(
                "data",
                ["interaction_net_runtime", "v0", "request", "data"],
                1,
                InteractionNetRequest::Data,
            ),
            EffectRequestSpec::new(
                "wire",
                ["interaction_net_runtime", "v0", "request", "wire"],
                2,
                InteractionNetRequest::Wire,
            ),
        ]
        .into()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<PublicValue>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskHalt> {
        match request {
            InteractionNetRequest::Bind => construct_bind(arguments, context, &self.brand),
            InteractionNetRequest::Copy => construct_copy(arguments, context, &self.brand),
            InteractionNetRequest::Data => construct_data(arguments, context, &self.brand),
            InteractionNetRequest::Wire => construct_wire(arguments, context, &self.brand),
        }
    }
}

type ConstructionHost = IsolatedTaskHost<()>;

pub(in crate::eval) struct NetConstructionMachine {
    brand: Arc<ConstructionBrand>,
    search: IsolatedEffectSearch<InteractionNetEffects>,
}

impl NetConstructionMachine {
    pub(in crate::eval) fn new(
        context: EvalContext,
        effect: Value,
    ) -> Result<Self, EvaluationHalt> {
        let brand = Arc::new(ConstructionBrand);
        let specialization = InteractionNetEffects {
            brand: brand.clone(),
        };
        let effect = PublicValue::from_core(context.values(), effect);
        let search = IsolatedEffectSearch::new_in_context(
            &effect,
            specialization,
            Arc::new(ConstructionHost::new_core(
                context.values().clone(),
                PublicValue::from_core(context.values(), Value::Dict(Dict::new_sync())),
                (),
            )),
            context,
        )
        .map_err(TaskHalt::into_evaluation_halt)?;
        Ok(Self { brand, search })
    }

    /// Advances construction without losing the freer machine or its journal.
    /// `Ok(None)` is a cooperative yield; dependencies are returned through
    /// `EvaluationHalt::Blocked` and recorded by the owning lazy task.
    pub(in crate::eval) fn poll(
        &mut self,
        context: &EvalContext,
        step_budget: usize,
    ) -> Result<Option<Value>, EvaluationHalt> {
        match self.search.poll(step_budget.max(1)) {
            IsolatedSearchPoll::Yielded => Ok(None),
            IsolatedSearchPoll::Blocked(blocked) => {
                if let Some(dependency) = blocked.dependency().cloned() {
                    return Err(EvaluationHalt::blocked(CoreWaitToken(dependency)));
                }
                match blocked.error() {
                    Some(halt) => Err(halt
                        .clone()
                        .with_context(PublicValue::from_core(
                            context.values(),
                            net_construction_context(),
                        ))
                        .into_evaluation_halt()),
                    None => Err(EvaluationHalt::new(
                        "interaction-net construction became blocked without a dependency or mutable host observation",
                    )),
                }
            }
            IsolatedSearchPoll::Complete(branches) => {
                let mut successes = branches.iter().filter(|branch| branch.value().is_some());
                let Some(branch) = successes.next() else {
                    return Err(EvaluationHalt::new(
                        "interaction-net construction produced no successful result",
                    ));
                };
                if successes.next().is_some() {
                    return Err(EvaluationHalt::new(
                        "interaction-net construction produced multiple results; use `.cut` to select one",
                    ));
                }
                let exposed = construction_port(
                    context,
                    branch
                        .value()
                        .expect("successful branch checked above")
                        .as_core(),
                    &self.brand,
                )?;
                replay(branch.journal(), exposed).map(Some)
            }
            IsolatedSearchPoll::Failed(halt) => Err(halt
                .with_context(PublicValue::from_core(
                    context.values(),
                    net_construction_context(),
                ))
                .into_evaluation_halt()),
            IsolatedSearchPoll::Cancelled => Err(EvaluationHalt::new(
                "interaction-net construction was cancelled",
            )),
        }
    }
}

fn net_construction_context() -> Value {
    crate::eval::evaluation_context_frame("net_construction")
}

fn construct_bind(
    arguments: Vec<PublicValue>,
    context: &mut RequestContext<'_, InteractionNetEffects>,
    brand: &Arc<ConstructionBrand>,
) -> Result<RequestResult, TaskHalt> {
    let []: [PublicValue; 0] = exact(arguments, "`.bind`")?;
    let mut transaction = construction_transaction(context)?;
    let (_, journal) = transaction.parts();
    let ports: [ConstructionPortId; 3] = journal
        .allocate_ports(3)?
        .try_into()
        .expect("three allocated ports must form a triple");
    journal.append(ConstructionOp::Bind { ports });
    Ok(RequestResult::Return(port_list(
        context.eval_context().values(),
        brand,
        ports,
    )))
}

fn construct_copy(
    arguments: Vec<PublicValue>,
    context: &mut RequestContext<'_, InteractionNetEffects>,
    brand: &Arc<ConstructionBrand>,
) -> Result<RequestResult, TaskHalt> {
    let [outputs] = exact(arguments, "`.copy`")?;
    let outputs = eval_index_number(
        context.eval_context(),
        outputs.as_core(),
        "`.copy`",
        "copy_count",
    )
    .map_err(task_eval_error)?;
    let port_count = outputs
        .checked_add(1)
        .ok_or_else(|| TaskHalt::new("`.copy` output count is too large"))?;
    let mut transaction = construction_transaction(context)?;
    let (_, journal) = transaction.parts();
    let ports = journal.allocate_ports(port_count)?;
    journal.append(ConstructionOp::Copy {
        ports: Arc::from(ports.clone()),
    });
    Ok(RequestResult::Return(port_list(
        context.eval_context().values(),
        brand,
        ports,
    )))
}

fn construct_data(
    arguments: Vec<PublicValue>,
    context: &mut RequestContext<'_, InteractionNetEffects>,
    brand: &Arc<ConstructionBrand>,
) -> Result<RequestResult, TaskHalt> {
    let [value] = exact(arguments, "`.data`")?;
    let mut transaction = construction_transaction(context)?;
    let (_, journal) = transaction.parts();
    let [port]: [ConstructionPortId; 1] = journal
        .allocate_ports(1)?
        .try_into()
        .expect("one allocated port must form a singleton");
    journal.append(ConstructionOp::Data {
        port,
        value: value.into_core(),
    });
    Ok(RequestResult::Return(port_list(
        context.eval_context().values(),
        brand,
        [port],
    )))
}

fn construct_wire(
    arguments: Vec<PublicValue>,
    context: &mut RequestContext<'_, InteractionNetEffects>,
    brand: &Arc<ConstructionBrand>,
) -> Result<RequestResult, TaskHalt> {
    let [left, right] = exact(arguments, "`.wire`")?;
    let left = construction_port(context.eval_context(), left.as_core(), brand)
        .map_err(task_eval_error)?;
    let right = construction_port(context.eval_context(), right.as_core(), brand)
        .map_err(task_eval_error)?;
    let mut transaction = construction_transaction(context)?;
    let (_, journal) = transaction.parts();
    journal.append(ConstructionOp::Wire { left, right });
    Ok(RequestResult::ReturnUnit)
}

fn construction_transaction<'a>(
    context: &'a mut RequestContext<'_, InteractionNetEffects>,
) -> Result<crate::reflection::TransactionContext<'a, InteractionNetEffects>, TaskHalt> {
    context.transaction().ok_or_else(|| {
        TaskHalt::new("interaction-net operation escaped its isolated construction transaction")
    })
}

fn exact<const N: usize>(
    arguments: Vec<PublicValue>,
    operation: &str,
) -> Result<[PublicValue; N], TaskHalt> {
    arguments.try_into().map_err(|_| {
        TaskHalt::new(format!(
            "{operation} received the wrong number of arguments"
        ))
    })
}

fn port_list(
    values: &CoreValueFactory,
    brand: &Arc<ConstructionBrand>,
    ports: impl IntoIterator<Item = ConstructionPortId>,
) -> PublicValue {
    PublicValue::from_core(
        values,
        Value::List(List::from_values(
            ports
                .into_iter()
                .map(|id| {
                    Value::Opaque(OpaqueValue::new(Arc::new(ConstructionPort {
                        brand: brand.clone(),
                        id,
                    })))
                })
                .collect(),
        )),
    )
}

fn construction_port(
    context: &EvalContext,
    value: &Value,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let value = eval_value(context, value)?;
    let Value::Opaque(port) = value else {
        return Err(EvaluationHalt::new(
            "interaction-net operation requires a construction port",
        ));
    };
    let port = port.downcast::<ConstructionPort>().ok_or_else(|| {
        EvaluationHalt::new("interaction-net operation requires a construction port")
    })?;
    if !Arc::ptr_eq(&port.brand, brand) {
        return Err(EvaluationHalt::new(
            "interaction-net construction port belongs to another invocation",
        ));
    }
    Ok(port.id)
}

fn replay(
    journal: &ConstructionJournal,
    exposed: ConstructionPortId,
) -> Result<Value, EvaluationHalt> {
    let capacity = usize::try_from(journal.next_port - 1)
        .map_err(|_| EvaluationHalt::new("interaction-net port count exceeds this target"))?;
    let mut mapped = Vec::new();
    mapped
        .try_reserve_exact(capacity)
        .map_err(|_| EvaluationHalt::new("interaction-net replay allocation is too large"))?;
    let mut builder = NetBuilder::<CoreSpecialization>::new();

    for operation in journal.operations() {
        match operation {
            ConstructionOp::Bind { ports } => {
                append_ports(&mut mapped, ports.iter().copied(), builder.bind())?;
            }
            ConstructionOp::Copy { ports } => {
                let copy = builder.copy(ports.len() - 1);
                append_ports(
                    &mut mapped,
                    ports.iter().copied(),
                    std::iter::once(copy.input).chain(copy.outputs),
                )?;
            }
            ConstructionOp::Data { port, value } => {
                append_ports(&mut mapped, [*port], [builder.data(value.clone())])?;
            }
            ConstructionOp::Wire { left, right } => {
                builder
                    .try_wire(mapped_port(&mapped, *left)?, mapped_port(&mapped, *right)?)
                    .map_err(|error| EvaluationHalt::new(error.to_string()))?;
            }
        }
    }

    let exposed = mapped_port(&mapped, exposed)?;
    let template = builder
        .try_finish(exposed)
        .map_err(|error| EvaluationHalt::new(error.to_string()))?;
    Ok(Value::Net(NetValue::new(template.instantiate_shared())))
}

fn append_ports(
    mapped: &mut Vec<Port>,
    logical: impl IntoIterator<Item = ConstructionPortId>,
    actual: impl IntoIterator<Item = Port>,
) -> Result<(), EvaluationHalt> {
    let mut logical = logical.into_iter();
    let mut actual = actual.into_iter();
    loop {
        match (logical.next(), actual.next()) {
            (Some(logical), Some(actual)) => {
                if logical.index()? != mapped.len() {
                    return Err(EvaluationHalt::new(
                        "interaction-net construction journal has nonsequential ports",
                    ));
                }
                mapped.push(actual);
            }
            (None, None) => return Ok(()),
            _ => {
                return Err(EvaluationHalt::new(
                    "interaction-net construction journal port arity mismatch",
                ));
            }
        }
    }
}

fn mapped_port(mapped: &[Port], port: ConstructionPortId) -> Result<Port, EvaluationHalt> {
    mapped.get(port.index()?).copied().ok_or_else(|| {
        EvaluationHalt::new("interaction-net construction refers to an unknown port")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_ports_are_scoped_to_one_invocation() {
        let local = Arc::new(ConstructionBrand);
        let foreign = Arc::new(ConstructionBrand);
        let value = Value::Opaque(OpaqueValue::new(Arc::new(ConstructionPort {
            brand: foreign,
            id: ConstructionPortId(NonZeroU64::new(1).unwrap()),
        })));

        let context = crate::eval::test_support::test_context();
        let error = construction_port(&context, &value, &local).unwrap_err();
        assert_eq!(
            error.to_string(),
            "interaction-net construction port belongs to another invocation"
        );
    }
}

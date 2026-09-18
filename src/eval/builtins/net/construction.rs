//! Lazy interpretation and checked replay of the `interaction_net` effect.

use std::num::NonZeroU64;
use std::sync::Arc;

use crate::api::{Value as PublicValue, Values};
use crate::core::{List, NetValue, OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue, Value};
use crate::core_net::{CoreRuntimeNet, CoreSpecialization};
use crate::evaluation::{EvalContext, EvaluatorStepContext, WorkDependency};
use crate::interaction_net::{NetBuilder, Port};
use crate::reflection::{
    EffectRequestSpec, IsolatedEffectSearch, IsolatedSearchPoll, IsolatedTaskHost, RequestContext,
    RequestResult, SpecializationRequestInput, SpecializationRequestPoll,
    SpecializationRequestWork, TaskHalt, TaskSpecialization, task_eval_error,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::super::super::{EvaluationHalt, eval_value_in};

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

#[cfg(test)]
pub(crate) fn assert_construction_port_family_shape() {
    fn inspect(port: &ConstructionPort) {
        let ConstructionPort { brand, id } = port;
        let _: &Arc<ConstructionBrand> = brand;
        let _: &ConstructionPortId = id;
    }

    let _: fn(&ConstructionPort) = inspect;
    assert_eq!(
        <ConstructionPort as OpaquePayloadFamily>::PAYLOAD_RECORD
            .fields()
            .2,
        "edge-free token"
    );
}

// SAFETY: a construction port contains only one construction-local brand and
// a scalar port ID. Neither field can contain or reach a Glam value, runtime
// root, managed pointer, or active runtime capability.
unsafe impl OpaquePayloadFamily for ConstructionPort {
    const PAYLOAD_RECORD: OpaquePayloadRecord = OpaquePayloadRecord::edge_free(
        "interaction-net construction port",
        "src/eval/builtins/net/construction.rs",
    );
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
        value: PublicValue,
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

enum InteractionNetRequestWork {
    Immediate {
        request: InteractionNetRequest,
        arguments: Vec<PublicValue>,
    },
    CopyStart(Vec<PublicValue>),
    CopyCount,
    WireStart(Vec<PublicValue>),
    WireLeft {
        right: PublicValue,
    },
    WireRight {
        left: ConstructionPortId,
    },
    Poisoned,
}

impl TaskSpecialization for InteractionNetEffects {
    type Host = ConstructionHost;
    type Request = InteractionNetRequest;
    type RequestWork = InteractionNetRequestWork;
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

    fn start_request(
        &self,
        request: Self::Request,
        arguments: Vec<PublicValue>,
    ) -> Self::RequestWork {
        match request {
            InteractionNetRequest::Copy => InteractionNetRequestWork::CopyStart(arguments),
            InteractionNetRequest::Wire => InteractionNetRequestWork::WireStart(arguments),
            request => InteractionNetRequestWork::Immediate { request, arguments },
        }
    }
}

impl SpecializationRequestWork<InteractionNetEffects> for InteractionNetRequestWork {
    fn poll(
        &mut self,
        specialization: &InteractionNetEffects,
        input: Option<SpecializationRequestInput>,
        context: &mut RequestContext<'_, InteractionNetEffects>,
    ) -> Result<SpecializationRequestPoll, TaskHalt> {
        #[cfg(test)]
        assert!(
            !crate::core::thread_has_runtime_value_access_for_test(),
            "interaction-net effect callbacks must not inherit an evaluator value-access region"
        );
        let work = std::mem::replace(self, Self::Poisoned);
        match work {
            Self::Immediate { request, arguments } => {
                assert!(
                    input.is_none(),
                    "immediate net request cannot have demand input"
                );
                let result = match request {
                    InteractionNetRequest::Bind => {
                        construct_bind(arguments, context, &specialization.brand)
                    }
                    InteractionNetRequest::Data => {
                        construct_data(arguments, context, &specialization.brand)
                    }
                    InteractionNetRequest::Copy | InteractionNetRequest::Wire => {
                        unreachable!("demanding net requests receive dedicated work")
                    }
                }?;
                Ok(SpecializationRequestPoll::Complete(result))
            }
            Self::CopyStart(arguments) => {
                assert!(input.is_none(), "new copy request cannot have demand input");
                let [outputs] = exact(arguments, "`.copy`")?;
                *self = Self::CopyCount;
                Ok(SpecializationRequestPoll::Demand(outputs))
            }
            Self::CopyCount => {
                let outputs = request_input(input, "resumed copy count").map_err(|halt| {
                    let values = context.values();
                    halt.with_context(
                        &values,
                        values.wrap(crate::diagnostic::evaluation_context_frame("copy_count")),
                    )
                })?;
                let result = construct_copy_count(outputs, context, &specialization.brand)?;
                Ok(SpecializationRequestPoll::Complete(result))
            }
            Self::WireStart(arguments) => {
                assert!(input.is_none(), "new wire request cannot have demand input");
                let [left, right] = exact(arguments, "`.wire`")?;
                *self = Self::WireLeft { right };
                Ok(SpecializationRequestPoll::Demand(left))
            }
            Self::WireLeft { right } => {
                let left = request_port(
                    request_input(input, "resumed left construction port")?,
                    context,
                    &specialization.brand,
                )?;
                *self = Self::WireRight { left };
                Ok(SpecializationRequestPoll::Demand(right))
            }
            Self::WireRight { left } => {
                let right = request_port(
                    request_input(input, "resumed right construction port")?,
                    context,
                    &specialization.brand,
                )?;
                let result = construct_wire_ports(left, right, context)?;
                Ok(SpecializationRequestPoll::Complete(result))
            }
            Self::Poisoned => panic!("completed net request work was polled again"),
        }
    }
}

fn request_input(
    input: Option<SpecializationRequestInput>,
    resumed: &str,
) -> Result<crate::api::EvaluatedValue, TaskHalt> {
    match input.expect(resumed) {
        SpecializationRequestInput::Value(value) => Ok(value),
        SpecializationRequestInput::Failed(error) => Err(error),
    }
}

fn construct_copy_count(
    outputs: crate::api::EvaluatedValue,
    context: &mut RequestContext<'_, InteractionNetEffects>,
    brand: &Arc<ConstructionBrand>,
) -> Result<RequestResult, TaskHalt> {
    let outputs = outputs
        .with_core(|value| {
            let Value::Number(number) = value else {
                return Err(TaskHalt::new("`.copy` builtin requires number values"));
            };
            number.to_usize_if_integer().ok_or_else(|| {
                TaskHalt::new("`.copy` builtin requires non-negative integer indices")
            })
        })
        .map_err(|error| TaskHalt::new(error.to_string()))??;
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
        &context.values(),
        brand,
        ports,
    )))
}

fn request_port(
    value: crate::api::EvaluatedValue,
    context: &RequestContext<'_, InteractionNetEffects>,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, TaskHalt> {
    let values = context.values();
    value
        .with_core(|value| construction_port_value(values.core(), value, brand))
        .map_err(|error| TaskHalt::new(error.to_string()))?
        .map_err(task_eval_error)
}

fn construct_wire_ports(
    left: ConstructionPortId,
    right: ConstructionPortId,
    context: &mut RequestContext<'_, InteractionNetEffects>,
) -> Result<RequestResult, TaskHalt> {
    let mut transaction = construction_transaction(context)?;
    let (_, journal) = transaction.parts();
    journal.append(ConstructionOp::Wire { left, right });
    Ok(RequestResult::ReturnUnit)
}

type ConstructionHost = IsolatedTaskHost<()>;

pub(in crate::eval) struct NetConstructionMachine {
    brand: Arc<ConstructionBrand>,
    search: IsolatedEffectSearch<InteractionNetEffects>,
}

pub(in crate::eval) enum NetConstructionPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

impl NetConstructionMachine {
    pub(in crate::eval) fn new(
        context: EvalContext,
        effect: RuntimeValueRoot,
    ) -> Result<Self, EvaluationHalt> {
        let brand = Arc::new(ConstructionBrand);
        let specialization = InteractionNetEffects {
            brand: brand.clone(),
        };
        let values = Values::from_core_factory(context.values().clone());
        let effect = PublicValue::from_runtime_root(effect);
        let search = IsolatedEffectSearch::new_in_context(
            &effect,
            specialization,
            Arc::new(ConstructionHost::new_core(
                context.values().clone(),
                values.empty_dict(),
                (),
            )),
            context,
        )
        .map_err(TaskHalt::into_evaluation_halt)?;
        Ok(Self { brand, search })
    }

    /// Advances construction without losing the freer machine or its journal.
    /// Dependencies and failures remain explicit durable scheduler outcomes;
    /// only a completed replay publishes a rooted net value.
    pub(in crate::eval) fn poll(
        &mut self,
        context: &EvaluatorStepContext<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> NetConstructionPoll {
        match self.search.poll_with_budget(step_budget) {
            IsolatedSearchPoll::Yielded => NetConstructionPoll::Yielded,
            IsolatedSearchPoll::Blocked(blocked) => {
                if let Some(dependency) = blocked.dependency().cloned() {
                    return NetConstructionPoll::Pending(WorkDependency::Wait(dependency));
                }
                let error = match blocked.error() {
                    Some(halt) => {
                        let values = Values::from_core_factory(context.context().values().clone());
                        halt.clone()
                            .with_context(&values, values.wrap(net_construction_context()))
                            .into_evaluation_halt()
                    }
                    None => EvaluationHalt::new(
                        "interaction-net construction became blocked without a dependency or mutable host observation",
                    ),
                };
                net_construction_halt(context, error)
            }
            IsolatedSearchPoll::Complete(branches) => {
                let mut successes = branches.iter().filter(|branch| branch.value().is_some());
                let Some(branch) = successes.next() else {
                    return NetConstructionPoll::Failed(
                        context.root_failure(
                            EvaluationHalt::new(
                                "interaction-net construction produced no successful result",
                            )
                            .into_permanent_failure(),
                        ),
                    );
                };
                if successes.next().is_some() {
                    return NetConstructionPoll::Failed(context.root_failure(
                        EvaluationHalt::new(
                            "interaction-net construction produced multiple results; use `.cut` to select one",
                        )
                        .into_permanent_failure(),
                    ));
                }
                let values = Values::from_core_factory(context.context().values().clone());
                let result = values
                    .clone_core(branch.value().expect("successful branch checked above"))
                    .map_err(|error| EvaluationHalt::new(error.to_string()))
                    .and_then(|exposed| construction_port_in(context, &exposed, &self.brand))
                    .and_then(|exposed| replay(context, branch.journal(), exposed));
                match result {
                    Ok(value) => NetConstructionPoll::Ready(value),
                    Err(error) => net_construction_halt(context, error),
                }
            }
            IsolatedSearchPoll::Failed(halt) => {
                let values = Values::from_core_factory(context.context().values().clone());
                let error = halt
                    .with_context(&values, values.wrap(net_construction_context()))
                    .into_evaluation_halt();
                net_construction_halt(context, error)
            }
            IsolatedSearchPoll::Cancelled => NetConstructionPoll::Failed(
                context.root_failure(
                    EvaluationHalt::new("interaction-net construction was cancelled")
                        .into_permanent_failure(),
                ),
            ),
        }
    }
}

fn net_construction_halt(
    context: &EvaluatorStepContext<'_>,
    error: EvaluationHalt,
) -> NetConstructionPoll {
    if let Some(wait) = error.blocked_on() {
        return NetConstructionPoll::Pending(WorkDependency::Wait(wait.0));
    }
    if let Some(promise) = error.unassigned_promise_root() {
        return NetConstructionPoll::Pending(WorkDependency::Promise(promise.clone()));
    }
    NetConstructionPoll::Failed(context.root_failure(error.into_permanent_failure()))
}

fn net_construction_context() -> Value {
    crate::diagnostic::evaluation_context_frame("net_construction")
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
        &context.values(),
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
    journal.append(ConstructionOp::Data { port, value });
    Ok(RequestResult::Return(port_list(
        &context.values(),
        brand,
        [port],
    )))
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
    values: &Values,
    brand: &Arc<ConstructionBrand>,
    ports: impl IntoIterator<Item = ConstructionPortId>,
) -> PublicValue {
    values.wrap(Value::List(List::from_values(
        ports
            .into_iter()
            .map(|id| {
                Value::Opaque(OpaqueValue::new(
                    values.core(),
                    Arc::new(ConstructionPort {
                        brand: brand.clone(),
                        id,
                    }),
                ))
            })
            .collect(),
    )))
}

fn construction_port_in(
    context: &EvaluatorStepContext<'_>,
    value: &Value,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let value = eval_value_in(context, value)?;
    construction_port_value(context.context().values(), &value, brand)
}

fn construction_port_value(
    values: &crate::core::CoreValueFactory,
    value: &Value,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let Value::Opaque(port) = value else {
        return Err(EvaluationHalt::new(
            "interaction-net operation requires a construction port",
        ));
    };
    let port = port.downcast::<ConstructionPort>(values).ok_or_else(|| {
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
    context: &EvaluatorStepContext<'_>,
    journal: &ConstructionJournal,
    exposed: ConstructionPortId,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    context.with_value_access(|access| {
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
                    let value = access.clone_root(&value.clone().into_runtime_root());
                    append_ports(&mut mapped, [*port], [builder.data(value)])?;
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
        let net_root = access
            .values()
            .construct_rooted_managed_core_net(template.instantiate())
            .expect("managed core-net representation must fit one collector run");
        let net = CoreRuntimeNet::from_root(&net_root, access.values());
        Ok(access
            .values()
            .root_runtime_value(Value::Net(NetValue::new(net))))
    })
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
        assert_construction_port_family_shape();
        let values = crate::core::test_value_factory();
        let local = Arc::new(ConstructionBrand);
        let foreign = Arc::new(ConstructionBrand);
        let value = Value::Opaque(OpaqueValue::new(
            &values,
            Arc::new(ConstructionPort {
                brand: foreign,
                id: ConstructionPortId(NonZeroU64::new(1).unwrap()),
            }),
        ));

        let error = construction_port_value(&values, &value, &local).unwrap_err();
        assert_eq!(
            error.to_string(),
            "interaction-net construction port belongs to another invocation"
        );
    }
}

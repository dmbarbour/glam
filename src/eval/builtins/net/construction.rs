//! Lazy interpretation and checked replay of the `interaction_net` effect.

use std::num::NonZeroU64;
use std::sync::Arc;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::api::{Value as PublicValue, Values};
use crate::core::{Dict, List, OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue, Value};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluationValueAccess, EvaluatorStepContext, WhnfOwnerPoll,
    WorkDependency, poll_whnf_computation,
};
use crate::reflection::{
    EffectRequestSpec, IsolatedEffectSearch, IsolatedSearchPoll, IsolatedTaskHost, RequestContext,
    RequestResult, SpecializationRequestInput, SpecializationRequestPoll,
    SpecializationRequestWork, TaskHalt, TaskSpecialization, task_eval_error,
};
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::super::super::EvaluationHalt;
use super::super::super::value::evaluation_context_frame_in;
use super::super::super::whnf::WhnfComputation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ConstructionPortId(NonZeroU64);

impl ConstructionPortId {
    #[cfg(test)]
    pub(super) fn new(id: u64) -> Option<Self> {
        NonZeroU64::new(id).map(Self)
    }

    pub(super) fn index(self) -> Result<usize, EvaluationHalt> {
        usize::try_from(self.0.get() - 1)
            .map_err(|_| EvaluationHalt::new("interaction-net port index exceeds this target"))
    }
}

#[derive(Default)]
pub(super) struct ConstructionBrand {
    #[cfg(test)]
    probe: std::sync::OnceLock<Arc<ConstructionProbe>>,
}

struct ConstructionToken {
    brand: Arc<ConstructionBrand>,
    port: Option<ConstructionPortId>,
}

#[cfg(test)]
pub(crate) fn assert_construction_port_family_shape() {
    fn inspect(token: &ConstructionToken) {
        let ConstructionToken { brand, port } = token;
        let _: &Arc<ConstructionBrand> = brand;
        let _: &Option<ConstructionPortId> = port;
    }

    let _: fn(&ConstructionToken) = inspect;
    assert_eq!(
        <ConstructionToken as OpaquePayloadFamily>::PAYLOAD_RECORD
            .fields()
            .2,
        "edge-free token"
    );
}

// SAFETY: a construction token contains only one construction-local brand and
// an optional scalar port ID. Neither field can contain or reach a Glam value,
// runtime root, managed pointer, or active runtime capability.
unsafe impl OpaquePayloadFamily for ConstructionToken {
    const PAYLOAD_RECORD: OpaquePayloadRecord = OpaquePayloadRecord::edge_free(
        "interaction-net construction token",
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

#[cfg(test)]
#[derive(Default)]
struct ConstructionProbe {
    completed_operations: AtomicUsize,
    replays: AtomicUsize,
}

#[cfg(test)]
impl ConstructionProbe {
    fn complete_operation(&self) {
        self.completed_operations.fetch_add(1, Ordering::SeqCst);
    }

    fn record_replay(&self) {
        self.replays.fetch_add(1, Ordering::SeqCst);
    }
}

impl InteractionNetEffects {
    #[cfg(test)]
    fn complete_operation(&self) {
        if let Some(probe) = self.brand.probe.get() {
            probe.complete_operation();
        }
    }
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
                #[cfg(test)]
                specialization.complete_operation();
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
                #[cfg(test)]
                specialization.complete_operation();
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
                #[cfg(test)]
                specialization.complete_operation();
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
        .with_core(|value| {
            let Value::Opaque(port) = value else {
                return Err(EvaluationHalt::new(
                    "interaction-net operation requires a construction port",
                ));
            };
            decode_construction_port(values.core(), port, brand)
        })
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
    state: NetConstructionState,
}

enum NetConstructionState {
    Search(Box<IsolatedEffectSearch<InteractionNetEffects>>),
    Exposed {
        journal: ConstructionJournal,
        demand: WhnfComputation,
    },
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
        let brand = Arc::new(ConstructionBrand::default());
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
        Ok(Self {
            brand,
            state: NetConstructionState::Search(Box::new(search)),
        })
    }

    #[cfg(test)]
    fn new_with_probe(
        context: EvalContext,
        effect: RuntimeValueRoot,
        probe: Arc<ConstructionProbe>,
    ) -> Result<Self, EvaluationHalt> {
        let machine = Self::new(context, effect)?;
        machine
            .brand
            .probe
            .set(probe)
            .unwrap_or_else(|_| panic!("construction probe must be installed exactly once"));
        Ok(machine)
    }

    /// Advances construction without losing the freer machine or its journal.
    /// Dependencies and failures remain explicit durable scheduler outcomes;
    /// only a completed replay publishes a rooted net value.
    pub(in crate::eval) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> NetConstructionPoll {
        loop {
            match &mut self.state {
                NetConstructionState::Search(search) => {
                    let transition = match search.poll_with_budget(step_budget) {
                        IsolatedSearchPoll::Yielded => return NetConstructionPoll::Yielded,
                        IsolatedSearchPoll::Blocked(blocked) => {
                            if let Some(dependency) = blocked.dependency().cloned() {
                                return NetConstructionPoll::Pending(WorkDependency::Wait(
                                    dependency,
                                ));
                            }
                            let error = match blocked.error() {
                                Some(halt) => {
                                    let values = Values::from_core_factory(
                                        context.context().values().clone(),
                                    );
                                    halt.clone()
                                        .with_context(&values, net_construction_context(context))
                                        .into_evaluation_halt()
                                }
                                None => EvaluationHalt::new(
                                    "interaction-net construction became blocked without a dependency or mutable host observation",
                                ),
                            };
                            return net_construction_halt(context, error);
                        }
                        IsolatedSearchPoll::Complete(branches) => {
                            let mut successes =
                                branches.iter().filter(|branch| branch.value().is_some());
                            let Some(branch) = successes.next() else {
                                return NetConstructionPoll::Failed(context.root_failure(
                                    EvaluationHalt::new(
                                        "interaction-net construction produced no successful result",
                                    )
                                    .into_permanent_failure(),
                                ));
                            };
                            if successes.next().is_some() {
                                return NetConstructionPoll::Failed(context.root_failure(
                                    EvaluationHalt::new(
                                        "interaction-net construction produced multiple results; use `.cut` to select one",
                                    )
                                    .into_permanent_failure(),
                                ));
                            }
                            (
                                branch.journal().clone(),
                                branch
                                    .value()
                                    .expect("successful branch checked above")
                                    .clone()
                                    .into_runtime_root(),
                            )
                        }
                        IsolatedSearchPoll::Failed(halt) => {
                            let values =
                                Values::from_core_factory(context.context().values().clone());
                            let error = halt
                                .with_context(&values, net_construction_context(context))
                                .into_evaluation_halt();
                            return net_construction_halt(context, error);
                        }
                        IsolatedSearchPoll::Cancelled => {
                            return NetConstructionPoll::Failed(
                                context.root_failure(
                                    EvaluationHalt::new(
                                        "interaction-net construction was cancelled",
                                    )
                                    .into_permanent_failure(),
                                ),
                            );
                        }
                    };
                    self.state = NetConstructionState::Exposed {
                        journal: transition.0,
                        demand: WhnfComputation::from_root(transition.1),
                    };
                }
                NetConstructionState::Exposed { journal, demand } => {
                    match poll_whnf_computation(demand, poll_context, durable_context, step_budget)
                    {
                        WhnfOwnerPoll::Ready(value) => {
                            let exposed = context.with_value_access(|access| {
                                construction_port_value(&access, &value, &self.brand)
                            });
                            return match exposed
                                .and_then(|exposed| replay(context, &self.brand, journal, exposed))
                            {
                                Ok(value) => {
                                    #[cfg(test)]
                                    if let Some(probe) = self.brand.probe.get() {
                                        probe.record_replay();
                                    }
                                    NetConstructionPoll::Ready(value)
                                }
                                Err(error) => net_construction_halt(context, error),
                            };
                        }
                        WhnfOwnerPoll::Pending(dependency) => {
                            return NetConstructionPoll::Pending(dependency);
                        }
                        WhnfOwnerPoll::Yielded => return NetConstructionPoll::Yielded,
                        WhnfOwnerPoll::Failed(failure) => {
                            return NetConstructionPoll::Failed(failure);
                        }
                        WhnfOwnerPoll::External(boundary) => {
                            unreachable!(
                                "net-construction exposed-port demand produced an external {boundary:?} boundary"
                            )
                        }
                    }
                }
            }
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

fn net_construction_context(context: &EvaluatorStepContext<'_>) -> PublicValue {
    PublicValue::from_runtime_root(context.with_value_access(|access| {
        access
            .values()
            .root_runtime_value(evaluation_context_frame_in(
                access.values(),
                "net_construction",
            ))
    }))
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
            .map(|id| Value::Opaque(construction_token(values.core(), brand, Some(id))))
            .collect(),
    )))
}

pub(super) fn encode_construction_brand(
    access: &crate::core::RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
) -> Value {
    Value::Opaque(construction_token(access.values(), brand, None))
}

pub(super) fn encode_construction_port(
    access: &crate::core::RuntimeValueAccess<'_>,
    brand: &Arc<ConstructionBrand>,
    id: ConstructionPortId,
) -> Value {
    Value::Opaque(construction_token(access.values(), brand, Some(id)))
}

fn construction_token(
    values: &crate::core::CoreValueFactory,
    brand: &Arc<ConstructionBrand>,
    port: Option<ConstructionPortId>,
) -> OpaqueValue {
    OpaqueValue::new(
        values,
        Arc::new(ConstructionToken {
            brand: Arc::clone(brand),
            port,
        }),
    )
}

fn construction_port_value(
    access: &EvaluationValueAccess<'_>,
    value: &RuntimeValueRoot,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let value = access.clone_root(value);
    let Value::Opaque(port) = value else {
        return Err(EvaluationHalt::new(
            "interaction-net operation requires a construction port",
        ));
    };
    decode_construction_port(access.values().values(), &port, brand)
}

pub(super) fn decode_construction_port(
    values: &crate::core::CoreValueFactory,
    port: &OpaqueValue,
    brand: &Arc<ConstructionBrand>,
) -> Result<ConstructionPortId, EvaluationHalt> {
    let token = decode_construction_token(values, port).ok_or_else(|| {
        EvaluationHalt::new("interaction-net operation requires a construction port")
    })?;
    if !Arc::ptr_eq(&token.brand, brand) {
        return Err(EvaluationHalt::new(
            "interaction-net construction port belongs to another invocation",
        ));
    }
    token.port.ok_or_else(|| {
        EvaluationHalt::new("interaction-net operation requires a construction port")
    })
}

pub(super) fn decode_construction_brand(
    values: &crate::core::CoreValueFactory,
    value: &OpaqueValue,
) -> Result<Arc<ConstructionBrand>, EvaluationHalt> {
    let token = decode_construction_token(values, value).ok_or_else(|| {
        EvaluationHalt::new("interaction-net builder brand must be a construction brand")
    })?;
    if token.port.is_some() {
        return Err(EvaluationHalt::new(
            "interaction-net builder brand must be a brand token",
        ));
    }
    Ok(Arc::clone(&token.brand))
}

fn decode_construction_token(
    values: &crate::core::CoreValueFactory,
    token: &OpaqueValue,
) -> Option<Arc<ConstructionToken>> {
    token.downcast::<ConstructionToken>(values)
}

fn replay(
    context: &EvaluatorStepContext<'_>,
    brand: &Arc<ConstructionBrand>,
    journal: &ConstructionJournal,
    exposed: ConstructionPortId,
) -> Result<RuntimeValueRoot, EvaluationHalt> {
    context.with_value_access(|access| {
        let reverse_operations = journal
            .operations()
            .into_iter()
            .rev()
            .map(|operation| match operation {
                ConstructionOp::Bind { ports } => {
                    super::netlist::encode_bind(access.values(), brand, *ports)
                }
                ConstructionOp::Copy { ports } => {
                    super::netlist::encode_copy(access.values(), brand, ports)
                }
                ConstructionOp::Data { port, value } => super::netlist::encode_data(
                    access.values(),
                    brand,
                    *port,
                    access.clone_root(&value.clone().into_runtime_root()),
                ),
                ConstructionOp::Wire { left, right } => {
                    super::netlist::encode_wire(access.values(), brand, *left, *right)
                }
            })
            .collect();
        let state = super::netlist::encode_builder_state(
            access.values(),
            brand,
            journal.next_port,
            reverse_operations,
            Value::Dict(Dict::new_sync()),
        );
        let selected =
            super::netlist::encode_selected_netlist(access.values(), state, brand, exposed);
        let net = super::netlist::interaction_net_from_netlist_in(access.values(), &selected)?;
        Ok(access.values().root_runtime_value(net))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::eval::test_support::{TestExpr, TestKey, closed_function_value_in};

    fn apply(function: TestExpr, argument: TestExpr) -> TestExpr {
        TestExpr::Apply(Arc::new(function), Arc::new(argument))
    }

    fn api_member(local: usize, name: &str) -> TestExpr {
        TestExpr::Access(
            Arc::new(TestExpr::Local(local)),
            Arc::from([TestKey::Key(crate::core::Key::atom_from_text(name))]),
        )
    }

    fn construction_returning_one_data_port(access: &crate::core::RuntimeValueAccess<'_>) -> Value {
        let values = access.values();
        let data_effect = crate::eval::application::effect_value(
            access,
            closed_function_value_in(
                values,
                1,
                apply(
                    api_member(0, "data"),
                    TestExpr::Value(Value::binary_from_text("route probe")),
                ),
            ),
        );
        // `.r` itself is an applicable effect value. Applying it to the first
        // returned construction port produces the continuation's effect.
        let return_effect = crate::eval::application::effect_value(
            access,
            closed_function_value_in(values, 1, api_member(0, "r")),
        );
        let return_first_port = closed_function_value_in(
            values,
            1,
            apply(
                TestExpr::Value(return_effect),
                apply(
                    TestExpr::Value(Value::Builtin(crate::core::Builtin::ListHead)),
                    TestExpr::Local(0),
                ),
            ),
        );
        let handler = closed_function_value_in(
            values,
            1,
            apply(
                apply(api_member(0, "seq"), TestExpr::Value(data_effect)),
                TestExpr::Value(return_first_port),
            ),
        );
        crate::eval::application::effect_value(access, handler)
    }

    fn rooted_construction_effect(
        context: &crate::evaluation::OwnedEvalContext,
    ) -> RuntimeValueRoot {
        context
            .values()
            .construct_runtime_value_root(construction_returning_one_data_port)
    }

    fn poll_construction(
        context: &crate::evaluation::OwnedEvalContext,
        machine: &mut NetConstructionMachine,
        steps: usize,
    ) -> NetConstructionPoll {
        let poll = EvaluationPollContext::for_context(context);
        poll.evaluate(context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                context,
                &mut crate::evaluation::EvaluationStepBudget::new(steps),
            )
        })
    }

    fn finish_construction(
        context: &crate::evaluation::OwnedEvalContext,
        machine: &mut NetConstructionMachine,
    ) -> RuntimeValueRoot {
        for _ in 0..512 {
            match poll_construction(context, machine, 1) {
                NetConstructionPoll::Yielded => {}
                NetConstructionPoll::Ready(value) => return value,
                NetConstructionPoll::Pending(WorkDependency::Wait(wait)) => {
                    pump_construction_wait(context, &wait)
                }
                NetConstructionPoll::Pending(dependency) => {
                    panic!("self-contained construction exposed {dependency:?}")
                }
                NetConstructionPoll::Failed(failure) => {
                    panic!("self-contained construction failed: {failure}")
                }
            }
        }
        panic!("self-contained construction exhausted its deterministic poll bound")
    }

    fn pump_construction_wait(
        context: &crate::evaluation::OwnedEvalContext,
        wait: &crate::evaluation::EvaluationWaitToken,
    ) {
        for _ in 0..512 {
            match context.pump_wait(wait, 32) {
                crate::evaluation::EvaluationPumpOutcome::TargetReady => return,
                crate::evaluation::EvaluationPumpOutcome::BudgetExhausted => {}
                crate::evaluation::EvaluationPumpOutcome::Busy => std::thread::yield_now(),
                crate::evaluation::EvaluationPumpOutcome::NoProgress => {
                    panic!("self-contained construction wait lost its producer")
                }
            }
        }
        panic!("self-contained construction wait exhausted its deterministic pump bound")
    }

    #[test]
    fn construction_effect_api_exposes_only_task_local_and_net_operations() {
        let context = EvalContext::standalone();
        let specialization = InteractionNetEffects {
            brand: Arc::new(ConstructionBrand::default()),
        };
        let Value::Dict(api) =
            crate::reflection::isolated_effect_api_for_test(context.values(), &specialization)
        else {
            panic!("construction effect API must be a dictionary")
        };

        let expected = [
            "r", "seq", "alt", "fail", "cut", "fix", "get", "set", "reset", "shift", "bind",
            "copy", "data", "wire",
        ];
        assert_eq!(api.iter().count(), expected.len());
        for name in expected {
            assert!(
                api.get(&crate::core::Key::atom_from_text(name)).is_some(),
                "construction effect API must expose `{name}`"
            );
        }
        for name in ["heap", "exit", "task", "log", "env"] {
            assert!(
                api.get(&crate::core::Key::atom_from_text(name)).is_none(),
                "construction effect API must not expose `{name}`"
            );
        }
    }

    #[test]
    fn uninterrupted_construction_executes_each_operation_and_replay_once() {
        let context = EvalContext::standalone();
        let effect = rooted_construction_effect(&context);
        let probe = Arc::new(ConstructionProbe::default());
        let mut machine = NetConstructionMachine::new_with_probe(
            EvalContext::clone(&context),
            effect,
            probe.clone(),
        )
        .expect("the probed construction should initialize");

        let value = finish_construction(&context, &mut machine);
        assert!(matches!(value.clone_core_for_test(), Value::Net(_)));
        assert_eq!(probe.completed_operations.load(Ordering::SeqCst), 1);
        assert_eq!(probe.replays.load(Ordering::SeqCst), 1);
    }

    #[test]
    #[ignore = "PNC5 must move partial construction progress into ordinary managed values"]
    fn construction_search_survives_route_loss_without_replaying_completed_operations() {
        let context = EvalContext::standalone();
        let effect = rooted_construction_effect(&context);
        let probe = Arc::new(ConstructionProbe::default());
        let mut first = NetConstructionMachine::new_with_probe(
            EvalContext::clone(&context),
            effect.clone(),
            probe.clone(),
        )
        .expect("the first construction route should initialize");

        for _ in 0..512 {
            let poll = poll_construction(&context, &mut first, 1);
            if probe.completed_operations.load(Ordering::SeqCst) == 1 {
                assert!(
                    matches!(poll, NetConstructionPoll::Yielded),
                    "the fixture must lose its route after the operation and before replay"
                );
                break;
            }
            match poll {
                NetConstructionPoll::Yielded => {}
                NetConstructionPoll::Pending(WorkDependency::Wait(wait)) => {
                    pump_construction_wait(&context, &wait)
                }
                NetConstructionPoll::Pending(_) => {
                    panic!("construction route exposed a non-wait dependency")
                }
                NetConstructionPoll::Ready(_) => {
                    panic!("construction route completed before its forced handoff")
                }
                NetConstructionPoll::Failed(failure) => {
                    panic!("construction route failed: {failure}")
                }
            }
        }
        assert_eq!(probe.completed_operations.load(Ordering::SeqCst), 1);

        drop(first);
        context
            .values()
            .collect_managed_for_test()
            .expect("dropping a route must leave no managed access active");

        let mut later = NetConstructionMachine::new_with_probe(
            EvalContext::clone(&context),
            effect,
            probe.clone(),
        )
        .expect("a later construction route should initialize");
        let value = finish_construction(&context, &mut later);
        assert!(matches!(value.clone_core_for_test(), Value::Net(_)));
        assert_eq!(
            probe.completed_operations.load(Ordering::SeqCst),
            1,
            "a later route must resume after the completed `.data` transition"
        );
        assert_eq!(probe.replays.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn exposed_port_demand_suspends_without_losing_the_selected_journal() {
        let context = EvalContext::standalone();
        let brand = Arc::new(ConstructionBrand::default());
        let values = Values::from_core_factory(context.values().clone());
        let mut journal = ConstructionJournal::default();
        let [port]: [ConstructionPortId; 1] = journal
            .allocate_ports(1)
            .expect("the fixture port should allocate")
            .try_into()
            .expect("one allocated port should form a singleton");
        journal.append(ConstructionOp::Data {
            port,
            value: values.wrap(Value::Number(42.into())),
        });

        let promise = crate::core::PromisedValue::new(
            context.values(),
            "suspended construction exposed port",
        );
        let exposed = values
            .wrap(Value::Promised(promise.clone()))
            .into_runtime_root();
        let mut machine = NetConstructionMachine {
            brand: brand.clone(),
            state: NetConstructionState::Exposed {
                journal,
                demand: WhnfComputation::from_root(exposed),
            },
        };
        let poll = EvaluationPollContext::for_context(&context);

        let pending = poll.evaluate(&context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                &context,
                &mut crate::evaluation::EvaluationStepBudget::new(16),
            )
        });
        let NetConstructionPoll::Pending(WorkDependency::Promise(dependency)) = pending else {
            panic!("the selected journal should wait on its unresolved exposed port")
        };
        assert_eq!(dependency.id(), promise.id(context.values()));

        let exposed_port = Value::Opaque(OpaqueValue::new(
            context.values(),
            Arc::new(ConstructionToken {
                brand,
                port: Some(port),
            }),
        ));
        crate::core::set_test_promise(context.values(), &promise, exposed_port)
            .expect("the exposed port should accept one assignment");

        let ready = poll.evaluate(&context, |evaluator| {
            machine.poll(
                &poll,
                evaluator,
                &context,
                &mut crate::evaluation::EvaluationStepBudget::new(16),
            )
        });
        let NetConstructionPoll::Ready(ready) = ready else {
            panic!("the retained journal should replay after exposed-port assignment")
        };
        assert!(matches!(ready.clone_core_for_test(), Value::Net(_)));
    }

    #[test]
    fn construction_ports_are_scoped_to_one_invocation() {
        assert_construction_port_family_shape();
        let values = crate::core::test_value_factory();
        let local = Arc::new(ConstructionBrand::default());
        let foreign = Arc::new(ConstructionBrand::default());
        let port = OpaqueValue::new(
            &values,
            Arc::new(ConstructionToken {
                brand: foreign,
                port: Some(ConstructionPortId(NonZeroU64::new(1).unwrap())),
            }),
        );

        let error = decode_construction_port(&values, &port, &local).unwrap_err();
        assert_eq!(
            error.to_string(),
            "interaction-net construction port belongs to another invocation"
        );
    }
}

//! Durable owners for saturated builtin computations.
//!
//! A builtin source roots its operands once, then delegates each semantic
//! demand to the ordinary WHNF owner. Family-specific immediate operations run
//! only after those demands complete and never retain raw values across polls.

use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Atom, Builtin, EvaluatedValue, EvaluationFailure, EvaluationHalt, FunctionValue, LazyId,
    LazyValue, Value, keys, trace_compatibility_value_managed_edges,
};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluationValueAccess, EvaluatorStepContext, WorkDependency,
};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::annotation_machine::AnnotationBuiltinMachine;
use super::comparison_machine::{ComparisonBuiltinMachine, ComparisonBuiltinPoll};
use super::dict_machine::RegionalDictBuiltinMachine;
use super::effect_machine::EffectBuiltinMachine;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::list_observation_machine::ListObservationMachine;
use super::list_transform_machine::{ListConcatMachine, ListMapMachine, TextLinesMachine};
use super::object_builtin_machine::ObjectBuiltinMachine;
use super::object_composition_machine::ObjectCompositionMachine;
use super::pattern_machine::{
    PatternDictPredicateMachine, PatternDictTakeMachine, PatternEqualMachine, PatternListMachine,
    PatternPathMachine,
};
use super::value::{evaluation_context_frame_in, index_from_evaluated, number_from_evaluated};
use super::whnf::{
    RegionalBoundaryRequest, RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place,
    reduce_semantic_shell,
};

pub(crate) enum BuiltinTaskPoll {
    Ready(RuntimeValueRoot),
    Pending(WorkDependency),
    Yielded,
    Failed(RuntimeFailureRoot),
}

/// Callback-free result of one regional builtin transition.
///
/// Values and failures remain raw while the managed checkpoint is protected
/// by one value-access region. Scheduler boundaries and best-effort spark
/// intents are interpreted only after that region closes.
pub(in crate::eval) enum RegionalBuiltinPoll {
    Ready(Value),
    SparkIntent(Value),
    Boundary(RegionalBoundaryRequest),
    Yielded,
    Failed(Arc<EvaluationFailure>),
}

/// Compile-exhaustive regional state for builtin families migrated beneath
/// the lazy-owned checkpoint.
pub(in crate::eval) enum RegionalBuiltinMachine {
    Assertion(RegionalAssertionMachine),
    Conditional(RegionalConditionalMachine),
    Dictionary(RegionalDictBuiltinMachine),
    Numeric(RegionalNumericMachine),
    Net(RegionalNetMachine),
    Provenance(RegionalProvenanceMachine),
    Strategy(RegionalStrategyMachine),
}

enum RegionalAssertionPhase {
    Value,
    DiagnosticContext { received: &'static str },
}

pub(in crate::eval) struct RegionalAssertionMachine {
    diagnostic_context: Value,
    value: Value,
    result: Value,
    phase: RegionalAssertionPhase,
    demand: Option<RegionalWhnfWork>,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalConditionalMachine {
    builtin: Builtin,
    results: Option<Value>,
    demand: Option<RegionalWhnfWork>,
    front: Option<RegionalListFront>,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalNumericMachine {
    builtin: Builtin,
    arguments: Vec<Value>,
    next: usize,
    demand: Option<RegionalWhnfWork>,
    numbers: Vec<Number>,
    source_owner: LazyId,
}

enum RegionalNetPhase {
    Construction { effect: Value },
    Arity { arity: RegionalWhnfWork, net: Value },
    Net { arity: usize, net: RegionalWhnfWork },
}

pub(in crate::eval) struct RegionalNetMachine {
    phase: RegionalNetPhase,
    source_owner: LazyId,
}

pub(in crate::eval) struct RegionalProvenanceMachine {
    demand: RegionalWhnfWork,
}

enum RegionalStrategyPhase {
    First,
    Metadata,
    Complete,
    SparkRequested,
}

pub(in crate::eval) struct RegionalStrategyMachine {
    builtin: Builtin,
    source: Value,
    target: Value,
    demand: Option<RegionalWhnfWork>,
    phase: RegionalStrategyPhase,
    source_owner: LazyId,
}

impl RegionalBuiltinMachine {
    pub(in crate::eval) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::AssertUnit
                | Builtin::IfResult
                | Builtin::MatchResult
                | Builtin::Add
                | Builtin::Subtract
                | Builtin::Multiply
                | Builtin::Divide
                | Builtin::Floor
                | Builtin::Mod
                | Builtin::InspectOrigin
                | Builtin::InteractionNet
                | Builtin::NetArity
                | Builtin::Seq
                | Builtin::Spark
                | Builtin::DictSingleton
                | Builtin::DictUnion
                | Builtin::DictUpdate
                | Builtin::MergeDuplicate
        )
    }

    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        assert!(Self::supports(builtin));
        assert_eq!(arguments.len(), builtin.arity());
        match builtin {
            Builtin::AssertUnit => {
                let [diagnostic_context, value, result] = arguments else {
                    unreachable!("unit assertion must retain three operands")
                };
                Self::Assertion(RegionalAssertionMachine {
                    diagnostic_context: access.values().duplicate_value(diagnostic_context),
                    value: access.values().duplicate_value(value),
                    result: access.values().duplicate_value(result),
                    phase: RegionalAssertionPhase::Value,
                    demand: None,
                    source_owner,
                })
            }
            Builtin::InspectOrigin => {
                let [origin] = arguments else {
                    unreachable!("origin inspection must retain one operand")
                };
                Self::Provenance(RegionalProvenanceMachine {
                    demand: RegionalWhnfWork::from_focus(
                        access,
                        access.values().duplicate_value(origin),
                    )
                    .with_source_owner(source_owner),
                })
            }
            Builtin::IfResult | Builtin::MatchResult => {
                let [results] = arguments else {
                    unreachable!("a conditional source must retain one result list")
                };
                Self::Conditional(RegionalConditionalMachine {
                    builtin,
                    results: Some(access.values().duplicate_value(results)),
                    demand: None,
                    front: None,
                    source_owner,
                })
            }
            Builtin::InteractionNet => {
                let [effect] = arguments else {
                    unreachable!("interaction-net construction must retain one effect")
                };
                Self::Net(RegionalNetMachine {
                    phase: RegionalNetPhase::Construction {
                        effect: access.values().duplicate_value(effect),
                    },
                    source_owner,
                })
            }
            Builtin::NetArity => {
                let [arity, net] = arguments else {
                    unreachable!("net arity must retain its arity and net operands")
                };
                Self::Net(RegionalNetMachine {
                    phase: RegionalNetPhase::Arity {
                        arity: RegionalWhnfWork::from_focus(
                            access,
                            access.values().duplicate_value(arity),
                        )
                        .with_source_owner(source_owner),
                        net: access.values().duplicate_value(net),
                    },
                    source_owner,
                })
            }
            Builtin::Seq | Builtin::Spark => {
                let [source, target] = arguments else {
                    unreachable!("a strategy source must retain two operands")
                };
                Self::Strategy(RegionalStrategyMachine {
                    builtin,
                    source: access.values().duplicate_value(source),
                    target: access.values().duplicate_value(target),
                    demand: None,
                    phase: RegionalStrategyPhase::First,
                    source_owner,
                })
            }
            Builtin::DictSingleton
            | Builtin::DictUnion
            | Builtin::DictUpdate
            | Builtin::MergeDuplicate => Self::Dictionary(RegionalDictBuiltinMachine::new_in(
                access,
                source_owner,
                builtin,
                arguments,
            )),
            _ => Self::Numeric(RegionalNumericMachine {
                builtin,
                arguments: arguments
                    .iter()
                    .map(|value| access.values().duplicate_value(value))
                    .collect(),
                next: 0,
                demand: None,
                numbers: Vec::with_capacity(arguments.len()),
                source_owner,
            }),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match self {
            Self::Assertion(machine) => machine.poll_in(access, step_budget),
            Self::Conditional(machine) => machine.poll_in(access, step_budget),
            Self::Dictionary(machine) => machine.poll_in(access, step_budget),
            Self::Numeric(machine) => machine.poll_in(access, step_budget),
            Self::Net(machine) => machine.poll_in(access, step_budget),
            Self::Provenance(machine) => machine.poll_in(access, step_budget),
            Self::Strategy(machine) => machine.poll_in(access, step_budget),
        }
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match self {
            Self::Assertion(machine) => machine.trace_managed_edges(visitor),
            Self::Conditional(machine) => machine.trace_managed_edges(visitor),
            Self::Dictionary(machine) => machine.trace_managed_edges(visitor),
            Self::Numeric(machine) => machine.trace_managed_edges(visitor),
            Self::Net(machine) => machine.trace_managed_edges(visitor),
            Self::Provenance(machine) => machine.trace_managed_edges(visitor),
            Self::Strategy(machine) => machine.trace_managed_edges(visitor),
        }
    }
}

impl RegionalConditionalMachine {
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self.front.is_none() {
            let demand = self.demand.get_or_insert_with(|| {
                RegionalWhnfWork::from_focus(
                    access,
                    access.values().duplicate_value(
                        self.results
                            .as_ref()
                            .expect("conditional result-list demand must retain its operand"),
                    ),
                )
                .with_source_owner(self.source_owner)
            });
            let results =
                match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
            let results = EvaluatedValue::try_from(results)
                .expect("conditional result-list demand must reach WHNF")
                .into_value();
            if !matches!(results, Value::List(_)) {
                return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(format!(
                    "{} search did not produce a result list",
                    conditional_name(self.builtin)
                ))));
            }
            self.results = None;
            self.demand = None;
            self.front = Some(RegionalListFront::new_in(
                access,
                results,
                Some(self.source_owner),
            ));
        }

        match self
            .front
            .as_mut()
            .expect("conditional list-front work must be installed")
            .poll_in(access, step_budget)
        {
            RegionalListFrontPoll::Ready(Some((result, _tail))) => {
                RegionalBuiltinPoll::Ready(result)
            }
            RegionalListFrontPoll::Ready(None) => RegionalBuiltinPoll::Failed(Arc::new(
                EvaluationFailure::message(match self.builtin {
                    Builtin::IfResult => "if search exhausted despite its required `else` branch",
                    Builtin::MatchResult => {
                        "match search exhausted despite its compiler-provided fallback"
                    }
                    _ => unreachable!(),
                }),
            )),
            RegionalListFrontPoll::Boundary(request) => RegionalBuiltinPoll::Boundary(request),
            RegionalListFrontPoll::Yielded => RegionalBuiltinPoll::Yielded,
            RegionalListFrontPoll::Failed(failure) => RegionalBuiltinPoll::Failed(failure),
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Some(results) = &self.results {
            trace_compatibility_value_managed_edges(results, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
        if let Some(front) = &self.front {
            front.trace_managed_edges(visitor);
        }
    }
}

impl RegionalAssertionMachine {
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let argument = match self.phase {
            RegionalAssertionPhase::Value => &self.value,
            RegionalAssertionPhase::DiagnosticContext { .. } => &self.diagnostic_context,
        };
        let demand = self.demand.get_or_insert_with(|| {
            RegionalWhnfWork::from_focus(access, access.values().duplicate_value(argument))
                .with_source_owner(self.source_owner)
        });
        let value =
            match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                RegionalWhnfStatus::Ready(value) => value,
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalWhnfStatus::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };
        self.demand = None;
        let value = EvaluatedValue::try_from(value)
            .expect("assertion operand demand must reach WHNF")
            .into_value();

        match self.phase {
            RegionalAssertionPhase::Value => {
                let is_unit = matches!(
                    &value,
                    Value::Atom(atom) if *atom == Atom::from_key(&keys::UNIT)
                );
                if is_unit {
                    return RegionalBuiltinPoll::Ready(
                        access.values().duplicate_value(&self.result),
                    );
                }
                self.phase = RegionalAssertionPhase::DiagnosticContext {
                    received: value.diagnostic_kind_name(),
                };
                RegionalBuiltinPoll::Yielded
            }
            RegionalAssertionPhase::DiagnosticContext { received } => {
                let Value::Binary(diagnostic_context) = value else {
                    return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(
                        "unit assertion diagnostic context must be text",
                    )));
                };
                RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(format!(
                    "{}: unit expected, received {received}",
                    String::from_utf8_lossy(&diagnostic_context)
                ))))
            }
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.diagnostic_context, visitor);
        trace_compatibility_value_managed_edges(&self.value, visitor);
        trace_compatibility_value_managed_edges(&self.result, visitor);
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

impl RegionalNumericMachine {
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        while self.next < self.arguments.len() {
            let demand = self.demand.get_or_insert_with(|| {
                RegionalWhnfWork::from_focus(
                    access,
                    access.values().duplicate_value(&self.arguments[self.next]),
                )
                .with_source_owner(self.source_owner)
            });
            let value =
                match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
            let value =
                EvaluatedValue::try_from(value).expect("numeric operand demand must reach WHNF");
            let number = match number_from_evaluated(value, numeric_name(self.builtin)) {
                Ok(number) => number,
                Err(error) => {
                    return RegionalBuiltinPoll::Failed(error.into_permanent_failure());
                }
            };
            self.numbers.push(number);
            self.next += 1;
            self.demand = None;
        }

        match numeric_result(self.builtin, &self.numbers) {
            Ok(result) => RegionalBuiltinPoll::Ready(Value::Number(result)),
            Err(message) => {
                RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message)))
            }
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for argument in &self.arguments {
            trace_compatibility_value_managed_edges(argument, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

impl RegionalNetMachine {
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        match &mut self.phase {
            RegionalNetPhase::Construction { effect } => {
                RegionalBuiltinPoll::Ready(Value::Lazy(LazyValue::from_net_construction_in(
                    access.values(),
                    access.values().duplicate_value(effect),
                )))
            }
            RegionalNetPhase::Arity { arity, net } => {
                let arity = match drive_regional_in_place(
                    access,
                    arity,
                    step_budget,
                    reduce_semantic_shell,
                ) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        let failure = EvaluationHalt::failure(failure).with_context(
                            access.values(),
                            evaluation_context_frame_in(access.values(), "net_arity"),
                        );
                        return RegionalBuiltinPoll::Failed(failure.into_permanent_failure());
                    }
                };
                let arity = match index_from_evaluated(
                    EvaluatedValue::try_from(arity).expect("net arity demand must reach WHNF"),
                    "net_arity",
                ) {
                    Ok(arity) => arity,
                    Err(failure) => {
                        return RegionalBuiltinPoll::Failed(failure.into_permanent_failure());
                    }
                };
                self.phase = RegionalNetPhase::Net {
                    arity,
                    net: RegionalWhnfWork::from_focus(access, access.values().duplicate_value(net))
                        .with_source_owner(self.source_owner),
                };
                RegionalBuiltinPoll::Yielded
            }
            RegionalNetPhase::Net { arity, net } => {
                let net = match drive_regional_in_place(
                    access,
                    net,
                    step_budget,
                    reduce_semantic_shell,
                ) {
                    RegionalWhnfStatus::Ready(value) => value,
                    RegionalWhnfStatus::Boundary(request) => {
                        return RegionalBuiltinPoll::Boundary(request);
                    }
                    RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                    RegionalWhnfStatus::Failed(failure) => {
                        return RegionalBuiltinPoll::Failed(failure);
                    }
                };
                let net = EvaluatedValue::try_from(net)
                    .expect("net operand demand must reach WHNF")
                    .into_value();
                let Value::Net(net) = net else {
                    return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(
                        "net_arity builtin requires an interaction-net value",
                    )));
                };
                let value = if *arity == 0 {
                    Value::Lazy(LazyValue::from_net_computation_in(access.values(), net))
                } else {
                    Value::Function(FunctionValue::new(net, *arity))
                };
                RegionalBuiltinPoll::Ready(value)
            }
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.phase {
            RegionalNetPhase::Construction { effect } => {
                trace_compatibility_value_managed_edges(effect, visitor);
            }
            RegionalNetPhase::Arity { arity, net } => {
                arity.trace_managed_edges(visitor);
                trace_compatibility_value_managed_edges(net, visitor);
            }
            RegionalNetPhase::Net { net, .. } => net.trace_managed_edges(visitor),
        }
    }
}

impl RegionalProvenanceMachine {
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let origin = match drive_regional_in_place(
            access,
            &mut self.demand,
            step_budget,
            reduce_semantic_shell,
        ) {
            RegionalWhnfStatus::Ready(value) => value,
            RegionalWhnfStatus::Boundary(request) => {
                return RegionalBuiltinPoll::Boundary(request);
            }
            RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
            RegionalWhnfStatus::Failed(failure) => {
                let failure = EvaluationHalt::failure(failure).with_context(
                    access.values(),
                    evaluation_context_frame_in(access.values(), "compilation_origin"),
                );
                return RegionalBuiltinPoll::Failed(failure.into_permanent_failure());
            }
        };
        let origin = EvaluatedValue::try_from(origin)
            .expect("origin demand must reach WHNF")
            .into_value();
        let Value::Opaque(origin) = origin else {
            return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(
                "origin inspection requires an opaque compilation origin",
            )));
        };
        match crate::diagnostic::inspect_compilation_origin(access.values().values(), &origin) {
            Some(origin) => RegionalBuiltinPoll::Ready(origin),
            None => RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(
                "origin inspection requires an opaque compilation origin",
            ))),
        }
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        self.demand.trace_managed_edges(visitor);
    }
}

impl RegionalStrategyMachine {
    fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        if self.builtin == Builtin::Spark {
            return match self.phase {
                RegionalStrategyPhase::First => {
                    if matches!(
                        self.source,
                        Value::Lazy(_) | Value::Promised(_) | Value::Metadata(_)
                    ) {
                        // Record the intent beneath the traced checkpoint
                        // before the caller roots and submits it outside this
                        // managed transition.
                        self.phase = RegionalStrategyPhase::SparkRequested;
                        RegionalBuiltinPoll::SparkIntent(
                            access.values().duplicate_value(&self.source),
                        )
                    } else {
                        RegionalBuiltinPoll::Ready(access.values().duplicate_value(&self.target))
                    }
                }
                RegionalStrategyPhase::SparkRequested => {
                    RegionalBuiltinPoll::Ready(access.values().duplicate_value(&self.target))
                }
                RegionalStrategyPhase::Metadata | RegionalStrategyPhase::Complete => {
                    unreachable!("spark does not perform inline strategy demand")
                }
            };
        }

        if matches!(self.phase, RegionalStrategyPhase::Complete) {
            return RegionalBuiltinPoll::Ready(access.values().duplicate_value(&self.target));
        }
        let demand = self.demand.get_or_insert_with(|| {
            RegionalWhnfWork::from_focus(access, access.values().duplicate_value(&self.source))
                .with_source_owner(self.source_owner)
        });
        let ready =
            match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
                RegionalWhnfStatus::Ready(value) => value,
                RegionalWhnfStatus::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalWhnfStatus::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalWhnfStatus::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            };

        if matches!(self.phase, RegionalStrategyPhase::First) {
            let metadata = EvaluatedValue::try_from(ready)
                .expect("strategy demand must produce WHNF")
                .into_value()
                .associated_metadata();
            if let Some(metadata) = metadata {
                self.demand = Some(
                    RegionalWhnfWork::from_focus(access, metadata)
                        .with_source_owner(self.source_owner),
                );
                self.phase = RegionalStrategyPhase::Metadata;
                return RegionalBuiltinPoll::Yielded;
            }
        }

        self.demand = None;
        self.phase = RegionalStrategyPhase::Complete;
        RegionalBuiltinPoll::Ready(access.values().duplicate_value(&self.target))
    }

    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.source, visitor);
        trace_compatibility_value_managed_edges(&self.target, visitor);
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
    }
}

pub(crate) enum BuiltinTaskMachine {
    Annotation(Box<AnnotationBuiltinMachine>),
    Comparison(ComparisonBuiltinMachine),
    Effect(EffectBuiltinMachine),
    ListObservation(Box<ListObservationMachine>),
    ListMap(ListMapMachine),
    ListConcat(ListConcatMachine),
    TextLines(TextLinesMachine),
    Object(Box<ObjectBuiltinMachine>),
    ObjectComposition(Box<ObjectCompositionMachine>),
    PatternList(PatternListMachine),
    PatternPath(Box<PatternPathMachine>),
    PatternDictPredicate(PatternDictPredicateMachine),
    PatternDictTake(Box<PatternDictTakeMachine>),
    PatternEqual(Box<PatternEqualMachine>),
}

impl BuiltinTaskMachine {
    pub(crate) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::AssertUnit
                | Builtin::IfResult
                | Builtin::MatchResult
                | Builtin::Greater
                | Builtin::GreaterEqual
                | Builtin::Equal
                | Builtin::NotEqual
                | Builtin::LessEqual
                | Builtin::Less
                | Builtin::Add
                | Builtin::Subtract
                | Builtin::Multiply
                | Builtin::Divide
                | Builtin::Floor
                | Builtin::Mod
                | Builtin::InspectOrigin
                | Builtin::Seq
                | Builtin::Spark
                | Builtin::DictSingleton
                | Builtin::DictUnion
                | Builtin::DictUpdate
                | Builtin::MergeDuplicate
                | Builtin::Slice
                | Builtin::ListLen
                | Builtin::ListSplit
                | Builtin::ListSplitEnd
                | Builtin::ListAt
                | Builtin::ListHead
                | Builtin::ListTail
                | Builtin::Map
                | Builtin::ListConcat
                | Builtin::TextLines
                | Builtin::InteractionNet
                | Builtin::NetArity
                | Builtin::PatternIsList
                | Builtin::PatternListTryUncons
                | Builtin::PatternListTryUnsnoc
                | Builtin::PatternListIsEmpty
                | Builtin::PatternPathEqual
                | Builtin::PatternIsDict
                | Builtin::PatternDictIsEmpty
                | Builtin::PatternDictTryTake
                | Builtin::PatternDictTryTakeOptional
                | Builtin::PatternEqual
                | Builtin::Anno
                | Builtin::ObjectSpec
                | Builtin::ObjectLocalName
                | Builtin::DiagnosticObject
                | Builtin::ObjectWithDefs
                | Builtin::ObjectComposedDefs
                | Builtin::ObjectOverrideDefs
                | Builtin::ObjectInstance
                | Builtin::ObjectInstanceFromParts
                | Builtin::ObjectDefaultDefs
                | Builtin::ObjectDictDefs
                | Builtin::ObjectFromDict
                | Builtin::EffectApply
                | Builtin::EffectCall
                | Builtin::EffectMap
                | Builtin::EffectMapRun
                | Builtin::EffectMapContinue
                | Builtin::Fixpoint
        )
    }

    pub(crate) fn new(builtin: Builtin, arguments: Vec<RuntimeValueRoot>) -> Self {
        assert!(Self::supports(builtin));
        assert_eq!(
            arguments.len(),
            builtin.arity(),
            "a builtin source must contain one saturated call"
        );
        match builtin {
            Builtin::Anno => Self::Annotation(Box::new(AnnotationBuiltinMachine::new(arguments))),
            builtin if EffectBuiltinMachine::supports(builtin) => {
                Self::Effect(EffectBuiltinMachine::new(builtin, arguments))
            }
            Builtin::Greater
            | Builtin::GreaterEqual
            | Builtin::Equal
            | Builtin::NotEqual
            | Builtin::LessEqual
            | Builtin::Less => Self::Comparison(ComparisonBuiltinMachine::new(builtin, arguments)),
            builtin if ListObservationMachine::supports(builtin) => {
                Self::ListObservation(Box::new(ListObservationMachine::new(builtin, arguments)))
            }
            Builtin::Map => Self::ListMap(ListMapMachine::new(arguments)),
            Builtin::ListConcat => Self::ListConcat(ListConcatMachine::new(arguments)),
            Builtin::TextLines => Self::TextLines(TextLinesMachine::new(arguments)),
            builtin if ObjectBuiltinMachine::supports(builtin) => {
                Self::Object(Box::new(ObjectBuiltinMachine::new(builtin, arguments)))
            }
            builtin if ObjectCompositionMachine::supports(builtin) => {
                Self::ObjectComposition(Box::new(ObjectCompositionMachine::new(builtin, arguments)))
            }
            builtin if PatternListMachine::supports(builtin) => {
                Self::PatternList(PatternListMachine::new(builtin, arguments))
            }
            Builtin::PatternPathEqual => {
                Self::PatternPath(Box::new(PatternPathMachine::new(arguments)))
            }
            Builtin::PatternIsDict | Builtin::PatternDictIsEmpty => {
                Self::PatternDictPredicate(PatternDictPredicateMachine::new(builtin, arguments))
            }
            Builtin::PatternDictTryTake | Builtin::PatternDictTryTakeOptional => {
                Self::PatternDictTake(Box::new(PatternDictTakeMachine::new(builtin, arguments)))
            }
            Builtin::PatternEqual => {
                Self::PatternEqual(Box::new(PatternEqualMachine::new(arguments)))
            }
            _ => unreachable!("migrated builtin family must install its managed checkpoint"),
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        match self {
            Self::Annotation(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::Comparison(machine) => {
                match machine.poll(poll_context, context, durable_context, step_budget) {
                    ComparisonBuiltinPoll::Ready(value) => BuiltinTaskPoll::Ready(value),
                    ComparisonBuiltinPoll::Pending(dependency) => {
                        BuiltinTaskPoll::Pending(dependency)
                    }
                    ComparisonBuiltinPoll::Yielded => BuiltinTaskPoll::Yielded,
                    ComparisonBuiltinPoll::Failed(failure) => BuiltinTaskPoll::Failed(failure),
                }
            }
            Self::Effect(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::ListObservation(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::ListMap(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::ListConcat(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::TextLines(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::Object(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::ObjectComposition(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::PatternList(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::PatternPath(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::PatternDictPredicate(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::PatternDictTake(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
            Self::PatternEqual(machine) => {
                machine.poll(poll_context, context, durable_context, step_budget)
            }
        }
    }
}

fn conditional_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::IfResult => "if",
        Builtin::MatchResult => "match",
        _ => unreachable!("conditional name requested for another builtin"),
    }
}

fn numeric_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Add => "add",
        Builtin::Subtract => "subtract",
        Builtin::Multiply => "multiply",
        Builtin::Divide => "divide",
        Builtin::Floor => "floor",
        Builtin::Mod => "mod",
        _ => unreachable!("numeric name requested for another builtin"),
    }
}

fn numeric_result(builtin: Builtin, arguments: &[Number]) -> Result<Number, &'static str> {
    match builtin {
        Builtin::Add => Ok(arguments[0].add(&arguments[1])),
        Builtin::Subtract => Ok(arguments[0].sub(&arguments[1])),
        Builtin::Multiply => Ok(arguments[0].mul(&arguments[1])),
        Builtin::Divide => arguments[0]
            .checked_div(&arguments[1])
            .ok_or("divide builtin cannot divide by zero"),
        Builtin::Floor => Ok(arguments[0].floor()),
        Builtin::Mod => arguments[0]
            .checked_mod(&arguments[1])
            .ok_or("mod builtin cannot divide by zero"),
        _ => unreachable!("numeric result requested for another builtin"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::{CoreValueFactory, ListThunk, PromisedValue};
    use crate::evaluation::EvalContext;
    use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

    fn context() -> crate::evaluation::OwnedEvalContext {
        EvalContext::isolated(CoreValueFactory::new(
            allocate_evaluation_runtime_id(),
            RuntimeIds::new(),
        ))
    }

    #[test]
    fn numeric_machine_resumes_at_the_second_operand_without_replaying_the_first() {
        let context = context();
        let first_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&first_demands);
        let first = Value::semantic_thunk(context.values(), "first numeric operand", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Number(1.into()))
        });
        let second = PromisedValue::new(context.values(), "second numeric operand");
        let sum = Value::builtin_call(
            context.values(),
            Builtin::Add,
            vec![first, Value::Promised(second.clone())],
        );
        let Value::Lazy(sum_lazy) = &sum else {
            unreachable!("a saturated numeric builtin must remain lazy")
        };
        let sum_root = sum_lazy.root(context.values());

        let blocked = crate::eval::eval_value(&context, &sum)
            .expect_err("the second operand must remain an exact suspension boundary");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
        context
            .values()
            .collect_managed_for_test()
            .expect("the numeric checkpoint must survive loss of its first poll route");
        crate::eval::eval_value(&context, &sum)
            .expect_err("a later route must resume the same second-operand dependency");
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);

        crate::core::set_test_promise(context.values(), &second, Value::Number(2.into()))
            .expect("the second operand should accept its assignment");
        context
            .values()
            .collect_managed_for_test()
            .expect("the assigned numeric checkpoint must retain its completed prefix");
        assert_eq!(
            crate::eval::eval_value(&context, &sum).expect("numeric work must resume"),
            Value::Number(3.into())
        );
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
        drop(sum_root);
    }

    #[test]
    fn numeric_machine_retains_numeric_failure_contracts() {
        let context = context();
        let divide = Value::builtin_call(
            context.values(),
            Builtin::Divide,
            vec![Value::Number(1.into()), Value::Number(0.into())],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &divide)
                .expect_err("division by zero must remain a permanent failure")
                .to_string(),
            "divide builtin cannot divide by zero"
        );

        let invalid = Value::builtin_call(
            context.values(),
            Builtin::Floor,
            vec![Value::binary_from_text("not a number")],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &invalid)
                .expect_err("numeric kind validation must remain a permanent failure")
                .to_string(),
            "floor builtin requires number values"
        );
    }

    #[test]
    fn assertion_machine_resumes_diagnostic_context_without_replaying_the_value() {
        let context = context();
        let value_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&value_demands);
        let value = Value::semantic_thunk(context.values(), "asserted value", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Number(7.into()))
        });
        let diagnostic_context =
            PromisedValue::new(context.values(), "unit assertion diagnostic context");
        let assertion = Value::builtin_call(
            context.values(),
            Builtin::AssertUnit,
            vec![
                Value::Promised(diagnostic_context.clone()),
                value,
                Value::Number(99.into()),
            ],
        );
        let Value::Lazy(assertion_lazy) = &assertion else {
            unreachable!("a saturated assertion builtin must remain lazy")
        };
        let assertion_root = assertion_lazy.root(context.values());

        let blocked = crate::eval::eval_value(&context, &assertion)
            .expect_err("assertion failure should suspend on its diagnostic context");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        assert_eq!(value_demands.load(Ordering::Relaxed), 1);
        context
            .values()
            .collect_managed_for_test()
            .expect("the assertion checkpoint must survive route loss");
        crate::eval::eval_value(&context, &assertion)
            .expect_err("a later route must resume the diagnostic-context dependency");
        assert_eq!(value_demands.load(Ordering::Relaxed), 1);

        crate::core::set_test_promise(
            context.values(),
            &diagnostic_context,
            Value::binary_from_text("definition foo"),
        )
        .expect("the diagnostic context should accept its assignment");
        context
            .values()
            .collect_managed_for_test()
            .expect("the assigned assertion checkpoint must retain its diagnostic phase");
        assert_eq!(
            crate::eval::eval_value(&context, &assertion)
                .expect_err("the resumed assertion must report its failure")
                .to_string(),
            "definition foo: unit expected, received Number"
        );
        assert_eq!(value_demands.load(Ordering::Relaxed), 1);
        drop(assertion_root);
    }

    #[test]
    fn conditional_machine_selects_first_result_and_retains_exhaustion_diagnostics() {
        let context = context();
        for builtin in [Builtin::IfResult, Builtin::MatchResult] {
            let selection = Value::builtin_call(
                context.values(),
                builtin,
                vec![Value::List(crate::core::List::from_values(vec![
                    Value::Number(1.into()),
                    Value::Number(2.into()),
                ]))],
            );
            assert_eq!(
                crate::eval::eval_value(&context, &selection)
                    .expect("a non-empty search should select its first result"),
                Value::Number(1.into())
            );
        }

        let empty_if = Value::builtin_call(
            context.values(),
            Builtin::IfResult,
            vec![Value::List(crate::core::List::empty())],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &empty_if)
                .expect_err("an if search should never exhaust its else branch")
                .to_string(),
            "if search exhausted despite its required `else` branch"
        );

        let empty_match = Value::builtin_call(
            context.values(),
            Builtin::MatchResult,
            vec![Value::List(crate::core::List::empty())],
        );
        assert_eq!(
            crate::eval::eval_value(&context, &empty_match)
                .expect_err("an empty match should be diagnosed")
                .to_string(),
            "match search exhausted despite its compiler-provided fallback"
        );
    }

    #[test]
    fn conditional_machine_retains_deferred_front_without_replaying_result_list() {
        let context = context();
        let result_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&result_demands);
        let front = PromisedValue::new(context.values(), "conditional deferred front");
        let deferred_front = front.clone();
        let results = Value::semantic_thunk(context.values(), "conditional results", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::List(crate::core::List::from_thunk(
                ListThunk::Promised(deferred_front.clone()),
            )))
        });
        let selection = Value::builtin_call(context.values(), Builtin::IfResult, vec![results]);
        let Value::Lazy(selection_lazy) = &selection else {
            unreachable!("a saturated conditional builtin must remain lazy")
        };
        let selection_root = selection_lazy.root(context.values());

        let blocked = crate::eval::eval_value(&context, &selection)
            .expect_err("the deferred list front must suspend selection");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        assert_eq!(result_demands.load(Ordering::Relaxed), 1);
        context
            .values()
            .collect_managed_for_test()
            .expect("the conditional checkpoint must trace its deferred list-front work");
        crate::eval::eval_value(&context, &selection)
            .expect_err("a later route must resume the exact deferred front");
        assert_eq!(result_demands.load(Ordering::Relaxed), 1);

        crate::core::set_test_promise(
            context.values(),
            &front,
            Value::List(crate::core::List::from_values(vec![Value::Number(
                42.into(),
            )])),
        )
        .expect("the deferred front should accept its result list");
        context
            .values()
            .collect_managed_for_test()
            .expect("the assigned front must remain live beneath the checkpoint");
        assert_eq!(
            crate::eval::eval_value(&context, &selection)
                .expect("conditional selection must resume"),
            Value::Number(42.into())
        );
        assert_eq!(result_demands.load(Ordering::Relaxed), 1);
        drop(selection_root);
    }

    #[test]
    fn dictionary_machine_retains_completed_operand_across_collection() {
        let context = context();
        let first_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&first_demands);
        let first =
            Value::semantic_thunk(context.values(), "first dictionary operand", move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(Value::Dict(crate::core::Dict::new_sync()))
            });
        let second = PromisedValue::new(context.values(), "second dictionary operand");
        let union = Value::builtin_call(
            context.values(),
            Builtin::DictUnion,
            vec![first, Value::Promised(second.clone())],
        );
        let Value::Lazy(union_lazy) = &union else {
            unreachable!("a saturated dictionary builtin must remain lazy")
        };
        let union_root = union_lazy.root(context.values());

        let blocked = crate::eval::eval_value(&context, &union)
            .expect_err("the second dictionary operand must suspend");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
        context
            .values()
            .collect_managed_for_test()
            .expect("the dictionary checkpoint must trace its completed prefix");
        crate::eval::eval_value(&context, &union)
            .expect_err("a later route must retain the same dictionary dependency");
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);

        crate::core::set_test_promise(
            context.values(),
            &second,
            Value::Dict(crate::core::Dict::new_sync()),
        )
        .expect("the second dictionary operand should accept its assignment");
        context
            .values()
            .collect_managed_for_test()
            .expect("the assigned dictionary checkpoint must remain live");
        assert!(matches!(
            crate::eval::eval_value(&context, &union)
                .expect("dictionary union must resume"),
            Value::Dict(dict) if dict.is_empty()
        ));
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
        drop(union_root);
    }

    #[test]
    fn provenance_machine_suspends_then_preserves_its_demand_context() {
        let context = context();
        let origin = PromisedValue::new(context.values(), "origin operand");
        let inspection = Value::builtin_call(
            context.values(),
            Builtin::InspectOrigin,
            vec![Value::Promised(origin.clone())],
        );
        let Value::Lazy(inspection_lazy) = &inspection else {
            unreachable!("a saturated origin builtin must remain lazy")
        };
        let inspection_root = inspection_lazy.root(context.values());

        let blocked = crate::eval::eval_value(&context, &inspection)
            .expect_err("unassigned origin demand must remain resumable");
        assert!(blocked.unassigned_promise_root().is_some() || blocked.blocked_on().is_some());
        context
            .values()
            .collect_managed_for_test()
            .expect("the provenance checkpoint must survive route loss");
        crate::eval::eval_value(&context, &inspection)
            .expect_err("a later route must resume the exact origin dependency");

        let failed = Value::semantic_thunk(context.values(), "failed origin", |_| {
            Err(EvaluationHalt::new("origin production failed"))
        });
        crate::core::set_test_promise(context.values(), &origin, failed)
            .expect("the origin operand should accept its assignment");
        context
            .values()
            .collect_managed_for_test()
            .expect("the provenance checkpoint must retain its assigned origin");
        let failure = crate::eval::eval_value(&context, &inspection)
            .expect_err("origin demand failure must propagate")
            .into_permanent_failure();
        assert_eq!(failure.to_string(), "origin production failed");
        assert_eq!(
            failure.contexts(),
            [super::super::value::evaluation_context_frame(
                "compilation_origin"
            )]
        );
        drop(inspection_root);
    }

    #[test]
    fn comparison_machine_resumes_second_operand_without_replaying_the_first() {
        let context = context();
        let first_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&first_demands);
        let first =
            Value::semantic_thunk(context.values(), "first comparison operand", move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(Value::Number(7.into()))
            });
        let second = PromisedValue::new(context.values(), "second comparison operand");
        let comparison = Value::builtin_call(
            context.values(),
            Builtin::Equal,
            vec![first, Value::Promised(second.clone())],
        );

        crate::eval::eval_value(&context, &comparison)
            .expect_err("the second comparison operand must suspend");
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
        crate::core::set_test_promise(context.values(), &second, Value::Number(7.into()))
            .expect("the second operand should accept its assignment");
        crate::eval::eval_value(&context, &comparison).expect("comparison must resume");
        assert_eq!(first_demands.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn comparison_machine_resumes_a_lazy_list_tail_without_replaying_its_prefix() {
        let context = context();
        let prefix_demands = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&prefix_demands);
        let prefix = Value::semantic_thunk(context.values(), "comparison list prefix", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::Number(1.into()))
        });
        let tail = PromisedValue::new(context.values(), "comparison list tail");
        let left = Value::List(crate::core::List::concat(
            crate::core::List::from_values(vec![prefix]),
            crate::core::List::from_thunk(ListThunk::Promised(tail.clone())),
        ));
        let right = Value::List(crate::core::List::from_values(vec![
            Value::Number(1.into()),
            Value::Number(2.into()),
        ]));
        let comparison = Value::builtin_call(context.values(), Builtin::Equal, vec![left, right]);

        crate::eval::eval_value(&context, &comparison)
            .expect_err("the deferred list tail must suspend comparison");
        assert_eq!(prefix_demands.load(Ordering::Relaxed), 1);
        crate::core::set_test_promise(
            context.values(),
            &tail,
            Value::List(crate::core::List::from_values(vec![Value::Number(
                2.into(),
            )])),
        )
        .expect("the list tail should accept its assignment");
        crate::eval::eval_value(&context, &comparison).expect("list comparison must resume");
        assert_eq!(prefix_demands.load(Ordering::Relaxed), 1);
    }
}

//! Durable annotation recognition and execution.
//!
//! Annotation selection, payload validation, collection traversal, and
//! metadata extraction all retain exact WHNF progress. Reflection work is
//! only packaged here; the containing lazy publishes it after evaluator
//! access has closed.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{Builtin, BuiltinCall, EvaluationHalt, Key, LazyValue, List, Value, keys};
use crate::evaluation::{
    EvalContext, EvaluationPollContext, EvaluatorStepContext, WhnfOwnerPoll, poll_whnf_computation,
};
use crate::number::Number;
use crate::runtime::{RuntimeFailureRoot, RuntimeValueRoot};

use super::builtin_machine::BuiltinTaskPoll;
use super::list_machine::{ListFrontMachine, ListFrontPoll};
use super::value::evaluation_context_frame_in;
use super::whnf::WhnfComputation;

pub(crate) struct AnnotationBuiltinMachine {
    target: RuntimeValueRoot,
    phase: AnnotationPhase,
}

enum AnnotationPhase {
    Recognize(WhnfComputation),
    ParseAssertion {
        kind: AssertionKind,
        payload: WhnfComputation,
    },
    ParseAssertUnit(WhnfComputation),
    AssertionName {
        kind: AssertionKind,
        name: WhnfComputation,
        value: RuntimeValueRoot,
    },
    AssertionValue {
        kind: AssertionKind,
        name: String,
        value: WhnfComputation,
    },
    AssertUnit {
        value: WhnfComputation,
        diagnostic_context: Option<RuntimeValueRoot>,
        success: RuntimeValueRoot,
    },
    AssertUnitContext {
        received: &'static str,
        diagnostic_context: WhnfComputation,
    },
    TargetCollection {
        kind: CollectionKind,
        target: WhnfComputation,
    },
    CollectionItems {
        kind: CollectionKind,
        front: ListFrontMachine,
        values: Vec<RuntimeValueRoot>,
        bytes: Vec<u8>,
        item: Option<(WhnfComputation, RuntimeValueRoot)>,
    },
    MetadataTarget {
        kind: MetadataKind,
        function: RuntimeValueRoot,
        target: WhnfComputation,
    },
    MetadataItems {
        kind: MetadataKind,
        function: RuntimeValueRoot,
        front: ListFrontMachine,
        metadata: Vec<RuntimeValueRoot>,
        item_index: usize,
        item: Option<(WhnfComputation, RuntimeValueRoot)>,
    },
    ErrorMessage(WhnfComputation),
    ContextTarget {
        diagnostic_context: RuntimeValueRoot,
        target: WhnfComputation,
    },
    Ready(RuntimeValueRoot),
    Done,
}

#[derive(Clone, Copy)]
enum AssertionKind {
    Defined,
    Undefined,
}

#[derive(Clone, Copy)]
enum CollectionKind {
    Array,
    Binary,
    Deque,
}

impl CollectionKind {
    fn name(self) -> &'static str {
        match self {
            Self::Array => "array",
            Self::Binary => "binary",
            Self::Deque => "deque",
        }
    }
}

#[derive(Clone, Copy)]
enum MetadataKind {
    Pure,
    Reflection,
}

impl MetadataKind {
    fn name(self) -> &'static str {
        match self {
            Self::Pure => "meta_pure",
            Self::Reflection => "meta_refl",
        }
    }
}

enum RecognizedAnnotation {
    Assert(AssertionKind, RuntimeValueRoot),
    AssertUnit(RuntimeValueRoot),
    MetadataInitialize,
    Metadata(MetadataKind, RuntimeValueRoot),
    Collection(CollectionKind),
    Reflection(RuntimeValueRoot),
    Seq(RuntimeValueRoot),
    Spark(RuntimeValueRoot),
    Error,
    Context(RuntimeValueRoot),
    Unknown(String),
}

enum DemandResult {
    Ready(RuntimeValueRoot),
    Pending(BuiltinTaskPoll),
}

impl AnnotationBuiltinMachine {
    pub(crate) fn new(arguments: Vec<RuntimeValueRoot>) -> Self {
        let [annotation, target]: [RuntimeValueRoot; 2] = arguments
            .try_into()
            .expect("an annotation source retains two operands");
        Self {
            target,
            phase: AnnotationPhase::Recognize(WhnfComputation::from_root(annotation)),
        }
    }

    pub(crate) fn poll(
        &mut self,
        poll_context: &EvaluationPollContext,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> BuiltinTaskPoll {
        loop {
            let phase = std::mem::replace(&mut self.phase, AnnotationPhase::Done);
            match phase {
                AnnotationPhase::Recognize(mut annotation) => {
                    let annotation = match poll_demand(
                        &mut annotation,
                        poll_context,
                        durable_context,
                        step_budget,
                    ) {
                        DemandResult::Ready(annotation) => annotation,
                        DemandResult::Pending(poll) => {
                            self.phase = AnnotationPhase::Recognize(annotation);
                            return contextualize_poll(context, poll, "annotation");
                        }
                    };
                    let recognized = context
                        .with_value_access(|access| recognize_annotation(&access, &annotation));
                    self.phase = self.begin_recognized(context, durable_context, recognized);
                }
                AnnotationPhase::ParseAssertion { kind, mut payload } => {
                    let payload =
                        match poll_demand(&mut payload, poll_context, durable_context, step_budget)
                        {
                            DemandResult::Ready(payload) => payload,
                            DemandResult::Pending(poll) => {
                                self.phase = AnnotationPhase::ParseAssertion { kind, payload };
                                return poll;
                            }
                        };
                    let parsed = context.with_value_access(|access| {
                        let Value::Dict(payload) = access.clone_root(&payload) else {
                            return None;
                        };
                        let name = payload.get(&*keys::NAME)?;
                        let value = payload.get(&*keys::VALUE)?;
                        Some((
                            access.values().root_runtime_value(name.clone()),
                            access.values().root_runtime_value(value.clone()),
                        ))
                    });
                    let Some((name, value)) = parsed else {
                        return annotation_error(
                            context,
                            format!("invalid `{}` annotation payload", assertion_name(kind)),
                        );
                    };
                    self.phase = AnnotationPhase::AssertionName {
                        kind,
                        name: WhnfComputation::from_root(name),
                        value,
                    };
                }
                AnnotationPhase::ParseAssertUnit(mut payload) => {
                    let payload =
                        match poll_demand(&mut payload, poll_context, durable_context, step_budget)
                        {
                            DemandResult::Ready(payload) => payload,
                            DemandResult::Pending(poll) => {
                                self.phase = AnnotationPhase::ParseAssertUnit(payload);
                                return poll;
                            }
                        };
                    let parsed = context.with_value_access(|access| {
                        let Value::Dict(payload) = access.clone_root(&payload) else {
                            return None;
                        };
                        let value = payload.get(&*keys::VALUE)?;
                        Some((
                            access.values().root_runtime_value(value.clone()),
                            payload
                                .get(&*keys::CONTEXT)
                                .map(|value| access.values().root_runtime_value(value.clone())),
                        ))
                    });
                    let Some((value, diagnostic_context)) = parsed else {
                        return annotation_error(
                            context,
                            "invalid `assert_unit` annotation payload",
                        );
                    };
                    self.phase = AnnotationPhase::AssertUnit {
                        value: WhnfComputation::from_root(value),
                        diagnostic_context,
                        success: self.target.clone(),
                    };
                }
                AnnotationPhase::AssertionName {
                    kind,
                    mut name,
                    value,
                } => {
                    let evaluated =
                        match poll_demand(&mut name, poll_context, durable_context, step_budget) {
                            DemandResult::Ready(name) => name,
                            DemandResult::Pending(poll) => {
                                self.phase = AnnotationPhase::AssertionName { kind, name, value };
                                return poll;
                            }
                        };
                    let name = context.with_value_access(|access| {
                        annotation_name(access.values(), access.clone_root(&evaluated))
                    });
                    self.phase = AnnotationPhase::AssertionValue {
                        kind,
                        name,
                        value: WhnfComputation::from_root(value),
                    };
                }
                AnnotationPhase::AssertionValue {
                    kind,
                    name,
                    mut value,
                } => {
                    let value =
                        match poll_demand(&mut value, poll_context, durable_context, step_budget) {
                            DemandResult::Ready(value) => value,
                            DemandResult::Pending(poll) => {
                                self.phase = AnnotationPhase::AssertionValue { kind, name, value };
                                return poll;
                            }
                        };
                    let defined = context.with_value_access(|access| {
                        !matches!(access.clone_root(&value), Value::Dict(dict) if dict.is_empty())
                    });
                    let passes = match kind {
                        AssertionKind::Defined => defined,
                        AssertionKind::Undefined => !defined,
                    };
                    if passes {
                        return BuiltinTaskPoll::Ready(self.target.clone());
                    }
                    let message = match kind {
                        AssertionKind::Defined => {
                            format!("cannot override `{name}` because it is not defined")
                        }
                        AssertionKind::Undefined => {
                            format!("cannot introduce `{name}` because it is already defined")
                        }
                    };
                    return annotation_error(context, message);
                }
                AnnotationPhase::AssertUnit {
                    mut value,
                    diagnostic_context,
                    success,
                } => {
                    let value =
                        match poll_demand(&mut value, poll_context, durable_context, step_budget) {
                            DemandResult::Ready(value) => value,
                            DemandResult::Pending(poll) => {
                                self.phase = AnnotationPhase::AssertUnit {
                                    value,
                                    diagnostic_context,
                                    success,
                                };
                                return poll;
                            }
                        };
                    let (is_unit, received) = context.with_value_access(|access| {
                        let value = access.clone_root(&value);
                        (
                            access
                                .values()
                                .same_representation(&value, &durable_context.values().unit()),
                            value.diagnostic_kind_name(),
                        )
                    });
                    if is_unit {
                        return BuiltinTaskPoll::Ready(success);
                    }
                    let Some(diagnostic_context) = diagnostic_context else {
                        return permanent_failure(
                            context,
                            format!("unit expected, received {received}"),
                        );
                    };
                    self.phase = AnnotationPhase::AssertUnitContext {
                        received,
                        diagnostic_context: WhnfComputation::from_root(diagnostic_context),
                    };
                }
                AnnotationPhase::AssertUnitContext {
                    received,
                    mut diagnostic_context,
                } => {
                    let diagnostic_context = match poll_demand(
                        &mut diagnostic_context,
                        poll_context,
                        durable_context,
                        step_budget,
                    ) {
                        DemandResult::Ready(value) => value,
                        DemandResult::Pending(poll) => {
                            self.phase = AnnotationPhase::AssertUnitContext {
                                received,
                                diagnostic_context,
                            };
                            return poll;
                        }
                    };
                    let diagnostic_context =
                        context.with_value_access(|access| access.clone_root(&diagnostic_context));
                    let Value::Binary(diagnostic_context) = diagnostic_context else {
                        return permanent_failure(
                            context,
                            "unit assertion diagnostic context must be text",
                        );
                    };
                    return permanent_failure(
                        context,
                        format!(
                            "{}: unit expected, received {received}",
                            String::from_utf8_lossy(&diagnostic_context)
                        ),
                    );
                }
                AnnotationPhase::TargetCollection { kind, mut target } => {
                    let target = match poll_demand(
                        &mut target,
                        poll_context,
                        durable_context,
                        step_budget,
                    ) {
                        DemandResult::Ready(target) => target,
                        DemandResult::Pending(poll) => {
                            self.phase = AnnotationPhase::TargetCollection { kind, target };
                            return poll;
                        }
                    };
                    let shape =
                        context.with_value_access(|access| match access.clone_root(&target) {
                            Value::Binary(bytes) => CollectionShape::Binary(bytes),
                            Value::List(_) => CollectionShape::List,
                            other => CollectionShape::Other(format!("{other:?}")),
                        });
                    match (kind, shape) {
                        (CollectionKind::Array, CollectionShape::Binary(bytes)) => {
                            let result = context.with_value_access(|access| {
                                let values = bytes
                                    .iter()
                                    .map(|byte| Value::Number(Number::from_u8(*byte)))
                                    .collect();
                                access
                                    .values()
                                    .root_runtime_value(Value::List(List::from_values(values)))
                            });
                            return BuiltinTaskPoll::Ready(result);
                        }
                        (CollectionKind::Binary, CollectionShape::Binary(_)) => {
                            return BuiltinTaskPoll::Ready(target);
                        }
                        (_, CollectionShape::List) => {
                            self.phase = AnnotationPhase::CollectionItems {
                                kind,
                                front: ListFrontMachine::unowned(target),
                                values: Vec::new(),
                                bytes: Vec::new(),
                                item: None,
                            };
                        }
                        (CollectionKind::Deque, CollectionShape::Binary(bytes)) => {
                            let rendered = format!("{:?}", Value::Binary(bytes));
                            let expected = match kind {
                                CollectionKind::Array | CollectionKind::Binary => "list or binary",
                                CollectionKind::Deque => "list",
                            };
                            return annotation_error(
                                context,
                                format!(
                                    "`{}` annotation requires a {expected} target, got {rendered}",
                                    kind.name()
                                ),
                            );
                        }
                        (_, CollectionShape::Other(rendered)) => {
                            let expected = match kind {
                                CollectionKind::Array | CollectionKind::Binary => "list or binary",
                                CollectionKind::Deque => "list",
                            };
                            return annotation_error(
                                context,
                                format!(
                                    "`{}` annotation requires a {expected} target, got {rendered}",
                                    kind.name()
                                ),
                            );
                        }
                    }
                }
                AnnotationPhase::CollectionItems {
                    kind,
                    mut front,
                    mut values,
                    mut bytes,
                    mut item,
                } => {
                    if let Some((mut demand, tail)) = item.take() {
                        let value = match poll_demand(
                            &mut demand,
                            poll_context,
                            durable_context,
                            step_budget,
                        ) {
                            DemandResult::Ready(value) => value,
                            DemandResult::Pending(poll) => {
                                self.phase = AnnotationPhase::CollectionItems {
                                    kind,
                                    front,
                                    values,
                                    bytes,
                                    item: Some((demand, tail)),
                                };
                                return contextualize_poll(context, poll, "binary_extraction");
                            }
                        };
                        let value = context.with_value_access(|access| access.clone_root(&value));
                        let Value::Number(number) = value else {
                            return permanent_failure(
                                context,
                                format!(
                                    "`binary` annotation requires list items to be byte integers, got {value:?}"
                                ),
                            );
                        };
                        let Some(byte) = number.to_u8_if_integer() else {
                            return permanent_failure(
                                context,
                                format!(
                                    "`binary` annotation cannot encode number `{number}` as a byte"
                                ),
                            );
                        };
                        bytes.push(byte);
                        front = ListFrontMachine::unowned(tail);
                        self.phase = AnnotationPhase::CollectionItems {
                            kind,
                            front,
                            values,
                            bytes,
                            item: None,
                        };
                        return BuiltinTaskPoll::Yielded;
                    }

                    match front.poll(poll_context, context, durable_context, step_budget) {
                        ListFrontPoll::Ready(Some((value, tail))) => {
                            if matches!(kind, CollectionKind::Binary) {
                                item = Some((WhnfComputation::from_root(value), tail));
                            } else {
                                values.push(value);
                                front = ListFrontMachine::unowned(tail);
                            }
                            self.phase = AnnotationPhase::CollectionItems {
                                kind,
                                front,
                                values,
                                bytes,
                                item,
                            };
                            return BuiltinTaskPoll::Yielded;
                        }
                        ListFrontPoll::Ready(None) => {
                            let result = context.with_value_access(|access| {
                                let value = match kind {
                                    CollectionKind::Binary => Value::Binary(Bytes::from(bytes)),
                                    CollectionKind::Array => {
                                        let values = values
                                            .iter()
                                            .map(|value| access.clone_root(value))
                                            .collect();
                                        Value::List(List::from_values(values))
                                    }
                                    CollectionKind::Deque => {
                                        let values = values
                                            .iter()
                                            .map(|value| access.clone_root(value))
                                            .collect();
                                        Value::List(List::from_values_balanced(values))
                                    }
                                };
                                access.values().root_runtime_value(value)
                            });
                            return BuiltinTaskPoll::Ready(result);
                        }
                        ListFrontPoll::Pending(dependency) => {
                            self.phase = AnnotationPhase::CollectionItems {
                                kind,
                                front,
                                values,
                                bytes,
                                item,
                            };
                            let poll = BuiltinTaskPoll::Pending(dependency);
                            return if matches!(kind, CollectionKind::Binary) {
                                contextualize_poll(context, poll, "binary_extraction")
                            } else {
                                poll
                            };
                        }
                        ListFrontPoll::Yielded => {
                            self.phase = AnnotationPhase::CollectionItems {
                                kind,
                                front,
                                values,
                                bytes,
                                item,
                            };
                            return BuiltinTaskPoll::Yielded;
                        }
                        ListFrontPoll::Failed(failure) => {
                            let poll = BuiltinTaskPoll::Failed(failure);
                            return if matches!(kind, CollectionKind::Binary) {
                                contextualize_poll(context, poll, "binary_extraction")
                            } else {
                                poll
                            };
                        }
                    }
                }
                AnnotationPhase::MetadataTarget {
                    kind,
                    function,
                    mut target,
                } => {
                    let target = match poll_demand(
                        &mut target,
                        poll_context,
                        durable_context,
                        step_budget,
                    ) {
                        DemandResult::Ready(target) => target,
                        DemandResult::Pending(poll) => {
                            self.phase = AnnotationPhase::MetadataTarget {
                                kind,
                                function,
                                target,
                            };
                            return poll;
                        }
                    };
                    let is_list = context.with_value_access(|access| {
                        matches!(access.clone_root(&target), Value::List(_))
                    });
                    if !is_list {
                        return permanent_failure(
                            context,
                            format!(
                                "`{}` annotation requires a list of sealed metadata carriers",
                                kind.name()
                            ),
                        );
                    }
                    self.phase = AnnotationPhase::MetadataItems {
                        kind,
                        function,
                        front: ListFrontMachine::unowned(target),
                        metadata: Vec::new(),
                        item_index: 0,
                        item: None,
                    };
                }
                AnnotationPhase::MetadataItems {
                    kind,
                    function,
                    mut front,
                    mut metadata,
                    mut item_index,
                    mut item,
                } => {
                    if let Some((mut demand, tail)) = item.take() {
                        let carrier = match poll_demand(
                            &mut demand,
                            poll_context,
                            durable_context,
                            step_budget,
                        ) {
                            DemandResult::Ready(carrier) => carrier,
                            DemandResult::Pending(poll) => {
                                self.phase = AnnotationPhase::MetadataItems {
                                    kind,
                                    function,
                                    front,
                                    metadata,
                                    item_index,
                                    item: Some((demand, tail)),
                                };
                                return poll;
                            }
                        };
                        let extracted = context.with_value_access(|access| {
                            let carrier = access.clone_root(&carrier);
                            let received = carrier.diagnostic_kind_name();
                            carrier.associated_metadata().map_or_else(
                                || Err(received),
                                |metadata| Ok(access.values().root_runtime_value(metadata)),
                            )
                        });
                        let extracted = match extracted {
                            Ok(extracted) => extracted,
                            Err(received) => {
                                return permanent_failure(
                                    context,
                                    format!(
                                        "`{}` annotation item {item_index} must be a sealed metadata carrier, received {received}",
                                        kind.name()
                                    ),
                                );
                            }
                        };
                        metadata.push(extracted);
                        item_index += 1;
                        front = ListFrontMachine::unowned(tail);
                        self.phase = AnnotationPhase::MetadataItems {
                            kind,
                            function,
                            front,
                            metadata,
                            item_index,
                            item: None,
                        };
                        return BuiltinTaskPoll::Yielded;
                    }

                    match front.poll(poll_context, context, durable_context, step_budget) {
                        ListFrontPoll::Ready(Some((carrier, tail))) => {
                            item = Some((WhnfComputation::from_root(carrier), tail));
                            self.phase = AnnotationPhase::MetadataItems {
                                kind,
                                function,
                                front,
                                metadata,
                                item_index,
                                item,
                            };
                            return BuiltinTaskPoll::Yielded;
                        }
                        ListFrontPoll::Ready(None) => {
                            return finish_metadata_update(context, kind, &function, &metadata);
                        }
                        ListFrontPoll::Pending(dependency) => {
                            self.phase = AnnotationPhase::MetadataItems {
                                kind,
                                function,
                                front,
                                metadata,
                                item_index,
                                item,
                            };
                            return BuiltinTaskPoll::Pending(dependency);
                        }
                        ListFrontPoll::Yielded => {
                            self.phase = AnnotationPhase::MetadataItems {
                                kind,
                                function,
                                front,
                                metadata,
                                item_index,
                                item,
                            };
                            return BuiltinTaskPoll::Yielded;
                        }
                        ListFrontPoll::Failed(failure) => {
                            return BuiltinTaskPoll::Failed(failure);
                        }
                    }
                }
                AnnotationPhase::ErrorMessage(mut target) => {
                    let target = match poll_demand(
                        &mut target,
                        poll_context,
                        durable_context,
                        step_budget,
                    ) {
                        DemandResult::Ready(target) => target,
                        DemandResult::Pending(poll) => {
                            self.phase = AnnotationPhase::ErrorMessage(target);
                            return contextualize_poll(context, poll, "error_message");
                        }
                    };
                    let failure = context.with_value_access(|access| {
                        EvaluationHalt::from_value(access.values(), access.clone_root(&target))
                    });
                    return BuiltinTaskPoll::Failed(
                        context.root_failure(failure.into_permanent_failure()),
                    );
                }
                AnnotationPhase::ContextTarget {
                    diagnostic_context,
                    mut target,
                } => {
                    let target = match poll_demand(
                        &mut target,
                        poll_context,
                        durable_context,
                        step_budget,
                    ) {
                        DemandResult::Ready(target) => target,
                        DemandResult::Pending(BuiltinTaskPoll::Failed(failure)) => {
                            let failure = context.with_value_access(|access| {
                                EvaluationHalt::failure(failure.into_failure()).with_context(
                                    access.values(),
                                    access.clone_root(&diagnostic_context),
                                )
                            });
                            return BuiltinTaskPoll::Failed(
                                context.root_failure(failure.into_permanent_failure()),
                            );
                        }
                        DemandResult::Pending(poll) => {
                            self.phase = AnnotationPhase::ContextTarget {
                                diagnostic_context,
                                target,
                            };
                            return poll;
                        }
                    };
                    return BuiltinTaskPoll::Ready(target);
                }
                AnnotationPhase::Ready(value) => return BuiltinTaskPoll::Ready(value),
                AnnotationPhase::Done => {
                    unreachable!("a completed annotation machine cannot be polled again")
                }
            }
        }
    }

    fn begin_recognized(
        &self,
        context: &EvaluatorStepContext<'_>,
        durable_context: &EvalContext,
        recognized: RecognizedAnnotation,
    ) -> AnnotationPhase {
        match recognized {
            RecognizedAnnotation::Assert(kind, payload) => AnnotationPhase::ParseAssertion {
                kind,
                payload: WhnfComputation::from_root(payload),
            },
            RecognizedAnnotation::AssertUnit(payload) => {
                AnnotationPhase::ParseAssertUnit(WhnfComputation::from_root(payload))
            }
            RecognizedAnnotation::MetadataInitialize => AnnotationPhase::AssertUnit {
                value: WhnfComputation::from_root(self.target.clone()),
                diagnostic_context: None,
                success: context.root_value(durable_context.values().initial_metadata()),
            },
            RecognizedAnnotation::Metadata(kind, function) => AnnotationPhase::MetadataTarget {
                kind,
                function,
                target: WhnfComputation::from_root(self.target.clone()),
            },
            RecognizedAnnotation::Collection(kind) => AnnotationPhase::TargetCollection {
                kind,
                target: WhnfComputation::from_root(self.target.clone()),
            },
            RecognizedAnnotation::Reflection(effect) => {
                AnnotationPhase::Ready(context.with_value_access(|access| {
                    let effect = access.clone_root(&effect);
                    let target = access.clone_root(&self.target);
                    let lazy = context.construct_lazy(|access| {
                        LazyValue::from_reflection_gate_in(access, effect, target)
                    });
                    access.values().root_runtime_value(Value::Lazy(lazy))
                }))
            }
            RecognizedAnnotation::Seq(value) => AnnotationPhase::Ready(root_builtin(
                context,
                Builtin::Seq,
                &[value, self.target.clone()],
            )),
            RecognizedAnnotation::Spark(value) => AnnotationPhase::Ready(root_builtin(
                context,
                Builtin::Spark,
                &[value, self.target.clone()],
            )),
            RecognizedAnnotation::Error => {
                AnnotationPhase::ErrorMessage(WhnfComputation::from_root(self.target.clone()))
            }
            RecognizedAnnotation::Context(diagnostic_context) => AnnotationPhase::ContextTarget {
                diagnostic_context,
                target: WhnfComputation::from_root(self.target.clone()),
            },
            RecognizedAnnotation::Unknown(rendered) => {
                warn_unknown_annotation(&rendered);
                AnnotationPhase::Ready(self.target.clone())
            }
        }
    }
}

enum CollectionShape {
    Binary(Bytes),
    List,
    Other(String),
}

fn recognize_annotation(
    access: &crate::evaluation::EvaluationValueAccess<'_>,
    annotation: &RuntimeValueRoot,
) -> RecognizedAnnotation {
    let annotation = access.clone_root(annotation);
    if let Value::Atom(atom) = &annotation {
        return recognize_simple_annotation(atom)
            .unwrap_or_else(|| RecognizedAnnotation::Unknown(format!("{annotation:?}")));
    }

    let Value::Dict(annotation_dict) = &annotation else {
        return RecognizedAnnotation::Unknown(format!("{annotation:?}"));
    };
    let Some((tag, payload)) = annotation_dict.iter().next() else {
        return RecognizedAnnotation::Unknown(format!("{annotation:?}"));
    };
    if annotation_dict.iter().nth(1).is_some() {
        return RecognizedAnnotation::Unknown(format!("{annotation:?}"));
    }
    let payload_root = || access.values().root_runtime_value(payload.clone());
    match key_atom_name(tag) {
        Some("refl") => RecognizedAnnotation::Reflection(payload_root()),
        Some("seq") => RecognizedAnnotation::Seq(payload_root()),
        Some("spark") => RecognizedAnnotation::Spark(payload_root()),
        Some("context") => RecognizedAnnotation::Context(payload_root()),
        Some("meta_pure") => RecognizedAnnotation::Metadata(MetadataKind::Pure, payload_root()),
        Some("meta_refl") => {
            RecognizedAnnotation::Metadata(MetadataKind::Reflection, payload_root())
        }
        Some("assert_defined") => {
            RecognizedAnnotation::Assert(AssertionKind::Defined, payload_root())
        }
        Some("assert_undefined") => {
            RecognizedAnnotation::Assert(AssertionKind::Undefined, payload_root())
        }
        Some("assert_unit") => RecognizedAnnotation::AssertUnit(payload_root()),
        _ if matches!(payload, Value::Dict(dict) if dict.is_empty()) => {
            if let Key::Atom(atom) = tag {
                recognize_simple_annotation(atom)
                    .unwrap_or_else(|| RecognizedAnnotation::Unknown(format!("{annotation:?}")))
            } else {
                RecognizedAnnotation::Unknown(format!("{annotation:?}"))
            }
        }
        _ => RecognizedAnnotation::Unknown(format!("{annotation:?}")),
    }
}

fn recognize_simple_annotation(atom: &crate::core::Atom) -> Option<RecognizedAnnotation> {
    match atom_name(atom)? {
        "deque" => Some(RecognizedAnnotation::Collection(CollectionKind::Deque)),
        "binary" => Some(RecognizedAnnotation::Collection(CollectionKind::Binary)),
        "array" => Some(RecognizedAnnotation::Collection(CollectionKind::Array)),
        "error" => Some(RecognizedAnnotation::Error),
        "meta_init" => Some(RecognizedAnnotation::MetadataInitialize),
        _ => None,
    }
}

fn key_atom_name(key: &Key) -> Option<&str> {
    let Key::Atom(atom) = key else {
        return None;
    };
    atom_name(atom)
}

fn atom_name(atom: &crate::core::Atom) -> Option<&str> {
    match atom.key() {
        Key::Binary(bytes) => std::str::from_utf8(bytes).ok(),
        _ => None,
    }
}

fn annotation_name(_access: &crate::core::RuntimeValueAccess<'_>, value: Value) -> String {
    match value {
        Value::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Value::Atom(atom) => atom_name(&atom)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{atom:?}")),
        Value::Number(number) => number.to_string(),
        other => format!("{other:?}"),
    }
}

fn assertion_name(kind: AssertionKind) -> &'static str {
    match kind {
        AssertionKind::Defined => "assert_defined",
        AssertionKind::Undefined => "assert_undefined",
    }
}

fn finish_metadata_update(
    context: &EvaluatorStepContext<'_>,
    kind: MetadataKind,
    function: &RuntimeValueRoot,
    metadata: &[RuntimeValueRoot],
) -> BuiltinTaskPoll {
    let result = context.with_value_access(|access| {
        let function = access.clone_root(function);
        let metadata = metadata
            .iter()
            .map(|value| access.clone_root(value))
            .collect::<Vec<_>>();
        let output_count = metadata.len();
        let effect = Value::Lazy(context.construct_lazy(|access| {
            LazyValue::from_application_in(
                access,
                function,
                Arc::from([Value::List(List::from_values(metadata))]),
            )
        }));
        let updates = match kind {
            MetadataKind::Pure => effect,
            MetadataKind::Reflection => {
                let Value::Lazy(lazy) = context.construct_lazy_value(|access| {
                    Value::reflection_task_result_in(access, effect)
                }) else {
                    unreachable!("a reflection task result is always lazy")
                };
                Value::Lazy(lazy)
            }
        };
        let projection_context = Value::Dict(crate::core::Dict::new_sync().insert(
            (*keys::CONTEXT).clone(),
            evaluation_context_frame_in(access.values(), "wrap_metadata"),
        ));
        let carriers = (0..output_count)
            .map(|index| {
                let projection = Value::Lazy(context.construct_lazy(|access| {
                    LazyValue::from_builtin_in(
                        access,
                        BuiltinCall {
                            builtin: Builtin::ListAt,
                            arguments: Arc::from([
                                Value::Number(Number::from_usize(index)),
                                updates.clone(),
                            ]),
                        },
                    )
                }));
                Value::metadata_carrier(Value::Lazy(context.construct_lazy(|access| {
                    LazyValue::from_builtin_in(
                        access,
                        BuiltinCall {
                            builtin: Builtin::Anno,
                            arguments: Arc::from([projection_context.clone(), projection]),
                        },
                    )
                })))
            })
            .collect();
        access
            .values()
            .root_runtime_value(Value::List(List::from_values(carriers)))
    });
    BuiltinTaskPoll::Ready(result)
}

fn root_builtin(
    context: &EvaluatorStepContext<'_>,
    builtin: Builtin,
    arguments: &[RuntimeValueRoot],
) -> RuntimeValueRoot {
    context.with_value_access(|access| {
        let arguments = arguments
            .iter()
            .map(|value| access.clone_root(value))
            .collect::<Vec<_>>();
        let lazy = context.construct_lazy(|access| {
            LazyValue::from_builtin_in(
                access,
                BuiltinCall {
                    builtin,
                    arguments: Arc::from(arguments),
                },
            )
        });
        access.values().root_runtime_value(Value::Lazy(lazy))
    })
}

fn annotation_error(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<String>,
) -> BuiltinTaskPoll {
    BuiltinTaskPoll::Ready(annotation_error_root(context, message))
}

fn annotation_error_root(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<String>,
) -> RuntimeValueRoot {
    let message = message.into();
    context.with_value_access(|access| {
        let lazy = context.construct_lazy(|access| LazyValue::error_in(access, message));
        access.values().root_runtime_value(Value::Lazy(lazy))
    })
}

fn permanent_failure(
    context: &EvaluatorStepContext<'_>,
    message: impl Into<String>,
) -> BuiltinTaskPoll {
    BuiltinTaskPoll::Failed(
        context.root_failure(EvaluationHalt::new(message).into_permanent_failure()),
    )
}

fn poll_demand(
    demand: &mut WhnfComputation,
    poll_context: &EvaluationPollContext,
    durable_context: &EvalContext,
    step_budget: &mut crate::evaluation::EvaluationStepBudget,
) -> DemandResult {
    match poll_whnf_computation(demand, poll_context, durable_context, step_budget) {
        WhnfOwnerPoll::Ready(value) => DemandResult::Ready(value),
        WhnfOwnerPoll::Pending(dependency) => {
            DemandResult::Pending(BuiltinTaskPoll::Pending(dependency))
        }
        WhnfOwnerPoll::Yielded => DemandResult::Pending(BuiltinTaskPoll::Yielded),
        WhnfOwnerPoll::Failed(failure) => DemandResult::Pending(BuiltinTaskPoll::Failed(failure)),
        WhnfOwnerPoll::External(boundary) => {
            unreachable!("annotation demand produced an external {boundary:?} boundary")
        }
    }
}

fn contextualize_poll(
    context: &EvaluatorStepContext<'_>,
    poll: BuiltinTaskPoll,
    operation: &str,
) -> BuiltinTaskPoll {
    let BuiltinTaskPoll::Failed(failure) = poll else {
        return poll;
    };
    BuiltinTaskPoll::Failed(contextual_failure(context, failure, operation))
}

fn contextual_failure(
    context: &EvaluatorStepContext<'_>,
    failure: RuntimeFailureRoot,
    operation: &str,
) -> RuntimeFailureRoot {
    let failure = context.with_value_access(|access| {
        EvaluationHalt::failure(failure.into_failure()).with_context(
            access.values(),
            evaluation_context_frame_in(access.values(), operation),
        )
    });
    context.root_failure(failure.into_permanent_failure())
}

fn warn_unknown_annotation(rendered: &str) {
    eprintln!("warning: unrecognized annotation encountered: {rendered}");
}

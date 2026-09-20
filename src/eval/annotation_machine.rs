//! Regional annotation recognition and execution.
//!
//! Annotation selection, payload validation, collection traversal, and
//! metadata extraction retain exact WHNF progress beneath the lazy-owned
//! builtin checkpoint. Reflection work is only packaged as an ordinary traced
//! lazy value here; its source publishes the autonomous task after evaluator
//! access has closed.

use std::sync::Arc;

use bytes::Bytes;
use glam_gc::Visitor;

use crate::core::{
    Builtin, BuiltinCall, EvaluationFailure, Key, LazyId, LazyValue, List, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};
use crate::number::Number;

use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::value::evaluation_context_frame_in;
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalAnnotationMachine {
    target: Value,
    phase: RegionalAnnotationPhase,
    source_owner: LazyId,
}

enum RegionalAnnotationPhase {
    Recognize(RegionalWhnfWork),
    ParseAssertion {
        kind: AssertionKind,
        payload: RegionalWhnfWork,
    },
    ParseAssertUnit(RegionalWhnfWork),
    AssertionName {
        kind: AssertionKind,
        name: RegionalWhnfWork,
        value: Value,
    },
    AssertionValue {
        kind: AssertionKind,
        name: String,
        value: RegionalWhnfWork,
    },
    AssertUnit {
        value: RegionalWhnfWork,
        diagnostic_context: Option<Value>,
        success: Value,
    },
    AssertUnitContext {
        received: &'static str,
        diagnostic_context: RegionalWhnfWork,
    },
    TargetCollection {
        kind: CollectionKind,
        target: RegionalWhnfWork,
    },
    CollectionItems {
        kind: CollectionKind,
        front: RegionalListFront,
        values: Vec<Value>,
        bytes: Vec<u8>,
        item: Option<(RegionalWhnfWork, Value)>,
    },
    MetadataTarget {
        kind: MetadataKind,
        function: Value,
        target: RegionalWhnfWork,
    },
    MetadataItems {
        kind: MetadataKind,
        function: Value,
        front: RegionalListFront,
        metadata: Vec<Value>,
        item_index: usize,
        item: Option<(RegionalWhnfWork, Value)>,
    },
    ErrorMessage(RegionalWhnfWork),
    ContextTarget {
        diagnostic_context: Value,
        target: RegionalWhnfWork,
    },
    Ready(Value),
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
    Assert(AssertionKind, Value),
    AssertUnit(Value),
    MetadataInitialize,
    Metadata(MetadataKind, Value),
    Collection(CollectionKind),
    Reflection(Value),
    Seq(Value),
    Spark(Value),
    Error,
    Context(Value),
    Unknown(String),
}

enum RegionalDemandResult {
    Ready(Value),
    Pending(RegionalBuiltinPoll),
}

impl RegionalAnnotationMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [annotation, target] = arguments else {
            panic!("an annotation source retains two operands")
        };
        Self {
            target: access.values().duplicate_value(target),
            phase: RegionalAnnotationPhase::Recognize(
                RegionalWhnfWork::from_focus(access, access.values().duplicate_value(annotation))
                    .with_source_owner(source_owner),
            ),
            source_owner,
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        loop {
            let phase = std::mem::replace(&mut self.phase, RegionalAnnotationPhase::Done);
            match phase {
                RegionalAnnotationPhase::Recognize(mut annotation) => {
                    let annotation = match poll_demand_in(&mut annotation, access, step_budget) {
                        RegionalDemandResult::Ready(annotation) => annotation,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::Recognize(annotation);
                            return contextualize_poll_in(access, poll, "annotation");
                        }
                    };
                    let recognized = recognize_annotation(access, annotation);
                    self.phase = self.begin_recognized(access, recognized);
                }
                RegionalAnnotationPhase::ParseAssertion { kind, mut payload } => {
                    let payload = match poll_demand_in(&mut payload, access, step_budget) {
                        RegionalDemandResult::Ready(payload) => payload,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::ParseAssertion { kind, payload };
                            return poll;
                        }
                    };
                    let Value::Dict(payload) = payload else {
                        return annotation_error_in(
                            access,
                            format!("invalid `{}` annotation payload", assertion_name(kind)),
                        );
                    };
                    let parsed = payload
                        .get(&*keys::NAME)
                        .zip(payload.get(&*keys::VALUE))
                        .map(|(name, value)| {
                            (
                                access.values().duplicate_value(name),
                                access.values().duplicate_value(value),
                            )
                        });
                    let Some((name, value)) = parsed else {
                        return annotation_error_in(
                            access,
                            format!("invalid `{}` annotation payload", assertion_name(kind)),
                        );
                    };
                    self.phase = RegionalAnnotationPhase::AssertionName {
                        kind,
                        name: self.demand_in(access, name),
                        value,
                    };
                }
                RegionalAnnotationPhase::ParseAssertUnit(mut payload) => {
                    let payload = match poll_demand_in(&mut payload, access, step_budget) {
                        RegionalDemandResult::Ready(payload) => payload,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::ParseAssertUnit(payload);
                            return poll;
                        }
                    };
                    let Value::Dict(payload) = payload else {
                        return annotation_error_in(
                            access,
                            "invalid `assert_unit` annotation payload",
                        );
                    };
                    let parsed = payload.get(&*keys::VALUE).map(|value| {
                        (
                            access.values().duplicate_value(value),
                            payload
                                .get(&*keys::CONTEXT)
                                .map(|value| access.values().duplicate_value(value)),
                        )
                    });
                    let Some((value, diagnostic_context)) = parsed else {
                        return annotation_error_in(
                            access,
                            "invalid `assert_unit` annotation payload",
                        );
                    };
                    self.phase = RegionalAnnotationPhase::AssertUnit {
                        value: self.demand_in(access, value),
                        diagnostic_context,
                        success: access.values().duplicate_value(&self.target),
                    };
                }
                RegionalAnnotationPhase::AssertionName {
                    kind,
                    mut name,
                    value,
                } => {
                    let evaluated = match poll_demand_in(&mut name, access, step_budget) {
                        RegionalDemandResult::Ready(name) => name,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase =
                                RegionalAnnotationPhase::AssertionName { kind, name, value };
                            return poll;
                        }
                    };
                    let name = annotation_name(access, evaluated);
                    self.phase = RegionalAnnotationPhase::AssertionValue {
                        kind,
                        name,
                        value: self.demand_in(access, value),
                    };
                }
                RegionalAnnotationPhase::AssertionValue {
                    kind,
                    name,
                    mut value,
                } => {
                    let value = match poll_demand_in(&mut value, access, step_budget) {
                        RegionalDemandResult::Ready(value) => value,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase =
                                RegionalAnnotationPhase::AssertionValue { kind, name, value };
                            return poll;
                        }
                    };
                    let defined = !matches!(value, Value::Dict(dict) if dict.is_empty());
                    let passes = match kind {
                        AssertionKind::Defined => defined,
                        AssertionKind::Undefined => !defined,
                    };
                    if passes {
                        return RegionalBuiltinPoll::Ready(
                            access.values().duplicate_value(&self.target),
                        );
                    }
                    let message = match kind {
                        AssertionKind::Defined => {
                            format!("cannot override `{name}` because it is not defined")
                        }
                        AssertionKind::Undefined => {
                            format!("cannot introduce `{name}` because it is already defined")
                        }
                    };
                    return annotation_error_in(access, message);
                }
                RegionalAnnotationPhase::AssertUnit {
                    mut value,
                    diagnostic_context,
                    success,
                } => {
                    let value = match poll_demand_in(&mut value, access, step_budget) {
                        RegionalDemandResult::Ready(value) => value,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::AssertUnit {
                                value,
                                diagnostic_context,
                                success,
                            };
                            return poll;
                        }
                    };
                    let is_unit = matches!(
                        &value,
                        Value::Atom(atom) if *atom == crate::core::Atom::from_key(&keys::UNIT)
                    );
                    let received = value.diagnostic_kind_name();
                    if is_unit {
                        return RegionalBuiltinPoll::Ready(success);
                    }
                    let Some(diagnostic_context) = diagnostic_context else {
                        return permanent_failure_in(format!("unit expected, received {received}"));
                    };
                    self.phase = RegionalAnnotationPhase::AssertUnitContext {
                        received,
                        diagnostic_context: self.demand_in(access, diagnostic_context),
                    };
                }
                RegionalAnnotationPhase::AssertUnitContext {
                    received,
                    mut diagnostic_context,
                } => {
                    let diagnostic_context =
                        match poll_demand_in(&mut diagnostic_context, access, step_budget) {
                            RegionalDemandResult::Ready(value) => value,
                            RegionalDemandResult::Pending(poll) => {
                                self.phase = RegionalAnnotationPhase::AssertUnitContext {
                                    received,
                                    diagnostic_context,
                                };
                                return poll;
                            }
                        };
                    let Value::Binary(diagnostic_context) = diagnostic_context else {
                        return permanent_failure_in(
                            "unit assertion diagnostic context must be text",
                        );
                    };
                    return permanent_failure_in(format!(
                        "{}: unit expected, received {received}",
                        String::from_utf8_lossy(&diagnostic_context)
                    ));
                }
                RegionalAnnotationPhase::TargetCollection { kind, mut target } => {
                    let target = match poll_demand_in(&mut target, access, step_budget) {
                        RegionalDemandResult::Ready(target) => target,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::TargetCollection { kind, target };
                            return poll;
                        }
                    };
                    let shape = match &target {
                        Value::Binary(bytes) => CollectionShape::Binary(bytes.clone()),
                        Value::List(_) => CollectionShape::List,
                        other => CollectionShape::Other(format!("{other:?}")),
                    };
                    match (kind, shape) {
                        (CollectionKind::Array, CollectionShape::Binary(bytes)) => {
                            let values = bytes
                                .iter()
                                .map(|byte| Value::Number(Number::from_u8(*byte)))
                                .collect();
                            return RegionalBuiltinPoll::Ready(Value::List(List::from_values(
                                values,
                            )));
                        }
                        (CollectionKind::Binary, CollectionShape::Binary(_)) => {
                            return RegionalBuiltinPoll::Ready(target);
                        }
                        (_, CollectionShape::List) => {
                            self.phase = RegionalAnnotationPhase::CollectionItems {
                                kind,
                                front: RegionalListFront::new_in(
                                    access,
                                    target,
                                    Some(self.source_owner),
                                ),
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
                            return annotation_error_in(
                                access,
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
                            return annotation_error_in(
                                access,
                                format!(
                                    "`{}` annotation requires a {expected} target, got {rendered}",
                                    kind.name()
                                ),
                            );
                        }
                    }
                }
                RegionalAnnotationPhase::CollectionItems {
                    kind,
                    mut front,
                    mut values,
                    mut bytes,
                    mut item,
                } => {
                    if let Some((mut demand, tail)) = item.take() {
                        let value = match poll_demand_in(&mut demand, access, step_budget) {
                            RegionalDemandResult::Ready(value) => value,
                            RegionalDemandResult::Pending(poll) => {
                                self.phase = RegionalAnnotationPhase::CollectionItems {
                                    kind,
                                    front,
                                    values,
                                    bytes,
                                    item: Some((demand, tail)),
                                };
                                return contextualize_poll_in(access, poll, "binary_extraction");
                            }
                        };
                        let Value::Number(number) = value else {
                            return permanent_failure_in(format!(
                                "`binary` annotation requires list items to be byte integers, got {value:?}"
                            ));
                        };
                        let Some(byte) = number.to_u8_if_integer() else {
                            return permanent_failure_in(format!(
                                "`binary` annotation cannot encode number `{number}` as a byte"
                            ));
                        };
                        bytes.push(byte);
                        front = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                        self.phase = RegionalAnnotationPhase::CollectionItems {
                            kind,
                            front,
                            values,
                            bytes,
                            item: None,
                        };
                        return RegionalBuiltinPoll::Yielded;
                    }

                    match front.poll_in(access, step_budget) {
                        RegionalListFrontPoll::Ready(Some((value, tail))) => {
                            if matches!(kind, CollectionKind::Binary) {
                                item = Some((self.demand_in(access, value), tail));
                            } else {
                                values.push(value);
                                front = RegionalListFront::new_in(
                                    access,
                                    tail,
                                    Some(self.source_owner),
                                );
                            }
                            self.phase = RegionalAnnotationPhase::CollectionItems {
                                kind,
                                front,
                                values,
                                bytes,
                                item,
                            };
                            return RegionalBuiltinPoll::Yielded;
                        }
                        RegionalListFrontPoll::Ready(None) => {
                            let result = match kind {
                                CollectionKind::Binary => Value::Binary(Bytes::from(bytes)),
                                CollectionKind::Array => Value::List(List::from_values(values)),
                                CollectionKind::Deque => {
                                    Value::List(List::from_values_balanced(values))
                                }
                            };
                            return RegionalBuiltinPoll::Ready(result);
                        }
                        RegionalListFrontPoll::Boundary(request) => {
                            self.phase = RegionalAnnotationPhase::CollectionItems {
                                kind,
                                front,
                                values,
                                bytes,
                                item,
                            };
                            let poll = RegionalBuiltinPoll::Boundary(request);
                            return if matches!(kind, CollectionKind::Binary) {
                                contextualize_poll_in(access, poll, "binary_extraction")
                            } else {
                                poll
                            };
                        }
                        RegionalListFrontPoll::Yielded => {
                            self.phase = RegionalAnnotationPhase::CollectionItems {
                                kind,
                                front,
                                values,
                                bytes,
                                item,
                            };
                            return RegionalBuiltinPoll::Yielded;
                        }
                        RegionalListFrontPoll::Failed(failure) => {
                            let poll = RegionalBuiltinPoll::Failed(failure);
                            return if matches!(kind, CollectionKind::Binary) {
                                contextualize_poll_in(access, poll, "binary_extraction")
                            } else {
                                poll
                            };
                        }
                    }
                }
                RegionalAnnotationPhase::MetadataTarget {
                    kind,
                    function,
                    mut target,
                } => {
                    let target = match poll_demand_in(&mut target, access, step_budget) {
                        RegionalDemandResult::Ready(target) => target,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::MetadataTarget {
                                kind,
                                function,
                                target,
                            };
                            return poll;
                        }
                    };
                    let is_list = matches!(target, Value::List(_));
                    if !is_list {
                        return permanent_failure_in(format!(
                            "`{}` annotation requires a list of sealed metadata carriers",
                            kind.name()
                        ));
                    }
                    self.phase = RegionalAnnotationPhase::MetadataItems {
                        kind,
                        function,
                        front: RegionalListFront::new_in(access, target, Some(self.source_owner)),
                        metadata: Vec::new(),
                        item_index: 0,
                        item: None,
                    };
                }
                RegionalAnnotationPhase::MetadataItems {
                    kind,
                    function,
                    mut front,
                    mut metadata,
                    mut item_index,
                    mut item,
                } => {
                    if let Some((mut demand, tail)) = item.take() {
                        let carrier = match poll_demand_in(&mut demand, access, step_budget) {
                            RegionalDemandResult::Ready(carrier) => carrier,
                            RegionalDemandResult::Pending(poll) => {
                                self.phase = RegionalAnnotationPhase::MetadataItems {
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
                        let received = carrier.diagnostic_kind_name();
                        let extracted = carrier.associated_metadata().ok_or(received);
                        let extracted = match extracted {
                            Ok(extracted) => extracted,
                            Err(received) => {
                                return permanent_failure_in(format!(
                                    "`{}` annotation item {item_index} must be a sealed metadata carrier, received {received}",
                                    kind.name()
                                ));
                            }
                        };
                        metadata.push(extracted);
                        item_index += 1;
                        front = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                        self.phase = RegionalAnnotationPhase::MetadataItems {
                            kind,
                            function,
                            front,
                            metadata,
                            item_index,
                            item: None,
                        };
                        return RegionalBuiltinPoll::Yielded;
                    }

                    match front.poll_in(access, step_budget) {
                        RegionalListFrontPoll::Ready(Some((carrier, tail))) => {
                            item = Some((self.demand_in(access, carrier), tail));
                            self.phase = RegionalAnnotationPhase::MetadataItems {
                                kind,
                                function,
                                front,
                                metadata,
                                item_index,
                                item,
                            };
                            return RegionalBuiltinPoll::Yielded;
                        }
                        RegionalListFrontPoll::Ready(None) => {
                            return RegionalBuiltinPoll::Ready(finish_metadata_update_in(
                                access, kind, function, metadata,
                            ));
                        }
                        RegionalListFrontPoll::Boundary(request) => {
                            self.phase = RegionalAnnotationPhase::MetadataItems {
                                kind,
                                function,
                                front,
                                metadata,
                                item_index,
                                item,
                            };
                            return RegionalBuiltinPoll::Boundary(request);
                        }
                        RegionalListFrontPoll::Yielded => {
                            self.phase = RegionalAnnotationPhase::MetadataItems {
                                kind,
                                function,
                                front,
                                metadata,
                                item_index,
                                item,
                            };
                            return RegionalBuiltinPoll::Yielded;
                        }
                        RegionalListFrontPoll::Failed(failure) => {
                            return RegionalBuiltinPoll::Failed(failure);
                        }
                    }
                }
                RegionalAnnotationPhase::ErrorMessage(mut target) => {
                    let target = match poll_demand_in(&mut target, access, step_budget) {
                        RegionalDemandResult::Ready(target) => target,
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::ErrorMessage(target);
                            return contextualize_poll_in(access, poll, "error_message");
                        }
                    };
                    return RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::emission(
                        target,
                    )));
                }
                RegionalAnnotationPhase::ContextTarget {
                    diagnostic_context,
                    mut target,
                } => {
                    let target = match poll_demand_in(&mut target, access, step_budget) {
                        RegionalDemandResult::Ready(target) => target,
                        RegionalDemandResult::Pending(RegionalBuiltinPoll::Failed(failure)) => {
                            return RegionalBuiltinPoll::Failed(Arc::new(
                                failure.with_context_in(access.values(), diagnostic_context),
                            ));
                        }
                        RegionalDemandResult::Pending(poll) => {
                            self.phase = RegionalAnnotationPhase::ContextTarget {
                                diagnostic_context,
                                target,
                            };
                            return poll;
                        }
                    };
                    return RegionalBuiltinPoll::Ready(target);
                }
                RegionalAnnotationPhase::Ready(value) => return RegionalBuiltinPoll::Ready(value),
                RegionalAnnotationPhase::Done => {
                    unreachable!("a completed annotation machine cannot be polled again")
                }
            }
        }
    }

    fn begin_recognized(
        &self,
        access: &EvaluationValueAccess<'_>,
        recognized: RecognizedAnnotation,
    ) -> RegionalAnnotationPhase {
        match recognized {
            RecognizedAnnotation::Assert(kind, payload) => {
                RegionalAnnotationPhase::ParseAssertion {
                    kind,
                    payload: self.demand_in(access, payload),
                }
            }
            RecognizedAnnotation::AssertUnit(payload) => {
                RegionalAnnotationPhase::ParseAssertUnit(self.demand_in(access, payload))
            }
            RecognizedAnnotation::MetadataInitialize => RegionalAnnotationPhase::AssertUnit {
                value: self.demand_in(access, access.values().duplicate_value(&self.target)),
                diagnostic_context: None,
                success: access.values().initial_metadata(),
            },
            RecognizedAnnotation::Metadata(kind, function) => {
                RegionalAnnotationPhase::MetadataTarget {
                    kind,
                    function,
                    target: self.demand_in(access, access.values().duplicate_value(&self.target)),
                }
            }
            RecognizedAnnotation::Collection(kind) => RegionalAnnotationPhase::TargetCollection {
                kind,
                target: self.demand_in(access, access.values().duplicate_value(&self.target)),
            },
            RecognizedAnnotation::Reflection(effect) => {
                let target = access.values().duplicate_value(&self.target);
                RegionalAnnotationPhase::Ready(Value::Lazy(LazyValue::from_reflection_gate_in(
                    access.values(),
                    effect,
                    target,
                )))
            }
            RecognizedAnnotation::Seq(value) => RegionalAnnotationPhase::Ready(builtin_value_in(
                access,
                Builtin::Seq,
                [value, access.values().duplicate_value(&self.target)],
            )),
            RecognizedAnnotation::Spark(value) => RegionalAnnotationPhase::Ready(builtin_value_in(
                access,
                Builtin::Spark,
                [value, access.values().duplicate_value(&self.target)],
            )),
            RecognizedAnnotation::Error => RegionalAnnotationPhase::ErrorMessage(
                self.demand_in(access, access.values().duplicate_value(&self.target)),
            ),
            RecognizedAnnotation::Context(diagnostic_context) => {
                RegionalAnnotationPhase::ContextTarget {
                    diagnostic_context,
                    target: self.demand_in(access, access.values().duplicate_value(&self.target)),
                }
            }
            RecognizedAnnotation::Unknown(rendered) => {
                warn_unknown_annotation(&rendered);
                RegionalAnnotationPhase::Ready(access.values().duplicate_value(&self.target))
            }
        }
    }

    fn demand_in(&self, access: &EvaluationValueAccess<'_>, value: Value) -> RegionalWhnfWork {
        RegionalWhnfWork::from_focus(access, value).with_source_owner(self.source_owner)
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        trace_compatibility_value_managed_edges(&self.target, visitor);
        self.phase.trace_managed_edges(visitor);
    }
}

enum CollectionShape {
    Binary(Bytes),
    List,
    Other(String),
}

fn recognize_annotation(
    access: &EvaluationValueAccess<'_>,
    annotation: Value,
) -> RecognizedAnnotation {
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
    let payload_value = || access.values().duplicate_value(payload);
    match key_atom_name(tag) {
        Some("refl") => RecognizedAnnotation::Reflection(payload_value()),
        Some("seq") => RecognizedAnnotation::Seq(payload_value()),
        Some("spark") => RecognizedAnnotation::Spark(payload_value()),
        Some("context") => RecognizedAnnotation::Context(payload_value()),
        Some("meta_pure") => RecognizedAnnotation::Metadata(MetadataKind::Pure, payload_value()),
        Some("meta_refl") => {
            RecognizedAnnotation::Metadata(MetadataKind::Reflection, payload_value())
        }
        Some("assert_defined") => {
            RecognizedAnnotation::Assert(AssertionKind::Defined, payload_value())
        }
        Some("assert_undefined") => {
            RecognizedAnnotation::Assert(AssertionKind::Undefined, payload_value())
        }
        Some("assert_unit") => RecognizedAnnotation::AssertUnit(payload_value()),
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

fn annotation_name(_access: &EvaluationValueAccess<'_>, value: Value) -> String {
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

fn finish_metadata_update_in(
    access: &EvaluationValueAccess<'_>,
    kind: MetadataKind,
    function: Value,
    metadata: Vec<Value>,
) -> Value {
    let output_count = metadata.len();
    let effect = Value::Lazy(LazyValue::from_application_in(
        access.values(),
        function,
        Arc::from([Value::List(List::from_values(metadata))]),
    ));
    let updates = match kind {
        MetadataKind::Pure => effect,
        MetadataKind::Reflection => Value::reflection_task_result_in(access.values(), effect),
    };
    let projection_context = Value::Dict(crate::core::Dict::new_sync().insert(
        (*keys::CONTEXT).clone(),
        evaluation_context_frame_in(access.values(), "wrap_metadata"),
    ));
    let carriers = (0..output_count)
        .map(|index| {
            let projection = builtin_value_in(
                access,
                Builtin::ListAt,
                [
                    Value::Number(Number::from_usize(index)),
                    access.values().duplicate_value(&updates),
                ],
            );
            Value::metadata_carrier(builtin_value_in(
                access,
                Builtin::Anno,
                [
                    access.values().duplicate_value(&projection_context),
                    projection,
                ],
            ))
        })
        .collect();
    Value::List(List::from_values(carriers))
}

fn builtin_value_in<const N: usize>(
    access: &EvaluationValueAccess<'_>,
    builtin: Builtin,
    arguments: [Value; N],
) -> Value {
    Value::Lazy(LazyValue::from_builtin_in(
        access.values(),
        BuiltinCall {
            builtin,
            arguments: Arc::from(arguments),
        },
    ))
}

fn annotation_error_in(
    access: &EvaluationValueAccess<'_>,
    message: impl Into<String>,
) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Ready(Value::Lazy(LazyValue::error_in(
        access.values(),
        message.into(),
    )))
}

fn permanent_failure_in(message: impl Into<String>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}

fn poll_demand_in(
    demand: &mut RegionalWhnfWork,
    access: &EvaluationValueAccess<'_>,
    step_budget: &mut EvaluationStepBudget,
) -> RegionalDemandResult {
    match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => RegionalDemandResult::Ready(value),
        RegionalWhnfStatus::Boundary(request) => {
            RegionalDemandResult::Pending(RegionalBuiltinPoll::Boundary(request))
        }
        RegionalWhnfStatus::Yielded => RegionalDemandResult::Pending(RegionalBuiltinPoll::Yielded),
        RegionalWhnfStatus::Failed(failure) => {
            RegionalDemandResult::Pending(RegionalBuiltinPoll::Failed(failure))
        }
    }
}

fn contextualize_poll_in(
    access: &EvaluationValueAccess<'_>,
    poll: RegionalBuiltinPoll,
    operation: &str,
) -> RegionalBuiltinPoll {
    let RegionalBuiltinPoll::Failed(failure) = poll else {
        return poll;
    };
    RegionalBuiltinPoll::Failed(Arc::new(failure.with_context_in(
        access.values(),
        evaluation_context_frame_in(access.values(), operation),
    )))
}

impl RegionalAnnotationPhase {
    fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        let trace_value = |value: &Value, visitor: &mut Visitor<'_>| {
            trace_compatibility_value_managed_edges(value, visitor);
        };
        match self {
            Self::Recognize(demand)
            | Self::ParseAssertUnit(demand)
            | Self::ErrorMessage(demand) => demand.trace_managed_edges(visitor),
            Self::ParseAssertion { payload, .. } => payload.trace_managed_edges(visitor),
            Self::AssertionName { name, value, .. } => {
                name.trace_managed_edges(visitor);
                trace_value(value, visitor);
            }
            Self::AssertionValue { value, .. } => value.trace_managed_edges(visitor),
            Self::AssertUnit {
                value,
                diagnostic_context,
                success,
            } => {
                value.trace_managed_edges(visitor);
                if let Some(diagnostic_context) = diagnostic_context {
                    trace_value(diagnostic_context, visitor);
                }
                trace_value(success, visitor);
            }
            Self::AssertUnitContext {
                diagnostic_context, ..
            } => diagnostic_context.trace_managed_edges(visitor),
            Self::TargetCollection { target, .. } => target.trace_managed_edges(visitor),
            Self::CollectionItems {
                front,
                values,
                item,
                ..
            } => {
                front.trace_managed_edges(visitor);
                for value in values {
                    trace_value(value, visitor);
                }
                if let Some((demand, tail)) = item {
                    demand.trace_managed_edges(visitor);
                    trace_value(tail, visitor);
                }
            }
            Self::MetadataTarget {
                function, target, ..
            } => {
                trace_value(function, visitor);
                target.trace_managed_edges(visitor);
            }
            Self::MetadataItems {
                function,
                front,
                metadata,
                item,
                ..
            } => {
                trace_value(function, visitor);
                front.trace_managed_edges(visitor);
                for value in metadata {
                    trace_value(value, visitor);
                }
                if let Some((demand, tail)) = item {
                    demand.trace_managed_edges(visitor);
                    trace_value(tail, visitor);
                }
            }
            Self::ContextTarget {
                diagnostic_context,
                target,
            } => {
                trace_value(diagnostic_context, visitor);
                target.trace_managed_edges(visitor);
            }
            Self::Ready(value) => trace_value(value, visitor),
            Self::Done => {}
        }
    }
}

fn warn_unknown_annotation(rendered: &str) {
    eprintln!("warning: unrecognized annotation encountered: {rendered}");
}

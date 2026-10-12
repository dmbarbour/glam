//! Replays a definition journal for conflicts (`perf-definition-assertions`).
//!
//! A journal is `{}` or `[prior, entry, ...]`, whose `prior` is a journal
//! too, and an entry is `{op:intro:path, at:Frame}` with `over` or `update`
//! in place of `intro`. The builtin accelerates a fold a front end could
//! write in glam: it returns one diagnostic per introduction of a path
//! already defined, in journal order, with the entry's `at` as context
//! (`msg.context`), then the earlier definition's. Defining a path defines
//! each namespace on the way, as the compile-time check counts them. An
//! override of a path the journal lacks is not reported: names can come from
//! outside any journal, such as a host's `asm.args` or a dict's members.

use std::collections::HashMap;
use std::sync::Arc;

use glam_gc::Visitor;

use crate::core::{
    Dict, EvaluatedValue, EvaluationFailure, Key, LazyId, List, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};

use super::access_machine::{RegionalConversionPoll, RegionalKeyConversion};
use super::builtin_machine::RegionalBuiltinPoll;
use super::list_machine::{RegionalListFront, RegionalListFrontPoll};
use super::value::format_name_part;
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};

pub(in crate::eval) struct RegionalJournalConflictsMachine {
    /// What remains to replay, the next step last.
    frames: Vec<JournalFrame>,
    /// Each defined path's first `at`.
    defined: HashMap<Key, Value>,
    conflicts: Vec<Value>,
    keys: JournalKeys,
    source_owner: LazyId,
}

/// The keys an entry is read by, interned once.
struct JournalKeys {
    op: Key,
    at: Key,
    intro: Key,
    over: Key,
    update: Key,
}

enum JournalFrame {
    Journal(RegionalWhnfWork),
    /// A stretch's remaining items, the first of which is its prior journal.
    Items {
        front: RegionalListFront,
        prior: bool,
    },
    Entry(RegionalWhnfWork),
    Op {
        op: Box<RegionalKeyConversion>,
        at: Option<Value>,
    },
}

impl RegionalJournalConflictsMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let [journal] = arguments else {
            panic!("journal conflicts retains one journal")
        };
        Self {
            frames: vec![JournalFrame::Journal(demand_in(
                access,
                access.values().duplicate_value(journal),
                source_owner,
            ))],
            defined: HashMap::new(),
            conflicts: Vec::new(),
            keys: JournalKeys {
                op: Key::atom_from_text("op"),
                at: Key::atom_from_text("at"),
                intro: Key::atom_from_text("intro"),
                over: Key::atom_from_text("over"),
                update: Key::atom_from_text("update"),
            },
            source_owner,
        }
    }

    /// Reads a journal or entry at WHNF.
    fn accept(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        value: Value,
        journal: bool,
    ) -> Result<(), RegionalBuiltinPoll> {
        match value {
            Value::Dict(dict) if journal && dict.is_empty() => Ok(()),
            value @ Value::List(_) if journal => {
                self.frames.push(JournalFrame::Items {
                    front: RegionalListFront::new_in(access, value, Some(self.source_owner)),
                    prior: true,
                });
                Ok(())
            }
            _ if journal => Err(failure_in("a definition journal must be `{}` or a list")),
            Value::Dict(entry) => {
                let Some(op) = entry.get(&self.keys.op) else {
                    return Err(failure_in("a definition journal entry must have an `op`"));
                };
                let at = entry
                    .get(&self.keys.at)
                    .map(|at| access.values().duplicate_value(at));
                // Compilers write ops as plain data.
                match access.values().key_from_value(op) {
                    Some(op) => self.replay(access, op, at).map_err(failure_in),
                    None => {
                        let op = RegionalKeyConversion::new(
                            access,
                            access.values().duplicate_value(op),
                            Some(self.source_owner),
                        );
                        self.frames.push(JournalFrame::Op {
                            op: Box::new(op),
                            at,
                        });
                        Ok(())
                    }
                }
            }
            _ => Err(failure_in("a definition journal entry must be a dict")),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let Some(frame) = self.frames.last_mut() else {
            let conflicts = std::mem::take(&mut self.conflicts);
            return RegionalBuiltinPoll::Ready(Value::List(List::from_values(conflicts)));
        };
        match frame {
            JournalFrame::Journal(demand) | JournalFrame::Entry(demand) => {
                let value = match drive_in(access, demand, step_budget) {
                    Ok(value) => value,
                    Err(poll) => return poll,
                };
                let journal = matches!(self.frames.pop(), Some(JournalFrame::Journal(_)));
                if let Err(poll) = self.accept(access, value, journal) {
                    return poll;
                }
            }
            JournalFrame::Items { front, prior } => match front.poll_in(access, step_budget) {
                RegionalListFrontPoll::Ready(Some((item, tail))) => {
                    *front = RegionalListFront::new_in(access, tail, Some(self.source_owner));
                    let journal = std::mem::take(prior);
                    if matches!(item, Value::Lazy(_) | Value::Promised(_)) {
                        let demand = demand_in(access, item, self.source_owner);
                        self.frames.push(if journal {
                            JournalFrame::Journal(demand)
                        } else {
                            JournalFrame::Entry(demand)
                        });
                    } else if let Err(poll) = self.accept(access, item, journal) {
                        return poll;
                    }
                }
                RegionalListFrontPoll::Ready(None) => {
                    self.frames.pop();
                }
                RegionalListFrontPoll::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalListFrontPoll::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalListFrontPoll::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            },
            JournalFrame::Op { op, at } => match op.poll_in(access, step_budget) {
                RegionalConversionPoll::Ready(op) => {
                    let at = at.take();
                    self.frames.pop();
                    if let Err(message) = self.replay(access, op, at) {
                        return failure_in(message);
                    }
                }
                RegionalConversionPoll::Boundary(request) => {
                    return RegionalBuiltinPoll::Boundary(request);
                }
                RegionalConversionPoll::Yielded => return RegionalBuiltinPoll::Yielded,
                RegionalConversionPoll::Failed(failure) => {
                    return RegionalBuiltinPoll::Failed(failure);
                }
            },
        }
        RegionalBuiltinPoll::Yielded
    }

    fn replay(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        op: Key,
        at: Option<Value>,
    ) -> Result<(), &'static str> {
        const INVALID: &str =
            "a definition journal op must be `intro`, `over` or `update` of a path";
        let Key::Dict(op) = op else {
            return Err(INVALID);
        };
        let [(tag, path @ Key::List(parts))] = op.as_ref() else {
            return Err(INVALID);
        };
        let name = || {
            parts
                .iter()
                .map(format_name_part)
                .collect::<Vec<_>>()
                .join(".")
        };
        let duplicate = |value: &Value| access.values().duplicate_value(value);
        // Defining a path defines each namespace on the way.
        for length in 1..parts.len() {
            let prefix = Key::List(Arc::from(&parts[..length]));
            self.defined
                .entry(prefix)
                .or_insert_with(|| at.as_ref().map_or_else(|| empty(access), duplicate));
        }
        if *tag == self.keys.intro {
            match self.defined.get(path) {
                Some(earlier) => {
                    let context = at.iter().chain([earlier]).map(duplicate).collect();
                    self.conflicts.push(conflict(
                        access,
                        format!(
                            "cannot introduce `{}` because it is already defined",
                            name()
                        ),
                        context,
                    ));
                }
                None => {
                    self.defined
                        .insert(path.clone(), at.unwrap_or_else(|| empty(access)));
                }
            }
        } else if *tag == self.keys.over || *tag == self.keys.update {
            self.defined
                .entry(path.clone())
                .or_insert_with(|| at.unwrap_or_else(|| empty(access)));
        } else {
            return Err(INVALID);
        }
        Ok(())
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        for frame in &self.frames {
            match frame {
                JournalFrame::Journal(demand) | JournalFrame::Entry(demand) => {
                    demand.trace_managed_edges(visitor);
                }
                JournalFrame::Items { front, .. } => front.trace_managed_edges(visitor),
                JournalFrame::Op { op, at } => {
                    op.trace_managed_edges(visitor);
                    if let Some(at) = at {
                        trace_compatibility_value_managed_edges(at, visitor);
                    }
                }
            }
        }
        for value in self.defined.values().chain(&self.conflicts) {
            trace_compatibility_value_managed_edges(value, visitor);
        }
    }
}

fn demand_in(
    access: &EvaluationValueAccess<'_>,
    value: Value,
    source_owner: LazyId,
) -> RegionalWhnfWork {
    RegionalWhnfWork::from_focus(access, value).with_source_owner(source_owner)
}

fn drive_in(
    access: &EvaluationValueAccess<'_>,
    demand: &mut RegionalWhnfWork,
    step_budget: &mut EvaluationStepBudget,
) -> Result<Value, RegionalBuiltinPoll> {
    match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
        RegionalWhnfStatus::Ready(value) => {
            Ok(EvaluatedValue::from_whnf_in(access.values(), value)
                .expect("a journal demand must reach WHNF")
                .into_value_in(access.values()))
        }
        RegionalWhnfStatus::Boundary(request) => Err(RegionalBuiltinPoll::Boundary(request)),
        RegionalWhnfStatus::Yielded => Err(RegionalBuiltinPoll::Yielded),
        RegionalWhnfStatus::Failed(failure) => Err(RegionalBuiltinPoll::Failed(failure)),
    }
}

fn empty(_access: &EvaluationValueAccess<'_>) -> Value {
    Value::Dict(Dict::new_sync())
}

fn conflict(_access: &EvaluationValueAccess<'_>, text: String, context: Vec<Value>) -> Value {
    let message = Dict::new_sync()
        .insert((*keys::TEXT).clone(), Value::binary_from_text(&text))
        .insert(
            (*keys::CONTEXT).clone(),
            Value::List(List::from_values(context)),
        );
    Value::Dict(Dict::new_sync().insert((*keys::MSG).clone(), Value::Dict(message)))
}

fn failure_in(message: impl Into<String>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}

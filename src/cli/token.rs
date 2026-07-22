//! Restricted parser for structured contents of one UTF-8 command-line token.

mod effects;
mod regex;

use std::sync::Arc;

use crate::api::Value;
use crate::evaluation::EvalContext;
use crate::reflection::{
    CommitResult, ExactConflictAnalysis, HostSnapshot, IsolatedEffectSearch, IsolatedSearchPoll,
    ReflectionStore, StoreSnapshot, TaskCommit, TaskEnvironment, TaskHost,
};

pub(super) use effects::request_specs;

const SEARCH_STEP_BUDGET: usize = 256;

#[derive(Clone)]
pub(super) struct TokenSnapshot {
    input: Arc<str>,
    completion_offset: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct TokenJournal {
    pub(super) cursor: usize,
    pub(super) expectations: Vec<TokenExpectation>,
    pub(super) candidates: Vec<TokenCandidate>,
}

#[derive(Clone, Debug)]
pub(super) struct TokenExpectation {
    pub(super) offset: usize,
    pub(super) label: String,
}

#[derive(Clone, Debug)]
pub(super) struct TokenCandidate {
    pub(super) offset: usize,
    pub(super) replacement: String,
}

pub(super) struct TokenRun {
    pub(super) values: Vec<Value>,
    pub(super) furthest: usize,
    pub(super) expectations: Vec<TokenExpectation>,
    pub(super) candidates: Vec<TokenCandidate>,
}

pub(super) struct TokenHost {
    snapshot: TokenSnapshot,
    store: StoreSnapshot,
}

impl TokenHost {
    fn new(input: Arc<str>, completion_offset: Option<usize>) -> Self {
        Self {
            snapshot: TokenSnapshot {
                input,
                completion_offset,
            },
            store: ReflectionStore::new(Arc::new(ExactConflictAnalysis)).snapshot(),
        }
    }
}

impl TaskEnvironment for TokenHost {
    fn reflection_environment(&self) -> Value {
        Value::empty_record()
    }
}

impl TaskHost<effects::TokenEffects> for TokenHost {
    fn snapshot(&self) -> HostSnapshot<effects::TokenEffects> {
        HostSnapshot::new(1, self.store.clone(), self.snapshot.clone())
    }

    fn commit(&self, _commit: TaskCommit<effects::TokenEffects>) -> CommitResult {
        CommitResult::Closed
    }

    fn wait_for_change(&self, _observed_generation: u64) -> bool {
        false
    }
}

pub(super) fn run(
    parser: &Value,
    input: Arc<str>,
    completion_offset: Option<usize>,
    eval_context: EvalContext,
) -> Result<TokenRun, String> {
    let input_len = input.len();
    let host = Arc::new(TokenHost::new(input, completion_offset));
    let mut search =
        IsolatedEffectSearch::new_in_context(parser, effects::TokenEffects, host, eval_context)
            .map_err(|error| format!("token parser could not start: {error}"))?;

    loop {
        match search.poll(SEARCH_STEP_BUDGET) {
            IsolatedSearchPoll::Yielded => {}
            IsolatedSearchPoll::Complete(branches) => {
                let mut values = Vec::new();
                let mut furthest = 0;
                let mut expectations = Vec::new();
                let mut candidates = Vec::new();

                for branch in branches.iter() {
                    let journal = branch.journal();
                    furthest = furthest.max(journal.cursor);
                    candidates.extend(journal.candidates.iter().cloned());
                    expectations.extend(journal.expectations.iter().cloned());
                    if let Some(value) = branch.value() {
                        if journal.cursor == input_len {
                            values.push(value.clone());
                        } else {
                            expectations.push(TokenExpectation {
                                offset: journal.cursor,
                                label: "end of token".to_owned(),
                            });
                        }
                    }
                }

                furthest = furthest
                    .max(
                        expectations
                            .iter()
                            .map(|item| item.offset)
                            .max()
                            .unwrap_or(0),
                    )
                    .max(candidates.iter().map(|item| item.offset).max().unwrap_or(0));
                expectations.retain(|item| item.offset == furthest);
                candidates.retain(|item| item.offset == furthest);
                expectations.dedup_by(|left, right| left.label == right.label);
                candidates.dedup_by(|left, right| left.replacement == right.replacement);

                return Ok(TokenRun {
                    values,
                    furthest,
                    expectations,
                    candidates,
                });
            }
            IsolatedSearchPoll::Blocked(blocked) => {
                let detail = blocked.error().map_or_else(
                    || "an unavailable dependency".to_owned(),
                    ToString::to_string,
                );
                return Err(format!("token parser became blocked on {detail}"));
            }
            IsolatedSearchPoll::Failed(error) => {
                return Err(format!("token parser failed: {error}"));
            }
            IsolatedSearchPoll::Cancelled => return Err("token parser was cancelled".to_owned()),
        }
    }
}

fn record_expectation(journal: &mut TokenJournal, offset: usize, label: impl Into<String>) {
    journal.expectations.push(TokenExpectation {
        offset,
        label: label.into(),
    });
}

fn literal_completion(input: &str, cursor: usize, split: usize, literal: &str) -> Option<String> {
    let entered = input.get(cursor..split)?;
    let remainder = literal.strip_prefix(entered)?;
    let suffix = input.get(split..)?;
    let overlap = (0..=remainder.len().min(suffix.len()))
        .rev()
        .find(|&len| {
            remainder.is_char_boundary(remainder.len() - len)
                && suffix.is_char_boundary(len)
                && remainder[remainder.len() - len..] == suffix[..len]
        })
        .unwrap_or(0);
    let insertion = &remainder[..remainder.len() - overlap];
    Some(format!("{}{}{}", &input[..split], insertion, suffix))
}

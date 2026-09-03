//! Restricted parser for structured contents of one UTF-8 command-line token.

mod effects;

use std::sync::Arc;

use glam::Value;
use glam::reflection::{IsolatedSearchPoll, IsolatedTaskHost, RequestContext, TaskSpecialization};

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

pub(super) type TokenHost = IsolatedTaskHost<TokenSnapshot>;

pub(super) fn run<S>(
    parser: &Value,
    input: Arc<str>,
    completion_offset: Option<usize>,
    context: &RequestContext<'_, S>,
) -> Result<TokenRun, String>
where
    S: TaskSpecialization,
{
    let input_len = input.len();
    let values = context.values();
    let environment = values.empty_dict();
    let host = Arc::new(
        TokenHost::new(
            &values,
            environment,
            TokenSnapshot {
                input,
                completion_offset,
            },
        )
        .map_err(|error| format!("token parser host could not start: {error}"))?,
    );
    let mut search = context
        .isolated_search(parser, effects::TokenEffects, host)
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

#[cfg(test)]
mod owner_tests {
    use super::*;

    fn assert_token_owner_inventory(run: &TokenRun) {
        let TokenRun {
            values,
            furthest: _,
            expectations: _,
            candidates: _,
        } = run;
        let _: &Vec<Value> = values;
    }

    #[test]
    fn token_run_owner_inventory_is_compile_exhaustive() {
        let _: fn(&TokenRun) = assert_token_owner_inventory;
    }
}

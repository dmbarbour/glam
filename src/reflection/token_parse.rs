//! The CLI's token parser, run by a pure handler (`pure-handler-token`):
//! the parser's `.token.*` requests are private builtins over the token's
//! text, and the handler keeps every branch, failed ones with their
//! evidence, which the run gathers at the furthest offset any reached.

use std::sync::Arc;

use super::protocol::{RequestContext, TaskSpecialization};
use crate::api::{Error, Value as PublicValue, Values};
use crate::core::{Builtin, BuiltinCall, Dict, Key, LazyValue, Value, keys};
use crate::evaluation::EvalContext;

/// What the token parser expected at an offset.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TokenExpectation {
    pub offset: usize,
    pub label: String,
}

/// A completion of the whole token, at the offset being completed.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TokenCandidate {
    pub offset: usize,
    pub replacement: String,
}

/// A token parse: the values of the parses that read the whole token, in
/// search order, and the evidence at the furthest offset any branch
/// reached.
#[doc(hidden)]
pub struct TokenRun {
    pub values: Vec<PublicValue>,
    pub furthest: usize,
    pub expectations: Vec<TokenExpectation>,
    pub candidates: Vec<TokenCandidate>,
}

/// Runs `parser`, an effect of `.token.*` requests and the pure handler's
/// core, over `input`; in completion, `completion_offset` is the offset
/// being completed.
#[doc(hidden)]
pub fn run_token_parser<S: TaskSpecialization>(
    context: &RequestContext<'_, S>,
    parser: &PublicValue,
    input: Arc<str>,
    completion_offset: Option<usize>,
) -> Result<TokenRun, String> {
    let values = context.values();
    let input_len = input.len();
    let run = values
        .with_access(|scoped| {
            let access = scoped.runtime_access();
            let parser = scoped.clone_core(parser)?;
            let state =
                crate::eval::token_state_in(access, values.core(), input, completion_offset);
            let call = BuiltinCall {
                builtin: Builtin::Handler,
                arguments: Arc::from([
                    crate::eval::token_api_in(access),
                    Value::Dict(Dict::new_sync()),
                    state,
                    parser,
                ]),
            };
            Ok::<_, Error>(scoped.wrap(Value::Lazy(LazyValue::from_builtin_in(access, call))))
        })
        .map_err(|error| format!("token parser could not start: {error}"))?;
    let reader = Reader {
        context: context.eval_context(),
        values: &values,
    };
    let results = reader.whnf(&run)?;
    let successes = reader.items(&reader.field(&results, &keys::OK)?)?;
    let failures = reader.items(&reader.field(&results, &keys::FAILED)?)?;

    let mut parsed = Vec::new();
    let mut furthest = 0;
    let mut expectations = Vec::new();
    let mut candidates = Vec::new();
    let branches = successes
        .iter()
        .map(|branch| (branch, true))
        .chain(failures.iter().map(|branch| (branch, false)));
    for (branch, success) in branches {
        let branch = reader.whnf(branch)?;
        let handler = reader.whnf(&reader.field(&branch, &keys::HANDLER)?)?;
        let cursor = reader.offset(&reader.field(&handler, &keys::CURSOR)?)?;
        furthest = furthest.max(cursor);
        for item in reader.items(&reader.field(&handler, &keys::EXPECTATIONS)?)? {
            let item = reader.whnf(&item)?;
            expectations.push(TokenExpectation {
                offset: reader.offset(&reader.field(&item, &keys::OFFSET)?)?,
                label: reader.text(&reader.field(&item, &keys::LABEL)?)?,
            });
        }
        for item in reader.items(&reader.field(&handler, &keys::CANDIDATES)?)? {
            let item = reader.whnf(&item)?;
            candidates.push(TokenCandidate {
                offset: reader.offset(&reader.field(&item, &keys::OFFSET)?)?,
                replacement: reader.text(&reader.field(&item, &keys::REPLACEMENT)?)?,
            });
        }
        if success {
            if cursor == input_len {
                parsed.push(reader.field(&branch, &keys::VALUE)?);
            } else {
                expectations.push(TokenExpectation {
                    offset: cursor,
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
    Ok(TokenRun {
        values: parsed,
        furthest,
        expectations,
        candidates,
    })
}

/// Reads a token parse's results, evaluating as it goes.
struct Reader<'a> {
    context: &'a EvalContext,
    values: &'a Values,
}

impl Reader<'_> {
    fn whnf(&self, value: &PublicValue) -> Result<PublicValue, String> {
        self.context
            .evaluate_root_whnf(value.clone().into_runtime_root())
            .map(PublicValue::from_runtime_root)
            .map_err(|error| {
                if error.blocked_on().is_some() {
                    "token parser became blocked on an unavailable dependency".to_owned()
                } else {
                    format!("token parser failed: {error}")
                }
            })
    }

    /// The member `key` of `dict`, evaluated already.
    fn field(&self, dict: &PublicValue, key: &Key) -> Result<PublicValue, String> {
        self.values
            .with_access(|scoped| {
                scoped.with_core(dict, |value| match value {
                    Value::Dict(dict) => dict
                        .get(key)
                        .map(|value| scoped.wrap(scoped.runtime_access().duplicate_value(value))),
                    _ => None,
                })
            })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("token parser result has no `{key:?}`"))
    }

    fn items(&self, list: &PublicValue) -> Result<Vec<PublicValue>, String> {
        let array = self
            .values
            .anno_array(list.clone())
            .map_err(|error| error.to_string())?;
        let array = self.whnf(&array)?;
        self.values
            .with_access(|scoped| {
                scoped.with_core(&array, |value| match value {
                    Value::List(list) => list.value_slice().map(|items| {
                        items
                            .iter()
                            .map(|item| scoped.wrap(scoped.runtime_access().duplicate_value(item)))
                            .collect::<Vec<_>>()
                    }),
                    _ => None,
                })
            })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "token parser result is not a list".to_owned())
    }

    fn offset(&self, value: &PublicValue) -> Result<usize, String> {
        let value = self.whnf(value)?;
        self.values
            .with_access(|scoped| {
                scoped.with_core(&value, |value| match value {
                    Value::Number(number) => number.to_usize_if_integer(),
                    _ => None,
                })
            })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "token parser offset is not an offset".to_owned())
    }

    fn text(&self, value: &PublicValue) -> Result<String, String> {
        let value = self.whnf(value)?;
        self.values
            .with_access(|scoped| {
                scoped.with_core(&value, |value| match value {
                    Value::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
                    _ => None,
                })
            })
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "token parser label is not text".to_owned())
    }
}

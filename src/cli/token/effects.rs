use crate::api::Value;
use crate::core::Value as CoreValue;
use crate::eval;
use crate::reflection::{
    EffectRequestSpec, RequestContext, RequestResult, TaskError, TaskSpecialization,
};

use super::{TokenHost, TokenJournal, literal_completion, record_expectation, regex};

#[derive(Clone, Copy)]
pub(super) struct TokenEffects;

#[derive(Clone, Copy)]
pub(in crate::cli) enum TokenRequest {
    Text,
    Regex,
    Any,
    End,
}

impl TaskSpecialization for TokenEffects {
    type Host = TokenHost;
    type Request = TokenRequest;
    type Snapshot = super::TokenSnapshot;
    type Journal = TokenJournal;

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        request_specs()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskError> {
        match request {
            TokenRequest::Text => text(arguments, context),
            TokenRequest::Regex => regex_span(arguments, context),
            TokenRequest::Any => any(arguments, context),
            TokenRequest::End => end(arguments, context),
        }
    }
}

pub(in crate::cli) fn request_specs() -> Vec<EffectRequestSpec<TokenRequest>> {
    vec![
        request("text", 1, TokenRequest::Text),
        request("regex", 1, TokenRequest::Regex),
        request("any", 0, TokenRequest::Any),
        request("end", 0, TokenRequest::End),
    ]
}

fn request(name: &str, arity: usize, request: TokenRequest) -> EffectRequestSpec<TokenRequest> {
    EffectRequestSpec::at_path(
        ["token", name],
        ["cli_token_runtime", "v0", "request", name],
        arity,
        request,
    )
}

fn text(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskError> {
    let [literal]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.token.text` received the wrong number of arguments"))?;
    let literal = text_value(context, literal, "`.token.text`")?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("token reader escaped its isolated transaction"))?;
    let (snapshot, journal) = transaction.parts();
    let input = snapshot.input.as_ref();
    let cursor = journal.cursor;

    if let Some(split) = snapshot.completion_offset
        && cursor <= split
        && cursor + literal.len() > split
    {
        if let Some(replacement) = literal_completion(input, cursor, split, &literal) {
            journal.candidates.push(super::TokenCandidate {
                offset: split,
                replacement,
            });
        }
        record_expectation(journal, split, format!("`{literal}`"));
        return Ok(RequestResult::Fail);
    }

    if input
        .get(cursor..)
        .is_some_and(|rest| rest.starts_with(&literal))
    {
        journal.cursor += literal.len();
        Ok(RequestResult::ReturnUnit)
    } else {
        let matched = input
            .get(cursor..)
            .map(|rest| common_prefix_bytes(rest, &literal))
            .unwrap_or(0);
        record_expectation(journal, cursor + matched, format!("`{literal}`"));
        Ok(RequestResult::Fail)
    }
}

fn regex_span(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskError> {
    let [pattern]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.token.regex` received the wrong number of arguments"))?;
    let pattern = text_value(context, pattern, "`.token.regex`")?;
    let matcher = regex::compile(&pattern).map_err(TaskError::new)?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("token reader escaped its isolated transaction"))?;
    let (snapshot, journal) = transaction.parts();
    let cursor = journal.cursor;
    let remaining = &snapshot.input[cursor..];
    let matched = matcher
        .find(remaining)
        .filter(|matched| matched.start() == 0);
    let Some(matched) = matched else {
        record_expectation(journal, cursor, "matching text");
        return Ok(RequestResult::Fail);
    };
    if snapshot
        .completion_offset
        .is_some_and(|split| cursor <= split && cursor + matched.end() > split)
    {
        let split = snapshot
            .completion_offset
            .expect("checked completion offset");
        record_expectation(journal, split, "matching text");
        return Ok(RequestResult::Fail);
    }
    journal.cursor = cursor + matched.end();
    Ok(RequestResult::Return(Value::record([(
        "span",
        Value::text(matched.as_str()),
    )])))
}

fn any(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.token.any` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("token reader escaped its isolated transaction"))?;
    let (snapshot, journal) = transaction.parts();
    if snapshot
        .completion_offset
        .is_some_and(|split| journal.cursor >= split)
    {
        record_expectation(journal, journal.cursor, "one character");
        return Ok(RequestResult::Fail);
    }
    let Some(character) = snapshot.input[journal.cursor..].chars().next() else {
        record_expectation(journal, journal.cursor, "one character");
        return Ok(RequestResult::Fail);
    };
    journal.cursor += character.len_utf8();
    Ok(RequestResult::Return(Value::text(character.to_string())))
}

fn end(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.token.end` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("token reader escaped its isolated transaction"))?;
    let (snapshot, journal) = transaction.parts();
    if journal.cursor == snapshot.input.len() {
        Ok(RequestResult::ReturnUnit)
    } else {
        record_expectation(journal, journal.cursor, "end of token");
        Ok(RequestResult::Fail)
    }
}

fn text_value(
    context: &RequestContext<'_, TokenEffects>,
    value: Value,
    request: &str,
) -> Result<String, TaskError> {
    let CoreValue::Binary(bytes) = eval::eval_value(context.eval_context(), value.as_core())
        .map_err(|error| TaskError::new(error.to_string()))?
    else {
        return Err(TaskError::new(format!("{request} requires text")));
    };
    String::from_utf8(bytes.to_vec())
        .map_err(|_| TaskError::new(format!("{request} requires UTF-8 text")))
}

fn common_prefix_bytes(left: &str, right: &str) -> usize {
    left.char_indices()
        .zip(right.chars())
        .take_while(|((_, left), right)| left == right)
        .map(|((offset, character), _)| offset + character.len_utf8())
        .last()
        .unwrap_or(0)
}

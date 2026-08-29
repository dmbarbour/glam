use glam::reflection::{
    EffectRequestSpec, RequestContext, RequestResult, TaskHalt, TaskSpecialization,
};
use glam::{TextPattern, Value, Values};

use super::{TokenHost, TokenJournal, literal_completion, record_expectation};

#[derive(Clone, Copy)]
pub(super) struct TokenEffects;

#[derive(Clone, Copy)]
pub(in crate::command_line) enum TokenRequest {
    Text,
    Regex,
    TextSpan,
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
    ) -> Result<RequestResult, TaskHalt> {
        match request {
            TokenRequest::Text => text(arguments, context),
            TokenRequest::Regex => regex_span(arguments, context),
            TokenRequest::TextSpan => text_span(arguments, context),
            TokenRequest::Any => any(arguments, context),
            TokenRequest::End => end(arguments, context),
        }
    }
}

pub(in crate::command_line) fn request_specs() -> Vec<EffectRequestSpec<TokenRequest>> {
    vec![
        request("text", 1, TokenRequest::Text),
        request("regex", 1, TokenRequest::Regex),
        request("text_span", 0, TokenRequest::TextSpan),
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
) -> Result<RequestResult, TaskHalt> {
    let [literal]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.token.text` received the wrong number of arguments"))?;
    let literal = text_value(context, literal, "`.token.text`")?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("token reader escaped its isolated transaction"))?;
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
) -> Result<RequestResult, TaskHalt> {
    let [pattern]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.token.regex` received the wrong number of arguments"))?;
    let pattern = text_value(context, pattern, "`.token.regex`")?;
    let matcher = TextPattern::parse(&pattern)
        .map_err(|error| TaskHalt::new(format!("invalid `.token.regex` pattern: {error}")))?;
    let values = context.values();
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("token reader escaped its isolated transaction"))?;
    let (snapshot, journal) = transaction.parts();
    let cursor = journal.cursor;
    let remaining = &snapshot.input[cursor..];
    let matched = matcher.match_prefix(remaining);
    let Some(matched) = matched else {
        record_expectation(journal, cursor, "matching text");
        return Ok(RequestResult::Fail);
    };
    if snapshot
        .completion_offset
        .is_some_and(|split| cursor <= split && cursor + matched.len() > split)
    {
        let split = snapshot
            .completion_offset
            .expect("checked completion offset");
        record_expectation(journal, split, "matching text");
        return Ok(RequestResult::Fail);
    }
    journal.cursor = cursor + matched.len();
    Ok(RequestResult::Return(span_value(&values, matched)))
}

fn text_span(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.token.text_span` received arguments"))?;
    let values = context.values();
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("token reader escaped its isolated transaction"))?;
    let (snapshot, journal) = transaction.parts();
    let cursor = journal.cursor;
    let remaining = &snapshot.input[cursor..];
    if remaining.is_empty() {
        record_expectation(journal, cursor, "remaining text");
        return Ok(RequestResult::Fail);
    }
    if snapshot
        .completion_offset
        .is_some_and(|split| cursor <= split && cursor + remaining.len() > split)
    {
        let split = snapshot
            .completion_offset
            .expect("checked completion offset");
        record_expectation(journal, split, "remaining text");
        return Ok(RequestResult::Fail);
    }
    journal.cursor = snapshot.input.len();
    Ok(RequestResult::Return(span_value(&values, remaining)))
}

fn any(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.token.any` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("token reader escaped its isolated transaction"))?;
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
    Ok(RequestResult::Return(
        context.values().text(character.to_string()),
    ))
}

fn span_value(values: &Values, span: &str) -> Value {
    values
        .record([("span", values.text(span))])
        .expect("token span values share one runtime")
}

fn end(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("`.token.end` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("token reader escaped its isolated transaction"))?;
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
) -> Result<String, TaskHalt> {
    let value = context.evaluate(&value)?;
    let bytes = value
        .as_bytes()?
        .ok_or_else(|| TaskHalt::new(format!("{request} requires text")))?;
    String::from_utf8(bytes.into())
        .map_err(|_| TaskHalt::new(format!("{request} requires UTF-8 text")))
}

fn common_prefix_bytes(left: &str, right: &str) -> usize {
    left.char_indices()
        .zip(right.chars())
        .take_while(|((_, left), right)| left == right)
        .map(|((offset, character), _)| offset + character.len_utf8())
        .last()
        .unwrap_or(0)
}

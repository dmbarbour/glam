use glam::reflection::{
    EffectRequestSpec, RequestContext, RequestResult, SpecializationRequestInput,
    SpecializationRequestPoll, SpecializationRequestWork, TaskHalt, TaskSpecialization,
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

pub(super) enum TokenRequestWork {
    Immediate {
        request: TokenRequest,
        arguments: Vec<Value>,
    },
    TextStart {
        request: TokenRequest,
        arguments: Vec<Value>,
    },
    TextValue(TokenRequest),
    Poisoned,
}

impl TaskSpecialization for TokenEffects {
    type Host = TokenHost;
    type Request = TokenRequest;
    type RequestWork = TokenRequestWork;
    type Snapshot = super::TokenSnapshot;
    type Journal = TokenJournal;

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        request_specs()
    }

    fn start_request(&self, request: Self::Request, arguments: Vec<Value>) -> Self::RequestWork {
        match request {
            TokenRequest::Text | TokenRequest::Regex => {
                TokenRequestWork::TextStart { request, arguments }
            }
            request => TokenRequestWork::Immediate { request, arguments },
        }
    }
}

impl SpecializationRequestWork<TokenEffects> for TokenRequestWork {
    fn poll(
        &mut self,
        _specialization: &TokenEffects,
        input: Option<SpecializationRequestInput>,
        context: &mut RequestContext<'_, TokenEffects>,
    ) -> Result<SpecializationRequestPoll, TaskHalt> {
        let work = std::mem::replace(self, Self::Poisoned);
        match work {
            Self::Immediate { request, arguments } => {
                assert!(
                    input.is_none(),
                    "immediate token request cannot have demand input"
                );
                let result = match request {
                    TokenRequest::TextSpan => text_span(arguments, context),
                    TokenRequest::Any => any(arguments, context),
                    TokenRequest::End => end(arguments, context),
                    TokenRequest::Text | TokenRequest::Regex => {
                        unreachable!("text token requests use durable preparation")
                    }
                }?;
                Ok(SpecializationRequestPoll::Complete(result))
            }
            Self::TextStart { request, arguments } => {
                assert!(input.is_none(), "new token text request cannot have input");
                let name = token_text_request_name(request);
                let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                    TaskHalt::new(format!("`.{name}` received the wrong number of arguments"))
                })?;
                *self = Self::TextValue(request);
                Ok(SpecializationRequestPoll::Demand(value))
            }
            Self::TextValue(request) => {
                let value = match input.expect("resumed token text request must have input") {
                    SpecializationRequestInput::Value(value) => value,
                    SpecializationRequestInput::Failed(error) => return Err(error),
                };
                let text = evaluated_text(value, token_text_request_name(request))?;
                let result = match request {
                    TokenRequest::Text => text_literal(text, context),
                    TokenRequest::Regex => regex_pattern(text, context),
                    TokenRequest::TextSpan | TokenRequest::Any | TokenRequest::End => {
                        unreachable!("only text token requests await values")
                    }
                }?;
                Ok(SpecializationRequestPoll::Complete(result))
            }
            Self::Poisoned => panic!("completed token request work was polled again"),
        }
    }
}

fn token_text_request_name(request: TokenRequest) -> &'static str {
    match request {
        TokenRequest::Text => "token.text",
        TokenRequest::Regex => "token.regex",
        TokenRequest::TextSpan | TokenRequest::Any | TokenRequest::End => {
            unreachable!("request does not consume text")
        }
    }
}

fn evaluated_text(value: glam::EvaluatedValue, request: &str) -> Result<String, TaskHalt> {
    let bytes = value
        .as_bytes()?
        .ok_or_else(|| TaskHalt::new(format!(".{request} requires text")))?;
    String::from_utf8(bytes.into())
        .map_err(|_| TaskHalt::new(format!(".{request} requires UTF-8 text")))
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

fn text_literal(
    literal: String,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskHalt> {
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

fn regex_pattern(
    pattern: String,
    context: &mut RequestContext<'_, TokenEffects>,
) -> Result<RequestResult, TaskHalt> {
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

fn common_prefix_bytes(left: &str, right: &str) -> usize {
    left.char_indices()
        .zip(right.chars())
        .take_while(|((_, left), right)| left == right)
        .map(|((offset, character), _)| offset + character.len_utf8())
        .last()
        .unwrap_or(0)
}

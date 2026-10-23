//! The CLI's token parser as a pure handler's operations
//! (`pure-handler-token`): private builtins over one token's text and the
//! offset being completed, which an opaque context holds for the whole
//! parse, and over each branch's cursor and evidence, its expectations and
//! completion candidates, which the handler's state holds as plain data.

use std::sync::Arc;

use glam_gc::Visitor;

use super::builtin_machine::RegionalBuiltinPoll;
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};
use crate::core::{
    Builtin, CoreValueFactory, Dict, EvaluationFailure, Key, LazyId, List, OpaquePayloadFamily,
    OpaquePayloadRecord, OpaqueValue, RuntimeValueAccess, Value, keys,
    trace_compatibility_value_managed_edges,
};
use crate::evaluation::EvaluationValueAccess;
use crate::number::Number;
use crate::text_pattern::TextPattern;

/// The token a parse reads: its text, and in completion, the offset being
/// completed.
pub(crate) struct TokenContext {
    input: Arc<str>,
    completion_offset: Option<usize>,
}

// SAFETY: the context holds a token's text and an offset: no Glam value,
// runtime root, managed pointer or capability.
unsafe impl OpaquePayloadFamily for TokenContext {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("CLI token parser context", "src/eval/token_machine.rs");
}

/// The token parser's operations, `.token.*`, which a pure handler adds to
/// its core's requests.
pub(crate) fn token_api_in(_access: &RuntimeValueAccess<'_>) -> Value {
    let operation = |name: &str, builtin| (Key::atom_from_text(name), Value::Builtin(builtin));
    let token = [
        operation("text", Builtin::TokenText),
        operation("regex", Builtin::TokenRegex),
        operation("text_span", Builtin::TokenTextSpan),
        operation("any", Builtin::TokenAny),
        operation("end", Builtin::TokenEnd),
    ]
    .into_iter()
    .fold(Dict::new_sync(), |dict, (key, value)| {
        dict.insert(key, value)
    });
    Value::Dict(Dict::new_sync().insert(Key::atom_from_text("token"), Value::Dict(token)))
}

/// The token parser's state before it reads `input`: its context, the
/// cursor at the start, and no evidence.
pub(crate) fn token_state_in(
    access: &RuntimeValueAccess<'_>,
    values: &CoreValueFactory,
    input: Arc<str>,
    completion_offset: Option<usize>,
) -> Value {
    debug_assert!(access.belongs_to(values));
    let context = OpaqueValue::new(
        values,
        Arc::new(TokenContext {
            input,
            completion_offset,
        }),
    );
    Value::Dict(
        Dict::new_sync()
            .insert(keys::CONTEXT.clone(), Value::Opaque(context))
            .insert(keys::CURSOR.clone(), Value::Number(Number::from_usize(0)))
            .insert(keys::EXPECTATIONS.clone(), Value::List(List::empty()))
            .insert(keys::CANDIDATES.clone(), Value::List(List::empty())),
    )
}

/// What a token operation waits for next.
enum TokenPhase {
    /// The operation's argument: a literal or a pattern.
    Argument(Value),
    /// The state, `{user, handler}`.
    State,
    /// The handler's state.
    Handler(Value),
    /// The handler's context, the cursor after it.
    Context { context: Value, cursor: Value },
    /// The handler's cursor.
    Cursor(Value),
}

/// One token operation, applied to its argument, if any, and the state.
pub(in crate::eval) struct RegionalTokenMachine {
    builtin: Builtin,
    phase: TokenPhase,
    demand: Option<RegionalWhnfWork>,
    state: Value,
    text: Option<String>,
    user: Option<Value>,
    handler: Option<Dict>,
    context: Option<Arc<TokenContext>>,
    source_owner: LazyId,
}

impl RegionalTokenMachine {
    pub(in crate::eval) fn supports(builtin: Builtin) -> bool {
        matches!(
            builtin,
            Builtin::TokenText
                | Builtin::TokenRegex
                | Builtin::TokenTextSpan
                | Builtin::TokenAny
                | Builtin::TokenEnd
        )
    }

    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        builtin: Builtin,
        arguments: &[Value],
    ) -> Self {
        let duplicate = |value: &Value| access.values().duplicate_value(value);
        let (phase, state) = match arguments {
            [argument, state] => (TokenPhase::Argument(duplicate(argument)), duplicate(state)),
            [state] => (TokenPhase::State, duplicate(state)),
            _ => unreachable!("a token operation takes its argument, if any, and the state"),
        };
        Self {
            builtin,
            phase,
            demand: None,
            state,
            text: None,
            user: None,
            handler: None,
            context: None,
            source_owner,
        }
    }

    fn whnf_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
        value: &Value,
    ) -> Result<Value, RegionalBuiltinPoll> {
        let source_owner = self.source_owner;
        let demand = self.demand.get_or_insert_with(|| {
            RegionalWhnfWork::from_focus(access, access.values().duplicate_value(value))
                .with_source_owner(source_owner)
        });
        match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
            RegionalWhnfStatus::Ready(value) => {
                self.demand = None;
                Ok(value)
            }
            RegionalWhnfStatus::Boundary(request) => Err(RegionalBuiltinPoll::Boundary(request)),
            RegionalWhnfStatus::BudgetExhausted => Err(RegionalBuiltinPoll::BudgetExhausted),
            RegionalWhnfStatus::Failed(failure) => Err(RegionalBuiltinPoll::Failed(failure)),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut crate::evaluation::EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        loop {
            let pending = match &self.phase {
                TokenPhase::Argument(value)
                | TokenPhase::Handler(value)
                | TokenPhase::Context { context: value, .. }
                | TokenPhase::Cursor(value) => access.values().duplicate_value(value),
                TokenPhase::State => access.values().duplicate_value(&self.state),
            };
            let value = match self.whnf_in(access, step_budget, &pending) {
                Ok(value) => value,
                Err(poll) => return poll,
            };
            self.phase = match std::mem::replace(&mut self.phase, TokenPhase::State) {
                TokenPhase::Argument(_) => {
                    let Value::Binary(bytes) = value else {
                        return failure(format!("{} expects text", token_name(self.builtin)));
                    };
                    let Ok(text) = String::from_utf8(bytes.to_vec()) else {
                        return failure(format!("{} expects UTF-8 text", token_name(self.builtin)));
                    };
                    self.text = Some(text);
                    TokenPhase::State
                }
                TokenPhase::State => {
                    let Value::Dict(state) = value else {
                        return failure("a token operation's state is a dict");
                    };
                    let field = |key: &Key| {
                        state
                            .get(key)
                            .map(|value| access.values().duplicate_value(value))
                            .unwrap_or_else(|| Value::Dict(Dict::new_sync()))
                    };
                    self.user = Some(field(&keys::USER));
                    TokenPhase::Handler(field(&keys::HANDLER))
                }
                TokenPhase::Handler(_) => {
                    let Value::Dict(handler) = value else {
                        return failure("the token parser's state is a dict");
                    };
                    let (Some(context), Some(cursor)) =
                        (handler.get(&keys::CONTEXT), handler.get(&keys::CURSOR))
                    else {
                        return failure("the token parser's state has a context and a cursor");
                    };
                    let phase = TokenPhase::Context {
                        context: access.values().duplicate_value(context),
                        cursor: access.values().duplicate_value(cursor),
                    };
                    self.handler = Some(handler);
                    phase
                }
                TokenPhase::Context { cursor, .. } => {
                    let Value::Opaque(context) = value else {
                        return failure("the token parser's context is opaque");
                    };
                    let Some(context) = context.downcast::<TokenContext>(access.values().values())
                    else {
                        return failure("the token parser's context is another's");
                    };
                    self.context = Some(context);
                    TokenPhase::Cursor(cursor)
                }
                TokenPhase::Cursor(_) => {
                    let Value::Number(cursor) = value else {
                        return failure("the token parser's cursor is a number");
                    };
                    let Some(cursor) = cursor.to_usize_if_integer() else {
                        return failure("the token parser's cursor is an offset");
                    };
                    return self.answer_in(access, cursor);
                }
            };
        }
    }

    /// Answers the operation once its inputs are in hand.
    fn answer_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        cursor: usize,
    ) -> RegionalBuiltinPoll {
        let context = self
            .context
            .take()
            .expect("the context precedes the cursor");
        let step = match token_step(self.builtin, self.text.as_deref(), &context, cursor) {
            Ok(step) => step,
            Err(message) => return failure(message),
        };
        let values = access.values();
        let handler = self
            .handler
            .take()
            .expect("the handler's state precedes its cursor");
        let evidence = |key: &Key, items: Vec<Value>| -> Result<Value, RegionalBuiltinPoll> {
            let prior = handler
                .get(key)
                .map(|value| values.duplicate_value(value))
                .unwrap_or_else(|| Value::List(List::empty()));
            if items.is_empty() {
                return Ok(prior);
            }
            super::sequence::append_values(values, prior, Value::List(List::from_values(items)))
                .map_err(|halt| RegionalBuiltinPoll::Failed(halt.into_permanent_failure()))
        };
        let item = |offset: usize, field: &Key, text: String| {
            Value::Dict(
                Dict::new_sync()
                    .insert(
                        keys::OFFSET.clone(),
                        Value::Number(Number::from_usize(offset)),
                    )
                    .insert(field.clone(), Value::binary_from_text(&text)),
            )
        };
        let expectations = step
            .expectations
            .into_iter()
            .map(|(offset, label)| item(offset, &keys::LABEL, label))
            .collect();
        let candidates = step
            .candidates
            .into_iter()
            .map(|(offset, replacement)| item(offset, &keys::REPLACEMENT, replacement))
            .collect();
        let (expectations, candidates) = match (
            evidence(&keys::EXPECTATIONS, expectations),
            evidence(&keys::CANDIDATES, candidates),
        ) {
            (Ok(expectations), Ok(candidates)) => (expectations, candidates),
            (Err(poll), _) | (_, Err(poll)) => return poll,
        };
        let cursor = step.read.as_ref().map_or(cursor, |(_, cursor)| *cursor);
        let handler = handler
            .insert(
                keys::CURSOR.clone(),
                Value::Number(Number::from_usize(cursor)),
            )
            .insert(keys::EXPECTATIONS.clone(), expectations)
            .insert(keys::CANDIDATES.clone(), candidates);
        let state = Value::Dict(
            Dict::new_sync()
                .insert(
                    keys::USER.clone(),
                    self.user.take().expect("the state precedes the handler's"),
                )
                .insert(keys::HANDLER.clone(), Value::Dict(handler)),
        );
        let outcome = match step.read {
            Some((read, _)) => Dict::new_sync()
                .insert(keys::VALUE.clone(), read.into_value_in(values))
                .insert(keys::STATE.clone(), state),
            None => Dict::new_sync().insert(keys::FAILED.clone(), state),
        };
        RegionalBuiltinPoll::Ready(Value::List(List::from_values(vec![Value::Dict(outcome)])))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        match &self.phase {
            TokenPhase::Argument(value)
            | TokenPhase::Handler(value)
            | TokenPhase::Cursor(value) => {
                trace_compatibility_value_managed_edges(value, visitor);
            }
            TokenPhase::Context { context, cursor } => {
                trace_compatibility_value_managed_edges(context, visitor);
                trace_compatibility_value_managed_edges(cursor, visitor);
            }
            TokenPhase::State => {}
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
        trace_compatibility_value_managed_edges(&self.state, visitor);
        if let Some(user) = &self.user {
            trace_compatibility_value_managed_edges(user, visitor);
        }
        if let Some(handler) = &self.handler {
            trace_compatibility_value_managed_edges(&Value::Dict(handler.clone()), visitor);
        }
    }
}

fn failure(message: impl Into<String>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}

fn token_name(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::TokenText => "`.token.text`",
        Builtin::TokenRegex => "`.token.regex`",
        Builtin::TokenTextSpan => "`.token.text_span`",
        Builtin::TokenAny => "`.token.any`",
        Builtin::TokenEnd => "`.token.end`",
        _ => unreachable!("only token operations are named here"),
    }
}

/// What a token operation reads: nothing, a span, or one character.
enum TokenRead {
    Unit,
    Span(String),
    Text(String),
}

impl TokenRead {
    fn into_value_in(self, values: &RuntimeValueAccess<'_>) -> Value {
        match self {
            Self::Unit => values.unit(),
            Self::Span(span) => Value::Dict(
                Dict::new_sync().insert(keys::SPAN.clone(), Value::binary_from_text(&span)),
            ),
            Self::Text(text) => Value::binary_from_text(&text),
        }
    }
}

/// An operation's answer: what it read with the cursor after it, or a
/// failure, and the evidence it adds either way.
struct TokenStep {
    read: Option<(TokenRead, usize)>,
    expectations: Vec<(usize, String)>,
    candidates: Vec<(usize, String)>,
}

impl TokenStep {
    fn read(read: TokenRead, cursor: usize) -> Self {
        Self {
            read: Some((read, cursor)),
            expectations: Vec::new(),
            candidates: Vec::new(),
        }
    }

    fn expected(offset: usize, label: impl Into<String>) -> Self {
        Self {
            read: None,
            expectations: vec![(offset, label.into())],
            candidates: Vec::new(),
        }
    }
}

fn token_step(
    builtin: Builtin,
    text: Option<&str>,
    context: &TokenContext,
    cursor: usize,
) -> Result<TokenStep, String> {
    let input = context.input.as_ref();
    let split = context.completion_offset;
    let crosses_split = |end: usize| split.is_some_and(|split| cursor <= split && end > split);
    Ok(match builtin {
        Builtin::TokenText => {
            let literal = text.expect("`.token.text` reads its literal");
            if let Some(split) = split.filter(|_| crosses_split(cursor + literal.len())) {
                let mut step = TokenStep::expected(split, format!("`{literal}`"));
                if let Some(replacement) = literal_completion(input, cursor, split, literal) {
                    step.candidates.push((split, replacement));
                }
                step
            } else if input
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with(literal))
            {
                TokenStep::read(TokenRead::Unit, cursor + literal.len())
            } else {
                let matched = input
                    .get(cursor..)
                    .map(|rest| common_prefix_bytes(rest, literal))
                    .unwrap_or(0);
                TokenStep::expected(cursor + matched, format!("`{literal}`"))
            }
        }
        Builtin::TokenRegex => {
            let pattern = text.expect("`.token.regex` reads its pattern");
            let matcher = TextPattern::parse(pattern)
                .map_err(|error| format!("invalid `.token.regex` pattern: {error}"))?;
            match matcher.match_prefix(&input[cursor..]) {
                None => TokenStep::expected(cursor, "matching text"),
                Some(matched) if crosses_split(cursor + matched.len()) => {
                    TokenStep::expected(split.expect("a crossing has a split"), "matching text")
                }
                Some(matched) => {
                    TokenStep::read(TokenRead::Span(matched.to_owned()), cursor + matched.len())
                }
            }
        }
        Builtin::TokenTextSpan => {
            let remaining = &input[cursor..];
            if remaining.is_empty() {
                TokenStep::expected(cursor, "remaining text")
            } else if crosses_split(input.len()) {
                TokenStep::expected(split.expect("a crossing has a split"), "remaining text")
            } else {
                TokenStep::read(TokenRead::Span(remaining.to_owned()), input.len())
            }
        }
        Builtin::TokenAny => match input[cursor..].chars().next() {
            Some(character) if !split.is_some_and(|split| cursor >= split) => TokenStep::read(
                TokenRead::Text(character.to_string()),
                cursor + character.len_utf8(),
            ),
            _ => TokenStep::expected(cursor, "one character"),
        },
        Builtin::TokenEnd if cursor == input.len() => TokenStep::read(TokenRead::Unit, cursor),
        Builtin::TokenEnd => TokenStep::expected(cursor, "end of token"),
        _ => unreachable!("only token operations step here"),
    })
}

/// The whole token completed with `literal` at `split`, keeping what
/// follows the split and overlaps the literal's end.
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

fn common_prefix_bytes(left: &str, right: &str) -> usize {
    left.char_indices()
        .zip(right.chars())
        .take_while(|((_, left), right)| left == right)
        .map(|((offset, character), _)| offset + character.len_utf8())
        .last()
        .unwrap_or(0)
}

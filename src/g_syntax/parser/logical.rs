//! Parser-neutral logical-token storage prepared for macro expansion.
//!
//! Phase 1 mirrors original tokens through compact payload arenas and rebuilds
//! structural indices without changing parser input. Later phases splice
//! generated fragments and embedded values into this representation before
//! materializing ordinary parser tokens.

use std::ops::Range;

use crate::core::Value;
use crate::number::Number;

use super::super::Diagnostic;
use super::lexical::{
    ByteSpan, Delimiter, EmbeddedValueId, GroupId, LeadingTrivia, LexedSource, NumberId, TextId,
    TokenKind, lex_source,
};

type ExpansionOriginId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalToken<'source> {
    kind: LogicalTokenKind<'source>,
    span: ByteSpan,
    leading: LeadingTrivia,
    expansion_origin: Option<ExpansionOriginId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LogicalTokenKind<'source> {
    Name(&'source str),
    Number(NumberId),
    InvalidNumber(&'source str),
    Text(TextId),
    Embedded(EmbeddedValueId),
    Symbol(&'source str),
    Open(Delimiter),
    Close(Delimiter),
    LineStart { indentation: usize },
    Unknown(char),
}

impl<'source> LogicalTokenKind<'source> {
    fn from_original(kind: &TokenKind<'source>) -> Self {
        match kind {
            TokenKind::Name(name) => Self::Name(name),
            TokenKind::Number(id) => Self::Number(*id),
            TokenKind::InvalidNumber(text) => Self::InvalidNumber(text),
            TokenKind::Text(id) => Self::Text(*id),
            TokenKind::Embedded(id) => Self::Embedded(*id),
            TokenKind::Symbol(symbol) => Self::Symbol(symbol),
            TokenKind::Open { delimiter, .. } => Self::Open(*delimiter),
            TokenKind::Close { delimiter, .. } => Self::Close(*delimiter),
            TokenKind::LineStart { indentation } => Self::LineStart {
                indentation: *indentation,
            },
            TokenKind::Unknown(scalar) => Self::Unknown(*scalar),
        }
    }

    fn materialize(self, group: Option<GroupId>) -> Option<TokenKind<'source>> {
        Some(match self {
            Self::Name(name) => TokenKind::Name(name),
            Self::Number(id) => TokenKind::Number(id),
            Self::InvalidNumber(text) => TokenKind::InvalidNumber(text),
            Self::Text(id) => TokenKind::Text(id),
            Self::Embedded(id) => TokenKind::Embedded(id),
            Self::Symbol(symbol) => TokenKind::Symbol(symbol),
            Self::Open(delimiter) => TokenKind::Open {
                group: group?,
                delimiter,
            },
            Self::Close(delimiter) => TokenKind::Close {
                group: group?,
                delimiter,
            },
            Self::LineStart { indentation } => TokenKind::LineStart { indentation },
            Self::Unknown(scalar) => TokenKind::Unknown(scalar),
        })
    }
}

/// Compact payload and structure mirror for one original source.
pub(super) struct LogicalSource<'source> {
    tokens: Vec<LogicalToken<'source>>,
    numbers: Vec<Number>,
    texts: Vec<String>,
    generated_texts: Vec<GeneratedText>,
    embedded_values: Vec<Value>,
    expansion_origins: Vec<ByteSpan>,
    index: LogicalIndex,
}

impl<'source> LogicalSource<'source> {
    pub(super) fn from_original(source: &LexedSource<'source>) -> Self {
        let tokens = source
            .tokens()
            .iter()
            .map(|token| LogicalToken {
                kind: LogicalTokenKind::from_original(token.kind()),
                span: token.span(),
                leading: token.leading(),
                expansion_origin: None,
            })
            .collect::<Vec<_>>();
        let index = LogicalIndex::rebuild(&tokens)
            .expect("the validated source lexer must emit balanced logical structure");
        Self {
            tokens,
            numbers: source.numbers().to_vec(),
            texts: source
                .texts()
                .iter()
                .map(|text| text.value().to_owned())
                .collect(),
            generated_texts: Vec::new(),
            embedded_values: source.embedded_values().to_vec(),
            expansion_origins: Vec::new(),
            index,
        }
    }

    pub(super) fn round_trips(&self, source: &LexedSource<'source>) -> bool {
        let Some(materialized) = self.materialize_original(source) else {
            return false;
        };
        self.tokens.len() == source.tokens().len()
            && self.numbers == source.numbers()
            && self.texts.len() == source.texts().len()
            && self
                .texts
                .iter()
                .zip(source.texts())
                .all(|(logical, original)| logical == original.value())
            && self.generated_texts.is_empty()
            && self.embedded_values == source.embedded_values()
            && self.expansion_origins.is_empty()
            && materialized.tokens() == source.tokens()
            && self.index.matches_original(source)
    }

    pub(super) fn materialize_original(
        &self,
        source: &LexedSource<'source>,
    ) -> Option<LexedSource<'source>> {
        let tokens = self
            .tokens
            .iter()
            .enumerate()
            .map(|(index, token)| {
                (token.expansion_origin.is_none()).then(|| {
                    Some(super::lexical::SpannedToken::new(
                        token
                            .kind
                            .clone()
                            .materialize(self.index.token_groups[index])?,
                        token.span,
                        token.leading,
                    ))
                })?
            })
            .collect::<Option<Vec<_>>>()?;
        Some(source.with_tokens(tokens))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalGroup {
    delimiter: Delimiter,
    open_token: usize,
    close_token: usize,
    parent: Option<GroupId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogicalIndex {
    token_groups: Vec<Option<GroupId>>,
    groups: Vec<LogicalGroup>,
    declaration_tokens: Vec<Range<usize>>,
}

impl LogicalIndex {
    fn rebuild(tokens: &[LogicalToken<'_>]) -> Result<Self, String> {
        let mut token_groups = vec![None; tokens.len()];
        let mut groups: Vec<LogicalGroup> = Vec::new();
        let mut stack: Vec<GroupId> = Vec::new();
        let mut declaration_tokens = Vec::new();
        let mut declaration_start = None;

        for (index, token) in tokens.iter().enumerate() {
            match token.kind {
                LogicalTokenKind::LineStart { indentation: 0 } if stack.is_empty() => {
                    if let Some(start) = declaration_start.replace(index) {
                        declaration_tokens.push(start..index);
                    }
                }
                LogicalTokenKind::Open(delimiter) => {
                    let group = groups.len();
                    token_groups[index] = Some(group);
                    groups.push(LogicalGroup {
                        delimiter,
                        open_token: index,
                        close_token: usize::MAX,
                        parent: stack.last().copied(),
                    });
                    stack.push(group);
                }
                LogicalTokenKind::Close(delimiter) => {
                    let Some(group) = stack.pop() else {
                        return Err("logical token stream closes an unopened group".to_owned());
                    };
                    if groups[group].delimiter != delimiter {
                        return Err("logical token stream closes the wrong delimiter".to_owned());
                    }
                    groups[group].close_token = index;
                    token_groups[index] = Some(group);
                }
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err("logical token stream leaves a group open".to_owned());
        }
        if let Some(start) = declaration_start {
            declaration_tokens.push(start..tokens.len());
        }

        Ok(Self {
            token_groups,
            groups,
            declaration_tokens,
        })
    }

    fn matches_original(&self, source: &LexedSource<'_>) -> bool {
        self.groups.len() == source.groups().len()
            && self
                .groups
                .iter()
                .zip(source.groups())
                .all(|(logical, original)| {
                    logical.delimiter == original.delimiter()
                        && logical.open_token == original.open_token()
                        && Some(logical.close_token) == original.close_token()
                        && logical.parent == original.parent()
                })
            && self.declaration_tokens.len() == source.declarations().len()
            && self
                .declaration_tokens
                .iter()
                .zip(source.declarations())
                .all(|(logical, original)| logical == &original.tokens())
    }
}

/// A locally classified `.write.text` payload.
///
/// Tokens refer to byte spans in `text`; parsed data remains in compact arenas.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneratedText {
    text: String,
    tokens: Vec<GeneratedToken>,
    numbers: Vec<Number>,
    texts: Vec<String>,
    index: LogicalIndex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneratedToken {
    kind: GeneratedTokenKind,
    span: ByteSpan,
    leading: LeadingTrivia,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GeneratedTokenKind {
    Name,
    Number(NumberId),
    Text(TextId),
    Symbol,
    Open(Delimiter),
    Close(Delimiter),
    LineStart { indentation: usize },
    Unknown(char),
}

impl GeneratedText {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Phase 1 foundation is consumed by the Phase 4 macro writer"
        )
    )]
    fn classify(text: String) -> Result<Self, Vec<Diagnostic>> {
        if text.contains(['@', '#']) {
            return Err(vec![Diagnostic::error(
                1,
                "generated macro text cannot contain `@` or `#`",
            )]);
        }

        let lexical = lex_source(&text);
        if lexical.has_errors() {
            return Err(lexical.diagnostics().to_vec());
        }
        let tokens = lexical
            .tokens()
            .iter()
            .map(|token| {
                let kind = match token.kind() {
                    TokenKind::Name(_) => GeneratedTokenKind::Name,
                    TokenKind::Number(id) => GeneratedTokenKind::Number(*id),
                    TokenKind::InvalidNumber(_) => {
                        unreachable!("invalid numbers make lexical classification fail")
                    }
                    TokenKind::Text(id) => GeneratedTokenKind::Text(*id),
                    TokenKind::Embedded(_) => {
                        unreachable!("text classification cannot produce embedded data")
                    }
                    TokenKind::Symbol(_) => GeneratedTokenKind::Symbol,
                    TokenKind::Open { delimiter, .. } => GeneratedTokenKind::Open(*delimiter),
                    TokenKind::Close { delimiter, .. } => GeneratedTokenKind::Close(*delimiter),
                    TokenKind::LineStart { indentation } => GeneratedTokenKind::LineStart {
                        indentation: *indentation,
                    },
                    TokenKind::Unknown(scalar) => GeneratedTokenKind::Unknown(*scalar),
                };
                GeneratedToken {
                    kind,
                    span: token.span(),
                    leading: token.leading(),
                }
            })
            .collect::<Vec<_>>();
        let logical_tokens = tokens
            .iter()
            .map(|token| LogicalToken {
                kind: match token.kind {
                    GeneratedTokenKind::Name => LogicalTokenKind::Name(""),
                    GeneratedTokenKind::Number(id) => LogicalTokenKind::Number(id),
                    GeneratedTokenKind::Text(id) => LogicalTokenKind::Text(id),
                    GeneratedTokenKind::Symbol => LogicalTokenKind::Symbol(""),
                    GeneratedTokenKind::Open(delimiter) => LogicalTokenKind::Open(delimiter),
                    GeneratedTokenKind::Close(delimiter) => LogicalTokenKind::Close(delimiter),
                    GeneratedTokenKind::LineStart { indentation } => {
                        LogicalTokenKind::LineStart { indentation }
                    }
                    GeneratedTokenKind::Unknown(scalar) => LogicalTokenKind::Unknown(scalar),
                },
                span: token.span,
                leading: token.leading,
                expansion_origin: None,
            })
            .collect::<Vec<_>>();
        let index = LogicalIndex::rebuild(&logical_tokens)
            .map_err(|error| vec![Diagnostic::error(1, error)])?;
        let numbers = lexical.numbers().to_vec();
        let texts = lexical
            .texts()
            .iter()
            .map(|text| text.value().to_owned())
            .collect();
        drop(lexical);

        Ok(Self {
            text,
            tokens,
            numbers,
            texts,
            index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Severity;

    #[test]
    fn original_tokens_and_indices_round_trip_without_reclassification() {
        let source = "language g0\nfirst = [1, (2)]\nobject nested with\n  member = \"x\"\n";
        let lexical = lex_source(source);
        let logical = LogicalSource::from_original(&lexical);

        assert!(logical.round_trips(&lexical));

        let materialized = logical.materialize_original(&lexical).unwrap();
        let direct = super::super::source::parse_lexed(&lexical);
        let round_trip = super::super::source::parse_lexed(&materialized);
        assert_eq!(round_trip, direct);
    }

    #[test]
    fn generated_text_is_classified_locally_and_reserves_source_markers() {
        let generated = GeneratedText::classify("name (42) \"text\"".to_owned()).unwrap();
        assert_eq!(generated.numbers, [Number::from(42_i64)]);
        assert_eq!(generated.texts, ["text"]);
        assert_eq!(generated.index.groups.len(), 1);

        for text in ["@next", "# comment", "left@right"] {
            let diagnostics = GeneratedText::classify(text.to_owned()).unwrap_err();
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.message.contains("cannot contain")
            }));
        }
    }

    #[test]
    fn invalid_numbers_fail_during_generated_text_classification() {
        let diagnostics =
            GeneratedText::classify("1e999999999999999999999999".to_owned()).unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("invalid number literal")),
            "{diagnostics:?}"
        );
    }
}

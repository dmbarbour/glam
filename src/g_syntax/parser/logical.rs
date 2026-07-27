//! Parser-neutral logical-token storage prepared for macro expansion.
//!
//! Phase 1 mirrors original tokens through compact payload arenas and rebuilds
//! structural indices without changing parser input. Later phases splice
//! generated fragments and embedded values into this representation before
//! materializing ordinary parser tokens.

use std::ops::Range;
use std::sync::Arc;

use crate::core::Value;
use crate::number::Number;

use super::super::Diagnostic;
use super::super::keywords::g0_layout_introducer;
use super::super::macro_expansion::{
    MacroDelimiter, MacroInput, MacroInputElement, MacroInputKind, MacroInputLayout, MacroOutput,
};
use super::input::{TokenRange, TokenView};
use super::layout::LayoutView;
use super::lexical::{
    ByteSpan, DeclarationSection, Delimiter, EmbeddedValueId, GroupId, LeadingTrivia, LexedSource,
    NumberId, TextId, TokenKind, lex_source,
};

type ExpansionOriginId = usize;
/// ASCII SUB, excluded from macro text writes with the rest of C0.
pub(super) const EMBEDDED_MARKER: char = '\u{001a}';

pub(super) struct MacroInvocation {
    pub(super) path: Vec<String>,
    pub(super) start: usize,
    anchor_position: bool,
    indentation: usize,
    pub(super) input: MacroInput,
}

struct MacroLogicalItem {
    tokens: TokenRange,
    anchor_position: bool,
    indentation: usize,
}

#[derive(Clone)]
pub(super) struct OriginalMacroInvocation {
    pub(super) id: usize,
    pub(super) path: Vec<String>,
    pub(super) start: usize,
    pub(super) line: usize,
}

pub(super) struct DeclarationMacroWork {
    line: usize,
    invocations: Vec<OriginalMacroInvocation>,
    text: String,
    embedded_values: Vec<Value>,
}

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

fn collect_macro_invocations(
    source: &LexedSource<'_>,
    declaration: &DeclarationSection,
) -> Result<Vec<OriginalMacroInvocation>, Diagnostic> {
    let tokens = source.tokens();
    let range = declaration.tokens();
    let mut invocations = Vec::new();
    let mut index = range.start;
    while index < range.end {
        if !matches!(tokens[index].kind(), TokenKind::Symbol("@")) {
            index += 1;
            continue;
        }
        let invocation = index;
        index += 1;
        let Some(first) = tokens.get(index).filter(|_| index < range.end) else {
            return Err(macro_head_error(source, tokens[invocation].span()));
        };
        if first.leading() != LeadingTrivia::Joint {
            return Err(macro_head_error(source, tokens[invocation].span()));
        }
        let TokenKind::Name(first) = first.kind() else {
            return Err(macro_head_error(source, tokens[invocation].span()));
        };
        let mut path = vec![(*first).to_owned()];
        index += 1;
        while index < range.end
            && matches!(tokens[index].kind(), TokenKind::Symbol("."))
            && tokens[index].leading() == LeadingTrivia::Joint
        {
            index += 1;
            let Some(component) = tokens.get(index).filter(|_| index < range.end) else {
                return Err(macro_head_error(source, tokens[invocation].span()));
            };
            if component.leading() != LeadingTrivia::Joint {
                return Err(macro_head_error(source, tokens[invocation].span()));
            }
            let TokenKind::Name(component) = component.kind() else {
                return Err(macro_head_error(source, tokens[invocation].span()));
            };
            path.push((*component).to_owned());
            index += 1;
        }
        invocations.push(OriginalMacroInvocation {
            id: invocations.len(),
            path,
            start: tokens[invocation].span().start(),
            line: source
                .line_at_byte(tokens[invocation].span().start())
                .unwrap_or(declaration.line()),
        });
    }
    Ok(invocations)
}

impl DeclarationMacroWork {
    pub(super) fn from_original(
        source: &LexedSource<'_>,
        declaration: &DeclarationSection,
    ) -> Result<Option<Self>, Diagnostic> {
        let mut invocations = collect_macro_invocations(source, declaration)?;
        if invocations.is_empty() {
            return Ok(None);
        }
        let declaration_start = declaration.span().start();
        for invocation in &mut invocations {
            invocation.start -= declaration_start;
        }
        invocations.sort_by_key(|invocation| std::cmp::Reverse(invocation.start));
        let text = source
            .source_slice(declaration.span())
            .expect("declaration spans should remain within their source")
            .to_owned();
        Ok(Some(Self {
            line: declaration.line(),
            invocations,
            text,
            embedded_values: Vec::new(),
        }))
    }

    pub(super) fn invocations(&self) -> &[OriginalMacroInvocation] {
        &self.invocations
    }

    pub(super) fn current_invocation(
        &self,
        original: &OriginalMacroInvocation,
    ) -> Result<MacroInvocation, Vec<Diagnostic>> {
        let lexical = self.lexical()?;
        let Some(declaration) = lexical.declarations().first() else {
            return Err(vec![Diagnostic::error(
                original.line,
                format!(
                    "macro invocation {} disappeared before it could expand",
                    original.id
                ),
            )]);
        };
        let invocation =
            macro_invocation_at(&lexical, declaration, original.start).map_err(|diagnostic| {
                vec![Diagnostic::error(
                    original.line,
                    format!(
                        "macro invocation {} became invalid: {}",
                        original.id, diagnostic.message
                    ),
                )]
            })?;
        if invocation.path != original.path {
            return Err(vec![Diagnostic::error(
                original.line,
                format!(
                    "macro invocation {} changed from `{}` to `{}` before expansion",
                    original.id,
                    original.path.join("."),
                    invocation.path.join(".")
                ),
            )]);
        }
        Ok(invocation)
    }

    pub(super) fn splice(
        &mut self,
        invocation: &MacroInvocation,
        consumed_end: usize,
        output: &[MacroOutput],
        line: usize,
    ) -> Result<(), Vec<Diagnostic>> {
        let anchor_expansion = matches!(output.first(), Some(MacroOutput::Anchor));
        if anchor_expansion && !invocation.anchor_position {
            return Err(vec![Diagnostic::error(
                line,
                "anchored macro output requires an invocation at the start of its logical item",
            )]);
        }
        let (generated, embedded) = generated_output(output, line, invocation.indentation)?;
        if !(invocation.start <= consumed_end && consumed_end <= self.text.len()) {
            return Err(vec![Diagnostic::error(
                line,
                "macro reader produced an invalid source range",
            )]);
        }
        if !self.text.is_char_boundary(invocation.start)
            || !self.text.is_char_boundary(consumed_end)
        {
            return Err(vec![Diagnostic::error(
                line,
                "macro reader stopped outside a UTF-8 boundary",
            )]);
        }
        let embedded_start = marker_count(&self.text[..invocation.start]);
        let embedded_end =
            embedded_start + marker_count(&self.text[invocation.start..consumed_end]);
        self.text
            .replace_range(invocation.start..consumed_end, &generated);
        self.embedded_values
            .splice(embedded_start..embedded_end, embedded);
        Ok(())
    }

    pub(super) fn materialize(&self) -> (String, Vec<Value>) {
        let mut source = "\n".repeat(self.line.saturating_sub(1));
        source.push_str(&self.text);
        (source, self.embedded_values.clone())
    }

    fn lexical(&self) -> Result<LexedSource<'_>, Vec<Diagnostic>> {
        let lexical = lex_source(&self.text)
            .replace_unknowns_with_embedded(EMBEDDED_MARKER, self.embedded_values.clone())
            .map_err(|error| vec![Diagnostic::error(self.line, error)])?;
        if lexical.has_errors() {
            let mut diagnostics = lexical.diagnostics().to_vec();
            for diagnostic in &mut diagnostics {
                diagnostic.line += self.line - 1;
            }
            return Err(diagnostics);
        }
        Ok(lexical)
    }
}

fn macro_invocation_at(
    source: &LexedSource<'_>,
    declaration: &DeclarationSection,
    start: usize,
) -> Result<MacroInvocation, Diagnostic> {
    let tokens = source.tokens();
    let range = declaration.tokens();
    let Some(invocation) = range.clone().find(|index| {
        tokens[*index].span().start() == start
            && matches!(tokens[*index].kind(), TokenKind::Symbol("@"))
    }) else {
        return Err(Diagnostic::error(
            declaration.line(),
            "macro invocation no longer starts on a token boundary",
        ));
    };
    if !matches!(tokens[invocation].kind(), TokenKind::Symbol("@")) {
        return Err(macro_head_error(source, tokens[invocation].span()));
    }
    let mut index = invocation + 1;
    let Some(first) = tokens.get(index).filter(|_| index < range.end) else {
        return Err(macro_head_error(source, tokens[invocation].span()));
    };
    if first.leading() != LeadingTrivia::Joint {
        return Err(macro_head_error(source, tokens[invocation].span()));
    }
    let TokenKind::Name(first) = first.kind() else {
        return Err(macro_head_error(source, tokens[invocation].span()));
    };
    let mut path = vec![(*first).to_owned()];
    index += 1;
    while index < range.end
        && matches!(tokens[index].kind(), TokenKind::Symbol("."))
        && tokens[index].leading() == LeadingTrivia::Joint
    {
        index += 1;
        let Some(component) = tokens.get(index).filter(|_| index < range.end) else {
            return Err(macro_head_error(source, tokens[invocation].span()));
        };
        if component.leading() != LeadingTrivia::Joint {
            return Err(macro_head_error(source, tokens[invocation].span()));
        }
        let TokenKind::Name(component) = component.kind() else {
            return Err(macro_head_error(source, tokens[invocation].span()));
        };
        path.push((*component).to_owned());
        index += 1;
    }
    let input_start = tokens[index - 1].span().end();
    let item = logical_item(source, declaration, invocation);
    let input_token_end = item.tokens.end();
    let input_end = if input_token_end < range.end {
        tokens[input_token_end].span().start()
    } else {
        declaration.span().end()
    };
    Ok(MacroInvocation {
        path,
        start,
        anchor_position: item.anchor_position,
        indentation: item.indentation,
        input: macro_input(source, index..input_token_end, input_start, input_end)?,
    })
}

fn logical_item(
    source: &LexedSource<'_>,
    declaration: &DeclarationSection,
    invocation: usize,
) -> MacroLogicalItem {
    let declaration_range = declaration.tokens();
    let mut member_start = declaration_range.start;
    let mut member_end = declaration_range.end;
    let mut inside_group = false;

    if let Some(group) = source
        .groups()
        .iter()
        .filter(|group| {
            group
                .close_token()
                .is_some_and(|close| group.open_token() < invocation && invocation < close)
        })
        .min_by_key(|group| {
            group
                .close_token()
                .expect("filtered groups are closed")
                .saturating_sub(group.open_token())
        })
    {
        inside_group = true;
        let close = group.close_token().expect("filtered groups are closed");
        member_start = group.open_token() + 1;
        member_end = close;
    }

    if hanging_layout_follows(source, member_start, invocation) {
        let view = TokenView::new(
            source,
            TokenRange::new(invocation, member_end).expect("hanging macro layout view is ordered"),
        )
        .expect("hanging macro layout remains within the source");
        let block = LayoutView::new(view).block();
        if let Some(first) = block.statements().first()
            && first.tokens().start() == invocation
        {
            return MacroLogicalItem {
                tokens: first.tokens(),
                anchor_position: true,
                indentation: block.anchor(),
            };
        }
    }

    let line_start = source.tokens()[member_start..invocation]
        .iter()
        .rposition(|token| matches!(token.kind(), TokenKind::LineStart { .. }))
        .map_or(member_start, |relative| member_start + relative + 1);
    let view = TokenView::new(
        source,
        TokenRange::new(line_start, member_end).expect("logical item view is ordered"),
    )
    .expect("logical item view remains within the source");
    let tokens = LayoutView::new(view)
        .block()
        .statements()
        .iter()
        .find(|statement| statement.tokens().contains(invocation))
        .map_or_else(
            || {
                TokenRange::new(member_start, member_end)
                    .expect("delimiter member range remains ordered")
            },
            |statement| statement.tokens(),
        );
    MacroLogicalItem {
        anchor_position: invocation == tokens.start() && !inside_group,
        indentation: TokenView::whole(source)
            .line_indentation_at(tokens.start())
            .unwrap_or(0),
        tokens,
    }
}

fn hanging_layout_follows(
    source: &LexedSource<'_>,
    member_start: usize,
    invocation: usize,
) -> bool {
    if source.tokens()[invocation].leading() == LeadingTrivia::Joint {
        return false;
    }
    source.tokens()[member_start..invocation]
        .iter()
        .rev()
        .find(|token| !matches!(token.kind(), TokenKind::LineStart { .. }))
        .is_some_and(
            |token| matches!(token.kind(), TokenKind::Name(name) if g0_layout_introducer(name)),
        )
}

fn generated_output(
    output: &[MacroOutput],
    line: usize,
    root_indentation: usize,
) -> Result<(String, Vec<Value>), Vec<Diagnostic>> {
    let mut generated = String::new();
    let mut embedded = Vec::new();
    let mut indentation = vec![root_indentation];
    let mut first_root_anchor = true;
    let mut resume_parent = false;
    for item in output {
        match item {
            MacroOutput::Text(text) => {
                resume_output_parent(&mut generated, &indentation, &mut resume_parent);
                generated.push_str(text);
            }
            MacroOutput::Data(value) => {
                resume_output_parent(&mut generated, &indentation, &mut resume_parent);
                generated.push(EMBEDDED_MARKER);
                embedded.push(value.as_core().clone());
            }
            MacroOutput::Separator => {
                resume_output_parent(&mut generated, &indentation, &mut resume_parent);
                generated.push(' ');
            }
            MacroOutput::LayoutStart => {
                indentation.push(indentation.last().copied().unwrap_or(0) + 2);
                resume_parent = false;
            }
            MacroOutput::LayoutEnd => {
                indentation.pop();
                resume_parent = true;
            }
            MacroOutput::Anchor => {
                resume_parent = false;
                if indentation.len() == 1 && first_root_anchor {
                    first_root_anchor = false;
                } else {
                    generated.push('\n');
                    generated.extend(std::iter::repeat_n(
                        ' ',
                        indentation.last().copied().unwrap_or(0),
                    ));
                }
            }
        }
    }
    GeneratedText::classify(generated.clone()).map_err(|mut diagnostics| {
        for diagnostic in &mut diagnostics {
            diagnostic.line += line - 1;
        }
        diagnostics
    })?;
    Ok((generated, embedded))
}

fn resume_output_parent(generated: &mut String, indentation: &[usize], resume_parent: &mut bool) {
    if !*resume_parent {
        return;
    }
    generated.push('\n');
    generated.extend(std::iter::repeat_n(
        ' ',
        indentation.last().copied().unwrap_or(0),
    ));
    *resume_parent = false;
}

fn marker_count(text: &str) -> usize {
    text.chars()
        .filter(|scalar| *scalar == EMBEDDED_MARKER)
        .count()
}

fn macro_input(
    source: &LexedSource<'_>,
    range: Range<usize>,
    start: usize,
    end: usize,
) -> Result<MacroInput, Diagnostic> {
    let mut elements = Vec::new();
    let mut element_tokens = Vec::new();
    let mut line_break = false;
    for (relative, token) in source.tokens()[range.clone()].iter().enumerate() {
        let token_index = range.start + relative;
        if matches!(token.kind(), TokenKind::LineStart { .. }) {
            line_break = true;
            continue;
        }
        let separated = line_break || token.leading() != LeadingTrivia::Joint;
        line_break = false;
        let kind = match token.kind() {
            TokenKind::Name(name) | TokenKind::InvalidNumber(name) | TokenKind::Symbol(name) => {
                MacroInputKind::Text {
                    text: Arc::from(*name),
                    delimiter: None,
                }
            }
            TokenKind::Number(id) => {
                MacroInputKind::Data(crate::api::Value::from_core(Value::Number(
                    source
                        .number(*id)
                        .expect("logical number token should reference its arena")
                        .clone(),
                )))
            }
            TokenKind::Text(id) => {
                MacroInputKind::Data(crate::api::Value::from_core(Value::binary_from_text(
                    source
                        .text(*id)
                        .expect("logical text token should reference its arena")
                        .value(),
                )))
            }
            TokenKind::Embedded(id) => MacroInputKind::Data(crate::api::Value::from_core(
                source
                    .embedded_value(*id)
                    .expect("embedded token should reference its arena")
                    .clone(),
            )),
            TokenKind::Open { delimiter, .. } => MacroInputKind::Text {
                text: Arc::from(delimiter_text(*delimiter, true)),
                delimiter: Some((macro_delimiter(*delimiter), true)),
            },
            TokenKind::Close { delimiter, .. } => MacroInputKind::Text {
                text: Arc::from(delimiter_text(*delimiter, false)),
                delimiter: Some((macro_delimiter(*delimiter), false)),
            },
            TokenKind::Unknown(scalar) => MacroInputKind::Text {
                text: Arc::from(scalar.to_string()),
                delimiter: None,
            },
            TokenKind::LineStart { .. } => unreachable!("line starts are skipped above"),
        };
        elements.push(MacroInputElement {
            kind,
            separated,
            start: token.span().start(),
            end: token.span().end(),
        });
        element_tokens.push(token_index);
    }
    let layouts = macro_input_layouts(source, range, &element_tokens);
    Ok(MacroInput::new(elements, start, end).with_layouts(layouts))
}

fn macro_input_layouts(
    source: &LexedSource<'_>,
    range: Range<usize>,
    element_tokens: &[usize],
) -> Vec<MacroInputLayout> {
    element_tokens
        .iter()
        .enumerate()
        .filter_map(|(start_element, token_index)| {
            if matches!(
                source.tokens()[*token_index].kind(),
                TokenKind::Close { .. }
            ) {
                return None;
            }
            let view = TokenView::new(
                source,
                TokenRange::new(*token_index, range.end)
                    .expect("macro layout candidate range remains ordered"),
            )?;
            let block = LayoutView::new(view).block();
            let items = block
                .statements()
                .iter()
                .map(|statement| {
                    element_boundary(element_tokens, statement.tokens().start())
                        ..element_boundary(element_tokens, statement.tokens().end())
                })
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            (!items.is_empty()).then(|| MacroInputLayout {
                start: start_element,
                end: element_boundary(element_tokens, block.end()),
                items: items.into(),
            })
        })
        .collect()
}

fn element_boundary(element_tokens: &[usize], token_boundary: usize) -> usize {
    element_tokens.partition_point(|token| *token < token_boundary)
}

fn macro_delimiter(delimiter: Delimiter) -> MacroDelimiter {
    match delimiter {
        Delimiter::Parenthesis => MacroDelimiter::Parenthesis,
        Delimiter::Bracket => MacroDelimiter::Bracket,
        Delimiter::Brace => MacroDelimiter::Brace,
    }
}

fn delimiter_text(delimiter: Delimiter, opening: bool) -> &'static str {
    match (delimiter, opening) {
        (Delimiter::Parenthesis, true) => "(",
        (Delimiter::Parenthesis, false) => ")",
        (Delimiter::Bracket, true) => "[",
        (Delimiter::Bracket, false) => "]",
        (Delimiter::Brace, true) => "{",
        (Delimiter::Brace, false) => "}",
    }
}

fn macro_head_error(source: &LexedSource<'_>, span: ByteSpan) -> Diagnostic {
    Diagnostic::error(
        source.line_at_byte(span.start()).unwrap_or(1),
        "macro invocation requires a joint static name path after `@`",
    )
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

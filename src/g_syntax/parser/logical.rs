//! Declaration-scoped macro input discovery and source replay.
//!
//! Macro expansion reads normalized elements from the authoritative lexer,
//! splices generated text and embedded values into one declaration, validates
//! generated text with that same lexer, then reparses the completed declaration.

use std::ops::Range;
use std::sync::Arc;

use crate::api::{Value as PublicValue, Values};
use crate::core::Value;

use super::super::Diagnostic;
use super::super::keywords::g0_layout_introducer;
use super::super::macro_expansion::{
    MacroDelimiter, MacroInput, MacroInputElement, MacroInputKind, MacroInputLayout, MacroOutput,
};
use super::input::{TokenRange, TokenView};
use super::layout::LayoutView;
use super::lexical::{
    ByteSpan, DeclarationSection, Delimiter, LeadingTrivia, LexedSource, TokenKind, lex_source,
};

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
    embedded_values: Vec<PublicValue>,
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
        values: &crate::core::CoreValueFactory,
        original: &OriginalMacroInvocation,
    ) -> Result<MacroInvocation, Vec<Diagnostic>> {
        let lexical = self.lexical(values)?;
        let Some(declaration) = lexical.declarations().first() else {
            return Err(vec![Diagnostic::error(
                original.line,
                format!(
                    "macro invocation {} disappeared before it could expand",
                    original.id
                ),
            )]);
        };
        let invocation = macro_invocation_at(&lexical, declaration, original.start, values)
            .map_err(|diagnostic| {
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

    pub(super) fn materialize(&self) -> (String, Vec<PublicValue>) {
        let mut source = "\n".repeat(self.line.saturating_sub(1));
        source.push_str(&self.text);
        (source, self.embedded_values.clone())
    }

    pub(super) fn position_at(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let prefix = &self.text[..offset];
        let relative_line = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
        (self.line + relative_line, offset - line_start + 1)
    }

    pub(super) fn normalized_excerpt(&self) -> String {
        const EXCERPT_SCALARS: usize = 160;

        let display = self.text.replace(EMBEDDED_MARKER, "<embedded-data>");
        let normalized = display.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut excerpt = normalized.chars().take(EXCERPT_SCALARS).collect::<String>();
        if normalized.chars().count() > EXCERPT_SCALARS {
            excerpt.push('…');
        }
        excerpt
    }

    fn lexical(
        &self,
        values: &crate::core::CoreValueFactory,
    ) -> Result<LexedSource<'_>, Vec<Diagnostic>> {
        let public_values = Values::from_core_factory(values.clone());
        let lexical = lex_source(&self.text)
            .replace_unknowns_with_embedded(
                EMBEDDED_MARKER,
                self.embedded_values
                    .iter()
                    .map(|value| {
                        public_values
                            .clone_core(value)
                            .expect("embedded macro data must belong to the parser runtime")
                    })
                    .collect(),
            )
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
    values: &crate::core::CoreValueFactory,
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
        input: macro_input(
            values,
            source,
            index..input_token_end,
            input_start,
            input_end,
        )?,
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
) -> Result<(String, Vec<PublicValue>), Vec<Diagnostic>> {
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
                embedded.push(value.clone());
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
    validate_generated_text(&generated).map_err(|mut diagnostics| {
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
    values: &crate::core::CoreValueFactory,
    source: &LexedSource<'_>,
    range: Range<usize>,
    start: usize,
    end: usize,
) -> Result<MacroInput, Diagnostic> {
    let public_values = Values::from_core_factory(values.clone());
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
            TokenKind::Number(id) => MacroInputKind::Data(
                public_values.wrap(Value::Number(
                    source
                        .number(*id)
                        .expect("logical number token should reference its arena")
                        .clone(),
                )),
            ),
            TokenKind::Text(id) => MacroInputKind::Data(
                public_values.wrap(Value::binary_from_text(
                    source
                        .text(*id)
                        .expect("logical text token should reference its arena")
                        .value(),
                )),
            ),
            TokenKind::Embedded(id) => MacroInputKind::Data(
                public_values.wrap(
                    source
                        .embedded_value(*id)
                        .expect("embedded token should reference its arena")
                        .clone(),
                ),
            ),
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
            let block_end = macro_layout_end(source, *token_index, block.end());
            let layout_end = element_boundary(element_tokens, block_end);
            let items = block
                .statements()
                .iter()
                .map(|statement| {
                    element_boundary(element_tokens, statement.tokens().start())
                        ..element_boundary(element_tokens, statement.tokens().end().min(block_end))
                })
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            (!items.is_empty()).then(|| MacroInputLayout {
                start: start_element,
                end: layout_end,
                items: items.into(),
            })
        })
        .collect()
}

fn macro_layout_end(source: &LexedSource<'_>, start: usize, mut end: usize) -> usize {
    while let Some(index) = end.checked_sub(1) {
        let TokenKind::Close { group, .. } = source.tokens()[index].kind() else {
            break;
        };
        let Some(parent) = source.group(*group) else {
            break;
        };
        if parent.open_token() >= start {
            break;
        }
        end = index;
    }
    end
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

fn validate_generated_text(text: &str) -> Result<(), Vec<Diagnostic>> {
    if text.contains(['@', '#']) {
        return Err(vec![Diagnostic::error(
            1,
            "generated macro text cannot contain `@` or `#`",
        )]);
    }

    let lexical = lex_source(text);
    if lexical.has_errors() {
        Err(lexical.diagnostics().to_vec())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EffectTokenDomain, Values};
    use crate::diagnostic::Severity;
    use std::sync::Weak;

    fn assert_rewrite_owner_inventory(
        invocation: &MacroInvocation,
        original: &OriginalMacroInvocation,
        work: &DeclarationMacroWork,
    ) {
        let MacroInvocation {
            path: _,
            start: _,
            anchor_position: _,
            indentation: _,
            input,
        } = invocation;
        let _: &MacroInput = input;
        let OriginalMacroInvocation {
            id: _,
            path: _,
            start: _,
            line: _,
        } = original;
        let DeclarationMacroWork {
            line: _,
            invocations: _,
            text: _,
            embedded_values,
        } = work;
        let _: &Vec<PublicValue> = embedded_values;
    }

    fn retained_value(domain: &EffectTokenDomain<Arc<()>>) -> (PublicValue, Weak<()>) {
        let payload = Arc::new(());
        let retained = Arc::downgrade(&payload);
        (domain.issue(payload), retained)
    }

    #[test]
    fn macro_rewrite_owner_inventory_is_compile_exhaustive() {
        let _: fn(&MacroInvocation, &OriginalMacroInvocation, &DeclarationMacroWork) =
            assert_rewrite_owner_inventory;
    }

    #[test]
    fn macro_input_and_rewrite_state_retain_embedded_data_until_retirement() {
        let values = Values::from_core_factory(crate::core::test_value_factory());
        let domain = EffectTokenDomain::new(&values);
        let (input_value, retained_input) = retained_value(&domain);
        let input = MacroInput::new(
            vec![MacroInputElement {
                kind: MacroInputKind::Data(input_value),
                separated: false,
                start: 0,
                end: 1,
            }],
            0,
            1,
        );
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_input.upgrade().is_some());
        drop(input);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_input.upgrade().is_none());

        let (embedded_value, retained_embedded) = retained_value(&domain);
        let work = DeclarationMacroWork {
            line: 1,
            invocations: Vec::new(),
            text: EMBEDDED_MARKER.to_string(),
            embedded_values: vec![embedded_value],
        };
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_embedded.upgrade().is_some());
        drop(work);
        domain.collect_and_drain_retired_external_owners_for_test();
        assert!(retained_embedded.upgrade().is_none());
    }

    fn original_macro_paths(source: &str) -> Result<Vec<Vec<String>>, Diagnostic> {
        let lexical = lex_source(source);
        assert!(
            !lexical.has_errors(),
            "macro-head fixture must be lexically valid: {:?}",
            lexical.diagnostics()
        );
        let declaration = lexical
            .declarations()
            .first()
            .expect("macro-head fixture must contain one declaration");
        Ok(DeclarationMacroWork::from_original(&lexical, declaration)?
            .map(|work| {
                work.invocations()
                    .iter()
                    .map(|invocation| invocation.path.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    #[test]
    fn production_macro_heads_are_static_joint_paths_in_expansion_order() {
        let cases: &[(&str, &[&[&str]])] = &[
            ("value = @name", &[&["name"]]),
            ("value = @name.child", &[&["name", "child"]]),
            ("value = @table.create input", &[&["table", "create"]]),
            ("value = @name .child", &[&["name"]]),
            ("value = (@outer @inner input)", &[&["inner"], &["outer"]]),
            ("value = @outer\n  @inner input", &[&["inner"], &["outer"]]),
            ("value = \"@text\" # @comment", &[]),
        ];

        for (source, expected) in cases {
            let actual = original_macro_paths(source)
                .unwrap_or_else(|diagnostic| panic!("{source:?} failed: {diagnostic:?}"));
            let expected = expected
                .iter()
                .map(|path| path.iter().map(|part| (*part).to_owned()).collect())
                .collect::<Vec<Vec<String>>>();
            assert_eq!(actual, expected, "{source:?}");
        }
    }

    #[test]
    fn production_macro_heads_reject_missing_dynamic_or_nonjoint_paths() {
        for source in [
            "@",
            "@ name",
            "@(name)",
            "@.name",
            "@name.[42]",
            "@name.",
            "@name. child",
            "@name..child",
        ] {
            let diagnostic = original_macro_paths(source)
                .expect_err("malformed macro head must fail in the production parser");
            assert_eq!(
                diagnostic.message, "macro invocation requires a joint static name path after `@`",
                "{source:?}"
            );
        }
    }

    #[test]
    fn generated_text_is_validated_locally_and_reserves_source_markers() {
        validate_generated_text("name (42) \"text\"").unwrap();

        for text in ["@next", "# comment", "left@right"] {
            let diagnostics = validate_generated_text(text).unwrap_err();
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.message.contains("cannot contain")
            }));
        }
    }

    #[test]
    fn invalid_numbers_fail_during_generated_text_classification() {
        let diagnostics = validate_generated_text("1e999999999999999999999999").unwrap_err();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("invalid number literal")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn generated_output_preserves_lexer_structure_diagnostics() {
        for (text, expected) in [
            ("(", "unclosed delimiter"),
            ("[)", "mismatched closing delimiter"),
            (")", "unmatched closing delimiter"),
        ] {
            let diagnostics =
                generated_output(&[MacroOutput::Text(text.to_owned())], 7, 0).unwrap_err();
            assert!(
                diagnostics.iter().any(|diagnostic| {
                    diagnostic.line == 7 && diagnostic.message.contains(expected)
                }),
                "{text:?}: {diagnostics:?}"
            );
        }
    }

    #[test]
    fn macro_layout_ranges_exclude_a_dedented_parent_closer() {
        let lexical = lex_source("(do\n  .r 1\n  .r (2)\n)\n");
        let start = lexical
            .tokens()
            .iter()
            .position(|token| matches!(token.kind(), TokenKind::Symbol(".")))
            .expect("the layout should contain a first return");
        let parent_close = lexical
            .tokens()
            .iter()
            .rposition(|token| matches!(token.kind(), TokenKind::Close { .. }))
            .expect("the parent group should be closed");

        assert_eq!(
            macro_layout_end(&lexical, start, parent_close + 1),
            parent_close,
        );
    }
}

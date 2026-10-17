//! Infix chains and their resolution, operator and path helpers, and the
//! entry from structural callers to the term parser.

use self::infix::resolve_infix_chain;
#[cfg(test)]
pub(in crate::g_syntax::parser) use self::infix::{operand_reads_ungrouped, operator_symbol};
use super::super::keywords::{g0_keyword, reserved_keyword_message};
use super::super::{
    Diagnostic, PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator, flatten_path_suffixes,
};
use super::expression_context::{ExpressionContext, ParsedExpression};
use super::input::{TokenRange, TokenView};
use super::layout::group_separator;
use super::lexical::{SpannedToken, TokenKind};

mod infix;

#[derive(Debug, PartialEq, Eq)]
pub(in crate::g_syntax::parser) struct InfixChain {
    first: SyntaxExpr,
    rest: Vec<(SyntaxOperator, SyntaxExpr)>,
    resumption_anchor: Option<usize>,
}

impl InfixChain {
    pub(in crate::g_syntax::parser) fn single(expression: SyntaxExpr) -> Self {
        Self {
            first: expression,
            rest: Vec::new(),
            resumption_anchor: None,
        }
    }

    /// Builds an unresolved chain from its operands, for the prefix-shared
    /// term parser.
    pub(in crate::g_syntax::parser) fn from_parts(
        first: SyntaxExpr,
        rest: Vec<(SyntaxOperator, SyntaxExpr)>,
    ) -> Self {
        Self {
            first,
            rest,
            resumption_anchor: None,
        }
    }

    pub(in crate::g_syntax::parser) fn with_resumption_anchor(
        mut self,
        indentation: usize,
        context: ExpressionContext,
    ) -> Result<Self, String> {
        self.accept_resumption_anchor(indentation, context)?;
        Ok(self)
    }

    fn accept_resumption_anchor(
        &mut self,
        indentation: usize,
        context: ExpressionContext,
    ) -> Result<(), String> {
        self.resumption_anchor = Some(Self::check_resumption_anchor(
            self.resumption_anchor,
            indentation,
            context,
        )?);
        Ok(())
    }

    /// Checks a line-led operator's indentation against the continuation
    /// floor and against `anchor`, the chain's earlier line-led operators,
    /// returning the chain's anchor.
    pub(in crate::g_syntax::parser) fn check_resumption_anchor(
        anchor: Option<usize>,
        indentation: usize,
        context: ExpressionContext,
    ) -> Result<usize, String> {
        if !context.accepts_layout_anchor(indentation) {
            return Err(format!(
                "leading infix operator is indented {indentation} spaces; expected more than continuation floor {}",
                context.continuation_floor()
            ));
        }
        match anchor {
            Some(anchor) if indentation != anchor => Err(format!(
                "leading infix operators must align at indentation {anchor}; found indentation {indentation}"
            )),
            _ => Ok(indentation),
        }
    }

    pub(in crate::g_syntax::parser) fn resolve(self) -> Result<SyntaxExpr, String> {
        resolve_infix_chain(self.first, self.rest)
    }
}

fn syntax_binary_expr(operator: SyntaxOperator, left: SyntaxExpr, right: SyntaxExpr) -> SyntaxExpr {
    match operator {
        SyntaxOperator::Builtin(builtin) => match builtin {
            crate::core::Builtin::Append => SyntaxExpr::Append(Box::new(left), Box::new(right)),
            crate::core::Builtin::Add => SyntaxExpr::Add(Box::new(left), Box::new(right)),
            crate::core::Builtin::Subtract => SyntaxExpr::Subtract(Box::new(left), Box::new(right)),
            crate::core::Builtin::Multiply => SyntaxExpr::Multiply(Box::new(left), Box::new(right)),
            crate::core::Builtin::Divide => SyntaxExpr::Divide(Box::new(left), Box::new(right)),
            _ => SyntaxExpr::OperatorApply {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            },
        },
        SyntaxOperator::BoolAnd
        | SyntaxOperator::BoolOr
        | SyntaxOperator::PipeForward
        | SyntaxOperator::PipeBackward
        | SyntaxOperator::ApplicativeForward
        | SyntaxOperator::ApplicativeBackward
        | SyntaxOperator::ComposeForward
        | SyntaxOperator::ComposeBackward
        | SyntaxOperator::EffectBind
        | SyntaxOperator::KleisliCompose
        | SyntaxOperator::EffectThen => SyntaxExpr::OperatorApply {
            operator,
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

pub(super) fn access_if_path(base: SyntaxExpr, suffixes: Vec<PathSuffix>) -> SyntaxExpr {
    match flatten_path_suffixes(suffixes) {
        parts if parts.is_empty() => base,
        parts => SyntaxExpr::Access(Box::new(base), parts),
    }
}

pub(super) fn quoted_path(suffixes: Vec<PathSuffix>) -> SyntaxExpr {
    let mut chunks = Vec::new();
    let mut literal = Vec::new();
    let flush_literal = |chunks: &mut Vec<SyntaxExpr>, literal: &mut Vec<SyntaxExpr>| {
        if !literal.is_empty() {
            chunks.push(SyntaxExpr::List(std::mem::take(literal)));
        }
    };

    for part in flatten_path_suffixes(suffixes) {
        match part {
            SyntaxKeyExpr::Atom(name) => literal.push(SyntaxExpr::Atom(name)),
            SyntaxKeyExpr::Index(expr) => literal.push(*expr),
            SyntaxKeyExpr::PathIndex(expr) => {
                flush_literal(&mut chunks, &mut literal);
                chunks.push(*expr);
            }
        }
    }
    flush_literal(&mut chunks, &mut literal);

    chunks
        .into_iter()
        .reduce(|left, right| SyntaxExpr::Append(Box::new(left), Box::new(right)))
        .unwrap_or_else(|| SyntaxExpr::List(Vec::new()))
}

/// The view a keyword form headed at `head_index` lies in: its group
/// member, never past `view`.
pub(in crate::g_syntax::parser) fn structural_view_at<'lex, 'source>(
    view: TokenView<'lex, 'source>,
    head_index: usize,
) -> TokenView<'lex, 'source> {
    let Some(group) = view
        .source()
        .enclosing_group(head_index)
        .map(|group| &view.source().groups()[group])
    else {
        return view;
    };
    let close = group.close_token().expect("filtered groups are closed");
    let contents = TokenView::new(
        view.source(),
        TokenRange::new(group.open_token() + 1, close)
            .expect("delimiter contents are an ordered range"),
    )
    .expect("delimiter contents remain within the source");
    let separator = group_separator(view.source(), group.delimiter(), group.open_token());
    // A form ends at its group member's end, and never past the view it is
    // parsed from, such as a conditional's branch.
    let end = contents
        .top_level()
        .find(|indexed| {
            indexed.index() > head_index
                && matches!(
                    indexed.token().kind(),
                    TokenKind::Symbol(symbol) if *symbol == separator
                )
        })
        .map_or(close, |indexed| indexed.index())
        .min(view.range().end());
    TokenView::new(
        view.source(),
        TokenRange::new(head_index, end).expect("structural group member is an ordered range"),
    )
    .expect("structural group member remains within the source")
}

pub(super) fn validate_expr_name(name: &str) -> Result<String, String> {
    if !name.starts_with(|character: char| character.is_ascii_alphabetic()) {
        return Err("expected name".to_owned());
    }
    match g0_keyword(name) {
        Some(keyword) if !matches!(keyword.spelling(), "module" | "module_origin" | "self") => {
            Err(reserved_keyword_message(keyword))
        }
        _ => Ok(name.to_owned()),
    }
}

pub(in crate::g_syntax::parser) fn syntax_operator(
    token: &SpannedToken<'_>,
) -> Option<SyntaxOperator> {
    use crate::core::Builtin::{
        Add, Append, Divide, Equal, Greater, GreaterEqual, Less, LessEqual, Multiply, NotEqual,
        Subtract,
    };

    Some(match token.kind() {
        TokenKind::Name("and") => SyntaxOperator::BoolAnd,
        TokenKind::Name("or") => SyntaxOperator::BoolOr,
        TokenKind::Symbol(">>=") => SyntaxOperator::EffectBind,
        TokenKind::Symbol(">=>") => SyntaxOperator::KleisliCompose,
        TokenKind::Symbol("=>>") => SyntaxOperator::EffectThen,
        TokenKind::Symbol("!>") => SyntaxOperator::ApplicativeForward,
        TokenKind::Symbol("<!") => SyntaxOperator::ApplicativeBackward,
        TokenKind::Symbol(">=") => SyntaxOperator::Builtin(GreaterEqual),
        TokenKind::Symbol("==") => SyntaxOperator::Builtin(Equal),
        TokenKind::Symbol("<>") => SyntaxOperator::Builtin(NotEqual),
        TokenKind::Symbol("=<") => SyntaxOperator::Builtin(LessEqual),
        TokenKind::Symbol(">>") => SyntaxOperator::ComposeForward,
        TokenKind::Symbol("<<") => SyntaxOperator::ComposeBackward,
        TokenKind::Symbol("|>") => SyntaxOperator::PipeForward,
        TokenKind::Symbol("<|") => SyntaxOperator::PipeBackward,
        TokenKind::Symbol(">") => SyntaxOperator::Builtin(Greater),
        TokenKind::Symbol("<") => SyntaxOperator::Builtin(Less),
        TokenKind::Symbol("++") => SyntaxOperator::Builtin(Append),
        TokenKind::Symbol("*") => SyntaxOperator::Builtin(Multiply),
        TokenKind::Symbol("/") => SyntaxOperator::Builtin(Divide),
        TokenKind::Symbol("+") => SyntaxOperator::Builtin(Add),
        TokenKind::Symbol("-") => SyntaxOperator::Builtin(Subtract),
        _ => return None,
    })
}

#[cfg(test)]
pub(super) fn parse_expression_fragment(source: &[u8]) -> Result<SyntaxExpr, Vec<Diagnostic>> {
    super::input::parse_expression_fragment(source, |view| {
        parse_expression_view(view, ExpressionContext::for_owner(view))
    })
}

#[cfg(test)]
pub(in crate::g_syntax::parser) fn parse_expression_view(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Result<SyntaxExpr, Vec<Diagnostic>> {
    parse_expression_chain_view(view, context).and_then(|parsed| {
        parsed.into_expression().map_err(|message| {
            let line = view
                .first_significant()
                .and_then(|(_, token)| view.line_at_span(token.span()))
                .unwrap_or(1);
            vec![Diagnostic::error(line, message)]
        })
    })
}

/// Parses `view` as one ordinary expression, returning its unresolved infix
/// chain and where it ends: at the view's end, or at a `where` or a `->`
/// that ends it (see `term::parse_term_chain`).
pub(in crate::g_syntax::parser) fn parse_expression_chain_view(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> Result<ParsedExpression, Vec<Diagnostic>> {
    match super::term::parse_term_chain(view, context) {
        Ok((chain, end)) => Ok(ParsedExpression::from_chain(chain, end)),
        Err(super::term::Fail::Error { at, message }) => {
            Err(vec![super::term::diagnostic(view, at, message)])
        }
        Err(super::term::Fail::Reported(diagnostics)) => Err(diagnostics),
    }
}

#[cfg(test)]
mod tests;

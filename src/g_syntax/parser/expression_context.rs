//! Floors and the arrow policy for one contextual expression parse.
//!
//! An owner establishes the exclusive physical floor for its continuations.
//! Nested owners validate only their newly stricter interval. Where a
//! pattern follows an operation after `->`, as in a binding or a view, an
//! open tail ends at a top-level `->` instead of running over it.

use super::super::Diagnostic;
use super::super::SyntaxExpr;
use super::expression::InfixChain;
use super::input::{TokenRange, TokenView};
use super::lexical::TokenKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpressionContext {
    continuation_floor: usize,
    validated_through: Option<usize>,
    floor_enforced: bool,
    /// Whether an open tail ends at a top-level `->`: an `else` branch, a
    /// `let` or `using` body, a postfix `if`'s fallback, or a lambda body.
    arrow_ends_tail: bool,
}

impl ExpressionContext {
    pub(super) fn for_owner(view: TokenView<'_, '_>) -> Self {
        Self {
            continuation_floor: owner_indentation(view).unwrap_or(0),
            validated_through: None,
            floor_enforced: true,
            arrow_ends_tail: false,
        }
    }

    /// `for_owner` for a view whose first significant token is `first`.
    pub(super) fn for_owner_at(view: TokenView<'_, '_>, first: usize) -> Self {
        Self {
            continuation_floor: view.line_indentation_at(first).unwrap_or(0),
            validated_through: None,
            floor_enforced: true,
            arrow_ends_tail: false,
        }
    }

    #[cfg(test)]
    pub(super) fn for_fragment(view: TokenView<'_, '_>) -> Self {
        // Isolated grammar fragments have no source owner. Their construct
        // layout remains checked, but there is no outer physical floor.
        Self {
            continuation_floor: owner_indentation(view).unwrap_or(0),
            validated_through: Some(usize::MAX),
            floor_enforced: false,
            arrow_ends_tail: false,
        }
    }

    pub(super) fn child_owner(self, view: TokenView<'_, '_>) -> Self {
        Self {
            continuation_floor: owner_indentation(view).unwrap_or(self.continuation_floor),
            validated_through: Some(
                self.validated_through
                    .map_or(self.continuation_floor, |validated| {
                        validated.max(self.continuation_floor)
                    }),
            ),
            floor_enforced: true,
            arrow_ends_tail: self.arrow_ends_tail,
        }
    }

    /// A complete hard range, such as a group's contents, which no `->`
    /// ends.
    /// `child_owner` for a view whose first significant token is `first`.
    pub(super) fn child_owner_at(self, view: TokenView<'_, '_>, first: usize) -> Self {
        Self {
            continuation_floor: view.line_indentation_at(first).unwrap_or(0),
            validated_through: Some(
                self.validated_through
                    .map_or(self.continuation_floor, |validated| {
                        validated.max(self.continuation_floor)
                    }),
            ),
            floor_enforced: true,
            arrow_ends_tail: self.arrow_ends_tail,
        }
    }

    /// A context in which no `->` ends an open tail.
    pub(super) fn complete(self) -> Self {
        Self {
            arrow_ends_tail: false,
            ..self
        }
    }

    /// Ends open tails at a top-level `->`, which a pattern follows.
    pub(super) fn ending_at_arrow(self) -> Self {
        Self {
            arrow_ends_tail: true,
            ..self
        }
    }

    pub(super) fn arrow_ends_tail(self) -> bool {
        self.arrow_ends_tail
    }

    pub(super) fn continuation_floor(self) -> usize {
        self.continuation_floor
    }

    pub(super) fn accepts_layout_anchor(self, anchor: usize) -> bool {
        !self.floor_enforced || anchor > self.continuation_floor
    }

    pub(super) fn with_continuation_floor(self, continuation_floor: usize) -> Self {
        Self {
            continuation_floor: self.continuation_floor.max(continuation_floor),
            floor_enforced: true,
            ..self
        }
    }

    pub(super) fn with_physical_line_floor(self, continuation_floor: usize) -> Self {
        if self.floor_enforced {
            self.with_continuation_floor(continuation_floor)
        } else {
            self
        }
    }

    fn needs_validation(self, indentation: usize) -> bool {
        indentation <= self.continuation_floor
            && self
                .validated_through
                .is_none_or(|validated| indentation > validated)
    }

    /// The context once its floors are checked, so that nested owners check
    /// only the lines its floor did not cover.
    pub(super) fn validated(self) -> Self {
        Self {
            validated_through: Some(
                self.validated_through
                    .map_or(self.continuation_floor, |validated| {
                        validated.max(self.continuation_floor)
                    }),
            ),
            ..self
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ParsedExpression {
    expression: InfixChain,
    end: usize,
}

impl ParsedExpression {
    pub(super) fn new(expression: SyntaxExpr, end: usize) -> Self {
        Self {
            expression: InfixChain::single(expression),
            end,
        }
    }

    pub(super) fn from_chain(expression: InfixChain, end: usize) -> Self {
        Self { expression, end }
    }

    pub(super) fn end(&self) -> usize {
        self.end
    }

    pub(super) fn into_expression(self) -> Result<SyntaxExpr, String> {
        self.expression.resolve()
    }
}

fn owner_indentation(view: TokenView<'_, '_>) -> Option<usize> {
    let (index, _) = view.first_significant()?;
    Some(view.line_indentation_at(index).unwrap_or(0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FloorViolationKind {
    UnderIndented {
        actual: usize,
        expected_at_least: usize,
    },
    CloserContinues,
    NonterminalAlignedCloser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FloorViolation {
    line: usize,
    kind: FloorViolationKind,
}

impl FloorViolation {
    fn into_diagnostic(self) -> Diagnostic {
        let message = match self.kind {
            FloorViolationKind::UnderIndented {
                actual,
                expected_at_least,
            } => format!(
                "expression continuation is indented {actual} spaces; expected at least {expected_at_least}"
            ),
            FloorViolationKind::CloserContinues => {
                "expression continues after a boundary-aligned closing delimiter; indent this line or end the enclosing expression after the delimiter".to_owned()
            }
            FloorViolationKind::NonterminalAlignedCloser => {
                "expression continues after a boundary-aligned closing delimiter; indent the closing delimiter to continue the expression".to_owned()
            }
        };
        Diagnostic::error(self.line, message)
    }
}

/// Validates the part of `view` not already covered by an enclosing floor.
///
/// Returning a marked context lets nested owners validate only the stricter
/// interval between the enclosing floor and their own. That makes overlapping
/// token ranges intentional and avoids deduplicating rendered diagnostics.
pub(super) fn validate_expression_floor(
    view: TokenView<'_, '_>,
    context: ExpressionContext,
) -> (ExpressionContext, Vec<Diagnostic>) {
    let validated_context = context.validated();
    let Some((_, first_token)) = view.first_significant() else {
        return (validated_context, Vec::new());
    };
    let first_line = view.line_at_span(first_token.span()).unwrap_or(1);
    let line_starts = view
        .tokens()
        .iter()
        .enumerate()
        .filter_map(|(relative, token)| match token.kind() {
            TokenKind::LineStart { indentation } => Some((
                view.range().start() + relative,
                *indentation,
                view.line_at_span(token.span()).unwrap_or(first_line),
            )),
            _ => None,
        })
        .filter(|(_, _, line)| *line > first_line)
        .collect::<Vec<_>>();
    let mut violations = Vec::new();

    for (position, (line_start, indentation, line)) in line_starts.iter().copied().enumerate() {
        if !context.needs_validation(indentation) {
            continue;
        }
        let line_end = line_starts
            .get(position + 1)
            .map_or(view.range().end(), |(next, _, _)| *next);
        let line_view = view
            .subview(
                TokenRange::new(line_start + 1, line_end)
                    .expect("a physical source line has ordered token boundaries"),
            )
            .expect("an expression line remains within its hard range");
        let mut line_tokens = line_view
            .tokens()
            .iter()
            .filter(|token| !matches!(token.kind(), TokenKind::LineStart { .. }));
        let Some(first) = line_tokens.next() else {
            continue;
        };
        let starts_with_closer = matches!(first.kind(), TokenKind::Close { .. });
        let closer_only = starts_with_closer
            && line_tokens.all(|token| matches!(token.kind(), TokenKind::Close { .. }));

        let kind = if indentation == context.continuation_floor && closer_only {
            (position + 1 < line_starts.len())
                .then_some(FloorViolationKind::NonterminalAlignedCloser)
        } else if starts_with_closer {
            Some(FloorViolationKind::CloserContinues)
        } else {
            Some(FloorViolationKind::UnderIndented {
                actual: indentation,
                expected_at_least: context.continuation_floor + 1,
            })
        };
        if let Some(kind) = kind {
            violations.push(FloorViolation { line, kind });
        }
    }

    (
        validated_context,
        violations
            .into_iter()
            .map(FloorViolation::into_diagnostic)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::g_syntax::parser::input::parse_expression_fragment;

    #[test]
    fn owner_context_uses_physical_indentation() {
        parse_expression_fragment(b"  value", |view| {
            let owner = ExpressionContext::for_owner(view);
            assert_eq!(owner.continuation_floor(), 2);
            assert_eq!(owner.ending_at_arrow().complete(), owner);
            Ok(())
        })
        .unwrap();
    }
}

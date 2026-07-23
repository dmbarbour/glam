//! Context carried while recovering one expression's token extent.
//!
//! Phase 1 records ownership without enforcing new layout. Floors become
//! authoritative in the later validation phase, and `MayYield` becomes
//! observable when structural expressions can return at dedent boundaries.

use super::super::SyntaxExpr;
use super::input::TokenView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpressionExtent {
    CompleteHardRange,
    MayYield,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpressionContext {
    continuation_floor: usize,
    extent: ExpressionExtent,
}

impl ExpressionContext {
    pub(super) fn for_owner(view: TokenView<'_, '_>) -> Self {
        Self {
            continuation_floor: owner_indentation(view).unwrap_or(0),
            extent: ExpressionExtent::CompleteHardRange,
        }
    }

    pub(super) fn child_owner(self, view: TokenView<'_, '_>) -> Self {
        Self {
            continuation_floor: owner_indentation(view).unwrap_or(self.continuation_floor),
            extent: ExpressionExtent::CompleteHardRange,
        }
    }

    pub(super) fn complete(self) -> Self {
        Self {
            extent: ExpressionExtent::CompleteHardRange,
            ..self
        }
    }

    pub(super) fn may_yield(self) -> Self {
        Self {
            extent: ExpressionExtent::MayYield,
            ..self
        }
    }

    #[cfg(test)]
    pub(super) fn continuation_floor(self) -> usize {
        self.continuation_floor
    }

    pub(super) fn permits_yield(self) -> bool {
        self.extent == ExpressionExtent::MayYield
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ParsedExpression {
    expression: SyntaxExpr,
    end: usize,
}

impl ParsedExpression {
    pub(super) fn new(expression: SyntaxExpr, end: usize) -> Self {
        Self { expression, end }
    }

    pub(super) fn end(&self) -> usize {
        self.end
    }

    pub(super) fn into_expression(self) -> SyntaxExpr {
        self.expression
    }
}

fn owner_indentation(view: TokenView<'_, '_>) -> Option<usize> {
    let (index, _) = view.first_significant()?;
    Some(view.line_indentation_at(index).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::g_syntax::parser::input::parse_expression_fragment;

    #[test]
    fn owner_context_uses_physical_indentation_and_preserves_it_for_hard_ranges() {
        parse_expression_fragment(b"  value", |view| {
            let owner = ExpressionContext::for_owner(view);
            assert_eq!(owner.continuation_floor(), 2);
            assert!(!owner.permits_yield());
            assert_eq!(owner.may_yield().complete(), owner);
            Ok(())
        })
        .unwrap();
    }
}

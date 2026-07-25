//! Name resolution and syntax-to-semantic lowering.

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Phase 1 builds the internal choice lowerer before Phase 2 activates syntax"
    )
)]
mod conditional;
mod do_expr;
mod effect_steps;
mod expression;
mod pattern;
mod scope;

pub(in crate::g_syntax) use do_expr::*;
pub(in crate::g_syntax) use expression::*;
pub(in crate::g_syntax) use scope::*;

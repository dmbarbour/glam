//! Name resolution and syntax-to-semantic lowering.

mod conditional;
mod do_expr;
mod effect_steps;
mod expression;
mod pattern;
mod scope;

pub(in crate::g_syntax) use do_expr::*;
pub(in crate::g_syntax) use effect_steps::for_each_effect_result_resolved;
pub(in crate::g_syntax) use expression::*;
pub(in crate::g_syntax) use scope::*;

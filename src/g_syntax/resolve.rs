//! Name resolution and syntax-to-semantic lowering.

mod do_expr;
mod expression;
mod scope;

pub(in crate::g_syntax) use do_expr::*;
pub(in crate::g_syntax) use expression::*;
pub(in crate::g_syntax) use scope::*;

//! Name resolution and syntax-to-semantic lowering.

mod expression;
mod scope;

pub(in crate::g_syntax) use expression::*;
pub(in crate::g_syntax) use scope::*;

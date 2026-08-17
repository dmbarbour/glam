//! Parsing façade for G source files and expressions.

mod conditional;
mod declaration;
mod do_expr;
mod expression;
mod expression_context;
#[cfg(test)]
mod floor_tests;
mod input;
mod layout;
mod lexical;
mod logical;
mod pattern;
mod source;
mod structural;

pub(in crate::g_syntax) use source::StagedSourceParser;
pub(crate) use source::inspect_source;
#[cfg(test)]
pub use source::parse_source;

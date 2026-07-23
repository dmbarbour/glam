//! Parsing façade for G source files and expressions.

mod compound;
mod declaration;
mod do_expr;
mod expression;
mod input;
mod layout;
mod lexical;
mod source;

pub(super) use compound::parse_expr_result_with_diagnostics;
#[cfg(test)]
pub(super) use declaration::definition_decl;
pub(super) use declaration::definition_target_parts;
#[cfg(test)]
pub(super) use expression::parse_expr;
pub(super) use expression::syntax_expr_parser;
pub use source::parse_source;

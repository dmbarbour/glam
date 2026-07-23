//! Parsing façade for G source files and expressions.

mod declaration;
mod do_expr;
mod expression;
mod expression_context;
#[cfg(test)]
mod floor_tests;
mod input;
mod layout;
mod lexical;
mod source;
mod structural;

pub use source::parse_source;

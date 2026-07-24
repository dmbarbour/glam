//! Pattern-to-primitive-do lowering.
//!
//! Surface patterns stop at this boundary. Irrefutable captures are reported
//! in semantic order so do lowering can either reuse the operation's primitive
//! step or append aliases of one internal result binding.

use super::super::SyntaxPattern;

pub(in crate::g_syntax) fn irrefutable_captures(pattern: &SyntaxPattern) -> Vec<&str> {
    let mut captures = Vec::new();
    pattern.visit_captures(&mut |name| captures.push(name));
    captures
}

//! Pattern-to-primitive-do lowering.
//!
//! Surface patterns stop at this boundary. A direct binding can reuse the
//! operation's primitive step; structural patterns will instead append
//! decomposition and source-capture steps through the primitive-do builder.

use super::super::{SyntaxPattern, SyntaxPatternKind};

pub(in crate::g_syntax) enum DirectPatternBinding<'a> {
    Capture(&'a str),
    Wildcard,
}

pub(in crate::g_syntax) fn direct_pattern_binding(
    pattern: &SyntaxPattern,
) -> DirectPatternBinding<'_> {
    match &pattern.kind {
        SyntaxPatternKind::Capture(name) => DirectPatternBinding::Capture(name),
        SyntaxPatternKind::Wildcard => DirectPatternBinding::Wildcard,
        SyntaxPatternKind::Group(pattern) => direct_pattern_binding(pattern),
    }
}

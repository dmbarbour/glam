//! Narrow inspection facade for the built-in `.g` front end.
//!
//! This deliberately reports parser observations rather than exposing the
//! bootstrap compiler's syntax tree or lowering context as a Rust API.

use std::sync::Arc;

use crate::diagnostic::Severity;
use crate::g_syntax::DeclarationKind;

/// A read-only summary produced by the built-in `.g` parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GSourceInspection {
    diagnostics: Arc<[GSourceDiagnostic]>,
    declarations: Arc<[GDeclarationSummary]>,
}

impl GSourceInspection {
    pub fn diagnostics(&self) -> &[GSourceDiagnostic] {
        &self.diagnostics
    }

    pub fn declarations(&self) -> &[GDeclarationSummary] {
        &self.declarations
    }

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

/// One source-local diagnostic from `.g` parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GSourceDiagnostic {
    severity: Severity,
    line: usize,
    message: Arc<str>,
}

impl GSourceDiagnostic {
    pub fn severity(&self) -> Severity {
        self.severity
    }

    pub fn line(&self) -> usize {
        self.line
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// One top-level declaration recognized by the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GDeclarationSummary {
    line: usize,
    kind: GDeclarationKind,
    preview: Arc<str>,
}

impl GDeclarationSummary {
    pub fn line(&self) -> usize {
        self.line
    }

    pub fn kind(&self) -> GDeclarationKind {
        self.kind
    }

    pub fn preview(&self) -> &str {
        &self.preview
    }
}

/// Stable declaration categories exposed by source inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GDeclarationKind {
    Language,
    Import,
    Abstract,
    Unique,
    Object,
    Extend,
    Definition,
    Unknown,
}

impl GDeclarationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::Import => "import",
            Self::Abstract => "abstract",
            Self::Unique => "unique",
            Self::Object => "object",
            Self::Extend => "extend",
            Self::Definition => "definition",
            Self::Unknown => "unknown",
        }
    }
}

/// Inspects bytes using the built-in `.g` parser without compiling, loading
/// imports, creating an assembler, or exposing the parser's syntax tree.
pub fn inspect_g_source(source: &[u8]) -> GSourceInspection {
    let parsed = crate::g_syntax::parse_source(source);
    let diagnostics = parsed
        .diagnostics
        .into_iter()
        .map(|diagnostic| GSourceDiagnostic {
            severity: diagnostic.severity,
            line: diagnostic.line,
            message: diagnostic.message.into(),
        })
        .collect::<Vec<_>>();
    let declarations = parsed
        .declarations
        .into_iter()
        .map(|declaration| GDeclarationSummary {
            line: declaration.line,
            kind: declaration_kind(&declaration.kind),
            preview: declaration.preview.into(),
        })
        .collect::<Vec<_>>();
    GSourceInspection {
        diagnostics: diagnostics.into(),
        declarations: declarations.into(),
    }
}

fn declaration_kind(kind: &DeclarationKind) -> GDeclarationKind {
    match kind {
        DeclarationKind::Language(_) => GDeclarationKind::Language,
        DeclarationKind::Import(_) => GDeclarationKind::Import,
        DeclarationKind::Abstract(_) => GDeclarationKind::Abstract,
        DeclarationKind::Unique(_) => GDeclarationKind::Unique,
        DeclarationKind::Object(_) => GDeclarationKind::Object,
        DeclarationKind::Extend(_) => GDeclarationKind::Extend,
        DeclarationKind::Definition(_) => GDeclarationKind::Definition,
        DeclarationKind::Unknown => GDeclarationKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_exposes_summaries_without_the_syntax_tree() {
        let report = inspect_g_source(b"language g0\nasm.result = \"hello\"\n");

        assert!(!report.has_errors());
        assert_eq!(report.declarations().len(), 2);
        assert_eq!(report.declarations()[0].kind(), GDeclarationKind::Language);
        assert_eq!(report.declarations()[1].preview(), "asm.result = \"hello\"");
    }
}

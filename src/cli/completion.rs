use std::ffi::{OsStr, OsString};
use std::sync::Arc;

use crate::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    arguments_before: Arc<[OsString]>,
    active_prefix: OsString,
    active_suffix: OsString,
    arguments_after: Arc<[OsString]>,
}

impl CompletionRequest {
    pub fn new<B, A>(
        arguments_before: B,
        active_prefix: impl Into<OsString>,
        active_suffix: impl Into<OsString>,
        arguments_after: A,
    ) -> Self
    where
        B: IntoIterator<Item = OsString>,
        A: IntoIterator<Item = OsString>,
    {
        Self {
            arguments_before: arguments_before.into_iter().collect(),
            active_prefix: active_prefix.into(),
            active_suffix: active_suffix.into(),
            arguments_after: arguments_after.into_iter().collect(),
        }
    }

    pub fn arguments_before(&self) -> &[OsString] {
        &self.arguments_before
    }

    pub fn active_prefix(&self) -> &OsStr {
        &self.active_prefix
    }

    pub fn active_suffix(&self) -> &OsStr {
        &self.active_suffix
    }

    pub fn arguments_after(&self) -> &[OsString] {
        &self.arguments_after
    }

    pub(super) fn active_argument(&self) -> OsString {
        let mut active = self.active_prefix.clone();
        active.push(&self.active_suffix);
        active
    }

    pub(super) fn flattened(&self) -> (Arc<[OsString]>, usize) {
        let active = self.arguments_before.len();
        let mut arguments =
            Vec::with_capacity(self.arguments_before.len() + 1 + self.arguments_after.len());
        arguments.extend(self.arguments_before.iter().cloned());
        arguments.push(self.active_argument());
        arguments.extend(self.arguments_after.iter().cloned());
        (arguments.into(), active)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Keyword,
    Value,
    File,
    Folder,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionCandidate {
    replacement: OsString,
    display: String,
    kind: CompletionKind,
}

impl CompletionCandidate {
    pub fn replacement(&self) -> &OsStr {
        &self.replacement
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn kind(&self) -> CompletionKind {
        self.kind
    }
}

#[derive(Debug, Clone)]
pub struct CliCompletion {
    candidates: Vec<CompletionCandidate>,
    expectations: Vec<CompletionExpectation>,
    diagnostics: Vec<Diagnostic>,
}

impl CliCompletion {
    pub(super) fn new(
        candidates: Vec<CompletionCandidate>,
        expectations: Vec<CompletionExpectation>,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            candidates,
            expectations,
            diagnostics,
        }
    }

    pub fn candidates(&self) -> &[CompletionCandidate] {
        &self.candidates
    }

    pub fn expectations(&self) -> &[CompletionExpectation] {
        &self.expectations
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionExpectation {
    argument: usize,
    token_offset: usize,
    label: String,
}

impl CompletionExpectation {
    pub fn argument(&self) -> usize {
        self.argument
    }

    pub fn token_offset(&self) -> usize {
        self.token_offset
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Frontier {
    pub(super) argument: usize,
    pub(super) token_offset: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ExpectationEvidence {
    pub(super) frontier: Frontier,
    pub(super) label: String,
}

impl ExpectationEvidence {
    pub(super) fn public(&self) -> CompletionExpectation {
        CompletionExpectation {
            argument: self.frontier.argument,
            token_offset: self.frontier.token_offset,
            label: self.label.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct CompletionEvidence {
    pub(super) frontier: Frontier,
    pub(super) candidate: CompletionCandidate,
    pub(super) complete_reader: bool,
}

impl CompletionEvidence {
    pub(super) fn new(
        frontier: Frontier,
        replacement: OsString,
        kind: CompletionKind,
        complete_reader: bool,
    ) -> Self {
        let display = replacement.to_string_lossy().into_owned();
        Self {
            frontier,
            candidate: CompletionCandidate {
                replacement,
                display,
                kind,
            },
            complete_reader,
        }
    }
}

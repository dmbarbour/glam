use std::ffi::{OsStr, OsString};
use std::sync::Arc;

use glam::{Diagnostic, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletionRequest {
    arguments_before: Arc<[OsString]>,
    active_argument: Option<ActiveArgument>,
    arguments_after: Arc<[OsString]>,
}

impl CompletionRequest {
    pub(crate) fn with_active<B, A>(
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
            active_argument: Some(ActiveArgument {
                prefix: active_prefix.into(),
                suffix: active_suffix.into(),
            }),
            arguments_after: arguments_after.into_iter().collect(),
        }
    }

    pub(crate) fn without_active<B, A>(arguments_before: B, arguments_after: A) -> Self
    where
        B: IntoIterator<Item = OsString>,
        A: IntoIterator<Item = OsString>,
    {
        Self {
            arguments_before: arguments_before.into_iter().collect(),
            active_argument: None,
            arguments_after: arguments_after.into_iter().collect(),
        }
    }

    pub(crate) fn arguments_before(&self) -> &[OsString] {
        &self.arguments_before
    }

    pub(crate) fn active_argument(&self) -> Option<&ActiveArgument> {
        self.active_argument.as_ref()
    }

    pub(crate) fn arguments_after(&self) -> &[OsString] {
        &self.arguments_after
    }

    pub(crate) fn arguments(&self) -> Arc<[OsString]> {
        let mut arguments = Vec::with_capacity(
            self.arguments_before.len()
                + usize::from(self.active_argument.is_some())
                + self.arguments_after.len(),
        );
        arguments.extend(self.arguments_before.iter().cloned());
        if let Some(active) = &self.active_argument {
            arguments.push(active.value());
        }
        arguments.extend(self.arguments_after.iter().cloned());
        arguments.into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveArgument {
    prefix: OsString,
    suffix: OsString,
}

impl ActiveArgument {
    pub(crate) fn prefix(&self) -> &OsStr {
        &self.prefix
    }

    pub(crate) fn suffix(&self) -> &OsStr {
        &self.suffix
    }

    pub(crate) fn value(&self) -> OsString {
        let mut value = self.prefix.clone();
        value.push(&self.suffix);
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CompletionKind {
    Keyword,
    Value,
    File,
    Folder,
    Path,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletionCandidate {
    replacement: OsString,
    kind: CompletionKind,
    explanations: Vec<CliCaseExplanation>,
}

impl CompletionCandidate {
    pub(super) fn new(replacement: impl Into<OsString>, kind: CompletionKind) -> Self {
        Self::with_explanations(replacement, kind, std::iter::empty())
    }

    pub(super) fn with_explanations(
        replacement: impl Into<OsString>,
        kind: CompletionKind,
        explanations: impl IntoIterator<Item = Value>,
    ) -> Self {
        let replacement = replacement.into();
        Self {
            replacement,
            kind,
            explanations: explanations
                .into_iter()
                .map(CliCaseExplanation::new)
                .collect(),
        }
    }

    pub(crate) fn replacement(&self) -> &OsStr {
        &self.replacement
    }

    pub(crate) fn kind(&self) -> CompletionKind {
        self.kind
    }

    /// Explained parser cases that remain viable for this replacement.
    #[cfg(test)]
    pub(crate) fn explanations(&self) -> &[CliCaseExplanation] {
        &self.explanations
    }

    pub(super) fn merge_explanations(
        &mut self,
        other: &Self,
        mut same: impl FnMut(&Value, &Value) -> bool,
    ) {
        for explanation in &other.explanations {
            if !self
                .explanations
                .iter()
                .any(|prior| same(prior.value(), explanation.value()))
            {
                self.explanations.push(explanation.clone());
            }
        }
    }
}

/// One lazy, structured explanation supplied to `.case` by configuration.
#[derive(Debug, Clone)]
pub(crate) struct CliCaseExplanation {
    value: Value,
}

impl CliCaseExplanation {
    pub(super) fn new(value: Value) -> Self {
        Self { value }
    }

    pub(crate) fn value(&self) -> &Value {
        &self.value
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CliCompletion {
    candidates: Vec<CompletionCandidate>,
    #[cfg_attr(not(test), allow(dead_code))]
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

    pub(crate) fn candidates(&self) -> &[CompletionCandidate] {
        &self.candidates
    }

    #[cfg(test)]
    pub(crate) fn expectations(&self) -> &[CompletionExpectation] {
        &self.expectations
    }

    pub(crate) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CompletionExpectation {
    argument: usize,
    token_offset: usize,
    label: String,
    explanations: Vec<CliCaseExplanation>,
}

impl CompletionExpectation {
    pub(crate) fn argument(&self) -> usize {
        self.argument
    }

    pub(crate) fn token_offset(&self) -> usize {
        self.token_offset
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    #[cfg(test)]
    pub(crate) fn explanations(&self) -> &[CliCaseExplanation] {
        &self.explanations
    }

    pub(super) fn merge_explanations(
        &mut self,
        other: &Self,
        mut same: impl FnMut(&Value, &Value) -> bool,
    ) {
        for explanation in &other.explanations {
            if !self
                .explanations
                .iter()
                .any(|prior| same(prior.value(), explanation.value()))
            {
                self.explanations.push(explanation.clone());
            }
        }
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
    pub(super) explanations: Vec<Value>,
}

impl ExpectationEvidence {
    pub(super) fn public(&self) -> CompletionExpectation {
        CompletionExpectation {
            argument: self.frontier.argument,
            token_offset: self.frontier.token_offset,
            label: self.label.clone(),
            explanations: self
                .explanations
                .iter()
                .cloned()
                .map(CliCaseExplanation::new)
                .collect(),
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
        explanations: Vec<Value>,
    ) -> Self {
        Self {
            frontier,
            candidate: CompletionCandidate::with_explanations(replacement, kind, explanations),
            complete_reader,
        }
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use glam::{EffectTokenDomain, EvaluationRuntime};

    fn assert_completion_owner_inventory(
        candidate: &CompletionCandidate,
        explanation: &CliCaseExplanation,
        completion: &CliCompletion,
        expectation: &CompletionExpectation,
        evidence: &ExpectationEvidence,
    ) {
        let CompletionCandidate {
            replacement: _,
            kind: _,
            explanations: _,
        } = candidate;
        let CliCaseExplanation { value } = explanation;
        let _: &Value = value;
        let CliCompletion {
            candidates: _,
            expectations: _,
            diagnostics: _,
        } = completion;
        let CompletionExpectation {
            argument: _,
            token_offset: _,
            label: _,
            explanations: _,
        } = expectation;
        let ExpectationEvidence {
            frontier: _,
            label: _,
            explanations,
        } = evidence;
        let _: &Vec<Value> = explanations;
    }

    #[test]
    fn completion_owner_inventory_is_compile_exhaustive() {
        let _: fn(
            &CompletionCandidate,
            &CliCaseExplanation,
            &CliCompletion,
            &CompletionExpectation,
            &ExpectationEvidence,
        ) = assert_completion_owner_inventory;
    }

    #[test]
    fn case_explanation_retires_its_value_root_exactly() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let domain = EffectTokenDomain::new(&runtime.values());
        let payload = Arc::new(());
        let retained = Arc::downgrade(&payload);
        let explanation = CliCaseExplanation::new(domain.issue(payload));

        assert!(retained.upgrade().is_some());
        drop(explanation);
        assert!(retained.upgrade().is_none());
    }
}

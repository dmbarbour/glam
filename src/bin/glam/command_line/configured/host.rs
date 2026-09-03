use std::ffi::OsString;
use std::sync::Arc;

use glam::reflection::{IsolatedTaskHost, ReflectionJournal, ReflectionTransaction};
use glam::{EffectTokenDomain, Value, Values};

use super::super::completion::{CompletionEvidence, ExpectationEvidence};
use super::super::model::CommandEdit;
use super::effects::PathHandle;

#[derive(Clone)]
pub(super) struct CliInvocation {
    pub(super) args: Arc<[OsString]>,
    pub(super) completion: Option<CompletionPoint>,
}

impl CliInvocation {
    pub(super) fn new(args: Arc<[OsString]>) -> Self {
        Self::from_parts(args, None)
    }

    pub(super) fn for_completion(
        args: Arc<[OsString]>,
        argument: usize,
        prefix: OsString,
        suffix: OsString,
    ) -> Self {
        Self::from_parts(
            args,
            Some(CompletionPoint {
                argument,
                prefix,
                suffix,
            }),
        )
    }

    fn from_parts(args: Arc<[OsString]>, completion: Option<CompletionPoint>) -> Self {
        Self { args, completion }
    }
}

#[derive(Clone)]
pub(super) struct CompletionPoint {
    pub(super) argument: usize,
    pub(super) prefix: OsString,
    pub(super) suffix: OsString,
}

#[derive(Clone)]
pub(super) struct CliSnapshot {
    pub(super) invocation: CliInvocation,
    pub(super) path_tokens: EffectTokenDomain<PathHandle>,
}

impl CliSnapshot {
    pub(super) fn new(values: &Values, invocation: CliInvocation) -> Self {
        Self {
            invocation,
            path_tokens: EffectTokenDomain::new(values),
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct CliJournal {
    pub(super) reflection: ReflectionJournal,
    pub(super) cursor: usize,
    pub(super) edits: Vec<CommandEdit>,
    pub(super) expectations: Vec<ExpectationEvidence>,
    pub(super) candidates: Vec<CompletionEvidence>,
    /// Cases currently enclosing the effect being interpreted. Failed reader
    /// evidence captures this stack before the branch terminates.
    pub(super) active_cases: Vec<Value>,
    /// Cases entered by this branch, retained for ambiguity explanations after
    /// successful scopes have closed.
    pub(super) visited_cases: Vec<Value>,
}

impl ReflectionTransaction for CliJournal {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal {
        &mut self.reflection
    }
}

pub(super) type CliHost = IsolatedTaskHost<CliSnapshot>;

#[cfg(test)]
mod owner_tests {
    use super::*;

    fn assert_cli_owner_inventory(snapshot: &CliSnapshot, journal: &CliJournal) {
        let CliSnapshot {
            invocation: _,
            path_tokens: _,
        } = snapshot;
        let CliJournal {
            reflection: _,
            cursor: _,
            edits: _,
            expectations: _,
            candidates: _,
            active_cases,
            visited_cases,
        } = journal;
        let _: &Vec<Value> = active_cases;
        let _: &Vec<Value> = visited_cases;
    }

    #[test]
    fn cli_journal_owner_inventory_is_compile_exhaustive() {
        let _: fn(&CliSnapshot, &CliJournal) = assert_cli_owner_inventory;
    }
}

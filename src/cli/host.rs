use std::ffi::OsString;
use std::sync::Arc;

use crate::api::Value;
use crate::reflection::{IsolatedTaskHost, ReflectionJournal, ReflectionTransaction};

use super::completion::{CompletionEvidence, ExpectationEvidence};
use super::model::CommandEdit;

#[derive(Clone)]
pub(super) struct CliInvocation {
    pub(super) id: u64,
    pub(super) args: Arc<[OsString]>,
    pub(super) completion: Option<CompletionPoint>,
}

impl CliInvocation {
    pub(super) fn new(id: u64, args: Arc<[OsString]>) -> Self {
        Self::from_parts(id, args, None)
    }

    pub(super) fn for_completion(
        id: u64,
        args: Arc<[OsString]>,
        argument: usize,
        prefix: OsString,
        suffix: OsString,
    ) -> Self {
        Self::from_parts(
            id,
            args,
            Some(CompletionPoint {
                argument,
                prefix,
                suffix,
            }),
        )
    }

    fn from_parts(id: u64, args: Arc<[OsString]>, completion: Option<CompletionPoint>) -> Self {
        Self {
            id,
            args,
            completion,
        }
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

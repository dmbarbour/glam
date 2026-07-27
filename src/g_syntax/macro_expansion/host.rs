use std::sync::Arc;

use crate::api::Value;
use crate::reflection::{
    CommitResult, ExactConflictAnalysis, HostSnapshot, ReflectionStore, StoreSnapshot, TaskCommit,
    TaskEnvironment, TaskHost,
};

use super::effects::MacroEffects;
use super::io::{MacroCursor, MacroInput, MacroOutput};

#[derive(Clone)]
pub(super) struct MacroSnapshot {
    pub(super) environment: Value,
    pub(super) input: Arc<MacroInput>,
}

#[derive(Clone, Default)]
pub(super) struct MacroJournal {
    diagnostics: Arc<Vec<crate::api::Diagnostic>>,
    pub(super) active_cases: Vec<Value>,
    pub(super) visited_cases: Vec<Value>,
    pub(super) cursor: MacroCursor,
    output: Vec<MacroOutput>,
    output_layouts: Vec<MacroOutputLayout>,
    root_anchor_expansion: bool,
    root_item_has_content: bool,
}

#[derive(Clone, Default)]
struct MacroOutputLayout {
    anchors: usize,
    item_has_content: bool,
}

impl MacroJournal {
    pub(super) fn push_diagnostic(&mut self, diagnostic: crate::api::Diagnostic) {
        Arc::make_mut(&mut self.diagnostics).push(diagnostic);
    }

    pub(super) fn diagnostics(&self) -> &[crate::api::Diagnostic] {
        &self.diagnostics
    }

    pub(super) fn output(&self) -> &[MacroOutput] {
        &self.output
    }

    pub(super) fn write_text(&mut self, text: String) {
        if !text.is_empty() {
            self.mark_output_content();
        }
        self.output.push(MacroOutput::Text(text));
    }

    pub(super) fn write_data(&mut self, value: Value) {
        self.mark_output_content();
        self.output.push(MacroOutput::Data(value));
    }

    pub(super) fn write_separator(&mut self) {
        self.output.push(MacroOutput::Separator);
    }

    pub(super) fn enter_output_layout(&mut self) {
        self.mark_output_content();
        self.output.push(MacroOutput::LayoutStart);
        self.output_layouts.push(MacroOutputLayout::default());
    }

    pub(super) fn write_anchor(&mut self) -> Result<(), &'static str> {
        if let Some(layout) = self.output_layouts.last_mut() {
            if layout.anchors > 0 && !layout.item_has_content {
                return Err("macro `.write.anchor` cannot follow an empty layout item");
            }
            layout.anchors += 1;
            layout.item_has_content = false;
            self.output.push(MacroOutput::Anchor);
            return Ok(());
        }

        if !self.root_anchor_expansion {
            if !self.output.is_empty() {
                return Err(
                    "macro `.write.anchor` must be the first output operation outside a layout",
                );
            }
            self.root_anchor_expansion = true;
        } else if !self.root_item_has_content {
            return Err("macro `.write.anchor` cannot follow an empty expansion item");
        }
        self.root_item_has_content = false;
        self.output.push(MacroOutput::Anchor);
        Ok(())
    }

    pub(super) fn exit_output_layout(&mut self) -> Result<(), &'static str> {
        let Some(layout) = self.output_layouts.last() else {
            return Err("internal macro output-layout stack became unbalanced");
        };
        if layout.anchors == 0 {
            return Err("macro `.write.layout` requires at least one anchored item");
        }
        if !layout.item_has_content {
            return Err("macro `.write.layout` cannot end with an empty item");
        }
        self.output_layouts.pop();
        self.output.push(MacroOutput::LayoutEnd);
        Ok(())
    }

    pub(super) fn output_is_complete(&self) -> bool {
        self.output_layouts.is_empty()
            && (!self.root_anchor_expansion || self.root_item_has_content)
    }

    pub(super) fn is_anchor_expansion(&self) -> bool {
        self.root_anchor_expansion
    }

    fn mark_output_content(&mut self) {
        if let Some(layout) = self.output_layouts.last_mut() {
            layout.item_has_content = true;
        } else {
            self.root_item_has_content = true;
        }
    }
}

pub(super) struct MacroHost {
    snapshot: MacroSnapshot,
    store: StoreSnapshot,
}

impl MacroHost {
    pub(super) fn new(environment: Value, input: MacroInput) -> Self {
        Self {
            snapshot: MacroSnapshot {
                environment: environment.clone(),
                input: Arc::new(input),
            },
            store: ReflectionStore::new(Arc::new(ExactConflictAnalysis)).snapshot(),
        }
    }
}

impl TaskEnvironment for MacroHost {
    fn reflection_environment(&self) -> Value {
        self.snapshot.environment.clone()
    }
}

impl TaskHost<MacroEffects> for MacroHost {
    fn snapshot(&self) -> HostSnapshot<MacroEffects> {
        HostSnapshot::new(1, self.store.clone(), self.snapshot.clone())
    }

    fn commit(&self, _commit: TaskCommit<MacroEffects>) -> CommitResult {
        // The all-results runner owns the outer transaction. Macro journals
        // are selected explicitly and never commit through the host.
        CommitResult::Closed
    }

    fn wait_for_change(&self, _observed_generation: u64) -> bool {
        false
    }
}

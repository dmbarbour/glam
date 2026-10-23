use std::sync::Arc;

use crate::api::Value;
use crate::eval::{MacroElement, MacroElementKind, MacroText};

pub(in crate::g_syntax) use crate::eval::{MacroDelimiter, MacroLayout as MacroInputLayout};

#[derive(Debug, Clone)]
pub(in crate::g_syntax) enum MacroInputKind {
    Text {
        text: Arc<str>,
        delimiter: Option<(MacroDelimiter, bool)>,
    },
    Data(Value),
}

#[derive(Debug, Clone)]
pub(in crate::g_syntax) struct MacroInputElement {
    pub(in crate::g_syntax) kind: MacroInputKind,
    pub(in crate::g_syntax) separated: bool,
    pub(in crate::g_syntax) start: usize,
    pub(in crate::g_syntax) end: usize,
}

#[derive(Debug, Clone)]
pub(in crate::g_syntax) struct MacroInput {
    elements: Arc<[MacroInputElement]>,
    layouts: Arc<[MacroInputLayout]>,
    start: usize,
    end: usize,
}

impl MacroInput {
    pub(in crate::g_syntax) fn new(
        elements: Vec<MacroInputElement>,
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            elements: elements.into(),
            layouts: Arc::new([]),
            start,
            end,
        }
    }

    pub(in crate::g_syntax) fn with_layouts(mut self, layouts: Vec<MacroInputLayout>) -> Self {
        self.layouts = layouts.into();
        self
    }

    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self::new(Vec::new(), 0, 0)
    }

    /// The input without its embedded values, which come after, in order:
    /// each data element names its value's index.
    pub(super) fn split(&self) -> (MacroText, Vec<Value>) {
        let mut data = Vec::new();
        let elements = self
            .elements
            .iter()
            .map(|element| MacroElement {
                kind: match &element.kind {
                    MacroInputKind::Text { text, delimiter } => MacroElementKind::Text {
                        text: Arc::clone(text),
                        delimiter: *delimiter,
                    },
                    MacroInputKind::Data(value) => {
                        data.push(value.clone());
                        MacroElementKind::Data(data.len() - 1)
                    }
                },
                separated: element.separated,
                start: element.start,
                end: element.end,
            })
            .collect();
        (
            MacroText::new(elements, Arc::clone(&self.layouts), self.start, self.end),
            data,
        )
    }
}

#[derive(Debug, Clone)]
pub(in crate::g_syntax) enum MacroOutput {
    Text(String),
    Data(Value),
    Separator,
    LayoutStart,
    LayoutEnd,
    Anchor,
}

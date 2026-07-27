use std::sync::Arc;

use crate::api::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::g_syntax) enum MacroDelimiter {
    Parenthesis,
    Bracket,
    Brace,
}

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
            start,
            end,
        }
    }

    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self::new(Vec::new(), 0, 0)
    }

    fn element(&self, index: usize) -> Option<&MacroInputElement> {
        self.elements.get(index)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct MacroCursor {
    element: usize,
    offset: usize,
    separator_consumed: bool,
    delimiters: Vec<MacroDelimiter>,
}

impl MacroCursor {
    pub(super) fn read_text(&mut self, input: &MacroInput, expected: &str) -> bool {
        let mut candidate = self.clone();
        for expected in expected.chars() {
            let Some((actual, width)) = candidate.current_scalar(input, true) else {
                return false;
            };
            if actual != expected || !candidate.advance_text(input, width) {
                return false;
            }
        }
        *self = candidate;
        true
    }

    pub(super) fn textual_run<'a>(&self, input: &'a MacroInput) -> Option<&'a str> {
        if !self.can_read_current(input) {
            return None;
        }
        let element = input.element(self.element)?;
        let MacroInputKind::Text { text, delimiter } = &element.kind else {
            return None;
        };
        if delimiter.is_some() {
            return None;
        }
        text.get(self.offset..)
    }

    pub(super) fn advance_run(&mut self, input: &MacroInput, bytes: usize) -> bool {
        if bytes == 0 {
            return true;
        }
        let Some(run) = self.textual_run(input) else {
            return false;
        };
        if bytes > run.len() || !run.is_char_boundary(bytes) {
            return false;
        }
        self.advance_text(input, bytes)
    }

    pub(super) fn read_data(&mut self, input: &MacroInput) -> Option<Value> {
        if !self.can_read_current(input) || self.offset != 0 {
            return None;
        }
        let element = input.element(self.element)?;
        let MacroInputKind::Data(value) = &element.kind else {
            return None;
        };
        let value = value.clone();
        self.finish_element();
        Some(value)
    }

    pub(super) fn read_separator(&mut self, input: &MacroInput) -> bool {
        let Some(element) = input.element(self.element) else {
            return false;
        };
        if self.offset != 0 || !element.separated || self.separator_consumed {
            return false;
        }
        self.separator_consumed = true;
        true
    }

    pub(super) fn at_end(&self, input: &MacroInput) -> bool {
        self.element == input.elements.len() && self.delimiters.is_empty()
    }

    pub(super) fn balanced(&self) -> bool {
        self.delimiters.is_empty()
    }

    pub(super) fn consumed_end(&self, input: &MacroInput) -> usize {
        let Some(element) = input.element(self.element) else {
            return input.end.max(input.start);
        };
        if self.offset > 0 {
            element.start + self.offset
        } else if self.separator_consumed {
            element.start
        } else if self.element == 0 {
            input.start
        } else {
            input
                .element(self.element - 1)
                .map_or(input.start, |previous| previous.end)
        }
    }

    fn current_scalar(&self, input: &MacroInput, structural: bool) -> Option<(char, usize)> {
        if !self.can_read_current(input) {
            return None;
        }
        let element = input.element(self.element)?;
        let MacroInputKind::Text { text, delimiter } = &element.kind else {
            return None;
        };
        if !structural && delimiter.is_some() {
            return None;
        }
        let scalar = text.get(self.offset..)?.chars().next()?;
        Some((scalar, scalar.len_utf8()))
    }

    fn can_read_current(&self, input: &MacroInput) -> bool {
        input
            .element(self.element)
            .is_some_and(|element| self.offset > 0 || !element.separated || self.separator_consumed)
    }

    fn advance_text(&mut self, input: &MacroInput, bytes: usize) -> bool {
        let Some(element) = input.element(self.element) else {
            return false;
        };
        let MacroInputKind::Text { text, delimiter } = &element.kind else {
            return false;
        };
        let next = self.offset + bytes;
        if next > text.len() || !text.is_char_boundary(next) {
            return false;
        }
        self.offset = next;
        if next != text.len() {
            return true;
        }
        if let Some((delimiter, opening)) = delimiter {
            if *opening {
                self.delimiters.push(*delimiter);
            } else if self.delimiters.pop() != Some(*delimiter) {
                return false;
            }
        }
        self.finish_element();
        true
    }

    fn finish_element(&mut self) {
        self.element += 1;
        self.offset = 0;
        self.separator_consumed = false;
    }
}

#[derive(Debug, Clone)]
pub(in crate::g_syntax) enum MacroOutput {
    Text(String),
    Data(Value),
    Separator,
}

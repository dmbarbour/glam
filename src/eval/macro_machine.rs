//! Source macros as a pure handler's operations (`pure-handler-macro-ops`):
//! private builtins over the macro's input, which an opaque context holds
//! for the whole run, and over each branch's reader position, output shape,
//! output, active cases and logs, which the handler's state holds. The
//! input's embedded data and the environment are plain values in the
//! state, since an opaque payload holds no Glam value.
//!
//! `.case`, `.read.layout` and `.write.layout` are scoped requests of the
//! pure handler's core: an opening operation, the user's operation, then a
//! closing one, so a failure within keeps what the opening changed as
//! evidence.

use std::ops::Range;
use std::sync::Arc;

use glam_gc::Visitor;

use super::builtin_machine::RegionalBuiltinPoll;
use super::whnf::{
    RegionalWhnfStatus, RegionalWhnfWork, drive_regional_in_place, reduce_semantic_shell,
};
use crate::core::{
    Builtin, BuiltinCall, CoreValueFactory, Dict, EvaluationFailure, EvaluationHalt, Key, LazyId,
    LazyValue, List, OpaquePayloadFamily, OpaquePayloadRecord, OpaqueValue, RuntimeValueAccess,
    Value, keys, trace_compatibility_value_managed_edges,
};
use crate::core_net::CoreDataKey;
use crate::evaluation::{EvaluationStepBudget, EvaluationValueAccess};
use crate::number::Number;
use crate::text_pattern::TextPattern;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacroDelimiter {
    Parenthesis,
    Bracket,
    Brace,
}

/// An attached layout in a macro's input: the elements it spans, and each
/// of its items'.
#[derive(Debug, Clone)]
pub(crate) struct MacroLayout {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) items: Arc<[Range<usize>]>,
}

#[derive(Debug, Clone)]
pub(crate) enum MacroElementKind {
    Text {
        text: Arc<str>,
        delimiter: Option<(MacroDelimiter, bool)>,
    },
    /// The index of an embedded value in the handler's data.
    Data(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct MacroElement {
    pub(crate) kind: MacroElementKind,
    pub(crate) separated: bool,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// A macro's input without its embedded values, which the handler's state
/// holds.
#[derive(Debug)]
pub(crate) struct MacroText {
    elements: Arc<[MacroElement]>,
    layouts: Arc<[MacroLayout]>,
    start: usize,
    end: usize,
}

// SAFETY: the input holds text, offsets and indices: no Glam value, runtime
// root, managed pointer or capability.
unsafe impl OpaquePayloadFamily for MacroText {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("source macro input", "src/eval/macro_machine.rs");
}

impl MacroText {
    pub(crate) fn new(
        elements: Arc<[MacroElement]>,
        layouts: Arc<[MacroLayout]>,
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            elements,
            layouts,
            start,
            end,
        }
    }

    pub(crate) fn start(&self) -> usize {
        self.start
    }

    fn element(&self, index: usize) -> Option<&MacroElement> {
        self.elements.get(index)
    }

    fn layout(&self, start: usize) -> Option<&MacroLayout> {
        self.layouts.iter().find(|layout| layout.start == start)
    }
}

/// A branch's reader: a forward-only position in the input.
#[derive(Debug, Clone, Default)]
pub(crate) struct MacroCursor {
    element: usize,
    offset: usize,
    separator_consumed: bool,
    delimiters: Vec<MacroDelimiter>,
    layouts: Vec<MacroLayoutCursor>,
}

// SAFETY: the cursor holds offsets and layouts' ranges: no Glam value,
// runtime root, managed pointer or capability.
unsafe impl OpaquePayloadFamily for MacroCursor {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("source macro reader", "src/eval/macro_machine.rs");
}

#[derive(Debug, Clone)]
struct MacroLayoutCursor {
    layout: MacroLayout,
    next_item: usize,
    item_end: Option<usize>,
    delimiter_depth: usize,
}

impl MacroCursor {
    fn read_text(&mut self, input: &MacroText, expected: &str) -> bool {
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

    fn textual_run<'a>(&self, input: &'a MacroText) -> Option<&'a str> {
        if !self.can_read_current(input) {
            return None;
        }
        let element = input.element(self.element)?;
        let MacroElementKind::Text { text, delimiter } = &element.kind else {
            return None;
        };
        if delimiter.is_some() {
            return None;
        }
        text.get(self.offset..)
    }

    fn advance_run(&mut self, input: &MacroText, bytes: usize) -> bool {
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

    /// Reads an embedded value: its index in the handler's data.
    fn read_data(&mut self, input: &MacroText) -> Option<usize> {
        if !self.can_read_current(input) || self.offset != 0 {
            return None;
        }
        let element = input.element(self.element)?;
        let MacroElementKind::Data(index) = &element.kind else {
            return None;
        };
        let index = *index;
        self.finish_element();
        Some(index)
    }

    fn read_separator(&mut self, input: &MacroText) -> bool {
        if !self.within_active_item() {
            return false;
        }
        let Some(element) = input.element(self.element) else {
            return false;
        };
        if self.offset != 0 || !element.separated || self.separator_consumed {
            return false;
        }
        self.separator_consumed = true;
        true
    }

    fn enter_layout(&mut self, input: &MacroText) -> bool {
        if self.offset != 0 || self.separator_consumed {
            return false;
        }
        let Some(layout) = input.layout(self.element).cloned() else {
            return false;
        };
        if layout.items.is_empty()
            || self
                .active_item_end()
                .is_some_and(|item_end| layout.end > item_end)
        {
            return false;
        }
        self.layouts.push(MacroLayoutCursor {
            layout,
            next_item: 0,
            item_end: None,
            delimiter_depth: self.delimiters.len(),
        });
        true
    }

    fn read_anchor(&mut self) -> bool {
        let Some(layout) = self.layouts.last_mut() else {
            return false;
        };
        if self.offset != 0
            || self.separator_consumed
            || self.delimiters.len() != layout.delimiter_depth
        {
            return false;
        }
        if let Some(item_end) = layout.item_end
            && self.element != item_end
        {
            return false;
        }
        let Some(item) = layout.layout.items.get(layout.next_item).cloned() else {
            return false;
        };
        if self.element != item.start {
            return false;
        }
        layout.next_item += 1;
        layout.item_end = Some(item.end);
        self.separator_consumed = true;
        true
    }

    fn exit_layout(&mut self) -> bool {
        if !self.at_end_of_layout() {
            return false;
        }
        self.layouts.pop();
        true
    }

    pub(crate) fn at_end(&self, input: &MacroText) -> bool {
        if self.layouts.is_empty() {
            self.element == input.elements.len() && self.delimiters.is_empty()
        } else {
            self.at_end_of_layout()
        }
    }

    pub(crate) fn balanced(&self) -> bool {
        self.delimiters.is_empty()
    }

    pub(crate) fn consumed_end(&self, input: &MacroText) -> usize {
        let Some(element) = input.element(self.element) else {
            return if input.elements.is_empty() {
                input.start
            } else {
                input.end.max(input.start)
            };
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

    fn current_scalar(&self, input: &MacroText, structural: bool) -> Option<(char, usize)> {
        if !self.can_read_current(input) {
            return None;
        }
        let element = input.element(self.element)?;
        let MacroElementKind::Text { text, delimiter } = &element.kind else {
            return None;
        };
        if !structural && delimiter.is_some() {
            return None;
        }
        let scalar = text.get(self.offset..)?.chars().next()?;
        Some((scalar, scalar.len_utf8()))
    }

    fn can_read_current(&self, input: &MacroText) -> bool {
        if !self.within_active_item() {
            return false;
        }
        input
            .element(self.element)
            .is_some_and(|element| self.offset > 0 || !element.separated || self.separator_consumed)
    }

    fn within_active_item(&self) -> bool {
        self.active_item_end()
            .is_none_or(|item_end| self.element < item_end)
    }

    fn active_item_end(&self) -> Option<usize> {
        self.layouts.last().and_then(|layout| layout.item_end)
    }

    fn at_end_of_layout(&self) -> bool {
        self.layouts.last().is_some_and(|layout| {
            layout.next_item == layout.layout.items.len()
                && layout.item_end == Some(layout.layout.end)
                && self.element == layout.layout.end
                && self.offset == 0
                && !self.separator_consumed
                && self.delimiters.len() == layout.delimiter_depth
        })
    }

    fn advance_text(&mut self, input: &MacroText, bytes: usize) -> bool {
        let Some(element) = input.element(self.element) else {
            return false;
        };
        let MacroElementKind::Text { text, delimiter } = &element.kind else {
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

/// A branch's output's shape: its open layouts and its root's anchors, to
/// check each write against.
#[derive(Debug, Clone, Default)]
pub(crate) struct MacroShape {
    layouts: Vec<MacroOutputLayout>,
    root_anchor_expansion: bool,
    root_item_has_content: bool,
    written: bool,
}

// SAFETY: the shape holds counts and flags: no Glam value, runtime root,
// managed pointer or capability.
unsafe impl OpaquePayloadFamily for MacroShape {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::edge_free("source macro output shape", "src/eval/macro_machine.rs");
}

#[derive(Debug, Clone, Default)]
struct MacroOutputLayout {
    anchors: usize,
    item_has_content: bool,
}

impl MacroShape {
    fn write_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.mark_content();
        }
        self.written = true;
    }

    fn write_data(&mut self) {
        self.mark_content();
        self.written = true;
    }

    fn write_separator(&mut self) {
        self.written = true;
    }

    fn enter_layout(&mut self) {
        self.mark_content();
        self.written = true;
        self.layouts.push(MacroOutputLayout::default());
    }

    fn write_anchor(&mut self) -> Result<(), &'static str> {
        if let Some(layout) = self.layouts.last_mut() {
            if layout.anchors > 0 && !layout.item_has_content {
                return Err("macro `.write.anchor` cannot follow an empty layout item");
            }
            layout.anchors += 1;
            layout.item_has_content = false;
            self.written = true;
            return Ok(());
        }

        if !self.root_anchor_expansion {
            if self.written {
                return Err(
                    "macro `.write.anchor` must be the first output operation outside a layout",
                );
            }
            self.root_anchor_expansion = true;
        } else if !self.root_item_has_content {
            return Err("macro `.write.anchor` cannot follow an empty expansion item");
        }
        self.root_item_has_content = false;
        self.written = true;
        Ok(())
    }

    fn exit_layout(&mut self) -> Result<(), &'static str> {
        let Some(layout) = self.layouts.last() else {
            return Err("internal macro output-layout stack became unbalanced");
        };
        if layout.anchors == 0 {
            return Err("macro `.write.layout` requires at least one anchored item");
        }
        if !layout.item_has_content {
            return Err("macro `.write.layout` cannot end with an empty item");
        }
        self.layouts.pop();
        self.written = true;
        Ok(())
    }

    pub(crate) fn output_is_complete(&self) -> bool {
        self.layouts.is_empty() && (!self.root_anchor_expansion || self.root_item_has_content)
    }

    pub(crate) fn is_anchor_expansion(&self) -> bool {
        self.root_anchor_expansion
    }

    fn mark_content(&mut self) {
        if let Some(layout) = self.layouts.last_mut() {
            layout.item_has_content = true;
        } else {
            self.root_item_has_content = true;
        }
    }
}

pub(crate) fn validate_written_text(text: &str) -> Result<(), &'static str> {
    if text.bytes().any(|byte| byte <= b' ' || byte == 0x7f) {
        return Err(
            "macro `.write.text` cannot emit ASCII C0 controls, SP, or DEL; use `.write.sep` within an item or `.write.anchor` between layout items",
        );
    }
    if text.contains(['@', '#']) {
        return Err("macro `.write.text` cannot emit `@` or `#`");
    }
    Ok(())
}

/// A macro operation, a request of the handler: `MacroOperation code` or
/// `MacroOperationOn code argument`, each applied to the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MacroOperation {
    ReadText,
    ReadRegex,
    ReadTextSpan,
    ReadData,
    ReadSep,
    ReadAnchor,
    ReadEnd,
    ReadLayoutOpen,
    ReadLayoutClose,
    WriteText,
    WriteData,
    WriteSep,
    WriteAnchor,
    WriteLayoutOpen,
    WriteLayoutClose,
    CaseOpen,
    CaseClose,
    Env,
    Log,
}

const OPERATIONS: [MacroOperation; 19] = [
    MacroOperation::ReadText,
    MacroOperation::ReadRegex,
    MacroOperation::ReadTextSpan,
    MacroOperation::ReadData,
    MacroOperation::ReadSep,
    MacroOperation::ReadAnchor,
    MacroOperation::ReadEnd,
    MacroOperation::ReadLayoutOpen,
    MacroOperation::ReadLayoutClose,
    MacroOperation::WriteText,
    MacroOperation::WriteData,
    MacroOperation::WriteSep,
    MacroOperation::WriteAnchor,
    MacroOperation::WriteLayoutOpen,
    MacroOperation::WriteLayoutClose,
    MacroOperation::CaseOpen,
    MacroOperation::CaseClose,
    MacroOperation::Env,
    MacroOperation::Log,
];

impl MacroOperation {
    fn code(self, _access: &RuntimeValueAccess<'_>) -> Value {
        let code = OPERATIONS
            .iter()
            .position(|operation| *operation == self)
            .expect("every operation is listed");
        Value::Number(Number::from_usize(code))
    }

    fn from_code(_access: &RuntimeValueAccess<'_>, code: &Value) -> Option<Self> {
        let Value::Number(code) = code else {
            return None;
        };
        OPERATIONS.get(code.to_usize_if_integer()?).copied()
    }

    /// The request applying this operation to the state.
    fn request(self, access: &RuntimeValueAccess<'_>) -> Value {
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::MacroOperation,
            arguments: Arc::from([self.code(access)]),
        })
    }

    /// The request applying this operation, with `argument`, to the state.
    fn request_on(self, access: &RuntimeValueAccess<'_>, argument: Value) -> Value {
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::MacroOperationOn,
            arguments: Arc::from([self.code(access), argument]),
        })
    }

    /// Whether the operation reads its argument as text.
    fn reads_text(self) -> bool {
        matches!(self, Self::ReadText | Self::ReadRegex | Self::WriteText)
    }

    fn name(self) -> &'static str {
        match self {
            Self::ReadText => "macro `.read.text`",
            Self::ReadRegex => "macro `.read.regex`",
            Self::ReadTextSpan => "macro `.read.text_span`",
            Self::ReadData => "macro `.read.data`",
            Self::ReadSep => "macro `.read.sep`",
            Self::ReadAnchor => "macro `.read.anchor`",
            Self::ReadEnd => "macro `.read.end`",
            Self::ReadLayoutOpen | Self::ReadLayoutClose => "macro `.read.layout`",
            Self::WriteText => "macro `.write.text`",
            Self::WriteData => "macro `.write.data`",
            Self::WriteSep => "macro `.write.sep`",
            Self::WriteAnchor => "macro `.write.anchor`",
            Self::WriteLayoutOpen | Self::WriteLayoutClose => "macro `.write.layout`",
            Self::CaseOpen | Self::CaseClose => "macro `.case`",
            Self::Env => "macro `.env`",
            Self::Log => "macro `.log`",
        }
    }
}

/// A scoped macro request: `MacroScoped code argument operation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MacroScope {
    Case,
    ReadLayout,
    WriteLayout,
}

const SCOPES: [MacroScope; 3] = [
    MacroScope::Case,
    MacroScope::ReadLayout,
    MacroScope::WriteLayout,
];

impl MacroScope {
    fn code(self, _access: &RuntimeValueAccess<'_>) -> Value {
        let code = SCOPES
            .iter()
            .position(|scope| *scope == self)
            .expect("every scope is listed");
        Value::Number(Number::from_usize(code))
    }
}

/// Source macros' operations, which a pure handler adds to its core's
/// requests.
pub(crate) fn macro_api_in(access: &RuntimeValueAccess<'_>) -> Value {
    let scoped = |scope: MacroScope, argument: Option<Value>| {
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::MacroScoped,
            arguments: std::iter::once(scope.code(access))
                .chain(argument)
                .collect(),
        })
    };
    let dict = |members: Vec<(&str, Value)>| {
        Value::Dict(
            members
                .into_iter()
                .fold(Dict::new_sync(), |dict, (name, value)| {
                    dict.insert(Key::atom_from_text(name), value)
                }),
        )
    };
    let read = dict(vec![
        ("text", MacroOperation::ReadText.request_unapplied(access)),
        ("regex", MacroOperation::ReadRegex.request_unapplied(access)),
        ("text_span", MacroOperation::ReadTextSpan.request(access)),
        ("data", MacroOperation::ReadData.request(access)),
        ("sep", MacroOperation::ReadSep.request(access)),
        (
            "layout",
            scoped(MacroScope::ReadLayout, Some(access.unit())),
        ),
        ("anchor", MacroOperation::ReadAnchor.request(access)),
        ("end", MacroOperation::ReadEnd.request(access)),
    ]);
    let write = dict(vec![
        ("text", MacroOperation::WriteText.request_unapplied(access)),
        ("data", MacroOperation::WriteData.request_unapplied(access)),
        ("sep", MacroOperation::WriteSep.request(access)),
        (
            "layout",
            scoped(MacroScope::WriteLayout, Some(access.unit())),
        ),
        ("anchor", MacroOperation::WriteAnchor.request(access)),
    ]);
    dict(vec![
        ("read", read),
        ("write", write),
        ("case", scoped(MacroScope::Case, None)),
        ("env", MacroOperation::Env.request_unapplied(access)),
        ("log", Value::Builtin(Builtin::MacroLog)),
    ])
}

impl MacroOperation {
    /// The operation awaiting its argument, then the state.
    fn request_unapplied(self, access: &RuntimeValueAccess<'_>) -> Value {
        Value::PartialBuiltin(BuiltinCall {
            builtin: Builtin::MacroOperationOn,
            arguments: Arc::from([self.code(access)]),
        })
    }
}

/// The handler's state before a macro reads `input`: the input, its
/// embedded `data`, the `environment`, the reader at the start, and nothing
/// written, open or logged.
pub(crate) fn macro_state_in(
    access: &RuntimeValueAccess<'_>,
    values: &CoreValueFactory,
    input: Arc<MacroText>,
    data: Vec<Value>,
    environment: Value,
) -> Value {
    debug_assert!(access.belongs_to(values));
    let opaque = |value: OpaqueValue| Value::Opaque(value);
    Value::Dict(
        Dict::new_sync()
            .insert(keys::INPUT.clone(), opaque(OpaqueValue::new(values, input)))
            .insert(keys::DATA.clone(), Value::List(List::from_values(data)))
            .insert(keys::ENV.clone(), environment)
            .insert(
                keys::CURSOR.clone(),
                opaque(OpaqueValue::new(values, Arc::new(MacroCursor::default()))),
            )
            .insert(
                keys::SHAPE.clone(),
                opaque(OpaqueValue::new(values, Arc::new(MacroShape::default()))),
            )
            .insert(keys::OUTPUT.clone(), Value::List(List::empty()))
            .insert(keys::CASES.clone(), Value::List(List::empty()))
            .insert(keys::LOGS.clone(), Value::List(List::empty())),
    )
}

/// Applies `MacroScoped`, a scoped request, and `MacroLog`, a request.
pub(in crate::eval) fn apply_macro_builtin_in(
    access: &EvaluationValueAccess<'_>,
    builtin: Builtin,
    arguments: Vec<Value>,
) -> Result<Value, EvaluationHalt> {
    let values = access.values();
    let wrong = || EvaluationHalt::new("a macro operation received the wrong arguments");
    match builtin {
        Builtin::MacroScoped => {
            let [scope, argument, operation]: [Value; 3] =
                arguments.try_into().map_err(|_| wrong())?;
            let Value::Number(scope) = scope else {
                return Err(wrong());
            };
            let scope = scope
                .to_usize_if_integer()
                .and_then(|scope| SCOPES.get(scope).copied())
                .ok_or_else(wrong)?;
            let (open, close) = match scope {
                MacroScope::Case => (
                    MacroOperation::CaseOpen.request_on(values, argument),
                    MacroOperation::CaseClose.request(values),
                ),
                MacroScope::ReadLayout => (
                    MacroOperation::ReadLayoutOpen.request(values),
                    MacroOperation::ReadLayoutClose.request(values),
                ),
                MacroScope::WriteLayout => (
                    MacroOperation::WriteLayoutOpen.request(values),
                    MacroOperation::WriteLayoutClose.request(values),
                ),
            };
            Ok(Value::PartialBuiltin(BuiltinCall {
                builtin: Builtin::HandlerScoped,
                arguments: Arc::from([open, operation, close]),
            }))
        }
        Builtin::MacroLog => {
            let [severity, message]: [Value; 2] = arguments.try_into().map_err(|_| wrong())?;
            let entry = Value::Dict(
                Dict::new_sync()
                    .insert(keys::SEVERITY.clone(), severity)
                    .insert(keys::MESSAGE.clone(), message),
            );
            Ok(MacroOperation::Log.request_on(values, entry))
        }
        _ => unreachable!("only macro builtins are applied here"),
    }
}

/// What a macro operation waits for next.
enum Phase {
    /// Its argument, as text.
    Argument,
    /// The state, `{user, handler}`.
    State,
    /// The handler's state.
    Handler(Value),
}

/// One macro operation, applied to its argument, if any, and the state.
pub(in crate::eval) struct RegionalMacroMachine {
    operation: Option<MacroOperation>,
    argument: Option<Value>,
    phase: Phase,
    demand: Option<RegionalWhnfWork>,
    state: Value,
    evaluated: Option<Dict>,
    text: Option<String>,
    source_owner: LazyId,
}

impl RegionalMacroMachine {
    pub(in crate::eval) fn new_in(
        access: &EvaluationValueAccess<'_>,
        source_owner: LazyId,
        arguments: &[Value],
    ) -> Self {
        let duplicate = |value: &Value| access.values().duplicate_value(value);
        let (code, argument, state) = match arguments {
            [code, argument, state] => (code, Some(duplicate(argument)), duplicate(state)),
            [code, state] => (code, None, duplicate(state)),
            _ => unreachable!(
                "a macro operation takes its code, its argument, if any, and the state"
            ),
        };
        let operation = MacroOperation::from_code(access.values(), code);
        let phase = if operation.is_some_and(MacroOperation::reads_text) {
            Phase::Argument
        } else {
            Phase::State
        };
        Self {
            operation,
            argument,
            phase,
            demand: None,
            state,
            evaluated: None,
            text: None,
            source_owner,
        }
    }

    fn whnf_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
        value: &Value,
    ) -> Result<Value, RegionalBuiltinPoll> {
        let source_owner = self.source_owner;
        let demand = self.demand.get_or_insert_with(|| {
            RegionalWhnfWork::from_focus(access, access.values().duplicate_value(value))
                .with_source_owner(source_owner)
        });
        match drive_regional_in_place(access, demand, step_budget, reduce_semantic_shell) {
            RegionalWhnfStatus::Ready(value) => {
                self.demand = None;
                Ok(value)
            }
            RegionalWhnfStatus::Boundary(request) => Err(RegionalBuiltinPoll::Boundary(request)),
            RegionalWhnfStatus::BudgetExhausted => Err(RegionalBuiltinPoll::BudgetExhausted),
            RegionalWhnfStatus::Failed(failure) => Err(RegionalBuiltinPoll::Failed(failure)),
        }
    }

    pub(in crate::eval) fn poll_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        step_budget: &mut EvaluationStepBudget,
    ) -> RegionalBuiltinPoll {
        let values = access.values();
        let Some(operation) = self.operation else {
            return failure("a macro operation's code is not its own");
        };
        if operation == MacroOperation::Env {
            // The environment at a path, read as `.get` reads the user's
            // state: lazily, and without the rest of the state.
            let path = self.argument.as_ref().expect("`.env` takes its path");
            let value = Value::Lazy(LazyValue::from_access_in(
                values,
                Arc::from([
                    CoreDataKey::Key(keys::HANDLER.clone()),
                    CoreDataKey::Key(keys::ENV.clone()),
                    CoreDataKey::PathIndex,
                ]),
                Arc::from([
                    values.duplicate_value(&self.state),
                    values.duplicate_value(path),
                ]),
            ));
            let state = values.duplicate_value(&self.state);
            return RegionalBuiltinPoll::Ready(outcome(values, Some(value), state));
        }
        loop {
            let pending = match &self.phase {
                Phase::Argument => values
                    .duplicate_value(self.argument.as_ref().expect("a text operation takes text")),
                Phase::State => values.duplicate_value(&self.state),
                Phase::Handler(handler) => values.duplicate_value(handler),
            };
            let value = match self.whnf_in(access, step_budget, &pending) {
                Ok(value) => value,
                Err(poll) => return poll,
            };
            self.phase = match std::mem::replace(&mut self.phase, Phase::State) {
                Phase::Argument => {
                    let Value::Binary(bytes) = value else {
                        return failure(format!("{} requires text", operation.name()));
                    };
                    let Ok(text) = String::from_utf8(bytes.to_vec()) else {
                        return failure(format!("{} requires UTF-8 text", operation.name()));
                    };
                    self.text = Some(text);
                    Phase::State
                }
                Phase::State => {
                    let Value::Dict(state) = value else {
                        return failure("a macro operation's state is a dict");
                    };
                    let handler = state
                        .get(&keys::HANDLER)
                        .map(|handler| values.duplicate_value(handler))
                        .unwrap_or_else(|| Value::Dict(Dict::new_sync()));
                    self.evaluated = Some(state);
                    Phase::Handler(handler)
                }
                Phase::Handler(_) => {
                    let Value::Dict(handler) = value else {
                        return failure("a macro's handler state is a dict");
                    };
                    return match self.answer_in(access, operation, &handler) {
                        Ok(poll) => poll,
                        Err(message) => failure(message),
                    };
                }
            };
        }
    }

    /// Answers `operation` from the handler's state, `handler`.
    fn answer_in(
        &mut self,
        access: &EvaluationValueAccess<'_>,
        operation: MacroOperation,
        handler: &Dict,
    ) -> Result<RegionalBuiltinPoll, String> {
        let values = access.values();
        let foreign = || format!("{}'s handler state is not its own", operation.name());
        let field = |key: &Key| handler.get(key).ok_or_else(foreign);
        let opaque = |key: &Key| match field(key)? {
            Value::Opaque(value) => Ok(value),
            _ => Err(foreign()),
        };
        let list = |key: &Key| match field(key)? {
            Value::List(list) => Ok(list.clone()),
            _ => Err(foreign()),
        };
        let input = || {
            opaque(&keys::INPUT)?
                .downcast::<MacroText>(values.values())
                .ok_or_else(foreign)
        };
        let cursor = || {
            opaque(&keys::CURSOR)?
                .downcast::<MacroCursor>(values.values())
                .map(|cursor| (*cursor).clone())
                .ok_or_else(foreign)
        };
        let shape = || {
            opaque(&keys::SHAPE)?
                .downcast::<MacroShape>(values.values())
                .map(|shape| (*shape).clone())
                .ok_or_else(foreign)
        };
        let cursor_field =
            |cursor: MacroCursor| (keys::CURSOR.clone(), new_opaque_cursor(values, cursor));
        let push = |key: &Key, item: Value| -> Result<(Key, Value), String> {
            Ok((
                key.clone(),
                Value::List(List::concat(List::from_values(vec![item]), list(key)?)),
            ))
        };
        let output = |name: &Key, payload: Value| {
            push(
                &keys::OUTPUT,
                Value::Dict(Dict::new_sync().insert(name.clone(), payload)),
            )
        };
        let text = || {
            self.text
                .clone()
                .expect("a text operation's text is read first")
        };
        let argument = || {
            values.duplicate_value(
                self.argument
                    .as_ref()
                    .expect("this operation takes its argument"),
            )
        };

        // Each answer: a value, or a failure, and the fields it changes.
        let (value, changes): (Option<Value>, Vec<(Key, Value)>) = match operation {
            MacroOperation::ReadText => {
                let mut cursor = cursor()?;
                if cursor.read_text(&*input()?, &text()) {
                    (Some(values.unit()), vec![cursor_field(cursor)])
                } else {
                    (None, Vec::new())
                }
            }
            MacroOperation::ReadRegex | MacroOperation::ReadTextSpan => {
                let input = input()?;
                let mut cursor = cursor()?;
                let span = match cursor.textual_run(&input) {
                    None => None,
                    Some(run) if operation == MacroOperation::ReadTextSpan => {
                        Some(run.to_owned()).filter(|run| !run.is_empty())
                    }
                    Some(run) => {
                        let matcher = TextPattern::parse(&text())
                            .map_err(|error| format!("invalid macro text pattern: {error}"))?;
                        matcher.match_prefix(run).map(str::to_owned)
                    }
                };
                match span {
                    None => (None, Vec::new()),
                    Some(span) => {
                        if !cursor.advance_run(&input, span.len()) {
                            return Err(format!(
                                "{} produced an invalid textual boundary",
                                operation.name()
                            ));
                        }
                        let value = Value::Dict(
                            Dict::new_sync()
                                .insert(keys::SPAN.clone(), Value::binary_from_text(&span)),
                        );
                        (Some(value), vec![cursor_field(cursor)])
                    }
                }
            }
            MacroOperation::ReadData => {
                let mut cursor = cursor()?;
                match cursor.read_data(&*input()?) {
                    None => (None, Vec::new()),
                    Some(index) => {
                        let data = list(&keys::DATA)?;
                        let item = data
                            .value_slice()
                            .and_then(|data| data.get(index))
                            .map(|item| values.duplicate_value(item))
                            .ok_or_else(foreign)?;
                        (Some(item), vec![cursor_field(cursor)])
                    }
                }
            }
            MacroOperation::ReadSep
            | MacroOperation::ReadAnchor
            | MacroOperation::ReadEnd
            | MacroOperation::ReadLayoutOpen
            | MacroOperation::ReadLayoutClose => {
                let input = input()?;
                let mut cursor = cursor()?;
                let read = match operation {
                    MacroOperation::ReadSep => cursor.read_separator(&input),
                    MacroOperation::ReadAnchor => cursor.read_anchor(),
                    MacroOperation::ReadEnd => cursor.at_end(&input),
                    MacroOperation::ReadLayoutOpen => cursor.enter_layout(&input),
                    _ => cursor.exit_layout(),
                };
                if read {
                    (Some(values.unit()), vec![cursor_field(cursor)])
                } else {
                    (None, Vec::new())
                }
            }
            MacroOperation::WriteText => {
                let text = text();
                validate_written_text(&text)?;
                let mut shape = shape()?;
                shape.write_text(&text);
                (
                    Some(values.unit()),
                    vec![
                        shape_field(values, shape),
                        output(&keys::TEXT, Value::binary_from_text(&text))?,
                    ],
                )
            }
            MacroOperation::WriteData
            | MacroOperation::WriteSep
            | MacroOperation::WriteAnchor
            | MacroOperation::WriteLayoutOpen
            | MacroOperation::WriteLayoutClose => {
                let mut shape = shape()?;
                let item = match operation {
                    MacroOperation::WriteData => {
                        shape.write_data();
                        output(&keys::DATA, argument())?
                    }
                    MacroOperation::WriteSep => {
                        shape.write_separator();
                        output(&keys::SEP, values.unit())?
                    }
                    MacroOperation::WriteAnchor => {
                        shape.write_anchor()?;
                        output(&keys::ANCHOR, values.unit())?
                    }
                    MacroOperation::WriteLayoutOpen => {
                        shape.enter_layout();
                        output(&keys::LAYOUT_START, values.unit())?
                    }
                    _ => {
                        shape.exit_layout()?;
                        output(&keys::LAYOUT_END, values.unit())?
                    }
                };
                (Some(values.unit()), vec![shape_field(values, shape), item])
            }
            MacroOperation::CaseOpen | MacroOperation::CaseClose => {
                let cases = list(&keys::CASES)?;
                let cases = cases.value_slice().ok_or_else(foreign)?;
                let cases: Vec<Value> = if operation == MacroOperation::CaseOpen {
                    std::iter::once(argument())
                        .chain(cases.iter().map(|case| values.duplicate_value(case)))
                        .collect()
                } else {
                    let Some((_, rest)) = cases.split_first() else {
                        return Err("macro case stack became unbalanced".to_owned());
                    };
                    rest.iter()
                        .map(|case| values.duplicate_value(case))
                        .collect()
                };
                (
                    Some(values.unit()),
                    vec![(keys::CASES.clone(), Value::List(List::from_values(cases)))],
                )
            }
            MacroOperation::Log => (Some(values.unit()), vec![push(&keys::LOGS, argument())?]),
            MacroOperation::Env => unreachable!("`.env` answers before reading the state"),
        };

        let state = self
            .evaluated
            .take()
            .expect("the state precedes the handler's");
        let state = match value {
            None => Value::Dict(state),
            Some(_) => {
                let handler = changes
                    .into_iter()
                    .fold(handler.clone(), |handler, (key, value)| {
                        handler.insert(key, value)
                    });
                Value::Dict(state.insert(keys::HANDLER.clone(), Value::Dict(handler)))
            }
        };
        Ok(RegionalBuiltinPoll::Ready(outcome(values, value, state)))
    }

    pub(in crate::eval) fn trace_managed_edges(&self, visitor: &mut Visitor<'_>) {
        if let Phase::Handler(handler) = &self.phase {
            trace_compatibility_value_managed_edges(handler, visitor);
        }
        if let Some(argument) = &self.argument {
            trace_compatibility_value_managed_edges(argument, visitor);
        }
        if let Some(demand) = &self.demand {
            demand.trace_managed_edges(visitor);
        }
        trace_compatibility_value_managed_edges(&self.state, visitor);
        if let Some(state) = &self.evaluated {
            trace_compatibility_value_managed_edges(&Value::Dict(state.clone()), visitor);
        }
    }
}

fn new_opaque_cursor(values: &RuntimeValueAccess<'_>, cursor: MacroCursor) -> Value {
    Value::Opaque(OpaqueValue::new(values.values(), Arc::new(cursor)))
}

fn shape_field(values: &RuntimeValueAccess<'_>, shape: MacroShape) -> (Key, Value) {
    (
        keys::SHAPE.clone(),
        Value::Opaque(OpaqueValue::new(values.values(), Arc::new(shape))),
    )
}

/// One outcome: `{value, state}`, or `{failed: state}` without a value.
fn outcome(_access: &RuntimeValueAccess<'_>, value: Option<Value>, state: Value) -> Value {
    let outcome = match value {
        Some(value) => Dict::new_sync()
            .insert(keys::VALUE.clone(), value)
            .insert(keys::STATE.clone(), state),
        None => Dict::new_sync().insert(keys::FAILED.clone(), state),
    };
    Value::List(List::from_values(vec![Value::Dict(outcome)]))
}

fn failure(message: impl Into<String>) -> RegionalBuiltinPoll {
    RegionalBuiltinPoll::Failed(Arc::new(EvaluationFailure::message(message.into())))
}

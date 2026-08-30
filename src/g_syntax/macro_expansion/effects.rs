use crate::api::{Diagnostic, Value};
use crate::core::{Dict, Key, List, Value as CoreValue};
use crate::eval;
use crate::reflection::{
    EffectRequestSpec, RequestContext, RequestResult, TaskHalt, TaskSpecialization, parse_severity,
    prepare_message,
};
use crate::text_pattern::TextPattern;

use super::host::{MacroHost, MacroJournal, MacroSnapshot};

const CASE_EXIT_TAG: [&str; 5] = ["macro_runtime", "g0", "request", "case", "exit"];
const READ_LAYOUT_EXIT_TAG: [&str; 5] = ["macro_runtime", "g0", "request", "read_layout", "exit"];
const WRITE_LAYOUT_EXIT_TAG: [&str; 5] = ["macro_runtime", "g0", "request", "write_layout", "exit"];

#[derive(Clone, Copy)]
pub(super) struct MacroEffects;

#[derive(Clone, Copy)]
pub(super) enum MacroRequest {
    Environment,
    Log,
    Case,
    CaseExit,
    ReadText,
    ReadRegex,
    ReadTextSpan,
    ReadData,
    ReadSeparator,
    ReadLayout,
    ReadAnchor,
    ReadLayoutExit,
    ReadEnd,
    WriteText,
    WriteData,
    WriteSeparator,
    WriteLayout,
    WriteAnchor,
    WriteLayoutExit,
}

impl TaskSpecialization for MacroEffects {
    type Host = MacroHost;
    type Request = MacroRequest;
    type Snapshot = MacroSnapshot;
    type Journal = MacroJournal;

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        vec![
            EffectRequestSpec::new(
                "env",
                ["reflection_runtime", "v0", "request", "env"],
                1,
                MacroRequest::Environment,
            ),
            EffectRequestSpec::new(
                "log",
                ["reflection_runtime", "v0", "request", "log"],
                2,
                MacroRequest::Log,
            ),
            EffectRequestSpec::new(
                "case",
                ["macro_runtime", "g0", "request", "case"],
                2,
                MacroRequest::Case,
            ),
            EffectRequestSpec::hidden(CASE_EXIT_TAG, 0, MacroRequest::CaseExit),
            request(["read", "text"], "read_text", 1, MacroRequest::ReadText),
            request(["read", "regex"], "read_regex", 1, MacroRequest::ReadRegex),
            request(
                ["read", "text_span"],
                "read_text_span",
                0,
                MacroRequest::ReadTextSpan,
            ),
            request(["read", "data"], "read_data", 0, MacroRequest::ReadData),
            request(["read", "sep"], "read_sep", 0, MacroRequest::ReadSeparator),
            request(
                ["read", "layout"],
                "read_layout",
                1,
                MacroRequest::ReadLayout,
            ),
            request(
                ["read", "anchor"],
                "read_anchor",
                0,
                MacroRequest::ReadAnchor,
            ),
            EffectRequestSpec::hidden(READ_LAYOUT_EXIT_TAG, 0, MacroRequest::ReadLayoutExit),
            request(["read", "end"], "read_end", 0, MacroRequest::ReadEnd),
            request(["write", "text"], "write_text", 1, MacroRequest::WriteText),
            request(["write", "data"], "write_data", 1, MacroRequest::WriteData),
            request(
                ["write", "sep"],
                "write_sep",
                0,
                MacroRequest::WriteSeparator,
            ),
            request(
                ["write", "layout"],
                "write_layout",
                1,
                MacroRequest::WriteLayout,
            ),
            request(
                ["write", "anchor"],
                "write_anchor",
                0,
                MacroRequest::WriteAnchor,
            ),
            EffectRequestSpec::hidden(WRITE_LAYOUT_EXIT_TAG, 0, MacroRequest::WriteLayoutExit),
        ]
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskHalt> {
        match request {
            MacroRequest::Environment => environment(arguments, context),
            MacroRequest::Log => log(arguments, context),
            MacroRequest::Case => enter_case(arguments, context),
            MacroRequest::CaseExit => exit_case(arguments, context),
            MacroRequest::ReadText => read_text(arguments, context),
            MacroRequest::ReadRegex => read_regex(arguments, context),
            MacroRequest::ReadTextSpan => read_text_span(arguments, context),
            MacroRequest::ReadData => read_data(arguments, context),
            MacroRequest::ReadSeparator => read_separator(arguments, context),
            MacroRequest::ReadLayout => read_layout(arguments, context),
            MacroRequest::ReadAnchor => read_anchor(arguments, context),
            MacroRequest::ReadLayoutExit => read_layout_exit(arguments, context),
            MacroRequest::ReadEnd => read_end(arguments, context),
            MacroRequest::WriteText => write_text(arguments, context),
            MacroRequest::WriteData => write_data(arguments, context),
            MacroRequest::WriteSeparator => write_separator(arguments, context),
            MacroRequest::WriteLayout => write_layout(arguments, context),
            MacroRequest::WriteAnchor => write_anchor(arguments, context),
            MacroRequest::WriteLayoutExit => write_layout_exit(arguments, context),
        }
    }
}

fn request(
    path: [&str; 2],
    tag: &str,
    arity: usize,
    request: MacroRequest,
) -> EffectRequestSpec<MacroRequest> {
    EffectRequestSpec::at_path(
        path,
        ["macro_runtime", "g0", "request", tag],
        arity,
        request,
    )
}

fn environment(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [path]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.env` received the wrong number of arguments"))?;
    let path = context.evaluate_key_path(&path)?;
    let environment = {
        let mut transaction = context
            .transaction()
            .ok_or_else(|| TaskHalt::new("macro `.env` escaped its isolated transaction"))?;
        transaction.parts().0.environment.clone()
    };
    Ok(RequestResult::Return(
        context.evaluate_path(&environment, &path)?,
    ))
}

fn log(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [severity, message]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.log` received the wrong number of arguments"))?;
    let severity = parse_severity(context, severity)?;
    let message = prepare_message(context, message)?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("macro `.log` escaped its isolated transaction"))?;
    transaction
        .parts()
        .1
        .push_diagnostic(Diagnostic::from_emission(severity, message));
    Ok(RequestResult::ReturnUnit)
}

fn enter_case(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [explanation, parser]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.case` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("macro `.case` escaped its isolated transaction"))?;
    let (_, journal) = transaction.parts();
    #[cfg(test)]
    journal.visited_cases.push(explanation.clone());
    journal.active_cases.push(explanation);
    Ok(RequestResult::Scoped {
        operation: parser,
        close: hidden_effect(context, CASE_EXIT_TAG),
    })
}

fn exit_case(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("internal macro case close received arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskHalt::new("macro case close escaped its isolated transaction"))?;
    transaction
        .parts()
        .1
        .active_cases
        .pop()
        .ok_or_else(|| TaskHalt::new("macro case stack became unbalanced"))?;
    Ok(RequestResult::ReturnUnit)
}

fn read_text(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [expected]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.read.text` received the wrong number of arguments"))?;
    let expected = text_value(context, expected, "macro `.read.text`")?;
    let mut transaction = macro_transaction(context, "macro `.read.text`")?;
    let (snapshot, journal) = transaction.parts();
    if journal.cursor.read_text(&snapshot.input, &expected) {
        Ok(RequestResult::ReturnUnit)
    } else {
        Ok(RequestResult::Fail)
    }
}

fn read_regex(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [pattern]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.read.regex` received the wrong number of arguments"))?;
    let pattern = text_value(context, pattern, "macro `.read.regex`")?;
    let matcher = TextPattern::parse(&pattern)
        .map_err(|error| TaskHalt::new(format!("invalid macro text pattern: {error}")))?;
    let mut transaction = macro_transaction(context, "macro `.read.regex`")?;
    let (snapshot, journal) = transaction.parts();
    let Some(run) = journal.cursor.textual_run(&snapshot.input) else {
        return Ok(RequestResult::Fail);
    };
    let Some(matched) = matcher.match_prefix(run) else {
        return Ok(RequestResult::Fail);
    };
    let matched = matched.to_owned();
    if !journal.cursor.advance_run(&snapshot.input, matched.len()) {
        return Err(TaskHalt::new(
            "macro regex reader produced an invalid textual boundary",
        ));
    }
    Ok(RequestResult::Return(span_value(context, matched)))
}

fn read_text_span(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments.try_into().map_err(|_| {
        TaskHalt::new("macro `.read.text_span` received the wrong number of arguments")
    })?;
    let mut transaction = macro_transaction(context, "macro `.read.text_span`")?;
    let (snapshot, journal) = transaction.parts();
    let Some(run) = journal.cursor.textual_run(&snapshot.input) else {
        return Ok(RequestResult::Fail);
    };
    if run.is_empty() {
        return Ok(RequestResult::Fail);
    }
    let span = run.to_owned();
    if !journal.cursor.advance_run(&snapshot.input, span.len()) {
        return Err(TaskHalt::new(
            "macro text-span reader produced an invalid textual boundary",
        ));
    }
    Ok(RequestResult::Return(span_value(context, span)))
}

fn read_data(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.read.data` received the wrong number of arguments"))?;
    let mut transaction = macro_transaction(context, "macro `.read.data`")?;
    let (snapshot, journal) = transaction.parts();
    Ok(journal
        .cursor
        .read_data(&snapshot.input)
        .map_or(RequestResult::Fail, RequestResult::Return))
}

fn read_separator(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.read.sep` received the wrong number of arguments"))?;
    let mut transaction = macro_transaction(context, "macro `.read.sep`")?;
    let (snapshot, journal) = transaction.parts();
    if journal.cursor.read_separator(&snapshot.input) {
        Ok(RequestResult::ReturnUnit)
    } else {
        Ok(RequestResult::Fail)
    }
}

fn read_layout(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [parser]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskHalt::new("macro `.read.layout` received the wrong number of arguments")
    })?;
    let mut transaction = macro_transaction(context, "macro `.read.layout`")?;
    let (snapshot, journal) = transaction.parts();
    if !journal.cursor.enter_layout(&snapshot.input) {
        return Ok(RequestResult::Fail);
    }
    Ok(RequestResult::Scoped {
        operation: parser,
        close: hidden_effect(context, READ_LAYOUT_EXIT_TAG),
    })
}

fn read_anchor(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.read.anchor` received arguments"))?;
    let mut transaction = macro_transaction(context, "macro `.read.anchor`")?;
    if transaction.parts().1.cursor.read_anchor() {
        Ok(RequestResult::ReturnUnit)
    } else {
        Ok(RequestResult::Fail)
    }
}

fn read_layout_exit(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("internal macro read-layout close received arguments"))?;
    let mut transaction = macro_transaction(context, "internal macro read-layout close")?;
    if transaction.parts().1.cursor.exit_layout() {
        Ok(RequestResult::ReturnUnit)
    } else {
        Ok(RequestResult::Fail)
    }
}

fn read_end(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.read.end` received the wrong number of arguments"))?;
    let mut transaction = macro_transaction(context, "macro `.read.end`")?;
    let (snapshot, journal) = transaction.parts();
    if journal.cursor.at_end(&snapshot.input) {
        Ok(RequestResult::ReturnUnit)
    } else {
        Ok(RequestResult::Fail)
    }
}

fn write_text(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [text]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.write.text` received the wrong number of arguments"))?;
    let text = text_value(context, text, "macro `.write.text`")?;
    validate_written_text(&text).map_err(TaskHalt::new)?;
    macro_transaction(context, "macro `.write.text`")?
        .parts()
        .1
        .write_text(text);
    Ok(RequestResult::ReturnUnit)
}

pub(super) fn validate_written_text(text: &str) -> Result<(), &'static str> {
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

fn write_data(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [value]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.write.data` received the wrong number of arguments"))?;
    macro_transaction(context, "macro `.write.data`")?
        .parts()
        .1
        .write_data(value);
    Ok(RequestResult::ReturnUnit)
}

fn write_separator(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.write.sep` received the wrong number of arguments"))?;
    macro_transaction(context, "macro `.write.sep`")?
        .parts()
        .1
        .write_separator();
    Ok(RequestResult::ReturnUnit)
}

fn write_layout(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let [writer]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskHalt::new("macro `.write.layout` received the wrong number of arguments")
    })?;
    macro_transaction(context, "macro `.write.layout`")?
        .parts()
        .1
        .enter_output_layout();
    Ok(RequestResult::Scoped {
        operation: writer,
        close: hidden_effect(context, WRITE_LAYOUT_EXIT_TAG),
    })
}

fn write_anchor(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("macro `.write.anchor` received arguments"))?;
    macro_transaction(context, "macro `.write.anchor`")?
        .parts()
        .1
        .write_anchor()
        .map_err(TaskHalt::new)?;
    Ok(RequestResult::ReturnUnit)
}

fn write_layout_exit(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskHalt> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskHalt::new("internal macro write-layout close received arguments"))?;
    macro_transaction(context, "internal macro write-layout close")?
        .parts()
        .1
        .exit_output_layout()
        .map_err(TaskHalt::new)?;
    Ok(RequestResult::ReturnUnit)
}

fn hidden_effect(context: &RequestContext<'_, MacroEffects>, tag: [&str; 5]) -> Value {
    let request = CoreValue::Dict(Dict::new_sync().insert(
        Key::abstract_global_path(tag),
        CoreValue::List(List::empty()),
    ));
    Value::from_core(
        context.eval_context().values(),
        eval::constant_effect(request),
    )
}

fn span_value(context: &RequestContext<'_, MacroEffects>, span: String) -> Value {
    Value::from_core(
        context.eval_context().values(),
        CoreValue::Dict(Dict::new_sync().insert(
            Key::atom_from_text("span"),
            CoreValue::binary_from_text(&span),
        )),
    )
}

fn macro_transaction<'context, 'request>(
    context: &'context mut RequestContext<'request, MacroEffects>,
    request: &str,
) -> Result<crate::reflection::TransactionContext<'context, MacroEffects>, TaskHalt> {
    context
        .transaction()
        .ok_or_else(|| TaskHalt::new(format!("{request} escaped its isolated transaction")))
}

fn text_value(
    context: &RequestContext<'_, MacroEffects>,
    value: Value,
    request: &str,
) -> Result<String, TaskHalt> {
    let value = context.evaluate(&value)?;
    let CoreValue::Binary(bytes) = value.as_value().as_core() else {
        return Err(TaskHalt::new(format!("{request} requires text")));
    };
    String::from_utf8(bytes.to_vec())
        .map_err(|_| TaskHalt::new(format!("{request} requires UTF-8 text")))
}

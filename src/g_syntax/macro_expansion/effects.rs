use crate::api::{Diagnostic, Value};
use crate::core::{Dict, Key, List, Value as CoreValue};
use crate::eval;
use crate::reflection::{
    EffectRequestSpec, RequestContext, RequestResult, TaskError, TaskSpecialization,
    get_value_path, parse_severity, prepare_message,
};
use crate::text_pattern::TextPattern;

use super::host::{MacroHost, MacroJournal, MacroSnapshot};
use super::io::MacroOutput;

const CASE_EXIT_TAG: [&str; 5] = ["macro_runtime", "g0", "request", "case", "exit"];

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
    ReadEnd,
    WriteText,
    WriteData,
    WriteSeparator,
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
            request(["read", "end"], "read_end", 0, MacroRequest::ReadEnd),
            request(["write", "text"], "write_text", 1, MacroRequest::WriteText),
            request(["write", "data"], "write_data", 1, MacroRequest::WriteData),
            request(
                ["write", "sep"],
                "write_sep",
                0,
                MacroRequest::WriteSeparator,
            ),
        ]
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskError> {
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
            MacroRequest::ReadEnd => read_end(arguments, context),
            MacroRequest::WriteText => write_text(arguments, context),
            MacroRequest::WriteData => write_data(arguments, context),
            MacroRequest::WriteSeparator => write_separator(arguments, context),
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
) -> Result<RequestResult, TaskError> {
    let [path]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.env` received the wrong number of arguments"))?;
    let path = eval::eval_key_path_list(context.eval_context(), path.as_core())
        .map_err(|error| TaskError::new(error.to_string()))?;
    let environment = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro `.env` escaped its isolated transaction"))?
        .parts()
        .0
        .environment
        .as_core()
        .clone();
    let value = get_value_path(context.eval_context(), &environment, &path)?;
    Ok(RequestResult::Return(Value::from_core(value)))
}

fn log(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let [severity, message]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.log` received the wrong number of arguments"))?;
    let severity = parse_severity(context.eval_context(), severity)?;
    let message = prepare_message(context.eval_context(), message)?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro `.log` escaped its isolated transaction"))?;
    transaction
        .parts()
        .1
        .push_diagnostic(Diagnostic::from_emission(severity, message));
    Ok(RequestResult::ReturnUnit)
}

fn enter_case(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let [explanation, parser]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.case` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro `.case` escaped its isolated transaction"))?;
    let (_, journal) = transaction.parts();
    journal.active_cases.push(explanation.clone());
    journal.visited_cases.push(explanation);
    Ok(RequestResult::Scoped {
        operation: parser,
        close: case_exit_effect(),
    })
}

fn exit_case(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("internal macro case close received arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("macro case close escaped its isolated transaction"))?;
    transaction
        .parts()
        .1
        .active_cases
        .pop()
        .ok_or_else(|| TaskError::new("macro case stack became unbalanced"))?;
    Ok(RequestResult::ReturnUnit)
}

fn case_exit_effect() -> Value {
    let request = CoreValue::Dict(Dict::new_sync().insert(
        Key::abstract_global_path(CASE_EXIT_TAG),
        CoreValue::List(List::empty()),
    ));
    Value::from_core(eval::constant_effect(request))
}

fn read_text(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let [expected]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.read.text` received the wrong number of arguments"))?;
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
) -> Result<RequestResult, TaskError> {
    let [pattern]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new("macro `.read.regex` received the wrong number of arguments")
    })?;
    let pattern = text_value(context, pattern, "macro `.read.regex`")?;
    let matcher = TextPattern::parse(&pattern)
        .map_err(|error| TaskError::new(format!("invalid macro text pattern: {error}")))?;
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
        return Err(TaskError::new(
            "macro regex reader produced an invalid textual boundary",
        ));
    }
    Ok(RequestResult::Return(Value::record([(
        "span",
        Value::text(matched),
    )])))
}

fn read_text_span(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments.try_into().map_err(|_| {
        TaskError::new("macro `.read.text_span` received the wrong number of arguments")
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
        return Err(TaskError::new(
            "macro text-span reader produced an invalid textual boundary",
        ));
    }
    Ok(RequestResult::Return(Value::record([(
        "span",
        Value::text(span),
    )])))
}

fn read_data(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.read.data` received the wrong number of arguments"))?;
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
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.read.sep` received the wrong number of arguments"))?;
    let mut transaction = macro_transaction(context, "macro `.read.sep`")?;
    let (snapshot, journal) = transaction.parts();
    if journal.cursor.read_separator(&snapshot.input) {
        Ok(RequestResult::ReturnUnit)
    } else {
        Ok(RequestResult::Fail)
    }
}

fn read_end(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.read.end` received the wrong number of arguments"))?;
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
) -> Result<RequestResult, TaskError> {
    let [text]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new("macro `.write.text` received the wrong number of arguments")
    })?;
    let text = text_value(context, text, "macro `.write.text`")?;
    if text.contains(['@', '#', '\u{e000}']) {
        return Err(TaskError::new(
            "macro `.write.text` cannot emit `@`, `#`, or the reserved embedded-data marker",
        ));
    }
    macro_transaction(context, "macro `.write.text`")?
        .parts()
        .1
        .output
        .push(MacroOutput::Text(text));
    Ok(RequestResult::ReturnUnit)
}

fn write_data(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new("macro `.write.data` received the wrong number of arguments")
    })?;
    macro_transaction(context, "macro `.write.data`")?
        .parts()
        .1
        .output
        .push(MacroOutput::Data(value));
    Ok(RequestResult::ReturnUnit)
}

fn write_separator(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MacroEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("macro `.write.sep` received the wrong number of arguments"))?;
    macro_transaction(context, "macro `.write.sep`")?
        .parts()
        .1
        .output
        .push(MacroOutput::Separator);
    Ok(RequestResult::ReturnUnit)
}

fn macro_transaction<'context, 'request>(
    context: &'context mut RequestContext<'request, MacroEffects>,
    request: &str,
) -> Result<crate::reflection::TransactionContext<'context, MacroEffects>, TaskError> {
    context
        .transaction()
        .ok_or_else(|| TaskError::new(format!("{request} escaped its isolated transaction")))
}

fn text_value(
    context: &RequestContext<'_, MacroEffects>,
    value: Value,
    request: &str,
) -> Result<String, TaskError> {
    let CoreValue::Binary(bytes) = eval::eval_value(context.eval_context(), value.as_core())
        .map_err(|error| TaskError::new(error.to_string()))?
    else {
        return Err(TaskError::new(format!("{request} requires text")));
    };
    String::from_utf8(bytes.to_vec())
        .map_err(|_| TaskError::new(format!("{request} requires UTF-8 text")))
}

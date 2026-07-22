use std::path::PathBuf;
use std::sync::Arc;

use crate::api::{ModuleInput, Value};
use crate::core::{Atom, Key, OpaqueValue, Value as CoreValue};
use crate::eval;
use crate::reflection::{
    EffectRequestSpec, ReflectionRequest, RequestContext, RequestResult, TaskError,
    TaskSpecialization, environment_log_request_specs, handle_reflection_request,
};

use super::completion::{CompletionEvidence, CompletionKind, ExpectationEvidence, Frontier};
use super::host::{CliHost, CliJournal};
use super::model::CommandEdit;
use super::path::{self, PathAccess, PathKind};
use super::token;

#[derive(Clone, Copy)]
pub(super) struct CliEffects;

#[derive(Clone)]
pub(super) enum CliRequest {
    Reflection(ReflectionRequest),
    ReadKeyword,
    ReadText,
    ReadToken,
    ReadPath,
    ReadEnd,
    WriteFile,
    WriteScript,
    WriteManifest,
    WriteReflectionArgument,
    WriteAssemblyArgument,
    WriteWorkerCount,
    EscapedToken,
}

impl TaskSpecialization for CliEffects {
    type Host = CliHost;
    type Request = CliRequest;
    type Snapshot = super::host::CliSnapshot;
    type Journal = CliJournal;

    fn exposes_shared_heap(&self) -> bool {
        false
    }

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        environment_log_request_specs()
            .into_iter()
            .map(|spec| spec.map_request(CliRequest::Reflection))
            .chain([
                request(
                    ["read", "keyword"],
                    "read_keyword",
                    1,
                    CliRequest::ReadKeyword,
                ),
                request(["read", "text"], "read_text", 1, CliRequest::ReadText),
                request(["read", "token"], "read_token", 2, CliRequest::ReadToken),
                request(["read", "path"], "read_path", 2, CliRequest::ReadPath),
                request(["read", "end"], "read_end", 0, CliRequest::ReadEnd),
                request(["write", "file"], "write_file", 1, CliRequest::WriteFile),
                request(
                    ["write", "script"],
                    "write_script",
                    2,
                    CliRequest::WriteScript,
                ),
                request(
                    ["write", "manifest"],
                    "write_manifest",
                    1,
                    CliRequest::WriteManifest,
                ),
                request(
                    ["write", "refl_arg"],
                    "write_refl_arg",
                    1,
                    CliRequest::WriteReflectionArgument,
                ),
                request(
                    ["write", "assembly_arg"],
                    "write_assembly_arg",
                    1,
                    CliRequest::WriteAssemblyArgument,
                ),
                request(
                    ["write", "worker_count"],
                    "write_worker_count",
                    1,
                    CliRequest::WriteWorkerCount,
                ),
            ])
            .chain(
                token::request_specs()
                    .into_iter()
                    .map(|spec| spec.map_request(|_| CliRequest::EscapedToken)),
            )
            .collect()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, TaskError> {
        match request {
            CliRequest::Reflection(request) => {
                handle_reflection_request(request, arguments, context)
            }
            CliRequest::ReadKeyword => read_keyword(arguments, context),
            CliRequest::ReadText => read_text(arguments, context),
            CliRequest::ReadToken => read_token(arguments, context),
            CliRequest::ReadPath => read_path(arguments, context),
            CliRequest::ReadEnd => read_end(arguments, context),
            CliRequest::WriteFile => write_path(arguments, context, PathWriter::File),
            CliRequest::WriteScript => write_script(arguments, context),
            CliRequest::WriteManifest => write_path(arguments, context, PathWriter::Manifest),
            CliRequest::WriteReflectionArgument => {
                write_text_argument(arguments, context, TextWriter::Reflection)
            }
            CliRequest::WriteAssemblyArgument => {
                write_text_argument(arguments, context, TextWriter::Assembly)
            }
            CliRequest::WriteWorkerCount => write_worker_count(arguments, context),
            CliRequest::EscapedToken => Err(TaskError::new(
                "token parser operation escaped `.read.token`",
            )),
        }
    }
}

fn request(
    api_path: [&str; 2],
    tag: &str,
    arity: usize,
    request: CliRequest,
) -> EffectRequestSpec<CliRequest> {
    EffectRequestSpec::at_path(
        api_path,
        ["cli_runtime", "v0", "request", tag],
        arity,
        request,
    )
}

fn read_keyword(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let [expected]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.read.keyword` received the wrong number of arguments"))?;
    let expected = text_value(context, expected, "`.read.keyword`")?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI reader escaped its isolated search transaction"))?;
    let (snapshot, journal) = transaction.parts();
    if let Some(completion) = snapshot
        .invocation
        .completion
        .as_ref()
        .filter(|completion| completion.argument == journal.cursor)
    {
        let offset = completion.prefix.as_encoded_bytes().len();
        record_expectation(journal, journal.cursor, offset, format!("`{expected}`"));
        if completion
            .prefix
            .to_str()
            .is_some_and(|prefix| expected.starts_with(prefix))
            && completion
                .suffix
                .to_str()
                .is_some_and(|suffix| expected.ends_with(suffix))
        {
            journal.candidates.push(CompletionEvidence::new(
                Frontier {
                    argument: journal.cursor,
                    token_offset: offset,
                },
                expected.into(),
                CompletionKind::Keyword,
                true,
            ));
        }
        return Ok(RequestResult::Fail);
    }
    let Some(argument) = snapshot.invocation.args.get(journal.cursor) else {
        record_expectation(journal, journal.cursor, 0, format!("`{expected}`"));
        return Ok(RequestResult::Fail);
    };
    if argument.to_str() != Some(expected.as_str()) {
        let matched = argument
            .to_str()
            .map(|argument| common_prefix_bytes(argument, &expected))
            .unwrap_or(0);
        record_expectation(journal, journal.cursor, matched, format!("`{expected}`"));
        return Ok(RequestResult::Fail);
    }
    journal.cursor += 1;
    Ok(RequestResult::ReturnUnit)
}

fn read_text(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let [expectation]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.read.text` received the wrong number of arguments"))?;
    let expectation = text_value(context, expectation, "`.read.text` expectation")?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI reader escaped its isolated search transaction"))?;
    let (snapshot, journal) = transaction.parts();
    if let Some(completion) = snapshot
        .invocation
        .completion
        .as_ref()
        .filter(|completion| completion.argument == journal.cursor)
    {
        record_expectation(
            journal,
            journal.cursor,
            completion.prefix.as_encoded_bytes().len(),
            expectation,
        );
        return Ok(RequestResult::Fail);
    }
    let Some(argument) = snapshot.invocation.args.get(journal.cursor) else {
        record_expectation(journal, journal.cursor, 0, expectation);
        return Ok(RequestResult::Fail);
    };
    let Some(argument) = argument.to_str() else {
        record_expectation(journal, journal.cursor, 0, expectation);
        return Ok(RequestResult::Fail);
    };
    journal.cursor += 1;
    Ok(RequestResult::Return(Value::text(argument)))
}

fn read_token(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let [expectation, parser]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.read.token` received the wrong number of arguments"))?;
    let expectation = text_value(context, expectation, "`.read.token` expectation")?;
    let eval_context = context.eval_context().clone();
    let (argument_index, argument, completion_offset) = {
        let mut transaction = context
            .transaction()
            .ok_or_else(|| TaskError::new("CLI reader escaped its isolated search transaction"))?;
        let (snapshot, journal) = transaction.parts();
        let argument_index = journal.cursor;
        let Some(argument) = snapshot.invocation.args.get(argument_index) else {
            record_expectation(journal, argument_index, 0, expectation);
            return Ok(RequestResult::Fail);
        };
        let Some(argument) = argument.to_str() else {
            record_expectation(journal, argument_index, 0, expectation);
            return Ok(RequestResult::Fail);
        };
        let completion_offset = snapshot
            .invocation
            .completion
            .as_ref()
            .filter(|completion| completion.argument == argument_index)
            .and_then(|completion| completion.prefix.to_str().map(str::len));
        (
            argument_index,
            Arc::<str>::from(argument),
            completion_offset,
        )
    };

    let result = token::run(&parser, argument, completion_offset, eval_context.clone())
        .map_err(TaskError::new)?;
    let completion_candidates = if completion_offset.is_some() {
        result
            .candidates
            .iter()
            .filter_map(|candidate| {
                let verification = token::run(
                    &parser,
                    Arc::from(candidate.replacement.as_str()),
                    None,
                    eval_context.clone(),
                )
                .ok()?;
                let complete_reader = !verification.values.is_empty();
                (complete_reader || verification.furthest == candidate.replacement.len())
                    .then(|| (candidate.clone(), complete_reader))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI reader escaped its isolated search transaction"))?;
    let (_, journal) = transaction.parts();
    if completion_offset.is_some() {
        for (candidate, complete_reader) in completion_candidates {
            journal
                .candidates
                .push(super::completion::CompletionEvidence::new(
                    super::completion::Frontier {
                        argument: argument_index,
                        token_offset: candidate.offset,
                    },
                    candidate.replacement.into(),
                    super::completion::CompletionKind::Value,
                    complete_reader,
                ));
        }
        let labels = if result.expectations.is_empty() {
            vec![expectation]
        } else {
            result
                .expectations
                .into_iter()
                .map(|item| item.label)
                .collect()
        };
        for label in labels {
            record_expectation(journal, argument_index, result.furthest, label);
        }
        return Ok(RequestResult::Fail);
    }
    if result.values.is_empty() {
        if result.expectations.is_empty() {
            record_expectation(journal, argument_index, result.furthest, expectation);
        } else {
            for item in result.expectations {
                record_expectation(journal, argument_index, item.offset, item.label);
            }
        }
        return Ok(RequestResult::Fail);
    }
    journal.cursor += 1;
    Ok(RequestResult::Alternatives(result.values))
}

fn read_end(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let []: [Value; 0] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.read.end` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI reader escaped its isolated search transaction"))?;
    let (snapshot, journal) = transaction.parts();
    if journal.cursor == snapshot.invocation.args.len() {
        Ok(RequestResult::ReturnUnit)
    } else {
        record_expectation(journal, journal.cursor, 0, "end of command");
        Ok(RequestResult::Fail)
    }
}

struct PathHandle {
    invocation: u64,
    argument: usize,
    kind: PathKind,
    access: PathAccess,
}

fn read_path(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let [kind, access]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.read.path` received the wrong number of arguments"))?;
    let kind = match atom_name(context, kind, &["file", "folder", "any"], "path kind")? {
        "file" => PathKind::File,
        "folder" => PathKind::Folder,
        "any" => PathKind::Any,
        _ => unreachable!(),
    };
    let access = match atom_name(context, access, &["r", "w"], "path access")? {
        "r" => PathAccess::Read,
        "w" => PathAccess::Write,
        _ => unreachable!(),
    };
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI reader escaped its isolated search transaction"))?;
    let (snapshot, journal) = transaction.parts();
    let argument_index = journal.cursor;
    let expectation = path::expectation(kind, access);
    if let Some(completion) = snapshot
        .invocation
        .completion
        .as_ref()
        .filter(|completion| completion.argument == argument_index)
    {
        let frontier = Frontier {
            argument: argument_index,
            token_offset: completion.prefix.as_encoded_bytes().len(),
        };
        record_expectation(journal, argument_index, frontier.token_offset, expectation);
        for (replacement, candidate_kind, complete_reader) in
            path::completions(&completion.prefix, &completion.suffix, kind, access)
        {
            journal.candidates.push(CompletionEvidence::new(
                frontier,
                replacement,
                candidate_kind,
                complete_reader,
            ));
        }
        return Ok(RequestResult::Fail);
    }
    let Some(argument) = snapshot.invocation.args.get(argument_index) else {
        record_expectation(journal, argument_index, 0, expectation);
        return Ok(RequestResult::Fail);
    };
    let path = PathBuf::from(argument);
    if !path::matches(&path, kind, access) {
        record_expectation(journal, argument_index, 0, expectation);
        return Ok(RequestResult::Fail);
    }
    journal.cursor += 1;
    Ok(RequestResult::Return(Value::from_core(CoreValue::Opaque(
        OpaqueValue::new(Arc::new(PathHandle {
            invocation: snapshot.invocation.id,
            argument: argument_index,
            kind,
            access,
        })),
    ))))
}

enum PathWriter {
    File,
    Manifest,
}

fn write_path(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
    writer: PathWriter,
) -> Result<RequestResult, TaskError> {
    let [handle]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("CLI path writer received the wrong number of arguments"))?;
    let CoreValue::Opaque(handle) = evaluated(context, handle)? else {
        return Err(TaskError::new("CLI path writer requires a path handle"));
    };
    let handle = handle
        .downcast::<PathHandle>()
        .ok_or_else(|| TaskError::new("CLI path writer requires a path handle"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI writer escaped its isolated search transaction"))?;
    let (snapshot, journal) = transaction.parts();
    if handle.invocation != snapshot.invocation.id {
        return Err(TaskError::new(
            "CLI path handle belongs to another invocation",
        ));
    }
    let path = snapshot
        .invocation
        .args
        .get(handle.argument)
        .map(PathBuf::from)
        .ok_or_else(|| TaskError::new("CLI path handle refers to an invalid argument"))?;
    let edit = match writer {
        PathWriter::File if handle.kind == PathKind::File && handle.access == PathAccess::Read => {
            CommandEdit::Input(ModuleInput::file(path))
        }
        PathWriter::Manifest
            if handle.kind == PathKind::File && handle.access == PathAccess::Write =>
        {
            CommandEdit::Manifest(path)
        }
        PathWriter::File => {
            return Err(TaskError::new(
                "`.write.file` requires a readable file path handle",
            ));
        }
        PathWriter::Manifest => {
            return Err(TaskError::new(
                "`.write.manifest` requires a writable file path handle",
            ));
        }
    };
    journal.edits.push(edit);
    Ok(RequestResult::ReturnUnit)
}

fn write_script(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let [extension, body]: [Value; 2] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.write.script` received the wrong number of arguments"))?;
    let extension = text_value(context, extension, "`.write.script` extension")?;
    if extension.is_empty() {
        return Err(TaskError::new(
            "`.write.script` requires a nonempty extension",
        ));
    }
    let body = text_value(context, body, "`.write.script` body")?;
    push_edit(
        context,
        CommandEdit::Input(ModuleInput::script(extension, body)),
    )?;
    Ok(RequestResult::ReturnUnit)
}

enum TextWriter {
    Reflection,
    Assembly,
}

fn write_text_argument(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
    writer: TextWriter,
) -> Result<RequestResult, TaskError> {
    let [argument]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new("CLI argument writer received the wrong number of arguments")
    })?;
    let argument = text_value(context, argument, "CLI argument writer")?;
    let edit = match writer {
        TextWriter::Reflection => CommandEdit::ReflectionArgument(argument.into()),
        TextWriter::Assembly => CommandEdit::AssemblyArgument(argument.into()),
    };
    push_edit(context, edit)?;
    Ok(RequestResult::ReturnUnit)
}

fn write_worker_count(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let [count]: [Value; 1] = arguments.try_into().map_err(|_| {
        TaskError::new("`.write.worker_count` received the wrong number of arguments")
    })?;
    let CoreValue::Number(count) = evaluated(context, count)? else {
        return Err(TaskError::new(
            "`.write.worker_count` requires a non-negative integer",
        ));
    };
    let count = count
        .to_u64_if_integer()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| {
            TaskError::new("`.write.worker_count` requires a supported non-negative integer")
        })?;
    push_edit(context, CommandEdit::WorkerCount(count))?;
    Ok(RequestResult::ReturnUnit)
}

fn push_edit(
    context: &mut RequestContext<'_, CliEffects>,
    edit: CommandEdit,
) -> Result<(), TaskError> {
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI writer escaped its isolated search transaction"))?;
    transaction.parts().1.edits.push(edit);
    Ok(())
}

fn text_value(
    context: &RequestContext<'_, CliEffects>,
    value: Value,
    request: &str,
) -> Result<String, TaskError> {
    let CoreValue::Binary(bytes) = evaluated(context, value)? else {
        return Err(TaskError::new(format!("{request} requires text")));
    };
    String::from_utf8(bytes.to_vec())
        .map_err(|_| TaskError::new(format!("{request} requires UTF-8 text")))
}

fn atom_name<'a>(
    context: &RequestContext<'_, CliEffects>,
    value: Value,
    accepted: &'a [&str],
    kind: &str,
) -> Result<&'a str, TaskError> {
    let value = evaluated(context, value)?;
    accepted
        .iter()
        .copied()
        .find(|name| value == CoreValue::Atom(Atom::from_key(&Key::binary_from_text(name))))
        .ok_or_else(|| TaskError::new(format!("invalid CLI {kind}")))
}

fn evaluated(
    context: &RequestContext<'_, CliEffects>,
    value: Value,
) -> Result<CoreValue, TaskError> {
    eval::eval_value(context.eval_context(), value.as_core())
        .map_err(|error| TaskError::new(error.to_string()))
}

fn record_expectation(
    journal: &mut CliJournal,
    argument: usize,
    token_offset: usize,
    label: impl Into<String>,
) {
    journal.expectations.push(ExpectationEvidence {
        frontier: Frontier {
            argument,
            token_offset,
        },
        label: label.into(),
    });
}

fn common_prefix_bytes(left: &str, right: &str) -> usize {
    left.char_indices()
        .zip(right.chars())
        .take_while(|((_, left), right)| left == right)
        .map(|((offset, character), _)| offset + character.len_utf8())
        .last()
        .unwrap_or(0)
}

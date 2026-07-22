use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::api::{ModuleInput, Value};
use crate::core::{Atom, Key, OpaqueValue, Value as CoreValue};
use crate::eval;
use crate::reflection::{
    EffectRequestSpec, ReflectionRequest, RequestContext, RequestResult, TaskError,
    TaskSpecialization, environment_log_request_specs, handle_reflection_request,
};

use super::host::{CliHost, CliJournal};
use super::model::CommandEdit;

#[derive(Clone, Copy)]
pub(super) struct CliEffects;

#[derive(Clone)]
pub(super) enum CliRequest {
    Reflection(ReflectionRequest),
    ReadKeyword,
    ReadText,
    ReadPath,
    ReadEnd,
    WriteFile,
    WriteScript,
    WriteManifest,
    WriteReflectionArgument,
    WriteAssemblyArgument,
    WriteWorkerCount,
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
    let Some(argument) = snapshot.invocation.args.get(journal.cursor) else {
        return Ok(RequestResult::Fail);
    };
    if argument.to_str() != Some(expected.as_str()) {
        return Ok(RequestResult::Fail);
    }
    journal.cursor += 1;
    Ok(RequestResult::ReturnUnit)
}

fn read_text(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, CliEffects>,
) -> Result<RequestResult, TaskError> {
    let [_expectation]: [Value; 1] = arguments
        .try_into()
        .map_err(|_| TaskError::new("`.read.text` received the wrong number of arguments"))?;
    let mut transaction = context
        .transaction()
        .ok_or_else(|| TaskError::new("CLI reader escaped its isolated search transaction"))?;
    let (snapshot, journal) = transaction.parts();
    let Some(argument) = snapshot.invocation.args.get(journal.cursor) else {
        return Ok(RequestResult::Fail);
    };
    let Some(argument) = argument.to_str() else {
        return Ok(RequestResult::Fail);
    };
    journal.cursor += 1;
    Ok(RequestResult::Return(Value::text(argument)))
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
        Ok(RequestResult::Fail)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathKind {
    File,
    Folder,
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathAccess {
    Read,
    Write,
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
    let Some(argument) = snapshot.invocation.args.get(argument_index) else {
        return Ok(RequestResult::Fail);
    };
    let path = PathBuf::from(argument);
    if !path_matches(&path, kind, access) {
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

fn path_matches(path: &Path, kind: PathKind, access: PathAccess) -> bool {
    match (std::fs::metadata(path), access) {
        (Ok(metadata), _) => match kind {
            PathKind::File => metadata.is_file(),
            PathKind::Folder => metadata.is_dir(),
            PathKind::Any => true,
        },
        (Err(_), PathAccess::Read) => false,
        (Err(_), PathAccess::Write) => path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .is_dir(),
    }
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

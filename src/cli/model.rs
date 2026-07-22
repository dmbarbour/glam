use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ModuleInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    message: String,
}

impl CliError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArguments {
    args: Arc<[OsString]>,
}

impl CliArguments {
    pub(super) fn new(args: Arc<[OsString]>) -> Self {
        Self { args }
    }

    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    pub(super) fn shared_args(&self) -> Arc<[OsString]> {
        self.args.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseVerbosity {
    Quiet,
    Normal,
    Verbose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopLevelCommand {
    Help,
    Version,
    InspectGSource {
        path: PathBuf,
        verbosity: ParseVerbosity,
    },
    CheckManifest {
        path: PathBuf,
        quiet: bool,
    },
    Assembly(CommandPlan),
    ConfiguredCli(CliArguments),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    inputs: Vec<ModuleInput>,
    assembly_args: Vec<OsString>,
    reflection_args: Vec<OsString>,
    manifest: Option<PathBuf>,
    worker_count: Option<usize>,
    process_args: Arc<[OsString]>,
    cli_arguments: CliArguments,
}

impl CommandPlan {
    pub fn cli_arguments(&self) -> &CliArguments {
        &self.cli_arguments
    }

    pub fn process_args(&self) -> &[OsString] {
        &self.process_args
    }

    pub fn into_parts(self) -> CommandPlanParts {
        CommandPlanParts {
            inputs: self.inputs,
            assembly_args: self.assembly_args,
            reflection_args: self.reflection_args,
            manifest: self.manifest,
            worker_count: self.worker_count,
            process_args: self.process_args,
            cli_arguments: self.cli_arguments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlanParts {
    pub inputs: Vec<ModuleInput>,
    pub assembly_args: Vec<OsString>,
    pub reflection_args: Vec<OsString>,
    pub manifest: Option<PathBuf>,
    pub worker_count: Option<usize>,
    pub process_args: Arc<[OsString]>,
    pub cli_arguments: CliArguments,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CommandEdit {
    Input(ModuleInput),
    AssemblyArgument(OsString),
    ReflectionArgument(OsString),
    Manifest(PathBuf),
    WorkerCount(usize),
}

#[derive(Default)]
pub(super) struct CommandPlanBuilder {
    edits: Vec<CommandEdit>,
    has_manifest: bool,
    has_worker_count: bool,
}

impl CommandPlanBuilder {
    pub(super) fn push(&mut self, edit: CommandEdit) -> Result<(), CliError> {
        match &edit {
            CommandEdit::Manifest(_) if self.has_manifest => {
                return Err(CliError::new("`--manifest` may be specified only once"));
            }
            CommandEdit::Manifest(_) => self.has_manifest = true,
            CommandEdit::WorkerCount(_) if self.has_worker_count => {
                return Err(CliError::new("`--workers` may be specified only once"));
            }
            CommandEdit::WorkerCount(_) => self.has_worker_count = true,
            _ => {}
        }
        self.edits.push(edit);
        Ok(())
    }

    pub(super) fn finish(self, cli_arguments: CliArguments) -> Result<CommandPlan, CliError> {
        let mut inputs = Vec::new();
        let mut assembly_args = Vec::new();
        let mut reflection_args = Vec::new();
        let mut manifest = None;
        let mut worker_count = None;

        for edit in self.edits {
            match edit {
                CommandEdit::Input(input) => inputs.push(input),
                CommandEdit::AssemblyArgument(argument) => assembly_args.push(argument),
                CommandEdit::ReflectionArgument(argument) => reflection_args.push(argument),
                CommandEdit::Manifest(path) => manifest = Some(path),
                CommandEdit::WorkerCount(count) => worker_count = Some(count),
            }
        }
        if inputs.is_empty() {
            return Err(CliError::new(
                "assembly needs at least one `--file` or `--script.<ext>` input",
            ));
        }

        let process_args = cli_arguments.shared_args();
        Ok(CommandPlan {
            inputs,
            assembly_args,
            reflection_args,
            manifest,
            worker_count,
            process_args,
            cli_arguments,
        })
    }
}

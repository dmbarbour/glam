use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::{Diagnostic, ModuleInput, Severity, Value};

use super::completion::{CliCaseExplanation, CompletionRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    message: String,
    diagnostics: Vec<Diagnostic>,
    explanations: Vec<CliCaseExplanation>,
}

impl CliError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            diagnostics: Vec::new(),
            explanations: Vec::new(),
        }
    }

    pub(super) fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub(super) fn with_explanations(
        mut self,
        explanations: impl IntoIterator<Item = crate::Value>,
    ) -> Self {
        self.explanations = explanations
            .into_iter()
            .map(CliCaseExplanation::new)
            .collect();
        self
    }

    pub fn explanations(&self) -> &[CliCaseExplanation] {
        &self.explanations
    }

    /// Projects this CLI failure into a rich diagnostic while retaining the
    /// original `.case` values for configured loggers and IDE clients.
    pub fn diagnostic(&self) -> Diagnostic {
        let mut entries = vec![("msg", Value::record([("text", Value::text(&self.message))]))];
        if !self.explanations.is_empty() {
            entries.push((
                "cli",
                Value::record([(
                    "cases",
                    Value::list(
                        self.explanations
                            .iter()
                            .map(|explanation| explanation.value().clone()),
                    ),
                )]),
            ));
        }
        Diagnostic::from_emission(Severity::Error, Value::record(entries))
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
    pub fn from_args(arguments: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            args: arguments.into_iter().collect(),
        }
    }

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
    InspectConfiguredCli {
        arguments: CliArguments,
        nul_terminated: bool,
    },
    Complete(CompletionRequest),
    CompletionScript {
        name: OsString,
        cli_arguments: CliArguments,
    },
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

    pub fn reflection_args(&self) -> &[OsString] {
        &self.reflection_args
    }

    pub fn manifest(&self) -> Option<&std::path::Path> {
        self.manifest.as_deref()
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

#[derive(Debug, Clone)]
pub struct CliExpansion {
    plan: CommandPlan,
    diagnostics: Vec<Diagnostic>,
}

impl CliExpansion {
    pub(super) fn new(plan: CommandPlan, diagnostics: Vec<Diagnostic>) -> Self {
        Self { plan, diagnostics }
    }

    pub fn plan(&self) -> &CommandPlan {
        &self.plan
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn into_parts(self) -> (CommandPlan, Vec<Diagnostic>) {
        (self.plan, self.diagnostics)
    }
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

struct CommandFields {
    inputs: Vec<ModuleInput>,
    assembly_args: Vec<OsString>,
    reflection_args: Vec<OsString>,
    manifest: Option<PathBuf>,
    worker_count: Option<usize>,
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
        let process_args = cli_arguments.shared_args();
        self.finish_with_process_args(cli_arguments, process_args)
    }

    pub(super) fn finish_configured(
        self,
        cli_arguments: CliArguments,
    ) -> Result<CommandPlan, CliError> {
        let CommandFields {
            inputs,
            assembly_args,
            reflection_args,
            manifest,
            worker_count,
        } = self.finalize_fields()?;
        let process_args = canonical_arguments(
            &inputs,
            &assembly_args,
            &reflection_args,
            manifest.as_deref(),
            worker_count,
        );
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

    fn finish_with_process_args(
        self,
        cli_arguments: CliArguments,
        process_args: Arc<[OsString]>,
    ) -> Result<CommandPlan, CliError> {
        let CommandFields {
            inputs,
            assembly_args,
            reflection_args,
            manifest,
            worker_count,
        } = self.finalize_fields()?;
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

    fn finalize_fields(self) -> Result<CommandFields, CliError> {
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

        Ok(CommandFields {
            inputs,
            assembly_args,
            reflection_args,
            manifest,
            worker_count,
        })
    }
}

fn canonical_arguments(
    inputs: &[ModuleInput],
    assembly_args: &[OsString],
    reflection_args: &[OsString],
    manifest: Option<&std::path::Path>,
    worker_count: Option<usize>,
) -> Arc<[OsString]> {
    let mut arguments = Vec::new();
    for input in inputs {
        match input {
            ModuleInput::File(path) => {
                arguments.push(OsString::from("--file"));
                arguments.push(path.as_os_str().to_owned());
            }
            ModuleInput::Script { extension, body } => {
                arguments.push(OsString::from(format!("--script.{extension}")));
                arguments.push(OsString::from(
                    String::from_utf8(body.to_vec())
                        .expect("configured script bodies originate as UTF-8 text"),
                ));
            }
        }
    }
    if let Some(manifest) = manifest {
        arguments.push(OsString::from("--manifest"));
        arguments.push(manifest.as_os_str().to_owned());
    }
    for argument in reflection_args {
        arguments.push(OsString::from("--refl"));
        arguments.push(argument.clone());
    }
    if let Some(worker_count) = worker_count {
        arguments.push(OsString::from("--workers"));
        arguments.push(OsString::from(worker_count.to_string()));
    }
    if !assembly_args.is_empty() {
        arguments.push(OsString::from("--"));
        arguments.extend(assembly_args.iter().cloned());
    }
    Arc::from(arguments)
}

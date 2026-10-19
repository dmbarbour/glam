pub(super) mod logger;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use glam::{
    Assembler, Error, EvaluationRuntime, FileSourceSystem, Machine, ModuleInput, Runtime, Value,
    ValueKind, Values,
};

use crate::command_line::{CliArguments, CommandPlanParts};

/// The machine on which the CLI takes its turns. Its state holds the
/// `prelude`, the `configuration` and the `environment` it selects.
pub(super) const CLI: &str = "cli";

/// The CLI's policy, in glam.
const PRELUDE: &str = include_str!("prelude.g");

pub(super) struct PreparedAssembly {
    pub(super) local_files: FileSourceSystem,
    /// The CLI's runtime, which the main thread opens and drops. Its
    /// diagnostics wait at its `logger` machine.
    pub(super) runtime: Runtime,
    /// Whether `process.args` and `process.refl_args` still wait on the
    /// command's expansion.
    environment_pending: bool,
}

impl PreparedAssembly {
    /// The runtime's assembler, for what has not moved to turns.
    pub(super) fn assembler(&self) -> &Assembler {
        glam::transition::assembler(&self.runtime)
    }

    /// The configuration module, for what has not moved to turns.
    pub(super) fn configuration(&mut self) -> Value {
        self.runtime
            .run(CLI, |m| {
                m.get("configuration")?;
                glam::transition::pop_value(m)
            })
            .expect("a prepared assembly's machine holds its configuration")
    }

    pub(super) fn resolve_environment(&mut self, command: &CommandPlanParts) -> Result<(), Error> {
        if !std::mem::take(&mut self.environment_pending) {
            return Ok(());
        }
        self.runtime.run((), |root| {
            fulfill_process(root, &command.process_args, &command.reflection_args)
        })
    }

    /// Breaks the promises of `process.args` and `process.refl_args` with
    /// `message`, where the command's expansion failed.
    pub(super) fn fail_environment(&mut self, message: &str) {
        if !std::mem::take(&mut self.environment_pending) {
            return;
        }
        let _ = self.runtime.run((), |root| {
            for resolver in ["args_resolver", "refl_args_resolver"] {
                root.get(resolver)?;
                root.push_text(message);
                root.break_promise()?;
            }
            Ok(())
        });
    }
}

pub(super) struct PreparationFailure {
    pub(super) local_files: FileSourceSystem,
    pub(super) runtime: Runtime,
    pub(super) error: Error,
}

pub(super) type InitialCliEnvironment = (Arc<[std::ffi::OsString]>, Arc<[std::ffi::OsString]>);

pub(super) fn prepare(
    cli_arguments: CliArguments,
    initial_environment: Option<InitialCliEnvironment>,
) -> Result<PreparedAssembly, Box<PreparationFailure>> {
    let local_files = FileSourceSystem::default();
    let evaluation_runtime =
        EvaluationRuntime::new(0).expect("a dormant evaluation runtime is valid");
    let diagnostics = glam::DiagnosticBus::for_runtime(&evaluation_runtime);
    let assembler = Assembler::builder()
        .source_system(local_files.clone())
        .evaluation_runtime(evaluation_runtime)
        .diagnostic_bus(diagnostics)
        .build()
        .expect("main's assembler configuration must be valid");
    let mut runtime = glam::transition::runtime_around(assembler)
        .expect("main's runtime routes its diagnostics to its root");
    logger::wire_logger(&mut runtime).expect("the logger's wiring is valid");
    if let Err(error) = load(&mut runtime, &cli_arguments, initial_environment.as_ref()) {
        return Err(Box::new(PreparationFailure {
            local_files,
            runtime,
            error,
        }));
    }
    Ok(PreparedAssembly {
        local_files,
        runtime,
        environment_pending: initial_environment.is_none(),
    })
}

/// The process's environment variables, by name, as byte strings.
fn environment_variables(values: &Values) -> Value {
    values
        .dictionary(env::vars_os().map(|(name, value)| {
            (
                values.bytes(name.as_encoded_bytes().to_vec()),
                values.bytes(value.as_encoded_bytes().to_vec()),
            )
        }))
        .expect("OS environment names must be keyable binary values")
}

/// Pushes `arguments` as a list of byte strings.
fn push_arguments(m: &mut Machine<'_, '_>, arguments: &[std::ffi::OsString]) -> Result<(), Error> {
    for argument in arguments {
        m.push_bytes(argument.as_encoded_bytes().to_vec());
    }
    m.make_list(arguments.len())
}

/// Binds the root's share of the environment, which reflection reads:
/// `process`, with the user's arguments at `cli.args`, the variables at
/// `env`, and, at `args` and `refl_args`, promised values that the CLI
/// fulfills once `conf.cli`'s expansion settles them, through the resolvers
/// the root keeps at `args_resolver` and `refl_args_resolver`.
fn bind_process(
    root: &mut Machine<'_, '_>,
    values: &Values,
    cli_arguments: &CliArguments,
) -> Result<(), Error> {
    root.promise();
    root.set("args_resolver")?;
    glam::transition::push_value(root, environment_variables(values))?;
    root.promise();
    root.set("refl_args_resolver")?;
    push_arguments(root, cli_arguments.args())?;
    root.make_dict(["args"])?;
    root.make_dict(["args", "env", "refl_args", "cli"])?;
    root.run(CLI, |m| {
        m.get("prelude")?;
        m.access("process_share")
    })?;
    root.take(CLI, 1)?;
    root.swap()?;
    root.apply(1)?;
    root.set_env_mixin()
}

/// Fulfills the root's promises of `process.args` and `process.refl_args`.
fn fulfill_process(
    root: &mut Machine<'_, '_>,
    args: &[std::ffi::OsString],
    refl_args: &[std::ffi::OsString],
) -> Result<(), Error> {
    root.get("args_resolver")?;
    push_arguments(root, args)?;
    root.fulfill()?;
    root.get("refl_args_resolver")?;
    push_arguments(root, refl_args)?;
    root.fulfill()
}

/// Compiles the prelude on the `cli` machine, binds the root's share of the
/// environment, fulfilling its promises where the arguments are already
/// known, then compiles the configuration and selects the environment.
fn load(
    runtime: &mut Runtime,
    cli_arguments: &CliArguments,
    initial_environment: Option<&InitialCliEnvironment>,
) -> Result<(), Error> {
    let values = glam::transition::assembler(runtime).values();
    runtime.run((), |root| {
        root.run(CLI, |m| {
            m.compile(["glam_cli"], [ModuleInput::script("g", PRELUDE)])?;
            m.set("prelude")
        })?;
        bind_process(root, &values, cli_arguments)?;
        if let Some((args, refl_args)) = initial_environment {
            fulfill_process(root, args, refl_args)?;
        }
        Ok(())
    })?;
    runtime.run(CLI, |m| {
        m.get("prelude")?;
        m.access("configuration_definitions")?;
        m.compile_over(
            ["configuration"],
            configuration_paths().into_iter().map(ModuleInput::file),
        )?;
        m.set("configuration")?;
        m.get("prelude")?;
        m.access("empty_env")?;
        m.get("configuration")?;
        m.access("conf.env")?;
        select_entry(m, "env", "conf.env")?;
        m.eval()?;
        m.set("environment")
    })
}

/// Replaces a fallback and a candidate, the configuration's value at
/// `path`, with the entry the prelude selects: the candidate, or the
/// fallback where it is undefined.
pub(super) fn select_entry(m: &mut Machine<'_, '_>, entry: &str, path: &str) -> Result<(), Error> {
    m.stash(2)?;
    m.get("prelude")?;
    m.access("entry")?;
    m.push_text(entry);
    m.push_text(path);
    m.stash_pop(2)?;
    m.apply(4)
}

/// Whether the top value, evaluated, is the empty list by which a
/// configuration selects nothing. The value stays.
pub(super) fn selects_nothing(m: &mut Machine<'_, '_>) -> Result<bool, Error> {
    if m.kind()? != ValueKind::List {
        return Ok(false);
    }
    m.dup()?;
    let items = m.unpack_list()?;
    m.discard(items)?;
    Ok(items == 0)
}

pub(super) fn entry_context(values: &Values, entry: &str) -> Result<Value, Error> {
    values.record([("conf", values.record([("entry", values.text(entry))])?)])
}

fn configuration_paths() -> Vec<PathBuf> {
    if let Some(paths) = configuration_paths_from_env("GLAM_CONF") {
        return paths;
    }

    if let Some(path) = default_user_configuration_path().filter(|path| path.exists()) {
        return vec![path];
    }

    Vec::new()
}

fn configuration_paths_from_env(name: &str) -> Option<Vec<PathBuf>> {
    env::var_os(name).map(|value| {
        env::split_paths(&value)
            .filter(|path| !path.as_os_str().is_empty())
            .collect()
    })
}

fn default_user_configuration_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("glam").join("conf.g"))
    }

    #[cfg(target_os = "macos")]
    {
        home_dir().map(|path| {
            path.join("Library")
                .join("Application Support")
                .join("glam")
                .join("conf.g")
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".config")))
            .map(|path| path.join("glam").join("conf.g"))
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        None
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    fn assert_configuration_owner_inventory(
        prepared: &PreparedAssembly,
        failure: &PreparationFailure,
    ) {
        let PreparedAssembly {
            local_files: _,
            runtime: _,
            environment_pending: _,
        } = prepared;
        let PreparationFailure {
            local_files: _,
            runtime: _,
            error: _,
        } = failure;
    }

    #[test]
    fn configuration_owner_inventory_is_compile_exhaustive() {
        let _: fn(&PreparedAssembly, &PreparationFailure) = assert_configuration_owner_inventory;
    }
}

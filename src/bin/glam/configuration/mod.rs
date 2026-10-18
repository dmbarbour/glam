pub(super) mod logger;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use glam::{
    Assembler, Error, EvaluationRuntime, FileSourceSystem, Machine, ModuleInput, PromiseResolver,
    Runtime, Value, ValueKind, Values,
};

use crate::command_line::{CliArguments, CommandPlanParts};
use logger::LogHost;

/// The machine on which the CLI takes its turns. Its state holds the
/// `prelude`, the `configuration` and the `environment` it selects.
pub(super) const CLI: &str = "cli";

/// The CLI's policy, in glam.
const PRELUDE: &str = include_str!("prelude.g");

pub(super) struct PreparedAssembly {
    pub(super) local_files: FileSourceSystem,
    /// The CLI's runtime, which the main thread opens and drops.
    pub(super) runtime: Runtime,
    pub(super) log_host: Arc<LogHost>,
    process_args: Option<PromiseResolver>,
    reflection_args: Option<PromiseResolver>,
}

impl PreparedAssembly {
    /// The runtime's assembler, for what has not moved to turns.
    pub(super) fn assembler(&self) -> &Assembler {
        glam::transition::assembler(&self.runtime)
    }

    /// The configuration module, for what has not moved to turns.
    pub(super) fn configuration(&self) -> Value {
        self.runtime
            .run(CLI, |m| {
                m.get("configuration")?;
                glam::transition::pop_value(m)
            })
            .expect("a prepared assembly's machine holds its configuration")
    }

    pub(super) fn resolve_environment(&mut self, command: &CommandPlanParts) -> Result<(), Error> {
        let values = self.assembler().values();
        if let Some(resolver) = self.process_args.take() {
            resolver.resolve(argument_values(&values, &command.process_args))?;
        }
        if let Some(resolver) = self.reflection_args.take() {
            resolver.resolve(argument_values(&values, &command.reflection_args))?;
        }
        Ok(())
    }

    pub(super) fn fail_environment(&mut self, message: &str) {
        if let Some(resolver) = self.process_args.take() {
            let _ = resolver.fail_message(message);
        }
        if let Some(resolver) = self.reflection_args.take() {
            let _ = resolver.fail_message(message);
        }
    }
}

pub(super) struct PreparationFailure {
    pub(super) local_files: FileSourceSystem,
    pub(super) log_host: Arc<LogHost>,
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
    let log_host = Arc::new(LogHost::with_runtime(
        evaluation_runtime.clone(),
        &diagnostics,
    ));
    let mut process_args = None;
    let mut reflection_args = None;
    let assembler = Assembler::builder()
        .source_system(local_files.clone())
        .evaluation_runtime(evaluation_runtime)
        .diagnostic_bus(diagnostics)
        .reflection_environment(|environment| {
            let (process_value, process_resolver) =
                environment.promise("canonical process arguments");
            let (reflection_value, reflection_resolver) =
                environment.promise("canonical reflection arguments");
            process_args = Some(process_resolver);
            reflection_args = Some(reflection_resolver);
            Ok(process_reflection_environment(
                &environment.values(),
                reflection_value,
                process_value,
                cli_arguments,
            ))
        })
        .expect("main's reflection environment must be a dictionary")
        .build()
        .expect("main's assembler configuration must be valid");
    let runtime = glam::transition::runtime_around(assembler);
    let assembler = glam::transition::assembler(&runtime);
    if let Some((process_arguments, reflection_arguments)) = initial_environment {
        let values = assembler.values();
        process_args
            .take()
            .expect("bootstrap process argument resolver should be present")
            .resolve(argument_values(&values, &process_arguments))
            .expect("fresh bootstrap process argument promise should resolve");
        reflection_args
            .take()
            .expect("bootstrap reflection argument resolver should be present")
            .resolve(argument_values(&values, &reflection_arguments))
            .expect("fresh bootstrap reflection argument promise should resolve");
    }
    if let Err(error) = load(&runtime) {
        return Err(Box::new(PreparationFailure {
            local_files,
            log_host,
            runtime,
            error,
        }));
    }
    Ok(PreparedAssembly {
        local_files,
        runtime,
        log_host,
        process_args,
        reflection_args,
    })
}

fn process_reflection_environment(
    values: &Values,
    reflection_arguments: Value,
    process_arguments: Value,
    cli_arguments: CliArguments,
) -> Value {
    fn os_value(values: &Values, value: &std::ffi::OsStr) -> Value {
        values.bytes(value.as_encoded_bytes().to_vec())
    }

    let variables = values
        .dictionary(env::vars_os().map(|(name, value)| {
            (
                values.bytes(name.as_encoded_bytes().to_vec()),
                os_value(values, &value),
            )
        }))
        .expect("OS environment names must be keyable binary values");
    let cli_arguments = values
        .list(
            cli_arguments
                .args()
                .iter()
                .map(|argument| os_value(values, argument)),
        )
        .expect("CLI argument values share one runtime");
    values
        .record([(
            "process",
            values
                .record([
                    ("args", process_arguments),
                    ("env", variables),
                    ("refl_args", reflection_arguments),
                    (
                        "cli",
                        values
                            .record([("args", cli_arguments)])
                            .expect("CLI values share one runtime"),
                    ),
                ])
                .expect("process values share one runtime"),
        )])
        .expect("process environment values share one runtime")
}

fn argument_values(values: &Values, arguments: &[std::ffi::OsString]) -> Value {
    values
        .list(
            arguments
                .iter()
                .map(|argument| values.bytes(argument.as_encoded_bytes().to_vec())),
        )
        .expect("argument values share one runtime")
}

/// Compiles the prelude and the configuration on the `cli` machine, and
/// selects the environment.
fn load(runtime: &Runtime) -> Result<(), Error> {
    runtime.run(CLI, |m| {
        m.compile(["glam_cli"], [ModuleInput::script("g", PRELUDE)])?;
        m.set("prelude")?;
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
pub(super) fn select_entry(m: &mut Machine<'_>, entry: &str, path: &str) -> Result<(), Error> {
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
pub(super) fn selects_nothing(m: &mut Machine<'_>) -> Result<bool, Error> {
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
            log_host: _,
            process_args: _,
            reflection_args: _,
        } = prepared;
        let PreparationFailure {
            local_files: _,
            log_host: _,
            runtime: _,
            error: _,
        } = failure;
    }

    #[test]
    fn configuration_owner_inventory_is_compile_exhaustive() {
        let _: fn(&PreparedAssembly, &PreparationFailure) = assert_configuration_owner_inventory;
    }
}

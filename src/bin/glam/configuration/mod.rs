pub(super) mod logger;

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use glam::{
    Assembler, Error, EvaluationRuntime, FileSourceSystem, ModuleInput, PromiseResolver, Value,
    Values,
};

use crate::command_line::{CliArguments, CommandPlanParts};
use logger::LogHost;

pub(super) struct PreparedAssembly {
    pub(super) local_files: FileSourceSystem,
    pub(super) runtime: EvaluationRuntime,
    pub(super) log_host: Arc<LogHost>,
    pub(super) assembler: Assembler,
    pub(super) configuration: LoadedConfiguration,
    process_args: Option<PromiseResolver>,
    reflection_args: Option<PromiseResolver>,
}

impl PreparedAssembly {
    pub(super) fn resolve_environment(&mut self, command: &CommandPlanParts) -> Result<(), Error> {
        if let Some(resolver) = self.process_args.take() {
            resolver.resolve(argument_values(
                &self.assembler.values(),
                &command.process_args,
            ))?;
        }
        if let Some(resolver) = self.reflection_args.take() {
            resolver.resolve(argument_values(
                &self.assembler.values(),
                &command.reflection_args,
            ))?;
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
    pub(super) assembler: Assembler,
    pub(super) error: Error,
}

pub(super) type InitialCliEnvironment = (Arc<[std::ffi::OsString]>, Arc<[std::ffi::OsString]>);

pub(super) fn prepare(
    cli_arguments: CliArguments,
    initial_environment: Option<InitialCliEnvironment>,
) -> Result<PreparedAssembly, Box<PreparationFailure>> {
    let local_files = FileSourceSystem::default();
    let runtime = EvaluationRuntime::new(0).expect("a dormant evaluation runtime is valid");
    let diagnostics = glam::DiagnosticBus::for_runtime(&runtime);
    let log_host = Arc::new(LogHost::with_runtime(runtime.clone(), &diagnostics));
    let mut process_args = None;
    let mut reflection_args = None;
    let assembler = Assembler::builder()
        .source_system(local_files.clone())
        .evaluation_runtime(runtime.clone())
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
    let configuration = load(&assembler).map_err(|error| {
        Box::new(PreparationFailure {
            local_files: local_files.clone(),
            log_host: log_host.clone(),
            assembler: assembler.clone(),
            error,
        })
    })?;
    Ok(PreparedAssembly {
        local_files,
        runtime,
        log_host,
        assembler,
        configuration,
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

pub(super) struct LoadedConfiguration {
    pub(super) value: Value,
    pub(super) environment: Value,
}

fn load(assembler: &Assembler) -> Result<LoadedConfiguration, Error> {
    let default_environment = empty_environment_object(&assembler.values());
    let values = assembler.values();
    let initial_definitions = values.record([("env", default_environment.clone())])?;
    let module = assembler
        .module(["configuration"])
        .initial_definitions(initial_definitions)
        .inputs(configuration_paths().into_iter().map(ModuleInput::file))
        .build()?;

    let environment = values
        .access_names(module.value(), ["conf", "env"])
        .and_then(|candidate| with_path_lookup_context(&values, candidate, "conf.env"))
        .and_then(|candidate| {
            values.apply(
                &values.defined_or_function(),
                [default_environment, candidate],
            )
        })
        .and_then(|environment| assembler.evaluator().eval(&environment))
        .map(glam::EvaluatedValue::into_value)
        .map_err(|error| {
            error
                .with_context(
                    &values,
                    entry_context(&values, "env").expect("configuration context is local"),
                )
                .expect("configuration context is local")
        })?;
    Ok(LoadedConfiguration {
        value: module.into_value(),
        environment,
    })
}

pub(super) fn entry_context(values: &Values, entry: &str) -> Result<Value, Error> {
    values.record([("conf", values.record([("entry", values.text(entry))])?)])
}

pub(super) fn with_path_lookup_context(
    values: &Values,
    value: Value,
    path: &str,
) -> Result<Value, Error> {
    let frame = values.record([(
        "eval",
        values.record([
            ("op", values.atom_from_text("path_lookup")),
            ("args", values.record([("path", values.text(path))])?),
        ])?,
    )])?;
    values.anno(values.record([("context", frame)])?, value)
}

fn empty_environment_object(values: &Values) -> Value {
    values
        .empty_object(values.abstract_global_path(["configuration", "env"]))
        .expect("empty environment components belong to one runtime")
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
    use glam::{DiagnosticBus, EffectTokenDomain};

    fn assert_configuration_owner_inventory(
        prepared: &PreparedAssembly,
        failure: &PreparationFailure,
        loaded: &LoadedConfiguration,
    ) {
        let PreparedAssembly {
            local_files: _,
            runtime: _,
            log_host: _,
            assembler: _,
            configuration: _,
            process_args: _,
            reflection_args: _,
        } = prepared;
        let PreparationFailure {
            local_files: _,
            log_host: _,
            assembler: _,
            error: _,
        } = failure;
        let LoadedConfiguration { value, environment } = loaded;
        let _: &Value = value;
        let _: &Value = environment;
    }

    #[test]
    fn configuration_owner_inventory_is_compile_exhaustive() {
        let _: fn(&PreparedAssembly, &PreparationFailure, &LoadedConfiguration) =
            assert_configuration_owner_inventory;
    }

    #[test]
    fn prepared_assembly_retires_loaded_configuration_roots_exactly() {
        let runtime = EvaluationRuntime::new(0).expect("runtime should build");
        let diagnostics = DiagnosticBus::for_runtime(&runtime);
        let log_host = Arc::new(LogHost::with_runtime(runtime.clone(), &diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(runtime.clone())
            .diagnostic_bus(diagnostics)
            .build()
            .expect("prepared assembler should build");
        let domain = EffectTokenDomain::new(&runtime.values());
        let value_payload = Arc::new(());
        let retained_value = Arc::downgrade(&value_payload);
        let environment_payload = Arc::new(());
        let retained_environment = Arc::downgrade(&environment_payload);
        let prepared = PreparedAssembly {
            local_files: FileSourceSystem::default(),
            runtime,
            log_host,
            assembler,
            configuration: LoadedConfiguration {
                value: domain.issue(value_payload),
                environment: domain.issue(environment_payload),
            },
            process_args: None,
            reflection_args: None,
        };

        assert!(retained_value.upgrade().is_some());
        assert!(retained_environment.upgrade().is_some());
        drop(prepared);
        assert!(retained_value.upgrade().is_none());
        assert!(retained_environment.upgrade().is_none());
    }
}

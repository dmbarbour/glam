mod batch;
mod command_line;
mod configuration;
mod rendering;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use command_line::{
    CliArguments, CompletionRoute, HELP_TEXT, TopLevelCommand, builtin_completion_script,
    complete_basic, complete_configured, dispatch_bootstrap, format_completion_replacements,
    format_parse_summary, route_completion,
};
use glam::{
    Assembler, Diagnostic, DiagnosticBus, DiagnosticEvent, Error, Severity, Value,
    check_local_manifest, inspect_g_source,
};

use batch::{assemble_inputs, configured_cli, finish_without_logger, prepare_assembly};
use configuration::with_path_lookup_context;

#[cfg(test)]
use batch::{assembly_result_context, finish_local_files, settle_batch_runtime};

#[cfg(test)]
use configuration::logger::{LogHost, LoggerSupervisor, LoggerTaskHost, MainEffects};
#[cfg(test)]
use glam::reflection::{EffectLifecycle, EffectRun, TaskOutcome};
#[cfg(test)]
use glam::{
    FileSourceSystem, RuntimeDeliveryOutcome, RuntimeDispositionKind, RuntimeEventJournal,
    RuntimeReadiness,
};
#[cfg(test)]
use std::thread;

trait DiagnosticBusLocal {
    fn publish_local(&self, diagnostic: Diagnostic) -> DiagnosticEvent;
}

impl DiagnosticBusLocal for DiagnosticBus {
    fn publish_local(&self, diagnostic: Diagnostic) -> DiagnosticEvent {
        self.publish(diagnostic)
            .expect("diagnostic and bus must belong to the same evaluation runtime")
    }
}

fn main() -> ExitCode {
    let command = match dispatch_bootstrap(env::args_os().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    match command {
        TopLevelCommand::Help => {
            print!("{HELP_TEXT}");
            ExitCode::SUCCESS
        }
        TopLevelCommand::Version => {
            println!("glam {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        TopLevelCommand::InspectGSource { path, verbosity } => {
            inspect_g_source_command(&path, verbosity)
        }
        TopLevelCommand::CheckManifest { path, quiet } => check_manifest_command(&path, quiet),
        TopLevelCommand::Assembly(plan) => assemble_inputs(plan),
        TopLevelCommand::ConfiguredCli(arguments) => configured_cli(arguments, None),
        TopLevelCommand::InspectConfiguredCli {
            arguments,
            nul_terminated,
        } => configured_cli(arguments, Some(nul_terminated)),
        TopLevelCommand::Complete(request) => completion_command(request),
        TopLevelCommand::CompletionScript {
            name,
            cli_arguments,
        } => completion_script_command(&name, cli_arguments),
    }
}

fn completion_command(request: command_line::CompletionRequest) -> ExitCode {
    match route_completion(request) {
        CompletionRoute::Basic(request) => write_completion(&complete_basic(&request)).map_or_else(
            |error| {
                eprintln!("error: {error}");
                ExitCode::from(1)
            },
            |()| ExitCode::SUCCESS,
        ),
        CompletionRoute::Configured(request) => configured_completion(request),
    }
}

fn configured_completion(request: command_line::CompletionRequest) -> ExitCode {
    let cli_arguments = CliArguments::from_args(request.arguments().iter().cloned());
    let prepared = match prepare_assembly(cli_arguments, None, None) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let completion =
        match complete_configured(&prepared.assembler, &prepared.configuration.value, request) {
            Ok(completion) => completion,
            Err(error) => {
                for diagnostic in error.diagnostics() {
                    prepared
                        .assembler
                        .diagnostic_bus()
                        .publish_local(diagnostic.clone());
                }
                prepared.assembler.diagnostic_bus().publish_local(
                    error
                        .diagnostic(&prepared.assembler.values())
                        .expect("configured CLI diagnostics belong to its assembler runtime"),
                );
                return finish_without_logger(prepared, None, true);
            }
        };
    for diagnostic in completion.diagnostics() {
        prepared
            .assembler
            .diagnostic_bus()
            .publish_local(diagnostic.clone());
    }
    let failed = write_completion(&completion).is_err_and(|error| {
        prepared
            .assembler
            .diagnostic_bus()
            .publish_local(Diagnostic::new(
                &prepared.assembler.values(),
                Severity::Error,
                error,
            ));
        true
    });
    finish_without_logger(prepared, None, failed)
}

fn write_completion(completion: &command_line::CliCompletion) -> Result<(), String> {
    io::stdout()
        .write_all(&format_completion_replacements(completion))
        .map_err(|error| format!("could not write completions to stdout: {error}"))
}

fn completion_script_command(name: &std::ffi::OsStr, cli_arguments: CliArguments) -> ExitCode {
    let Some(name) = name
        .to_str()
        .filter(|name| !name.is_empty() && !name.contains('.'))
    else {
        eprintln!("error: completion script binding name must be nonempty UTF-8 without `.`");
        return ExitCode::from(2);
    };
    let initial_environment = Some((Arc::from(cli_arguments.args().to_vec()), Arc::from([])));
    let prepared = match prepare_assembly(cli_arguments, None, initial_environment) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let values = prepared.assembler.values();
    let configured = values
        .access_names(
            &prepared.configuration.value,
            ["conf", "completion_script", name],
        )
        .and_then(|candidate| {
            with_path_lookup_context(
                &values,
                candidate,
                &format!("conf.completion_script.{name}"),
            )
        })
        .and_then(|candidate| {
            values.apply(&values.defined_or_function(), [values.list([])?, candidate])
        })
        .and_then(|selected| prepared.assembler.evaluator().eval(&selected))
        .ok()
        .and_then(|selected| {
            selected
                .array_items()
                .filter(Vec::is_empty)
                .map(|_| None)
                .unwrap_or_else(|| Some(selected.into_value()))
        });
    let output: Result<Vec<u8>, String> = match configured {
        Some(function) => configured_completion_script(&prepared.assembler, &function),
        None => builtin_completion_script(name)
            .map(|script| script.as_bytes().to_vec())
            .ok_or_else(|| format!("unknown completion script binding `{name}`")),
    };
    let failed = match output {
        Ok(output) => io::stdout().write_all(&output).is_err_and(|error| {
            prepared
                .assembler
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler.values(),
                    Severity::Error,
                    format!("could not write completion script to stdout: {error}"),
                ));
            true
        }),
        Err(error) => {
            prepared
                .assembler
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler.values(),
                    Severity::Error,
                    error,
                ));
            true
        }
    };
    finish_without_logger(prepared, None, failed)
}

fn configured_completion_script(
    assembler: &Assembler,
    function: &Value,
) -> Result<Vec<u8>, String> {
    let executable = env::args_os().next().unwrap_or_else(|| "glam".into());
    let values = assembler.values();
    let context = values
        .record([
            (
                "executable",
                values.bytes(executable.as_encoded_bytes().to_vec()),
            ),
            ("protocol", values.text("v0")),
            ("request", values.text("--completions")),
        ])
        .expect("completion-script context uses one runtime");
    values
        .apply(function, [context])
        .and_then(|value| values.anno_binary(value))
        .and_then(|binary| assembler.evaluator().eval(&binary))
        .and_then(|binary| {
            binary
                .as_bytes()
                .map(ToOwned::to_owned)
                .ok_or_else(|| Error::new("completion script did not evaluate to binary data"))
        })
        .map_err(|error| error.to_string())
}

fn check_manifest_command(manifest: &Path, quiet: bool) -> ExitCode {
    match check_local_manifest(manifest) {
        Ok(mismatches) if mismatches.is_empty() => ExitCode::SUCCESS,
        Ok(mismatches) => {
            if !quiet {
                let mut stdout = io::stdout().lock();
                for mismatch in mismatches {
                    if writeln!(stdout, "{mismatch}").is_err() {
                        eprintln!("error: could not write manifest check results to stdout");
                        return ExitCode::from(1);
                    }
                }
            }
            ExitCode::from(1)
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

fn inspect_g_source_command(path: &Path, verbosity: command_line::ParseVerbosity) -> ExitCode {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("error: could not read `{}`: {error}", path.display());
            return ExitCode::from(1);
        }
    };
    let parsed = inspect_g_source(&bytes);
    let output = format_parse_summary(path, &parsed, verbosity);
    if io::stdout().write_all(output.as_bytes()).is_err() {
        eprintln!("error: could not write parse inspection to stdout");
        return ExitCode::from(1);
    }

    if parsed.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::SourceSystem;

    trait TestValueFacade {
        fn get(&self, root: &Value, path: &str) -> Result<Value, Error>;
        fn get_evaluated(&self, root: &Value, path: &str) -> Result<glam::EvaluatedValue, Error>;
    }

    impl TestValueFacade for Assembler {
        fn get(&self, root: &Value, path: &str) -> Result<Value, Error> {
            self.get_evaluated(root, path)
                .map(glam::EvaluatedValue::into_value)
        }

        fn get_evaluated(&self, root: &Value, path: &str) -> Result<glam::EvaluatedValue, Error> {
            let value = self.values().access_names(root, path.split('.'))?;
            self.evaluator().eval(&value)
        }
    }

    #[test]
    fn final_local_file_change_is_only_a_warning() {
        let directory =
            env::temp_dir().join(format!("glam-final-file-warning-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let path = directory.join("input.g");
        fs::write(&path, "used").expect("test input should be written");
        let files = FileSourceSystem::default();
        files
            .load_top_level(&path)
            .expect("assembly read should succeed");
        fs::write(&path, "later edit").expect("test input should be changed");
        let diagnostics = DiagnosticBus::new();
        let queue = Arc::new(LogHost::new(&diagnostics));
        let values = queue.runtime.values();

        assert!(!finish_local_files(&files, None, &diagnostics, &values));
        let warning = queue
            .take_diagnostic()
            .expect("final file change should emit a diagnostic");
        assert_eq!(warning.severity(), Severity::Warning);
        assert_eq!(diagnostics.counts().warnings(), 1);
        assert_eq!(diagnostics.counts().errors(), 0);
    }

    #[test]
    fn assembly_result_context_names_the_executable_output_boundary() {
        let assembler = Assembler::default();
        assert_eq!(
            assembler
                .get_evaluated(
                    &assembly_result_context(&assembler.values())
                        .expect("result context should be local"),
                    "asm.result",
                )
                .expect("assembly result context should identify its output")
                .as_bytes(),
            Some(b"asm.result".as_slice())
        );
    }

    #[test]
    fn logger_exit_vote_retries_new_input_before_terminal_fallback() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("logger retry assembler should build");
        let module = assembler
            .module(["logger_exit_retry"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .cut (.alt ",
                    "(.read_log >>= (\\_message -> .r ())) ",
                    "(.exit.success))\n",
                ),
            )
            .build()
            .expect("logger retry fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("logger retry fixture should define its effect");
        let fallback = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fallback_values = fallback.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            fallback_values
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .push(diagnostic.message().to_owned());
        });
        let installation = supervisor.install().expect("logger should install");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule_diagnostic_consumer(&installation.lifecycle, &input.diagnostic_ingress)
        .expect("logger root should enter coordinator work");

        input.runtime.pump_until_stable();
        assert!(
            matches!(input.runtime.readiness(), glam::RuntimeReadiness::Ready(_)),
            "the retryable exit should be ready for settlement before disturbance"
        );
        diagnostics.publish_local(Diagnostic::new(
            &input.runtime.values(),
            Severity::Info,
            "arrived before settlement",
        ));

        assert!(matches!(task.run().unwrap(), TaskOutcome::Complete(_)));
        assert!(matches!(
            installation.lifecycle.status(),
            glam::reflection::EffectLifecycleStatus::Complete(_)
        ));
        assert!(
            fallback
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .is_empty(),
            "the disturbed logger must consume the pre-settlement diagnostic"
        );

        diagnostics.publish_local(Diagnostic::new(
            &input.runtime.values(),
            Severity::Info,
            "after terminalization",
        ));
        supervisor
            .deliver_fallback()
            .expect("terminal route should deliver later diagnostics");
        assert_eq!(
            *fallback
                .lock()
                .expect("fallback collection mutex should not be poisoned"),
            ["after terminalization"]
        );
        supervisor.finish(&installation);
    }

    #[test]
    fn recursive_logger_drains_input_queued_before_its_first_poll() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("prequeued logger assembler should build");
        let module = assembler
            .module(["prequeued_logger"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "object logger as logger_object with\n",
                    "  run = (.cut (.alt ",
                    "(.read_log >>= (\\_message -> .r ())) ",
                    "(.exit.success))) =>> logger_object\n",
                    "  eff = logger_object.run.eff\n",
                    "refl.effect = (.heap.get ['start] >>= ",
                    "(\\start -> (start == 1) =>> .r ())) =>> logger\n",
                    "refl.start = .heap.set ['start] 1\n",
                ),
            )
            .build()
            .expect("prequeued logger fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("prequeued logger fixture should define its effect");
        let start = assembler
            .get(module.value(), "refl.start")
            .expect("prequeued logger fixture should define its release effect");
        let fallback = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fallback_messages = fallback.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            fallback_messages
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .push(diagnostic.message().to_owned());
        });
        let installation = supervisor.install().expect("logger should install");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host.clone(),
        )
        .schedule_diagnostic_consumer(&installation.lifecycle, &input.diagnostic_ingress)
        .expect("recursive logger root should enter coordinator work");
        input.runtime.pump_until_stable();
        assert!(matches!(
            input.runtime.readiness(),
            RuntimeReadiness::Deadlocked(_)
        ));
        assert!(matches!(
            installation.lifecycle.status(),
            glam::reflection::EffectLifecycleStatus::Blocked
        ));

        diagnostics.publish_local(Diagnostic::new(
            &input.runtime.values(),
            Severity::Info,
            "queued before first poll",
        ));
        let (_generation, _store, input_snapshot) = input.task_capability.transaction_snapshot();
        let mut input_probe = RuntimeEventJournal::new(input_snapshot);
        assert!(
            input_probe
                .read(&input.diagnostic_reader)
                .expect("diagnostic probe should match its runtime")
                .is_some(),
            "the barrier must release only after the diagnostic is admitted"
        );
        let release_lifecycle = EffectLifecycle::new(&input.runtime);
        let release = EffectRun::new(&input.runtime, &start, MainEffects::new(assembler), host)
            .schedule(&release_lifecycle)
            .expect("logger release effect should schedule");
        assert!(matches!(release.run().unwrap(), TaskOutcome::Complete(_)));

        input.runtime.pump_until_stable();
        let RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("the recursive logger should drain prequeued input and vote to exit")
        };
        let report = snapshot.settle().expect("logger exit should settle");
        assert!(report.task_failures().is_empty());
        assert!(report.killed_work().is_empty());
        assert!(
            task.run().is_err(),
            "settlement should terminalize the exit vote"
        );
        supervisor.finish(&installation);
        assert_eq!(
            supervisor
                .fallback_and_deliver()
                .expect("terminal logger should drain fallback input"),
            0
        );
        assert!(
            fallback
                .lock()
                .expect("fallback collection mutex should not be poisoned")
                .is_empty()
        );
    }

    #[test]
    fn settled_report_renders_task_and_nonfallback_delivery_failures_once() {
        let source_diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&source_diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("reporting assembler should build");
        let module = assembler
            .module(["settled_failure_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .task.new (.fail) >>= (\\_task -> .r ())\n",
                ),
            )
            .build()
            .expect("failure report fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("failure report fixture should define its effect");
        let rendered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rendered_messages = rendered.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            rendered_messages
                .lock()
                .expect("rendered diagnostic collection should not be poisoned")
                .push(diagnostic);
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");
        let logger_diagnostics = DiagnosticBus::for_runtime(&input.runtime);
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            logger_diagnostics.clone(),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let lifecycle = EffectLifecycle::new(&input.runtime);
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule(&lifecycle)
        .expect("failure-report root should schedule");
        assert!(task.run().is_err());

        let failed_output = input
            .runtime
            .output_endpoint(Ok::<Value, Error>, |_value| {
                Err(Error::new("nonfallback output adapter failed"))
            })
            .expect("failed output endpoint should register");
        let (_generation, store, snapshot) = input.task_capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events
            .write(&failed_output.writer(), input.runtime.values().integer(1))
            .expect("failed output intent should buffer");
        assert_eq!(
            input
                .task_capability
                .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events,),
            glam::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(
            failed_output.delivery().deliver_next().unwrap(),
            Some(RuntimeDeliveryOutcome::Failed(_))
        ));

        input.runtime.pump_until_stable();
        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("terminal failures should be retained state, not active work")
        };
        let mut report = snapshot.settle().expect("failure report should settle");
        assert_eq!(report.task_failures().len(), 1);
        assert_eq!(report.delivery_failures().failures().len(), 1);
        assert_eq!(source_diagnostics.counts().total(), 0);
        assert_eq!(logger_diagnostics.counts().total(), 0);

        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 2);
        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 0);
        let rendered = rendered
            .lock()
            .expect("rendered diagnostic collection should not be poisoned");
        assert_eq!(rendered.len(), 2);
        assert!(
            rendered
                .iter()
                .any(|diagnostic| diagnostic.message().contains("reflection task failed"))
        );
        assert!(rendered.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("nonfallback output adapter failed")
        }));
        assert!(
            rendered
                .iter()
                .all(|diagnostic| { assembler.get(diagnostic.emission(), "msg.context").is_ok() })
        );
        assert_eq!(source_diagnostics.counts().total(), 0);
        assert_eq!(logger_diagnostics.counts().total(), 0);
    }

    #[test]
    fn settled_report_renders_exit_errors_and_killed_work_once() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("settlement-report assembler should build");
        let module = assembler
            .module(["settled_disposition_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.exit = .exit.error {msg:{text:\"settled exit failure\"}}\n",
                    "refl.blocked = .cut (.read_log)\n",
                ),
            )
            .build()
            .expect("settlement-report fixture should compile");
        let exit = assembler
            .get(module.value(), "refl.exit")
            .expect("fixture should define its exit effect");
        let blocked = assembler
            .get(module.value(), "refl.blocked")
            .expect("fixture should define its blocked effect");
        let rendered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rendered_diagnostics = rendered.clone();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            rendered_diagnostics
                .lock()
                .expect("rendered diagnostic collection should not be poisoned")
                .push(diagnostic);
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let exit_lifecycle = EffectLifecycle::new(&input.runtime);
        let blocked_lifecycle = EffectLifecycle::new(&input.runtime);
        let _exit_task = EffectRun::new(
            &input.runtime,
            &exit,
            MainEffects::new(assembler.clone()),
            host.clone(),
        )
        .schedule(&exit_lifecycle)
        .expect("exit effect should schedule");
        let _blocked_task = EffectRun::new(
            &input.runtime,
            &blocked,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule(&blocked_lifecycle)
        .expect("blocked effect should schedule");

        input.runtime.pump_until_stable();
        let glam::RuntimeReadiness::Deadlocked(snapshot) = input.runtime.readiness() else {
            panic!("an exit vote plus a blocked reader should retain a deadlock")
        };
        let mut report = snapshot
            .kill(glam::RuntimeKillReason::Deadlock)
            .settle()
            .expect("forced deadlock report should settle");
        assert!(report.dispositions().iter().any(|disposition| {
            matches!(disposition.kind(), RuntimeDispositionKind::ExitError(_))
        }));
        let exit_message = report
            .dispositions()
            .iter()
            .find_map(|disposition| match disposition.kind() {
                RuntimeDispositionKind::ExitError(message) => Some(message),
                _ => None,
            })
            .expect("report should retain the exit message");
        assert_eq!(
            assembler
                .get_evaluated(exit_message, "msg.text")
                .expect("exit error should retain its structured message")
                .as_bytes(),
            Some(b"settled exit failure".as_slice())
        );
        assert_eq!(report.killed_work().len(), 1);

        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 2);
        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 0);
        let rendered = rendered
            .lock()
            .expect("rendered diagnostic collection should not be poisoned");
        assert_eq!(rendered.len(), 2);
        assert!(
            rendered
                .iter()
                .any(|diagnostic| diagnostic.message() == "settled exit failure"),
            "rendered messages: {:?}",
            rendered.iter().map(Diagnostic::message).collect::<Vec<_>>()
        );
        assert!(rendered.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("runtime killed reflection_task work")
        }));
    }

    #[test]
    fn settled_report_rendering_drains_work_admitted_by_fallback_delivery() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let assembler = Assembler::builder()
            .evaluation_runtime(input.runtime.clone())
            .build()
            .expect("reentrant reporting assembler should build");
        let module = assembler
            .module(["reentrant_settled_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .task.new (.fail) >>= (\\_task -> .r ())\n",
                ),
            )
            .build()
            .expect("reentrant reporting fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("fixture should define its effect");
        let rendered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let rendered_messages = rendered.clone();
        let republished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_republished = republished.clone();
        let callback_bus = diagnostics.clone();
        let callback_values = input.runtime.values();
        let supervisor = LoggerSupervisor::new(input.clone(), move |diagnostic| {
            rendered_messages
                .lock()
                .expect("rendered diagnostic collection should not be poisoned")
                .push(diagnostic.message().to_owned());
            if !callback_republished.swap(true, std::sync::atomic::Ordering::SeqCst) {
                callback_bus.publish_local(Diagnostic::new(
                    &callback_values,
                    Severity::Info,
                    "admitted during fallback delivery",
                ));
            }
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");
        let host = Arc::new(LoggerTaskHost::new(
            input.clone(),
            DiagnosticBus::for_runtime(&input.runtime),
            assembler.reflection_environment_for_role("logger"),
            assembler.clone(),
        ));
        let lifecycle = EffectLifecycle::new(&input.runtime);
        let task = EffectRun::new(
            &input.runtime,
            &effect,
            MainEffects::new(assembler.clone()),
            host,
        )
        .schedule(&lifecycle)
        .expect("reentrant reporting root should schedule");
        assert!(task.run().is_err());
        input.runtime.pump_until_stable();
        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("retained task failure should be ready")
        };
        let mut report = snapshot
            .settle()
            .expect("task failure report should settle");

        assert_eq!(supervisor.render_settled_report(&mut report).unwrap(), 1);
        assert_eq!(
            *rendered
                .lock()
                .expect("rendered diagnostic collection should not be poisoned"),
            [
                "reflection task failed permanently".to_owned(),
                "admitted during fallback delivery".to_owned(),
            ]
        );
        assert_eq!(diagnostics.counts().total(), 1);
    }

    #[test]
    fn fallback_delivery_failure_is_retained_without_recursive_rendering() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_calls = fallback_calls.clone();
        let supervisor = LoggerSupervisor::new_fallible(input.clone(), move |_diagnostic| {
            callback_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(Error::new("fallback output adapter failed"))
        });
        supervisor
            .fallback_and_deliver()
            .expect("reporting should begin on fallback");

        let failed_output = input
            .runtime
            .output_endpoint(Ok::<Value, Error>, |_value| {
                Err(Error::new("initial output adapter failed"))
            })
            .expect("initial failed output endpoint should register");
        let (_generation, store, snapshot) = input.task_capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events
            .write(&failed_output.writer(), input.runtime.values().integer(1))
            .expect("initial failed output intent should buffer");
        assert_eq!(
            input
                .task_capability
                .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events),
            glam::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(
            failed_output.delivery().deliver_next().unwrap(),
            Some(RuntimeDeliveryOutcome::Failed(_))
        ));
        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("a retained delivery failure should be ready")
        };
        let mut report = snapshot
            .settle()
            .expect("initial delivery failure should settle");

        let error = supervisor
            .render_settled_report(&mut report)
            .expect_err("fallback delivery should report its adapter failure");
        assert_eq!(error.to_string(), "fallback output adapter failed");
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            supervisor.render_settled_report(&mut report).unwrap(),
            0,
            "accepted fallback output must not be replayed after adapter failure"
        );
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let glam::RuntimeReadiness::Ready(snapshot) = input.runtime.readiness() else {
            panic!("failed fallback delivery should remain reportable state")
        };
        let mut repeated = snapshot
            .settle()
            .expect("fallback delivery failure should settle");
        assert_eq!(repeated.delivery_failures().failures().len(), 2);
        assert_eq!(supervisor.render_settled_report(&mut repeated).unwrap(), 0);
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            repeated
                .delivery_failures()
                .failures()
                .iter()
                .any(|failure| failure.endpoint_id() == supervisor.fallback_delivery.id())
        );
    }

    #[test]
    fn batch_settlement_remains_failed_when_fallback_rendering_fails() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let supervisor = LoggerSupervisor::new_fallible(input.clone(), |_diagnostic| {
            Err(Error::new("fallback renderer failed"))
        });
        supervisor
            .fallback_and_deliver()
            .expect("empty fallback route should activate");

        let failed_output = input
            .runtime
            .output_endpoint(Ok::<Value, Error>, |_value| {
                Err(Error::new("authoritative adapter failure"))
            })
            .expect("failed output endpoint should register");
        let (_generation, store, snapshot) = input.task_capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events
            .write(&failed_output.writer(), input.runtime.values().integer(1))
            .expect("failed output intent should buffer");
        assert_eq!(
            input
                .task_capability
                .try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events),
            glam::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(
            failed_output.delivery().deliver_next().unwrap(),
            Some(RuntimeDeliveryOutcome::Failed(_))
        ));

        assert!(settle_batch_runtime(&input.runtime, &supervisor));
        let failures = input.runtime.delivery_failure_snapshot().failures();
        assert_eq!(failures.len(), 2);
        assert!(failures.iter().any(|failure| {
            failure
                .error()
                .to_string()
                .contains("fallback renderer failed")
        }));
    }

    #[test]
    fn diagnostic_publication_racing_ingress_rearm_is_routed_once() {
        let diagnostics = DiagnosticBus::new();
        let input = Arc::new(LogHost::new(&diagnostics));
        let supervisor = LoggerSupervisor::new(input.clone(), |_| {});
        let first = supervisor.install().expect("first logger should install");
        let values = input.runtime.values();
        let publishing_bus = diagnostics.clone();
        let publishing_values = values.clone();
        let (ready, wait) = std::sync::mpsc::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        let publisher = thread::spawn(move || {
            ready.send(()).expect("rearm test should signal readiness");
            resumed
                .recv()
                .expect("rearm test should resume publication");
            publishing_bus.publish_local(Diagnostic::new(
                &publishing_values,
                Severity::Warning,
                "publication during rearm",
            ))
        });
        wait.recv()
            .expect("publisher should reach the rearm barrier");
        supervisor.finish(&first);
        let _second = supervisor.install().expect("logger should rearm");
        input
            .diagnostic_ingress
            .activate()
            .expect("ingress should rearm");
        resume.send(()).expect("publisher should resume");
        let event = publisher.join().expect("publisher should not panic");

        assert_eq!(event.sequence(), diagnostics.counts().latest_sequence());
        assert_eq!(diagnostics.counts().total(), 1);
        assert_eq!(
            input
                .take_diagnostic()
                .expect("racing publication should remain buffered")
                .message(),
            "publication during rearm"
        );
        assert!(input.take_diagnostic().is_none());
    }
}

mod batch;
mod command_line;
mod configuration;
mod profile;
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
    Diagnostic, DiagnosticBus, DiagnosticEvent, Error, Runtime, Severity, check_local_manifest,
    inspect_g_source,
};

use batch::{assemble_inputs, configured_cli, finish_without_logger, prepare_assembly};
use configuration::{CLI, select_entry, selects_nothing};

#[cfg(test)]
use batch::{finish_local_files, settle_batch_runtime};

#[cfg(test)]
use configuration::logger::test_support::{
    hand_over_logger, held_by_logger, logger_runtime, take_from_fallback, take_unread,
};
#[cfg(test)]
use configuration::logger::{LoggerTaskHost, MainEffects, render_settled_report};
#[cfg(test)]
use glam::reflection::{EffectLifecycle, EffectRun, TaskOutcome};
#[cfg(test)]
use glam::{
    EvaluationRuntime, FileSourceSystem, RuntimeDeliveryOutcome, RuntimeDispositionKind,
    RuntimeEventJournal, RuntimeReadiness,
};

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
    profile::start();
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
    let mut prepared = match prepare_assembly(cli_arguments, None, None) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let configuration = prepared.configuration();
    let completion = match complete_configured(prepared.assembler(), &configuration, request) {
        Ok(completion) => completion,
        Err(error) => {
            for diagnostic in error.diagnostics() {
                prepared
                    .assembler()
                    .diagnostic_bus()
                    .publish_local(diagnostic.clone());
            }
            prepared.assembler().diagnostic_bus().publish_local(
                error
                    .diagnostic(&prepared.assembler().values())
                    .expect("configured CLI diagnostics belong to its assembler runtime"),
            );
            return finish_without_logger(prepared, None, true);
        }
    };
    for diagnostic in completion.diagnostics() {
        prepared
            .assembler()
            .diagnostic_bus()
            .publish_local(diagnostic.clone());
    }
    let failed = write_completion(&completion).is_err_and(|error| {
        prepared
            .assembler()
            .diagnostic_bus()
            .publish_local(Diagnostic::new(
                &prepared.assembler().values(),
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
    let mut prepared = match prepare_assembly(cli_arguments, None, initial_environment) {
        Ok(prepared) => prepared,
        Err(exit) => return exit,
    };
    let output = match run_completion_script(&mut prepared.runtime, name) {
        Ok(output) => output,
        Err(error) => {
            let diagnostic = error
                .diagnostic(&prepared.assembler().values())
                .expect("configuration context is local");
            prepared
                .assembler()
                .diagnostic_bus()
                .publish_local(diagnostic);
            return finish_without_logger(prepared, None, true);
        }
    };
    let failed = match output {
        Ok(output) => io::stdout().write_all(&output).is_err_and(|error| {
            prepared
                .assembler()
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler().values(),
                    Severity::Error,
                    format!("could not write completion script to stdout: {error}"),
                ));
            true
        }),
        Err(error) => {
            prepared
                .assembler()
                .diagnostic_bus()
                .publish_local(Diagnostic::new(
                    &prepared.assembler().values(),
                    Severity::Error,
                    error,
                ));
            true
        }
    };
    finish_without_logger(prepared, None, failed)
}

/// Runs the completion script that `conf.completion_script.<name>`
/// selects, or takes the built-in one where it selects nothing. A failure to
/// select is the error; a selected script's failure is the output's.
fn run_completion_script(
    runtime: &mut Runtime,
    name: &str,
) -> Result<Result<Vec<u8>, String>, Error> {
    runtime.run(CLI, |m| {
        m.list_pack(0)?;
        m.local_get("configuration")?;
        select_entry(m, "completion_script", &["conf", "completion_script", name])?;
        // An undefined binding falls back to the built-in script. A
        // configured binding that fails to evaluate is reported, never
        // silently replaced.
        m.eval()?;
        if selects_nothing(m)? {
            m.discard(1)?;
            return Ok(builtin_completion_script(name)
                .map(|script| script.as_bytes().to_vec())
                .ok_or_else(|| format!("unknown completion script binding `{name}`")));
        }
        let executable = env::args_os().next().unwrap_or_else(|| "glam".into());
        m.local_get("prelude")?;
        m.dict_get("completion_request")?;
        m.bytes_push(executable.as_encoded_bytes().to_vec());
        m.apply(1)?;
        m.apply(1)?;
        Ok(m.bytes_pop()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| error.to_string()))
    })
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
    use glam::{Assembler, HandleValue, SourceSystem};
    use rendering::rendered_text_for_test;

    trait TestValueFacade {
        fn get(&self, root: &HandleValue, path: &str) -> Result<HandleValue, Error>;
        fn get_evaluated(
            &self,
            root: &HandleValue,
            path: &str,
        ) -> Result<glam::EvaluatedValue, Error>;
    }

    impl TestValueFacade for Assembler {
        fn get(&self, root: &HandleValue, path: &str) -> Result<HandleValue, Error> {
            self.get_evaluated(root, path)
                .map(glam::EvaluatedValue::into_value)
        }

        fn get_evaluated(
            &self,
            root: &HandleValue,
            path: &str,
        ) -> Result<glam::EvaluatedValue, Error> {
            let value = self.values().access_names(root, path.split('.'))?;
            self.evaluator().eval(&value)
        }
    }

    fn messages(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics.iter().map(Diagnostic::message).collect()
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
        let assembler = Assembler::default();
        let mut runtime = logger_runtime(assembler.clone());
        let diagnostics = assembler.diagnostic_bus();

        assert!(!finish_local_files(
            &files,
            None,
            &diagnostics,
            &assembler.values()
        ));
        let unread = take_unread(&mut runtime);
        assert_eq!(
            unread.len(),
            1,
            "final file change should emit a diagnostic"
        );
        assert_eq!(unread[0].severity(), Severity::Warning);
        assert_eq!(diagnostics.counts().warnings(), 1);
        assert_eq!(diagnostics.counts().errors(), 0);
    }

    /// A configured logger's host over `assembler`'s runtime, whose own
    /// reports go to `diagnostics`.
    fn logger_host(assembler: &Assembler, diagnostics: DiagnosticBus) -> Arc<LoggerTaskHost> {
        Arc::new(LoggerTaskHost::new(
            &assembler.evaluation_runtime(),
            diagnostics,
            assembler.reflection_environment_for_role("logger"),
        ))
    }

    #[test]
    fn logger_exit_vote_retries_new_input_before_its_hand_over() {
        let assembler = Assembler::default();
        let mut runtime = logger_runtime(assembler.clone());
        let evaluation_runtime = assembler.evaluation_runtime();
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
                    "refl.producer = .log 'info {msg:{text:\"arrived before settlement\"}}\n",
                ),
            )
            .build()
            .expect("logger retry fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("logger retry fixture should define its effect");
        let producer = assembler
            .get(module.value(), "refl.producer")
            .expect("logger retry fixture should define its producer");
        // The producer reports on the assembler's bus, which reaches the
        // logger's input, as an independent producer's reports do.
        let host = logger_host(&assembler, assembler.diagnostic_bus());
        let lifecycle = EffectLifecycle::new_closing_session(&evaluation_runtime);
        let task = EffectRun::new(
            &evaluation_runtime,
            &effect,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&lifecycle)
        .expect("logger root should enter coordinator work");

        let report = evaluation_runtime.pump_background(4096);
        assert_eq!(report.state, glam::BackgroundPumpState::Stable);
        assert!(
            matches!(evaluation_runtime.readiness(), RuntimeReadiness::Ready(_)),
            "the retryable exit should be ready for settlement before disturbance"
        );
        let producer_lifecycle = EffectLifecycle::new(&evaluation_runtime);
        let producer_task = EffectRun::new(
            &evaluation_runtime,
            &producer,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&producer_lifecycle)
        .expect("independent diagnostic producer should schedule");
        let report = evaluation_runtime.pump_background(4096);
        assert_eq!(report.state, glam::BackgroundPumpState::Stable);
        assert!(matches!(
            producer_task.run_with(|| host.deliver()).unwrap(),
            TaskOutcome::Complete(_)
        ));

        assert!(matches!(
            task.run_with(|| host.deliver()).unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert!(matches!(
            lifecycle.status(),
            glam::reflection::EffectLifecycleStatus::Complete(_)
        ));
        assert_eq!(
            held_by_logger(&mut runtime),
            0,
            "the disturbed logger must consume the pre-settlement diagnostic"
        );

        hand_over_logger(&mut runtime);
        assembler.diagnostic_bus().publish_local(Diagnostic::new(
            &assembler.values(),
            Severity::Info,
            "after the hand-over",
        ));
        assert_eq!(
            messages(&take_from_fallback(&mut runtime)),
            ["after the hand-over"]
        );
    }

    #[test]
    fn logger_demand_resumes_failed_choice_before_reading_queued_input() {
        let mut resolver = None;
        let assembler = Assembler::builder()
            .evaluation_runtime(EvaluationRuntime::new(0).expect("a runtime should open"))
            .reflection_environment(|environment| {
                let (gate, gate_resolver) = environment.promise("logger stderr gate");
                resolver = Some(gate_resolver);
                environment.values().record([("gate", gate)])
            })
            .expect("logger gate environment should build")
            .build()
            .expect("logger gate assembler should build");
        let mut runtime = logger_runtime(assembler.clone());
        let evaluation_runtime = assembler.evaluation_runtime();
        let module = assembler
            .module(["logger_demand_resume"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .env ['gate] >>= (\\gate -> .cut (.alt ",
                    "(.write_stderr gate =>> .fail) ",
                    "(.read_log >>= (\\_message -> .r ()))))\n",
                ),
            )
            .build()
            .expect("logger demand fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("logger demand fixture should define its effect");
        let host = logger_host(&assembler, DiagnosticBus::for_runtime(&evaluation_runtime));
        let lifecycle = EffectLifecycle::new_closing_session(&evaluation_runtime);
        let task = EffectRun::new(
            &evaluation_runtime,
            &effect,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&lifecycle)
        .expect("logger demand fixture should schedule");

        evaluation_runtime.pump_until_stable();
        assert!(matches!(
            lifecycle.status(),
            glam::reflection::EffectLifecycleStatus::Blocked
        ));
        assembler.diagnostic_bus().publish_local(Diagnostic::new(
            &assembler.values(),
            Severity::Info,
            "queued while stderr demand is blocked",
        ));
        resolver
            .take()
            .expect("logger gate resolver should remain available")
            .resolve(assembler.values().text("ABANDONED STDERR\n"))
            .expect("logger stderr gate should resolve once");

        evaluation_runtime.pump_until_stable();
        assert!(matches!(
            task.run_with(|| host.deliver()).unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert_eq!(
            held_by_logger(&mut runtime),
            0,
            "the resumed fallback branch must consume the queued diagnostic"
        );
    }

    #[test]
    fn recursive_logger_drains_input_queued_before_its_first_poll() {
        let assembler = Assembler::default();
        let mut runtime = logger_runtime(assembler.clone());
        let evaluation_runtime = assembler.evaluation_runtime();
        let module = assembler
            .module(["prequeued_logger"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "object logger with\n  alias logger_object\n",
                    "  run = (.cut (.alt ",
                    "(.read_log >>= (\\_message -> .r ())) ",
                    "(.exit.success))) =>> logger_object\n",
                    "  eff = logger_object.run.eff\n",
                    "refl.effect = (.cut (.heap.get ['start] >>= ",
                    "(\\start -> (start == 1) =>> .r ()))) =>> logger\n",
                    "refl.start = .heap.set ['start] 1\n",
                    "refl.producer = .log 'info {msg:{text:\"queued before first poll\"}}\n",
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
        let producer = assembler
            .get(module.value(), "refl.producer")
            .expect("prequeued logger fixture should define its producer");
        let host = logger_host(&assembler, assembler.diagnostic_bus());
        let lifecycle = EffectLifecycle::new_closing_session(&evaluation_runtime);
        let task = EffectRun::new(
            &evaluation_runtime,
            &effect,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&lifecycle)
        .expect("recursive logger root should enter coordinator work");
        evaluation_runtime.pump_until_stable();
        assert!(matches!(
            evaluation_runtime.readiness(),
            RuntimeReadiness::Deadlocked(_)
        ));
        assert!(matches!(
            lifecycle.status(),
            glam::reflection::EffectLifecycleStatus::Blocked
        ));

        let producer_lifecycle = EffectLifecycle::new(&evaluation_runtime);
        let producer_task = EffectRun::new(
            &evaluation_runtime,
            &producer,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&producer_lifecycle)
        .expect("prequeued diagnostic producer should schedule");
        let report = evaluation_runtime.pump_background(4096);
        assert_eq!(report.state, glam::BackgroundPumpState::Stable);
        assert!(matches!(
            producer_task.run_with(|| host.deliver()).unwrap(),
            TaskOutcome::Complete(_)
        ));
        assert_eq!(
            held_by_logger(&mut runtime),
            1,
            "the barrier must release only after the diagnostic is admitted"
        );
        let release_lifecycle = EffectLifecycle::new(&evaluation_runtime);
        let release = EffectRun::new(
            &evaluation_runtime,
            &start,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&release_lifecycle)
        .expect("logger release effect should schedule");
        assert!(matches!(
            release.run_with(|| host.deliver()).unwrap(),
            TaskOutcome::Complete(_)
        ));

        evaluation_runtime.pump_until_stable();
        let RuntimeReadiness::Ready(snapshot) = evaluation_runtime.readiness() else {
            panic!("the recursive logger should drain prequeued input and vote to exit")
        };
        let report = snapshot.settle().expect("logger exit should settle");
        assert!(report.failed_machines().is_empty());
        assert!(report.killed_work().is_empty());
        assert!(
            task.run_with(|| host.deliver()).is_err(),
            "settlement should terminalize the exit vote"
        );
        assert_eq!(
            held_by_logger(&mut runtime),
            0,
            "the logger read what was queued before its first poll"
        );
    }

    /// Commits a value to an output whose adapter fails, and delivers it,
    /// leaving a delivery failure for settlement to report.
    fn fail_a_delivery(runtime: &EvaluationRuntime, message: &'static str) {
        let failed_output = runtime
            .output_endpoint(Ok::<HandleValue, Error>, move |_value| {
                Err(Error::new(message))
            })
            .expect("failed output endpoint should register");
        let capability = runtime.task_capability();
        let (_generation, store, snapshot) = capability.transaction_snapshot();
        let mut events = RuntimeEventJournal::new(snapshot);
        events
            .write(&failed_output.writer(), runtime.values().integer(1))
            .expect("failed output intent should buffer");
        assert_eq!(
            capability.try_commit_transaction(&glam::reflection::StoreJournal::new(store), &events),
            glam::reflection::StoreCommitResult::Committed
        );
        assert!(matches!(
            failed_output.delivery().deliver_next().unwrap(),
            Some(RuntimeDeliveryOutcome::Failed(_))
        ));
    }

    #[test]
    fn settled_report_renders_machine_and_delivery_failures_once() {
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
        let module = assembler
            .module(["settled_failure_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .r () >>= (\\_ -> seq (anno refl:(.fail) ()) (.r ()))\n",
                ),
            )
            .build()
            .expect("failure report fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("failure report fixture should define its effect");
        let logger_diagnostics = DiagnosticBus::for_runtime(&evaluation_runtime);
        let host = logger_host(&assembler, logger_diagnostics.clone());
        let lifecycle = EffectLifecycle::new(&evaluation_runtime);
        let task = EffectRun::new(
            &evaluation_runtime,
            &effect,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&lifecycle)
        .expect("failure-report root should schedule");
        assert!(task.run_with(|| host.deliver()).is_err());
        fail_a_delivery(&evaluation_runtime, "output adapter failed");

        evaluation_runtime.pump_until_stable();
        let RuntimeReadiness::Ready(snapshot) = evaluation_runtime.readiness() else {
            panic!("terminal failures should be retained state, not active work")
        };
        let mut report = snapshot.settle().expect("failure report should settle");
        assert_eq!(report.failed_machines().len(), 1);
        assert_eq!(report.delivery_failures().failures().len(), 1);

        let values = assembler.values();
        let mut rendered = Vec::new();
        assert_eq!(
            render_settled_report(&mut report, &values, |diagnostic| rendered.push(diagnostic))
                .unwrap(),
            2
        );
        assert_eq!(
            render_settled_report(&mut report, &values, |diagnostic| rendered.push(diagnostic))
                .unwrap(),
            0
        );
        assert_eq!(rendered.len(), 2);
        assert!(rendered.iter().any(|diagnostic| {
            rendered_text_for_test(&assembler, diagnostic).contains("reflection task failed")
        }));
        assert!(
            rendered
                .iter()
                .any(|diagnostic| diagnostic.message().contains("output adapter failed"))
        );
        assert!(
            rendered
                .iter()
                .all(|diagnostic| { assembler.get(diagnostic.emission(), "msg.context").is_ok() })
        );
        assert_eq!(assembler.diagnostic_bus().counts().total(), 0);
        assert_eq!(logger_diagnostics.counts().total(), 0);
    }

    #[test]
    fn settled_report_renders_exit_errors_and_killed_work_once() {
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
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
        let host = logger_host(&assembler, DiagnosticBus::for_runtime(&evaluation_runtime));
        let exit_lifecycle = EffectLifecycle::new(&evaluation_runtime);
        let blocked_lifecycle = EffectLifecycle::new(&evaluation_runtime);
        let _exit_task = EffectRun::new(
            &evaluation_runtime,
            &exit,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&exit_lifecycle)
        .expect("exit effect should schedule");
        let _blocked_task = EffectRun::new(
            &evaluation_runtime,
            &blocked,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&blocked_lifecycle)
        .expect("blocked effect should schedule");

        evaluation_runtime.pump_until_stable();
        let RuntimeReadiness::Deadlocked(snapshot) = evaluation_runtime.readiness() else {
            panic!("an exit vote plus a blocked reader should retain a deadlock")
        };
        let mut report = snapshot
            .kill(glam::RuntimeKillReason::Deadlock)
            .settle()
            .expect("forced deadlock report should settle");
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
                .as_bytes()
                .unwrap()
                .as_deref(),
            Some(b"settled exit failure".as_slice())
        );
        assert_eq!(report.killed_work().len(), 1);

        let values = assembler.values();
        let mut rendered = Vec::new();
        assert_eq!(
            render_settled_report(&mut report, &values, |diagnostic| rendered.push(diagnostic))
                .unwrap(),
            2
        );
        assert_eq!(
            render_settled_report(&mut report, &values, |diagnostic| rendered.push(diagnostic))
                .unwrap(),
            0
        );
        assert_eq!(rendered.len(), 2);
        assert!(
            rendered
                .iter()
                .any(|diagnostic| rendered_text_for_test(&assembler, diagnostic)
                    == "settled exit failure"),
            "rendered messages: {:?}",
            rendered
                .iter()
                .map(|diagnostic| rendered_text_for_test(&assembler, diagnostic))
                .collect::<Vec<_>>()
        );
        assert!(rendered.iter().any(|diagnostic| {
            diagnostic
                .message()
                .contains("runtime killed reflection_task work")
        }));
    }

    #[test]
    fn batch_settlement_renders_what_its_rendering_reports() {
        let assembler = Assembler::default();
        let mut runtime = logger_runtime(assembler.clone());
        hand_over_logger(&mut runtime);
        let evaluation_runtime = assembler.evaluation_runtime();
        let module = assembler
            .module(["reentrant_settled_report"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .r () >>= (\\_ -> seq (anno refl:(.fail) ()) (.r ()))\n",
                ),
            )
            .build()
            .expect("reentrant reporting fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("fixture should define its effect");
        let host = logger_host(&assembler, DiagnosticBus::for_runtime(&evaluation_runtime));
        let lifecycle = EffectLifecycle::new(&evaluation_runtime);
        let task = EffectRun::new(
            &evaluation_runtime,
            &effect,
            MainEffects::new(assembler.values()),
            host.clone(),
        )
        .schedule(&lifecycle)
        .expect("reentrant reporting root should schedule");
        assert!(task.run_with(|| host.deliver()).is_err());

        let rendered = std::cell::RefCell::new(Vec::new());
        let render = |diagnostic: Diagnostic| {
            let first = rendered.borrow().is_empty();
            rendered
                .borrow_mut()
                .push(rendered_text_for_test(&assembler, &diagnostic));
            if first {
                assembler.diagnostic_bus().publish_local(Diagnostic::new(
                    &assembler.values(),
                    Severity::Info,
                    "reported while rendering",
                ));
            }
        };
        let failed = settle_batch_runtime(&evaluation_runtime, &assembler.values(), render, || {
            for diagnostic in take_from_fallback(&mut runtime) {
                render(diagnostic);
            }
            Ok(())
        });

        assert!(failed, "the failed machine fails the batch");
        assert_eq!(
            *rendered.borrow(),
            [
                "reflection task failed permanently".to_owned(),
                "reported while rendering".to_owned(),
            ]
        );
        assert_eq!(assembler.diagnostic_bus().counts().total(), 1);
    }

    #[test]
    fn batch_settlement_fails_on_a_retained_delivery_failure() {
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
        fail_a_delivery(&evaluation_runtime, "authoritative adapter failure");

        assert!(settle_batch_runtime(
            &evaluation_runtime,
            &assembler.values(),
            |_| {},
            || Ok(())
        ));
        assert_eq!(
            evaluation_runtime
                .delivery_failure_snapshot()
                .failures()
                .len(),
            1
        );
    }

    #[test]
    fn configured_batch_settlement_services_managed_maintenance() {
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
        evaluation_runtime
            .request_managed_collection()
            .expect("explicit maintenance should publish");

        assert!(!settle_batch_runtime(
            &evaluation_runtime,
            &assembler.values(),
            |_| {},
            || Ok(())
        ));
        assert!(matches!(
            evaluation_runtime.readiness(),
            RuntimeReadiness::Ready(_)
        ));
    }
}

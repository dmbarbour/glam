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
use batch::{BatchLogger, finish_local_files, quiesce_batch};

#[cfg(test)]
use configuration::logger::test_support::{
    hand_over_logger, held_by_logger, logger_runtime, take_from_fallback, take_unread,
};
#[cfg(test)]
use configuration::logger::{LoggerTaskHost, MainEffects, PublishedIssues};
#[cfg(test)]
use glam::reflection::{EffectLifecycle, EffectRun, TaskOutcome};
#[cfg(test)]
use glam::{
    EvaluationRuntime, FileSourceSystem, RuntimeDeliveryOutcome, RuntimeEventJournal,
    RuntimeIssueFamily, RuntimeReadiness,
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
                    "  run = (.cut (.read_log >>= (\\_message -> .r ()))) =>> logger_object\n",
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
        let stop = task.stopper();
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
        let analysis = evaluation_runtime.analysis().unwrap();
        assert!(
            analysis.quiescent() && analysis.issues().is_empty(),
            "the recursive logger should drain prequeued input and wait on its input, idle"
        );
        assert_eq!(
            held_by_logger(&mut runtime),
            0,
            "the logger read what was queued before its first poll"
        );
        stop.stop();
        assert!(matches!(
            task.run_with(|| host.deliver()),
            Ok(TaskOutcome::Cancelled)
        ));
    }

    /// Commits a value to an output whose adapter fails, and delivers it,
    /// leaving a delivery failure for an analysis to report.
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

    /// A batch's logger that drains with `drain` and runs no configured
    /// logger.
    struct DrainingLogger<'a>(&'a mut dyn FnMut() -> Result<(), Error>);

    impl BatchLogger for DrainingLogger<'_> {
        fn drain(&mut self) -> Result<(), Error> {
            (self.0)()
        }

        fn halt(&mut self, _unread: usize) -> bool {
            false
        }
    }

    #[test]
    fn analysis_issues_render_once_with_their_context() {
        // `quiescence-is-analyzed`: a batch publishes each issue of an
        // analysis once, with a report context.
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
        let module = assembler
            .module(["analyzed_failures"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.effect = .r () >>= (\\_ -> seq (anno refl:(.fail) ()) (.r ()))\n",
                ),
            )
            .build()
            .expect("failure fixture should compile");
        let effect = assembler
            .get(module.value(), "refl.effect")
            .expect("failure fixture should define its effect");
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
        .expect("failure root should schedule");
        assert!(task.run_with(|| host.deliver()).is_err());
        fail_a_delivery(&evaluation_runtime, "output adapter failed");
        evaluation_runtime.pump_until_stable();

        let values = assembler.values();
        let mut published = PublishedIssues::default();
        let analysis = evaluation_runtime.analysis().unwrap();
        let families: Vec<_> = analysis
            .issues()
            .iter()
            .map(|issue| issue.family())
            .collect();
        assert_eq!(
            families,
            [
                RuntimeIssueFamily::MachineInError,
                RuntimeIssueFamily::DeliveryFailure
            ]
        );
        let rendered = published.take_new(&values, &analysis).unwrap();
        assert_eq!(rendered.len(), 2);
        assert!(
            published
                .take_new(&values, &evaluation_runtime.analysis().unwrap())
                .unwrap()
                .is_empty(),
            "each issue renders once"
        );
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
                .all(|diagnostic| assembler.get(diagnostic.emission(), "msg.context").is_ok())
        );
    }

    #[test]
    fn waits_render_as_promise_and_erred_waits() {
        // A wait on a promise no one resolves, and a cut waiting for a
        // change after an error, render with their work; the erred wait's
        // error stays one nested message.
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
        let module = assembler
            .module(["analyzed_waits"])
            .script(
                "g",
                concat!(
                    "language g0\n",
                    "import 'std\n",
                    "refl.erred = .cut (.heap.get ['never])\n",
                    "refl.awaits = .promise.new >>= (\\pr -> .heap.set ['kept] pr.resolver =>> seq pr.promise (.r ()))\n",
                ),
            )
            .build()
            .expect("wait fixture should compile");
        let host = logger_host(&assembler, DiagnosticBus::for_runtime(&evaluation_runtime));
        let lifecycles = [
            EffectLifecycle::new(&evaluation_runtime),
            EffectLifecycle::new(&evaluation_runtime),
        ];
        let _tasks: Vec<_> = ["refl.erred", "refl.awaits"]
            .iter()
            .zip(&lifecycles)
            .map(|(name, lifecycle)| {
                let effect = assembler
                    .get(module.value(), name)
                    .expect("fixture should define its effect");
                EffectRun::new(
                    &evaluation_runtime,
                    &effect,
                    MainEffects::new(assembler.values()),
                    host.clone(),
                )
                .schedule(lifecycle)
                .expect("the effect should schedule")
            })
            .collect();
        evaluation_runtime.pump_until_stable();

        let values = assembler.values();
        let analysis = evaluation_runtime.analysis().unwrap();
        assert!(analysis.quiescent());
        let mut families: Vec<_> = analysis
            .issues()
            .iter()
            .map(|issue| issue.family())
            .collect();
        families.sort();
        assert_eq!(
            families,
            [
                RuntimeIssueFamily::PromiseWait,
                RuntimeIssueFamily::ErredWait
            ]
        );
        let rendered = PublishedIssues::default()
            .take_new(&values, &analysis)
            .unwrap();
        let messages: Vec<_> = rendered
            .iter()
            .map(|diagnostic| diagnostic.message())
            .collect();
        assert!(
            messages
                .iter()
                .any(|message| message.ends_with("waits for a change after an error")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|message| message
                .ends_with("waits on a value that nothing in progress will produce")),
            "{messages:?}"
        );
    }

    #[test]
    fn a_batch_renders_what_its_publishing_reports() {
        let assembler = Assembler::default();
        let mut runtime = logger_runtime(assembler.clone());
        hand_over_logger(&mut runtime);
        let evaluation_runtime = assembler.evaluation_runtime();
        let module = assembler
            .module(["reentrant_report"])
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
        let mut drain = || {
            for diagnostic in take_from_fallback(&mut runtime) {
                render(diagnostic);
            }
            Ok(())
        };
        let failed = quiesce_batch(
            &evaluation_runtime,
            &assembler.values(),
            render,
            &mut DrainingLogger(&mut drain),
        );

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
    fn a_batch_fails_on_a_retained_delivery_failure() {
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
        fail_a_delivery(&evaluation_runtime, "authoritative adapter failure");

        assert!(quiesce_batch(
            &evaluation_runtime,
            &assembler.values(),
            |_| {},
            &mut DrainingLogger(&mut || Ok(())),
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
    fn a_batch_services_managed_maintenance() {
        let assembler = Assembler::default();
        let evaluation_runtime = assembler.evaluation_runtime();
        evaluation_runtime
            .request_managed_collection()
            .expect("explicit maintenance should publish");

        assert!(!quiesce_batch(
            &evaluation_runtime,
            &assembler.values(),
            |_| {},
            &mut DrainingLogger(&mut || Ok(())),
        ));
        assert!(matches!(
            evaluation_runtime.readiness(),
            RuntimeReadiness::Ready(_)
        ));
    }
}

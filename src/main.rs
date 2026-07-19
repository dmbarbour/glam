use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use bytes::Bytes;
use glam::reflection::{
    CommitResult, EffectRequestSpec, HostSnapshot, ReflectionEffects, ReflectionHost,
    ReflectionJournal, ReflectionRequest, ReflectionTransaction, RequestContext, RequestResult,
    TaskCommit, TaskHost, TaskOutcome, TaskSpecialization, handle_reflection_request,
    reflection_request_specs, run_unit_with_reflection_host,
};
use glam::{
    Assembler, Builtin, DEFAULT_DIAGNOSTIC_CAPACITY, Diagnostic, DiagnosticSink, Error,
    ModuleInput, ReasoningReport, ReasoningStatus, ReasoningTaskState, Severity, Value,
};

// Parse inspection intentionally remains on the front-end API while ordinary
// assembly uses only the embedding facade.
use glam::g_syntax::{DeclarationKind, ParsedSource, parse_source};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(first) = args.next() else {
        print_help();
        return ExitCode::SUCCESS;
    };

    match first.as_str() {
        "-h" | "--help" => {
            print_help();
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            println!("glam {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "--parse" => {
            let Some(path) = args.next() else {
                eprintln!("error: `glam --parse` needs a source path");
                return ExitCode::from(2);
            };
            parse_path(&path)
        }
        "-f" | "--file" => {
            let Some(path) = args.next() else {
                eprintln!("error: `{}` needs a source path", first);
                return ExitCode::from(2);
            };

            let mut inputs = vec![ModuleInput::file(path)];
            let mut cli_args = Vec::new();
            match collect_assembly_inputs(args, &mut inputs, &mut cli_args) {
                Ok(()) => assemble_inputs(inputs, cli_args),
                Err(exit_code) => exit_code,
            }
        }
        option if script_extension(option).is_some() => {
            let extension = script_extension(option).expect("checked above").to_owned();
            let Some(body) = args.next() else {
                eprintln!("error: `{option}` needs a script body");
                return ExitCode::from(2);
            };

            let mut inputs = vec![ModuleInput::script(extension, body)];
            let mut cli_args = Vec::new();
            match collect_assembly_inputs(args, &mut inputs, &mut cli_args) {
                Ok(()) => assemble_inputs(inputs, cli_args),
                Err(exit_code) => exit_code,
            }
        }
        option if option.starts_with('-') => {
            eprintln!("error: unknown option `{option}`");
            ExitCode::from(2)
        }
        _arg => {
            eprintln!(
                "error: bare command-line arguments are reserved for configured `conf.cli` rewriting; use `--parse <PATH>` to inspect a source file"
            );
            ExitCode::from(2)
        }
    }
}

fn collect_assembly_inputs(
    mut args: impl Iterator<Item = String>,
    inputs: &mut Vec<ModuleInput>,
    cli_args: &mut Vec<String>,
) -> Result<(), ExitCode> {
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                cli_args.extend(args);
                return Ok(());
            }
            "-f" | "--file" => {
                let Some(path) = args.next() else {
                    eprintln!("error: `{arg}` needs a source path");
                    return Err(ExitCode::from(2));
                };
                inputs.push(ModuleInput::file(path));
            }
            option if script_extension(option).is_some() => {
                let extension = script_extension(option).expect("checked above").to_owned();
                let Some(body) = args.next() else {
                    eprintln!("error: `{option}` needs a script body");
                    return Err(ExitCode::from(2));
                };
                inputs.push(ModuleInput::script(extension, body));
            }
            option if option.starts_with('-') => {
                eprintln!("error: unknown option `{option}`");
                return Err(ExitCode::from(2));
            }
            _arg => {
                eprintln!(
                    "error: bare command-line arguments are reserved for configured `conf.cli` rewriting"
                );
                return Err(ExitCode::from(2));
            }
        }
    }

    Ok(())
}

fn script_extension(option: &str) -> Option<&str> {
    option
        .strip_prefix("--script.")
        .or_else(|| option.strip_prefix("-s."))
        .filter(|extension| !extension.is_empty())
}

fn assemble_inputs(inputs: Vec<ModuleInput>, cli_args: Vec<String>) -> ExitCode {
    let log_host = Arc::new(LogHost::new(DEFAULT_DIAGNOSTIC_CAPACITY));
    let assembler = Assembler::default().with_diagnostic_sink(log_host.clone());
    let configuration = match load_configuration(&assembler) {
        Ok(configuration) => configuration,
        Err(error) => {
            log_host.emit(Diagnostic::new(Severity::Error, error.to_string()));
            log_host.close_input();
            log_host.drain_default(&DefaultLogger::new(assembler.clone()));
            return ExitCode::from(1);
        }
    };
    let logger = start_logger(&assembler, &configuration.value, log_host.clone());
    let result = assemble(&assembler, inputs, cli_args, configuration.environment);
    let mut operation_failed = false;
    match result {
        Ok(bytes) => {
            if let Err(error) = io::stdout().write_all(&bytes) {
                operation_failed = true;
                log_host.emit(Diagnostic::new(
                    Severity::Error,
                    format!("could not write stdout: {error}"),
                ));
            }
        }
        Err(error) => {
            operation_failed = true;
            log_host.emit(Diagnostic::new(Severity::Error, error.to_string()));
        }
    }

    report_reasoning(&log_host, &assembler.drain_reasoning());
    log_host.close_input();
    logger.join().expect("logger task should not panic");
    log_host.cancel();

    if operation_failed || log_host.error_count() > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn report_reasoning(host: &LogHost, report: &ReasoningReport) {
    for failure in report.failures() {
        host.emit(Diagnostic::new(
            Severity::Error,
            format!(
                "reflection task {} failed: {}",
                failure.task_id(),
                failure.message()
            ),
        ));
    }
    if report.status() != ReasoningStatus::Deadlocked {
        return;
    }

    let mut message = format!(
        "reflection scheduler deadlocked with {} unfinished task{}",
        report.unfinished().len(),
        if report.unfinished().len() == 1 {
            ""
        } else {
            "s"
        }
    );
    for task in report.unfinished() {
        let detail = match task.state() {
            ReasoningTaskState::Blocked => match (
                task.waiting_on_task(),
                task.observed_generation(),
                task.wait_id(),
            ) {
                (Some(dependency), Some(generation), _) => {
                    format!("waits on task {dependency} and shared-state generation {generation}")
                }
                (Some(dependency), None, _) => format!("waits on task {dependency}"),
                (None, Some(generation), Some(wait)) => {
                    format!("waits on token {wait} and shared-state generation {generation}")
                }
                (None, Some(generation), None) => {
                    format!("waits on shared-state generation {generation}")
                }
                (None, None, Some(wait)) => format!("waits on token {wait}"),
                (None, None, None) => "is blocked without a wake condition".to_owned(),
            },
            state => format!("remains in anomalous {state:?} state"),
        };
        message.push_str(&format!("\ntask {} {detail}", task.task_id()));
    }
    host.emit(Diagnostic::new(Severity::Error, message));
}

fn assemble(
    assembler: &Assembler,
    inputs: Vec<ModuleInput>,
    cli_args: Vec<String>,
    environment: Value,
) -> Result<Bytes, Error> {
    let arguments = Value::list(cli_args.into_iter().map(Value::text));
    let initial_definitions = Value::record([
        ("asm", Value::record([("args", arguments)])),
        ("env", environment),
    ]);
    let module = assembler
        .module(["assembly"])
        .initial_definitions(initial_definitions)
        .inputs(inputs)
        .build()?;
    assembler.binary_at(module.value(), "asm.result")
}

struct LoadedConfiguration {
    value: Value,
    environment: Value,
}

fn load_configuration(assembler: &Assembler) -> Result<LoadedConfiguration, Error> {
    let default_environment = empty_environment_object();
    let initial_definitions = Value::record([("env", default_environment.clone())]);
    let module = assembler
        .module(["configuration"])
        .initial_definitions(initial_definitions)
        .inputs(configuration_paths().into_iter().map(ModuleInput::file))
        .build()?;

    let environment = match assembler.get(module.value(), "conf.env") {
        Ok(environment) if !environment.is_undefined() => assembler.evaluate(&environment)?,
        Ok(_) | Err(_) => default_environment,
    };
    Ok(LoadedConfiguration {
        value: module.into_value(),
        environment,
    })
}

fn start_logger(
    assembler: &Assembler,
    configuration: &Value,
    input: Arc<LogHost>,
) -> thread::JoinHandle<()> {
    let logger = Arc::new(DefaultLogger::new(assembler.clone()));
    let output: Arc<dyn DiagnosticSink> = logger.clone();
    let host = Arc::new(LoggerTaskHost::new(input.clone(), output));
    let effect_assembler = assembler.clone();
    let custom = assembler
        .get(configuration, "conf.log")
        .ok()
        .filter(|logger| !logger.is_undefined());
    thread::spawn(move || {
        if let Some(custom) = custom {
            let reflection_host: Arc<dyn ReflectionHost<ReflectionEffects>> = host.clone();
            match run_unit_with_reflection_host(
                &custom,
                MainEffects::new(effect_assembler),
                host.clone(),
                reflection_host,
            ) {
                Ok(TaskOutcome::Complete(_)) => {}
                Ok(TaskOutcome::Cancelled) => {
                    input.emit_default(
                        logger.as_ref(),
                        Diagnostic::new(
                            Severity::Error,
                            "configured logger remained blocked after the log stream closed",
                        ),
                    );
                }
                Err(error) => {
                    input.emit_default(
                        logger.as_ref(),
                        Diagnostic::new(
                            Severity::Error,
                            format!("configured logger failed: {error}"),
                        ),
                    );
                }
            }
        }
        input.drain_default(logger.as_ref());
    })
}

#[derive(Clone)]
struct MainEffects {
    assembler: Assembler,
}

impl MainEffects {
    fn new(assembler: Assembler) -> Self {
        Self { assembler }
    }
}

#[derive(Clone)]
enum MainRequest {
    Reflection(ReflectionRequest),
    LogStatus,
    ReadLog,
    WriteStderr,
}

#[derive(Clone)]
struct MainSnapshot {
    diagnostics: Arc<[Diagnostic]>,
    input_closed: bool,
}

#[derive(Clone, Default)]
struct MainJournal {
    reflection: ReflectionJournal,
    consumed_diagnostics: usize,
    stderr: Vec<Bytes>,
}

impl ReflectionTransaction for MainJournal {
    fn reflection_journal(&mut self) -> &mut ReflectionJournal {
        &mut self.reflection
    }
}

impl TaskSpecialization for MainEffects {
    type Host = LoggerTaskHost;
    type Request = MainRequest;
    type Snapshot = MainSnapshot;
    type Journal = MainJournal;

    fn requests(&self) -> Vec<EffectRequestSpec<Self::Request>> {
        reflection_request_specs()
            .into_iter()
            .map(|request| request.map_request(MainRequest::Reflection))
            .chain([
                EffectRequestSpec::new(
                    "log_status",
                    ["glam_cli", "v0", "request", "log_status"],
                    0,
                    MainRequest::LogStatus,
                ),
                EffectRequestSpec::new(
                    "read_log",
                    ["glam_cli", "v0", "request", "read_log"],
                    0,
                    MainRequest::ReadLog,
                ),
                EffectRequestSpec::new(
                    "write_stderr",
                    ["glam_cli", "v0", "request", "write_stderr"],
                    1,
                    MainRequest::WriteStderr,
                ),
            ])
            .collect()
    }

    fn handle_request(
        &self,
        request: Self::Request,
        arguments: Vec<Value>,
        context: &mut RequestContext<'_, Self>,
    ) -> Result<RequestResult, glam::reflection::TaskError> {
        match request {
            MainRequest::Reflection(request) => {
                handle_reflection_request(request, arguments, context)
            }
            MainRequest::LogStatus => log_status(arguments, context),
            MainRequest::ReadLog => read_log(context),
            MainRequest::WriteStderr => {
                let [value]: [Value; 1] = arguments.try_into().map_err(|_| {
                    glam::reflection::TaskError::new(
                        "`.write_stderr` received the wrong number of arguments",
                    )
                })?;
                let bytes = self
                    .assembler
                    .to_binary(&value)
                    .map_err(|error| glam::reflection::TaskError::new(error.to_string()))?;
                if let Some(mut transaction) = context.transaction() {
                    transaction.parts().1.stderr.push(bytes);
                } else {
                    context.host().input.write_stderr(bytes);
                    context.committed();
                }
                Ok(RequestResult::ReturnUnit)
            }
        }
    }
}

fn log_status(
    arguments: Vec<Value>,
    context: &mut RequestContext<'_, MainEffects>,
) -> Result<RequestResult, glam::reflection::TaskError> {
    if !arguments.is_empty() {
        return Err(glam::reflection::TaskError::new(
            "`.log_status` received the wrong number of arguments",
        ));
    }
    let (generation, input_closed) = if let Some(generation) = context.transaction_generation() {
        let mut transaction = context
            .transaction()
            .expect("checked active reflection transaction");
        (generation, transaction.parts().0.input_closed)
    } else {
        let snapshot = <LoggerTaskHost as TaskHost<MainEffects>>::snapshot(context.host());
        (snapshot.generation(), snapshot.extra().input_closed)
    };
    context.observe_host_generation(generation);
    Ok(RequestResult::Return(Value::atom_from_text(
        if input_closed { "closed" } else { "open" },
    )))
}

fn read_log(
    context: &mut RequestContext<'_, MainEffects>,
) -> Result<RequestResult, glam::reflection::TaskError> {
    if let Some(generation) = context.transaction_generation() {
        context.observe_host_generation(generation);
        let mut transaction = context
            .transaction()
            .expect("checked active reflection transaction");
        let (snapshot, journal) = transaction.parts();
        if let Some(diagnostic) = snapshot.diagnostics.get(journal.consumed_diagnostics) {
            journal.consumed_diagnostics += 1;
            return diagnostic
                .enrich()
                .map(RequestResult::Return)
                .map_err(|error| glam::reflection::TaskError::new(error.to_string()));
        }
        // Queue reads observe only the host snapshot. Journaled writes remain
        // invisible until commit, just as writes from concurrent tasks do.
        return Ok(RequestResult::Fail);
    }

    loop {
        let snapshot = <LoggerTaskHost as TaskHost<MainEffects>>::snapshot(context.host());
        context.observe_host_generation(snapshot.generation());
        let Some(diagnostic) = snapshot.extra().diagnostics.first() else {
            return Ok(RequestResult::Fail);
        };
        let value = diagnostic
            .enrich()
            .map_err(|error| glam::reflection::TaskError::new(error.to_string()))?;
        let commit = TaskCommit::new(
            snapshot.generation(),
            snapshot.heap().clone(),
            MainJournal {
                reflection: ReflectionJournal::default(),
                consumed_diagnostics: 1,
                stderr: Vec::new(),
            },
        );
        match <LoggerTaskHost as TaskHost<MainEffects>>::commit(context.host(), commit) {
            CommitResult::Committed => {
                context.committed();
                return Ok(RequestResult::Return(value));
            }
            CommitResult::Conflict => {}
            CommitResult::Closed => return Ok(RequestResult::Cancelled),
        }
    }
}

struct LogHost {
    capacity: usize,
    state: Mutex<LogHostState>,
    changed: Condvar,
}

/// Capabilities and mutable state belonging to the logger's evaluation
/// session. Incoming assembler diagnostics remain in `input`; diagnostics
/// emitted by this session go only to `output`.
struct LoggerTaskHost {
    input: Arc<LogHost>,
    output: Arc<dyn DiagnosticSink>,
}

impl LoggerTaskHost {
    fn new(input: Arc<LogHost>, output: Arc<dyn DiagnosticSink>) -> Self {
        Self { input, output }
    }

    fn emit_output(&self, diagnostic: Diagnostic) {
        self.input.record_error(&diagnostic);
        self.output.emit(diagnostic);
    }
}

struct LogHostState {
    generation: u64,
    heap: Value,
    diagnostics: VecDeque<Diagnostic>,
    stderr: VecDeque<Bytes>,
    input_closed: bool,
    cancelled: bool,
    error_count: u64,
}

impl LogHost {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(LogHostState {
                generation: 1,
                heap: Value::empty_record(),
                diagnostics: VecDeque::new(),
                stderr: VecDeque::new(),
                input_closed: false,
                cancelled: false,
                error_count: 0,
            }),
            changed: Condvar::new(),
        }
    }

    fn close_input(&self) {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        state.input_closed = true;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn cancel(&self) {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        state.cancelled = true;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn error_count(&self) -> u64 {
        self.state
            .lock()
            .expect("log host mutex should not be poisoned")
            .error_count
    }

    fn record_error(&self, diagnostic: &Diagnostic) {
        if diagnostic.severity() == Severity::Error {
            let mut state = self
                .state
                .lock()
                .expect("log host mutex should not be poisoned");
            state.error_count = state.error_count.saturating_add(1);
        }
    }

    fn emit_default(&self, logger: &DefaultLogger, diagnostic: Diagnostic) {
        self.record_error(&diagnostic);
        logger.emit(&diagnostic);
    }

    fn drain_default(&self, logger: &DefaultLogger) {
        while let Some(diagnostic) = self.take_diagnostic() {
            logger.emit(&diagnostic);
        }
        self.flush_stderr();
    }

    fn take_diagnostic(&self) -> Option<Diagnostic> {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        loop {
            if let Some(diagnostic) = state.diagnostics.pop_front() {
                state.generation = state.generation.wrapping_add(1);
                self.changed.notify_all();
                return Some(diagnostic);
            }
            if state.input_closed || state.cancelled {
                return None;
            }
            state = self
                .changed
                .wait(state)
                .expect("log host mutex should not be poisoned");
        }
    }

    fn flush_stderr(&self) {
        let output = {
            let mut state = self
                .state
                .lock()
                .expect("log host mutex should not be poisoned");
            state.stderr.drain(..).collect::<Vec<_>>()
        };
        let mut stderr = io::stderr().lock();
        for bytes in output {
            let _ = stderr.write_all(&bytes);
        }
    }

    fn write_stderr(&self, bytes: Bytes) {
        self.state
            .lock()
            .expect("log host mutex should not be poisoned")
            .stderr
            .push_back(bytes);
        self.flush_stderr();
    }

    fn push_diagnostic(&self, state: &mut LogHostState, diagnostic: Diagnostic) {
        if diagnostic.severity() == Severity::Error {
            state.error_count = state.error_count.saturating_add(1);
        }
        if self.capacity == 0 {
            return;
        }
        if state.diagnostics.len() == self.capacity {
            state.diagnostics.pop_front();
        }
        state.diagnostics.push_back(diagnostic);
    }
}

impl DiagnosticSink for LogHost {
    fn emit(&self, diagnostic: Diagnostic) {
        let mut state = self
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        self.push_diagnostic(&mut state, diagnostic);
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }
}

impl ReflectionHost<MainEffects> for LoggerTaskHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        self.emit_output(diagnostic);
    }

    fn os_environment_variable(&self, name: &str) -> Option<std::ffi::OsString> {
        env::var_os(name)
    }

    fn command_line_arguments(&self) -> Vec<std::ffi::OsString> {
        env::args_os().collect()
    }
}

impl ReflectionHost<ReflectionEffects> for LoggerTaskHost {
    fn emit_diagnostic(&self, diagnostic: Diagnostic) {
        self.emit_output(diagnostic);
    }

    fn os_environment_variable(&self, name: &str) -> Option<std::ffi::OsString> {
        env::var_os(name)
    }

    fn command_line_arguments(&self) -> Vec<std::ffi::OsString> {
        env::args_os().collect()
    }
}

impl TaskHost<MainEffects> for LoggerTaskHost {
    fn snapshot(&self) -> HostSnapshot<MainEffects> {
        let state = self
            .input
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        HostSnapshot::new(
            state.generation,
            state.heap.clone(),
            MainSnapshot {
                diagnostics: Arc::from(state.diagnostics.iter().cloned().collect::<Vec<_>>()),
                input_closed: state.input_closed,
            },
        )
    }

    fn commit(&self, commit: TaskCommit<MainEffects>) -> CommitResult {
        let diagnostics = {
            let mut state = self
                .input
                .state
                .lock()
                .expect("log host mutex should not be poisoned");
            if state.generation != commit.generation()
                || state.diagnostics.len() < commit.extra().consumed_diagnostics
            {
                return CommitResult::Conflict;
            }
            state.heap = commit.heap().clone();
            state
                .diagnostics
                .drain(..commit.extra().consumed_diagnostics);
            state.stderr.extend(commit.extra().stderr.iter().cloned());
            state.generation = state.generation.wrapping_add(1);
            self.input.changed.notify_all();
            commit.extra().reflection.diagnostics().to_vec()
        };
        for diagnostic in diagnostics {
            self.emit_output(diagnostic);
        }
        commit.extra().reflection.commit_task_updates();
        self.input.flush_stderr();
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        let mut state = self
            .input
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        if state.generation != observed_generation {
            return true;
        }
        while state.generation == observed_generation
            && !state.cancelled
            && !(state.input_closed && state.diagnostics.is_empty())
        {
            state = self
                .input
                .changed
                .wait(state)
                .expect("log host mutex should not be poisoned");
        }
        state.generation != observed_generation
    }
}

impl TaskHost<ReflectionEffects> for LoggerTaskHost {
    fn snapshot(&self) -> HostSnapshot<ReflectionEffects> {
        let state = self
            .input
            .state
            .lock()
            .expect("log host mutex should not be poisoned");
        HostSnapshot::new(state.generation, state.heap.clone(), ())
    }

    fn commit(&self, commit: TaskCommit<ReflectionEffects>) -> CommitResult {
        let diagnostics = {
            let mut state = self
                .input
                .state
                .lock()
                .expect("log host mutex should not be poisoned");
            if state.generation != commit.generation() {
                return CommitResult::Conflict;
            }
            state.heap = commit.heap().clone();
            state.generation = state.generation.wrapping_add(1);
            self.input.changed.notify_all();
            commit.extra().diagnostics().to_vec()
        };
        for diagnostic in diagnostics {
            self.emit_output(diagnostic);
        }
        commit.extra().commit_task_updates();
        CommitResult::Committed
    }

    fn wait_for_change(&self, observed_generation: u64) -> bool {
        <LoggerTaskHost as TaskHost<MainEffects>>::wait_for_change(self, observed_generation)
    }
}

fn empty_environment_object() -> Value {
    let spec = Value::record([
        (
            "name",
            Value::abstract_global_path(["configuration", "env"]),
        ),
        ("deps", Value::list(std::iter::empty())),
        ("defs", Value::builtin(Builtin::ObjectDefaultDefs)),
    ]);
    Value::builtin_call(Builtin::ObjectInstance, [spec])
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

struct DefaultLogger {
    evaluator: Assembler,
    working_directory: PathBuf,
}

impl DefaultLogger {
    const AUTO_INDENT: usize = 2;

    fn new(evaluator: Assembler) -> Self {
        Self {
            evaluator,
            working_directory: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    fn emit(&self, diagnostic: &Diagnostic) {
        let terminal = TerminalContext::snapshot();
        let updates = self.viewer_updates(diagnostic, &terminal);
        let text = diagnostic
            .enrich_with(updates)
            .and_then(|message| self.evaluator.get(&message, "msg.text"))
            .and_then(|text| self.evaluator.to_binary(&text))
            .map(|text| String::from_utf8_lossy(&text).into_owned())
            .unwrap_or_else(|_| diagnostic.message().to_owned());
        let rendered = self.render(diagnostic, &text, terminal.color);

        let _ = io::stderr().lock().write_all(rendered.as_bytes());
    }

    fn viewer_updates(&self, diagnostic: &Diagnostic, terminal: &TerminalContext) -> Value {
        let mut viewer = vec![
            ("kind", Value::text("terminal")),
            (
                "columns",
                Value::integer(i64::try_from(terminal.columns).unwrap_or(i64::MAX)),
            ),
            ("color", Value::text(terminal.color.name())),
            ("auto_indent", Value::integer(Self::AUTO_INDENT as i64)),
        ];
        if let Some(term) = &terminal.term {
            viewer.push(("term", Value::text(term)));
        }
        if let Some(language) = &terminal.language {
            viewer.push(("lang", Value::text(language)));
        }
        if let Some(source) = diagnostic.source().and_then(|source| {
            let path = Path::new(source);
            path.is_absolute().then(|| self.display_source(path))
        }) {
            viewer.push(("source", Value::record([("file", Value::text(source))])));
        }
        Value::record([("viewer", Value::record(viewer))])
    }

    fn render(&self, diagnostic: &Diagnostic, text: &str, color: TerminalColor) -> String {
        let severity = diagnostic.severity().to_string();
        let severity = color.paint(diagnostic.severity(), &severity);
        let location = match (diagnostic.source(), diagnostic.line()) {
            (Some(source), Some(line)) => {
                format!("{}:{line}: ", self.display_source(Path::new(source)))
            }
            (Some(source), None) => format!("{}: ", self.display_source(Path::new(source))),
            (None, Some(line)) => format!("line {line}: "),
            (None, None) => String::new(),
        };
        let mut rendered = format!("{location}{severity}: ");
        let mut lines = text.split('\n');
        rendered.push_str(lines.next().unwrap_or_default());
        for line in lines {
            rendered.push('\n');
            if !line.is_empty() {
                rendered.push_str(&" ".repeat(Self::AUTO_INDENT));
                rendered.push_str(line);
            }
        }
        rendered.push('\n');
        rendered
    }

    fn display_source(&self, source: &Path) -> String {
        source
            .strip_prefix(&self.working_directory)
            .unwrap_or(source)
            .display()
            .to_string()
    }
}

impl DiagnosticSink for DefaultLogger {
    fn emit(&self, diagnostic: Diagnostic) {
        DefaultLogger::emit(self, &diagnostic);
    }
}

struct TerminalContext {
    columns: usize,
    color: TerminalColor,
    term: Option<String>,
    language: Option<String>,
}

impl TerminalContext {
    fn snapshot() -> Self {
        let term = env::var("TERM").ok();
        let color = TerminalColor::detect(term.as_deref());
        Self {
            columns: env::var("COLUMNS")
                .ok()
                .and_then(|columns| columns.parse().ok())
                .filter(|columns| *columns > 0)
                .unwrap_or(80),
            color,
            term,
            language: ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty())),
        }
    }
}

#[derive(Clone, Copy)]
enum TerminalColor {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl TerminalColor {
    fn detect(term: Option<&str>) -> Self {
        if !io::stderr().is_terminal() || env::var_os("NO_COLOR").is_some() || term == Some("dumb")
        {
            return Self::None;
        }
        if env::var("COLORTERM").is_ok_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        }) {
            Self::TrueColor
        } else if term.is_some_and(|term| term.contains("256color")) {
            Self::Ansi256
        } else {
            Self::Ansi16
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Ansi16 => "ansi16",
            Self::Ansi256 => "ansi256",
            Self::TrueColor => "truecolor",
        }
    }

    fn paint(self, severity: Severity, text: &str) -> String {
        let code = match (self, severity) {
            (Self::None, _) => return text.to_owned(),
            (_, Severity::Info) => 36,
            (_, Severity::Warning) => 33,
            (_, Severity::Error) => 31,
        };
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

fn parse_path(path: &str) -> ExitCode {
    let parsed = match parse_source_path(path) {
        Ok(parsed) => parsed,
        Err(exit_code) => return exit_code,
    };

    print_parse_summary(path, &parsed)
}

fn parse_source_path(path: &str) -> Result<ParsedSource, ExitCode> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("error: could not read `{path}`: {error}");
            return Err(ExitCode::from(1));
        }
    };

    Ok(parse_source(&bytes))
}

fn print_parse_summary(path: &str, parsed: &ParsedSource) -> ExitCode {
    let has_errors = parsed
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.severity, Severity::Error));

    let logger = DefaultLogger::new(Assembler::default());
    for diagnostic in &parsed.diagnostics {
        logger.emit(
            &Diagnostic::new(diagnostic.severity, diagnostic.message.clone())
                .with_source_location(path, diagnostic.line),
        );
    }

    let out = &mut io::stderr();

    writeln!(out, "{} declarations", parsed.declarations.len())
        .expect("failed to write parse summary");
    for declaration in &parsed.declarations {
        writeln!(
            out,
            "{:>4}: {:<12} {}",
            declaration.line,
            declaration_label(&declaration.kind),
            declaration.text.lines().next().unwrap_or("")
        )
        .expect("failed to write parse summary");
    }

    if has_errors {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn declaration_label(kind: &DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Language(_) => "language",
        DeclarationKind::Import(_) => "import",
        DeclarationKind::Abstract(_) => "abstract",
        DeclarationKind::Unique(_) => "unique",
        DeclarationKind::Object(_) => "object",
        DeclarationKind::Extend(_) => "extend",
        DeclarationKind::Definition(_) => "definition",
        DeclarationKind::Unknown => "unknown",
    }
}

fn print_help() {
    const HELP: &str = "\
Usage: glam [(-f|--file) <PATH> | (-s|--script).<EXT> <TEXT>]...
       glam --parse <PATH>
       glam --help
       glam --version

Assembly inputs are applied as mixins; earlier inputs override later inputs.
Configuration is loaded from GLAM_CONF as an OS path-list, or from the user config/default fixture.
Bare arguments are reserved for configured `conf.cli` rewriting.
";

    print!("{HELP}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_logger_indents_nonempty_continuation_lines_by_two_spaces() {
        let logger = DefaultLogger {
            evaluator: Assembler::default(),
            working_directory: PathBuf::from("/work"),
        };
        let diagnostic = Diagnostic::new(Severity::Warning, "first\nsecond\n\nfourth")
            .with_source_location("/work/src/test.g", 4);

        assert_eq!(
            logger.render(&diagnostic, diagnostic.message(), TerminalColor::None),
            "src/test.g:4: warning: first\n  second\n\n  fourth\n"
        );
    }

    #[test]
    fn terminal_viewer_context_is_an_independent_diagnostic_mixin() {
        let logger = DefaultLogger::new(Assembler::default());
        let diagnostic = Diagnostic::new(Severity::Info, "hello");
        let terminal = TerminalContext {
            columns: 100,
            color: TerminalColor::Ansi256,
            term: Some("xterm-256color".to_owned()),
            language: Some("en_US.UTF-8".to_owned()),
        };
        let enriched = diagnostic
            .enrich_with(logger.viewer_updates(&diagnostic, &terminal))
            .expect("terminal viewer metadata should mix into a diagnostic");

        assert_eq!(
            logger
                .evaluator
                .get(&enriched, "viewer.auto_indent")
                .expect("viewer should declare automatic indentation")
                .as_i64(),
            Some(2)
        );
        assert_eq!(
            logger
                .evaluator
                .get(&enriched, "viewer.term")
                .expect("viewer should declare its terminal")
                .as_binary(),
            Some(b"xterm-256color".as_slice())
        );
        assert!(
            logger
                .evaluator
                .get(diagnostic.emission(), "viewer")
                .is_err()
        );
    }

    #[test]
    fn log_error_count_survives_queue_drops_and_reads() {
        let dropped = LogHost::new(0);
        dropped.emit(Diagnostic::new(Severity::Error, "dropped"));
        assert_eq!(dropped.error_count(), 1);

        let retained = LogHost::new(1);
        retained.emit(Diagnostic::new(Severity::Error, "retained"));
        assert!(retained.take_diagnostic().is_some());
        assert_eq!(retained.error_count(), 1);
        retained.close_input();
    }

    #[test]
    fn logger_session_output_is_separate_from_assembler_input() {
        let input = Arc::new(LogHost::new(4));
        let output = Arc::new(glam::DiagnosticBuffer::new(4));
        let host = LoggerTaskHost::new(input.clone(), output.clone());

        <LoggerTaskHost as ReflectionHost<MainEffects>>::emit_diagnostic(
            &host,
            Diagnostic::new(Severity::Error, "session output"),
        );

        assert!(
            input
                .state
                .lock()
                .expect("log host mutex should not be poisoned")
                .diagnostics
                .is_empty()
        );
        let output = output.as_ref().read();
        assert_eq!(output.entries().len(), 1);
        assert_eq!(output.entries()[0].message(), "session output");
        assert_eq!(input.error_count(), 1);
    }
}

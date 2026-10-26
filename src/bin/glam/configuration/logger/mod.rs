//! The CLI's loggers, machines under the root
//! (`machines-communicate-through-wired-ports`). The root's diagnostics go
//! to `logger`, which runs `conf.log` as a turn where one is configured, and
//! hands what it holds, and what reaches it later, to `default_logger`
//! when it ends, or at once where none is. The default logger runs
//! continuously: it renders what reaches it, the configured logger's own
//! `.log` among it, so a configured logger may leave rendering to it and
//! focus on enrichment. Both write to `stderr`, which writes the process's
//! standard error. The root owns their wiring, out of the configured
//! logger's reach.

use std::io::{self, Write};
use std::sync::Arc;
use std::thread;

use glam::reflection::EffectRequestSpec;
use glam::{
    Answer, Assembler, Diagnostic, DiagnosticBus, EffectProvider, Endpoint, Error, ErrorKind, Fork,
    Machine, MachinePath, Runtime, Severity,
};

use crate::DiagnosticBusLocal;
use crate::configuration::{CLI, entry_context, select_entry, selects_nothing};
use crate::rendering::{DefaultLogger, port_diagnostic};

mod reports;

pub(crate) use reports::PublishedIssues;

/// The machine, a direct child of the root, to which the root sends the
/// diagnostics gathered at its input. A configured logger reads them from
/// its input.
pub(crate) const LOGGER: &str = "logger";

/// The root's child that renders what the logger hands over and what a
/// configured logger logs, running continuously.
pub(crate) const DEFAULT_LOGGER: &str = "default_logger";

/// The root's child that writes what reaches its input of the same name,
/// binaries, to the process's standard error.
pub(crate) const STDERR: &str = "stderr";

pub(crate) const DIAGNOSTICS: &str = "diagnostics";

/// The logger's output to [`DEFAULT_LOGGER`], for what its input holds when
/// it hands over.
const UNREAD: &str = "unread";

/// Wires the loggers, before anything is published: the root's diagnostics
/// to the logger, the logger's hand-over and its own `.log` to the default
/// logger, rather than back to the root, and both loggers' `stderr` outputs
/// to `stderr`. The loggers keep what reaches them, in place of the default
/// pass-through to their parent.
pub(crate) fn wire_logger(runtime: &mut Runtime) -> Result<(), Error> {
    let keep = |machine: &mut Machine<'_, '_>| {
        machine.wire_del(Endpoint::input(DIAGNOSTICS), Endpoint::output(DIAGNOSTICS))
    };
    runtime.run(LOGGER, keep)?;
    runtime.run(DEFAULT_LOGGER, keep)?;
    runtime.run((), |root| {
        root.wire_add(
            Endpoint::input(DIAGNOSTICS),
            Endpoint::child(LOGGER, DIAGNOSTICS),
        )?;
        for source in [UNREAD, DIAGNOSTICS] {
            root.wire_add(
                Endpoint::child(LOGGER, source),
                Endpoint::child(DEFAULT_LOGGER, DIAGNOSTICS),
            )?;
        }
        root.wire_del(
            Endpoint::child(LOGGER, DIAGNOSTICS),
            Endpoint::input(DIAGNOSTICS),
        )?;
        for writer in [LOGGER, DEFAULT_LOGGER] {
            root.wire_add(
                Endpoint::child(writer, STDERR),
                Endpoint::child(STDERR, STDERR),
            )?;
        }
        Ok(())
    })
}

/// Starts the loggers on forks of the root's children: `stderr`, the
/// default logger, and the logger that the configuration selects, or,
/// where it selects none, hands the logger's diagnostics to the default
/// logger at once.
pub(crate) fn start_logger<'scope>(
    root: &mut Machine<'scope, '_>,
    assembler: &Assembler,
) -> Result<LoggerRun<'scope>, Error> {
    let renderer = Arc::new(DefaultLogger::new(assembler.clone()));
    let rendered = DiagnosticBus::for_runtime(&assembler.evaluation_runtime());
    let stderr = root.fork(STDERR, {
        let assembler = assembler.clone();
        move |stderr| write_stderr(stderr, &assembler)
    })?;
    let default_logger = root.fork(DEFAULT_LOGGER, {
        let renderer = renderer.clone();
        let rendered = rendered.clone();
        move |default_logger| render_diagnostics(default_logger, &renderer, &rendered)
    })?;
    let selected = match root.run(CLI, select_logger) {
        Ok(selected) => selected,
        Err(error) => {
            publish_error(assembler, error);
            false
        }
    };
    let logger = if selected {
        root.take(CLI, 1)?;
        root.give(LOGGER, 1)?;
        let builder = thread::Builder::new()
            .name("glam-logger".to_owned())
            // The logger evaluates ordinary Glam configuration and therefore
            // needs the same practical stack headroom as the process main
            // thread. Deep evaluator paths are being migrated to explicit
            // machines, but the bootstrap must not make a configured logger
            // depend on the platform's smaller default child-thread stack in
            // the meantime.
            .stack_size(8 * 1024 * 1024);
        let assembler = assembler.clone();
        Some(root.fork_with(builder, LOGGER, move |logger| {
            run_configured_logger(logger, &assembler);
            // What the configured logger left, and what arrives later, is
            // the default logger's.
            hand_over(logger)
        })?)
    } else {
        root.run(LOGGER, hand_over)?;
        None
    };
    Ok(LoggerRun {
        logger,
        default_logger: Some(default_logger),
        stderr: Some(stderr),
        rendered,
        renderer,
    })
}

/// Runs the configured logger, on top of the logger's stack, as an effect
/// of the logger's turn, until it ends or the batch interrupts it. Its
/// failures, and a result other than unit, are reported on the assembler's
/// bus, which brings them to the default logger once the logger hands
/// over.
fn run_configured_logger(logger: &mut Machine<'_, '_>, assembler: &Assembler) {
    let ran = (|| {
        logger.dict_pack(Vec::<&str>::new())?;
        logger.swap()?;
        logger.effect_run(&mut ConfiguredLogger)?;
        logger.eval()?;
        logger.unit_push();
        let unit = logger.same_representation()?;
        logger.discard(1)?;
        if !unit {
            return Err(Error::new(format!(
                "configured logger result: unit expected, received {:?}",
                logger.kind()?
            )));
        }
        logger.discard(2)
    })();
    match ran {
        // Only the batch interrupts the logger: at its end, where the
        // logger waits on its input, idle, or halting it, with a diagnostic
        // of its own, for leaving its input unread (`quiescence-is-analyzed`).
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) => {
            let values = assembler.values();
            let error = entry_context(&values, "log")
                .and_then(|context| error.clone().with_context(&values, context))
                .unwrap_or(error);
            publish_error(assembler, error);
        }
    }
}

/// A configured logger's effects: every family passes through to the
/// logger machine, its `.log` to the default logger and its ports to the
/// logger's.
struct ConfiguredLogger;

impl EffectProvider for ConfiguredLogger {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        Vec::new()
    }

    fn answer(&mut self, _: &mut Machine<'_, '_>, (): &()) -> Result<Answer, Error> {
        unreachable!("the configured logger makes no requests of the CLI's own")
    }
}

/// Renders what reaches the default logger, in order, writing each
/// rendering to its `stderr` output and counting it on `rendered`, until
/// the batch interrupts it where its input holds nothing more.
fn render_diagnostics(
    default_logger: &mut Machine<'_, '_>,
    renderer: &DefaultLogger,
    rendered: &DiagnosticBus,
) -> Result<(), Error> {
    loop {
        match default_logger.port_read(DIAGNOSTICS, 1) {
            Ok(()) => {
                let message = glam::transition::pop_value(default_logger)?;
                let diagnostic = port_diagnostic(renderer.assembler(), message)?;
                default_logger.bytes_push(renderer.rendered(&diagnostic));
                rendered.publish_local(diagnostic);
                default_logger.port_write(STDERR, 1)?;
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

/// Writes what reaches `stderr` to the process's standard error, in order,
/// until the batch interrupts it where its input holds nothing more.
fn write_stderr(stderr: &mut Machine<'_, '_>, assembler: &Assembler) -> Result<(), Error> {
    loop {
        match stderr.port_read(STDERR, 1) {
            Ok(()) => {
                let value = glam::transition::pop_value(stderr)?;
                write_binary(assembler, value);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

/// Writes `value`, evaluated to binary data, to the process's standard
/// error, reporting a value that is not binary data on the assembler's bus.
fn write_binary(assembler: &Assembler, value: glam::HandleValue) {
    let values = assembler.values();
    let written = values
        .anno_binary(value)
        .and_then(|binary| assembler.evaluator().eval(&binary))
        .and_then(|binary| binary.as_bytes())
        .and_then(|bytes| bytes.ok_or_else(|| Error::new("stderr output requires binary data")));
    match written {
        Ok(bytes) => {
            let _ = io::stderr().write_all(&bytes);
        }
        Err(error) => publish_error(assembler, error),
    }
}

/// Publishes `error` on the assembler's bus.
fn publish_error(assembler: &Assembler, error: Error) {
    let values = assembler.values();
    assembler.diagnostic_bus().publish_local(
        error
            .diagnostic(&values)
            .expect("logger failures belong to the assembler's runtime"),
    );
}

/// Hands what the logger's input holds, and what reaches it later, to the
/// default logger, in one cut, so it receives every diagnostic in order:
/// the logger wires its input on to its unread output, and sends what the
/// input holds through it.
fn hand_over(logger: &mut Machine<'_, '_>) -> Result<(), Error> {
    logger.cut(|logger| {
        logger.wire_add(Endpoint::input(DIAGNOSTICS), Endpoint::output(UNREAD))?;
        let unread = logger.port_available(DIAGNOSTICS);
        logger.port_read(DIAGNOSTICS, unread)?;
        logger.port_write(UNREAD, unread)
    })
}

/// Takes the diagnostics waiting at the default logger, in order, as its
/// renderer, `assembler`'s, reads them.
pub(crate) fn take_diagnostics(
    default_logger: &mut Machine<'_, '_>,
    assembler: &Assembler,
) -> Result<Vec<Diagnostic>, Error> {
    let count = default_logger.port_available(DIAGNOSTICS);
    default_logger.port_read(DIAGNOSTICS, count)?;
    let mut held = (0..count)
        .map(|_| glam::transition::pop_value(default_logger))
        .collect::<Result<Vec<_>, _>>()?;
    held.reverse();
    held.into_iter()
        .map(|message| port_diagnostic(assembler, message))
        .collect()
}

/// Renders what the loggers left, where none runs: what the logger holds,
/// which it hands over first, what waits at `stderr`, and what waits at the
/// default logger. Machines in error are recovered.
pub(crate) fn render_unread(
    root: &mut Machine<'_, '_>,
    renderer: &DefaultLogger,
) -> Result<(), Error> {
    root.recover(LOGGER, hand_over)?;
    let assembler = renderer.assembler().clone();
    root.recover(STDERR, |stderr| {
        let count = stderr.port_available(STDERR);
        stderr.port_read(STDERR, count)?;
        let mut held = (0..count)
            .map(|_| glam::transition::pop_value(stderr))
            .collect::<Result<Vec<_>, _>>()?;
        held.reverse();
        for value in held {
            write_binary(&assembler, value);
        }
        Ok(())
    })?;
    for diagnostic in root.recover(DEFAULT_LOGGER, |default_logger| {
        take_diagnostics(default_logger, &assembler)
    })? {
        renderer.emit(&diagnostic);
    }
    Ok(())
}

/// Leaves the logger `conf.log` selects, evaluated, on the stack, and
/// returns whether it selects one.
fn select_logger(m: &mut Machine<'_, '_>) -> Result<bool, Error> {
    m.list_pack(0)?;
    m.local_get("configuration")?;
    select_entry(m, "log", &["conf", "log"])?;
    m.eval()?;
    if selects_nothing(m)? {
        m.discard(1)?;
        return Ok(false);
    }
    Ok(true)
}

/// The loggers' forks, which the batch ends.
pub(crate) struct LoggerRun<'scope> {
    /// The configured logger's fork, where one runs.
    logger: Option<Fork<'scope>>,
    default_logger: Option<Fork<'scope>>,
    stderr: Option<Fork<'scope>>,
    /// What the default logger rendered, counted.
    pub(crate) rendered: DiagnosticBus,
    pub(crate) renderer: Arc<DefaultLogger>,
}

impl<'scope> LoggerRun<'scope> {
    /// Halts the configured logger, which left `unread` diagnostics in its
    /// input at quiescence, so that the default logger renders them, saying
    /// so; `false` where no configured logger runs.
    pub(crate) fn halt(&mut self, root: &mut Machine<'scope, '_>, unread: usize) -> bool {
        let Some(logger) = self.logger.take() else {
            return false;
        };
        let assembler = self.renderer.assembler();
        let values = assembler.values();
        assembler.diagnostic_bus().publish_local(Diagnostic::new(
            &values,
            Severity::Error,
            format!(
                "the configured logger stopped reading, leaving {unread} diagnostics unread; \
                 the default logger renders them"
            ),
        ));
        end(root, assembler, LOGGER, logger);
        true
    }

    /// Ends the loggers, at the batch's end: the configured logger, which
    /// hands what it holds to the default logger, then the default logger
    /// and `stderr`, each once its input holds nothing more; then renders
    /// what is left. Returns whether that failed.
    pub(crate) fn finish(&mut self, root: &mut Machine<'scope, '_>) -> bool {
        let assembler = self.renderer.assembler().clone();
        let mut failed = false;
        for (machine, fork) in [
            (LOGGER, self.logger.take()),
            (DEFAULT_LOGGER, self.default_logger.take()),
            (STDERR, self.stderr.take()),
        ] {
            if let Some(fork) = fork {
                failed |= !end(root, &assembler, machine, fork);
            }
        }
        if let Err(error) = render_unread(root, &self.renderer) {
            failed = true;
            eprintln!("error: could not render what the loggers left: {error}");
        }
        failed
    }
}

/// Interrupts the turn on the root's child `machine`, whose wait then ends,
/// and joins its fork. Returns whether it ended without error.
fn end<'scope>(
    root: &mut Machine<'scope, '_>,
    assembler: &Assembler,
    machine: &str,
    fork: Fork<'scope>,
) -> bool {
    let values = assembler.values();
    if let Err(error) = assembler.evaluation_runtime().interrupt(
        &MachinePath::from([machine]),
        values.text("the batch ended"),
    ) {
        eprintln!("error: could not interrupt the {machine} machine: {error}");
    }
    match root.join(fork) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("error: the {machine} machine ended in error: {error}");
            false
        }
    }
}

/// A runtime whose diagnostics wait at the logger, and probes of it.
#[cfg(test)]
pub(crate) mod test_support {
    use glam::{Assembler, Diagnostic, Runtime};

    use super::{DEFAULT_LOGGER, DIAGNOSTICS, LOGGER, hand_over, take_diagnostics, wire_logger};

    /// A runtime around `assembler`, wired as the CLI wires its own.
    pub(crate) fn logger_runtime(assembler: Assembler) -> Runtime {
        let mut runtime =
            glam::transition::runtime_around(assembler).expect("a runtime should open");
        wire_logger(&mut runtime).expect("the logger's wiring is valid");
        runtime
    }

    /// How many diagnostics wait at the logger's input.
    pub(crate) fn held_by_logger(runtime: &mut Runtime) -> usize {
        runtime
            .run(LOGGER, |logger| Ok(logger.port_available(DIAGNOSTICS)))
            .expect("the logger is idle")
    }

    /// Hands the logger's diagnostics to the default logger, as the CLI
    /// does when no logger is configured or the configured one ends.
    pub(crate) fn hand_over_logger(runtime: &mut Runtime) {
        runtime
            .run(LOGGER, hand_over)
            .expect("the logger hands over");
    }

    /// Takes the diagnostics waiting at the default logger.
    pub(crate) fn take_from_default_logger(runtime: &mut Runtime) -> Vec<Diagnostic> {
        let assembler = glam::transition::assembler(runtime).clone();
        runtime
            .run(DEFAULT_LOGGER, |default_logger| {
                take_diagnostics(default_logger, &assembler)
            })
            .expect("the default logger holds diagnostics")
    }

    /// Takes every diagnostic the logger holds, handing them over first.
    pub(crate) fn take_unread(runtime: &mut Runtime) -> Vec<Diagnostic> {
        hand_over_logger(runtime);
        take_from_default_logger(runtime)
    }
}

#[cfg(test)]
mod tests {
    use glam::{Assembler, Diagnostic, Severity};

    use super::test_support::{
        hand_over_logger, held_by_logger, logger_runtime, take_from_default_logger, take_unread,
    };
    use crate::DiagnosticBusLocal;
    use crate::rendering::rendered_text_for_test;

    #[test]
    fn the_logger_keeps_diagnostics_and_its_hand_over_keeps_their_order() {
        let assembler = Assembler::default();
        let mut runtime = logger_runtime(assembler.clone());
        let publish = |message: &str| {
            assembler.diagnostic_bus().publish_local(Diagnostic::new(
                &assembler.values(),
                Severity::Info,
                message,
            ));
        };
        publish("first");
        publish("second");
        assert_eq!(held_by_logger(&mut runtime), 2, "the logger keeps them");
        hand_over_logger(&mut runtime);
        assert_eq!(held_by_logger(&mut runtime), 0, "it handed them over");
        publish("third");
        let taken = take_from_default_logger(&mut runtime);
        assert_eq!(
            taken
                .iter()
                .map(|diagnostic| rendered_text_for_test(&assembler, diagnostic))
                .collect::<Vec<_>>(),
            ["first", "second", "third"],
            "what the logger held, then what arrived after"
        );
        assert!(take_from_default_logger(&mut runtime).is_empty());
    }

    #[test]
    fn bus_error_count_survives_absent_subscribers_and_logger_reads() {
        let assembler = Assembler::default();
        let mut runtime = logger_runtime(assembler.clone());
        let diagnostics = assembler.diagnostic_bus();
        let values = assembler.values();
        diagnostics.publish_local(Diagnostic::new(&values, Severity::Error, "unread"));
        assert_eq!(diagnostics.counts().errors(), 1);

        diagnostics.publish_local(Diagnostic::new(&values, Severity::Error, "read"));
        assert_eq!(take_unread(&mut runtime).len(), 2);
        assert_eq!(diagnostics.counts().errors(), 2);
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    fn assert_logger_run_owner_inventory(run: &LoggerRun<'_>) {
        let LoggerRun {
            logger: _,
            default_logger: _,
            stderr: _,
            rendered: _,
            renderer: _,
        } = run;
    }

    #[test]
    fn logger_run_owner_inventory_is_compile_exhaustive() {
        let _: fn(&LoggerRun<'_>) = assert_logger_run_owner_inventory;
    }
}

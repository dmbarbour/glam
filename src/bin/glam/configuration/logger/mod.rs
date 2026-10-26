use std::sync::Arc;
use std::thread;

use glam::reflection::{
    EffectLifecycle, EffectRun, EffectRunStop, ScheduledEffectRun, TaskOutcome,
};
use glam::{
    Assembler, Diagnostic, DiagnosticBus, DiagnosticSubscription, Endpoint, Error, Fork,
    HandleValue, Machine, Runtime, Values,
};

use crate::DiagnosticBusLocal;
use crate::configuration::{CLI, entry_context, select_entry, selects_nothing};
use crate::rendering::{DefaultLogger, port_diagnostic};

mod effects;
mod reports;

pub(crate) use effects::{LoggerTaskHost, MainEffects};
pub(crate) use reports::PublishedIssues;

/// The machine, a direct child of the root, to which the root sends the
/// diagnostics gathered at its input. A configured logger reads them from
/// the logger's input; the logger's child [`FALLBACK`] takes what it leaves,
/// and everything when none is configured.
pub(crate) const LOGGER: &str = "logger";

/// The logger's child, whose input holds diagnostics for the CLI to render
/// as [`DefaultLogger`] does.
pub(crate) const FALLBACK: &str = "fallback";

pub(crate) const DIAGNOSTICS: &str = "diagnostics";

/// The logger's output to [`FALLBACK`], for what its input holds when it
/// hands over.
const UNREAD: &str = "unread";

/// Wires the root's diagnostics to the logger, before anything is
/// published. The logger and its fallback each keep what reaches them, in
/// place of the default pass-through to their parents: the logger until a
/// configured logger reads it or the logger hands it to its fallback, and
/// the fallback until the CLI renders it.
pub(crate) fn wire_logger(runtime: &mut Runtime) -> Result<(), Error> {
    let keep = |machine: &mut Machine<'_, '_>| {
        machine.wire_del(Endpoint::input(DIAGNOSTICS), Endpoint::output(DIAGNOSTICS))
    };
    runtime.run([LOGGER, FALLBACK], keep)?;
    runtime.run(LOGGER, |logger| {
        keep(logger)?;
        logger.wire_add(
            Endpoint::output(UNREAD),
            Endpoint::child(FALLBACK, DIAGNOSTICS),
        )
    })?;
    runtime.run((), |root| {
        root.wire_add(
            Endpoint::input(DIAGNOSTICS),
            Endpoint::child(LOGGER, DIAGNOSTICS),
        )
    })
}

/// Starts the logger that the configuration selects, on a fork of the
/// root's child [`LOGGER`], or, where it selects none, hands the logger's
/// diagnostics to its fallback at once.
pub(crate) fn start_logger<'scope>(
    root: &mut Machine<'scope, '_>,
    assembler: &Assembler,
) -> Result<LoggerRun<'scope>, Error> {
    let renderer = Arc::new(DefaultLogger::new(assembler.clone()));
    let evaluation_runtime = assembler.evaluation_runtime();
    let diagnostics = DiagnosticBus::for_runtime(&evaluation_runtime);
    let subscription = diagnostics.subscribe(renderer.clone());
    let values = assembler.values();
    let custom = match root.run(CLI, select_logger) {
        Ok(custom) => custom,
        Err(error) => {
            let diagnostic = error
                .diagnostic(&values)
                .expect("configuration failure belongs to the assembler runtime");
            diagnostics.publish_local(diagnostic);
            None
        }
    };
    let task_diagnostics = diagnostics.clone();
    let task_values = evaluation_runtime.values();
    // The configured logger's environment as the logger machine sees it,
    // taken once, since the logger is not yet a turn.
    let environment = root.run(LOGGER, |logger| {
        Ok(glam::transition::reflection_environment(logger, "logger"))
    })?;
    let scheduled = custom.and_then(|custom| {
        let lifecycle = EffectLifecycle::new_closing_session(&evaluation_runtime);
        let host = Arc::new(LoggerTaskHost::new(
            &evaluation_runtime,
            diagnostics.clone(),
            environment,
        ));
        let run = EffectRun::new(
            &evaluation_runtime,
            &custom,
            MainEffects::new(values.clone()),
            host.clone(),
        )
        .contextualizing_failures(
            entry_context(&task_values, "log").expect("configuration context is local"),
        );
        match run.and_then(|run| {
            run.asserting_unit_result("configured logger result")
                .requiring_unit_result()
                .schedule(&lifecycle)
        }) {
            Ok(task) => Some((task, host)),
            Err(error) => {
                task_diagnostics.publish_local(
                    error
                        .with_context(
                            &task_values,
                            entry_context(&task_values, "log")
                                .expect("configuration context is local"),
                        )
                        .diagnostic(&task_values),
                );
                None
            }
        }
    });
    let Some((task, host)) = scheduled else {
        root.run(LOGGER, hand_over)?;
        return Ok(LoggerRun {
            thread: None,
            stop: None,
            diagnostics,
            renderer,
            _subscription: subscription,
        });
    };
    let fork_renderer = renderer.clone();
    let stop = task.stopper();
    let builder = thread::Builder::new()
        .name("glam-logger".to_owned())
        // The logger evaluates ordinary Glam configuration and therefore
        // needs the same practical stack headroom as the process main thread.
        // Deep evaluator paths are being migrated to explicit machines, but
        // the bootstrap must not make a configured logger depend on the
        // platform's smaller default child-thread stack in the meantime.
        .stack_size(8 * 1024 * 1024);
    let thread = root.fork_with(builder, LOGGER, move |logger| {
        run_configured_logger(task, &host, &task_diagnostics, &task_values);
        // What the configured logger left, and what arrives later, is the
        // fallback's.
        hand_over(logger)?;
        drain_fallback(logger, &fork_renderer)
    })?;
    Ok(LoggerRun {
        thread: Some(thread),
        stop: Some(stop),
        diagnostics,
        renderer,
        _subscription: subscription,
    })
}

/// Runs the configured logger to its end, delivering what it commits on
/// this client thread, wherever the task runs, so rendering stays off the
/// workers. Its failures go to its own bus.
fn run_configured_logger(
    task: ScheduledEffectRun,
    host: &LoggerTaskHost,
    diagnostics: &DiagnosticBus,
    values: &Values,
) {
    match task.run_with(|| host.deliver()) {
        // Only the batch stops the logger: at its end, where the logger
        // waits on its input, idle, or halting it, with a diagnostic of its
        // own, for leaving its input unread (`quiescence-is-analyzed`).
        Ok(TaskOutcome::Complete(_) | TaskOutcome::Cancelled) => {}
        // A failure is the logger's own, which its caller reports.
        Err(error) => {
            diagnostics.publish_local(error.diagnostic(values));
        }
    }
}

/// Hands what the logger's input holds, and what reaches it later, to its
/// fallback, in one cut, so the fallback receives every diagnostic in
/// order: the logger wires its input on to the fallback, and sends what the
/// input holds through its unread output.
fn hand_over(logger: &mut Machine<'_, '_>) -> Result<(), Error> {
    logger.cut(|logger| {
        logger.wire_add(
            Endpoint::input(DIAGNOSTICS),
            Endpoint::child(FALLBACK, DIAGNOSTICS),
        )?;
        let unread = logger.port_available(DIAGNOSTICS);
        logger.port_read(DIAGNOSTICS, unread)?;
        logger.port_write(UNREAD, unread)
    })
}

/// Takes the diagnostics waiting at the logger's fallback, in order, as its
/// renderer, `assembler`'s, reads them.
pub(crate) fn take_fallback(
    logger: &mut Machine<'_, '_>,
    assembler: &Assembler,
) -> Result<Vec<Diagnostic>, Error> {
    let held = logger.run(FALLBACK, |fallback| {
        let count = fallback.port_available(DIAGNOSTICS);
        fallback.port_read(DIAGNOSTICS, count)?;
        let mut held = (0..count)
            .map(|_| glam::transition::pop_value(fallback))
            .collect::<Result<Vec<_>, _>>()?;
        held.reverse();
        Ok(held)
    })?;
    held.into_iter()
        .map(|message| port_diagnostic(assembler, message))
        .collect()
}

/// Renders the diagnostics waiting at the logger's fallback.
pub(crate) fn drain_fallback(
    logger: &mut Machine<'_, '_>,
    renderer: &DefaultLogger,
) -> Result<(), Error> {
    for diagnostic in take_fallback(logger, renderer.assembler())? {
        renderer.emit(&diagnostic);
    }
    Ok(())
}

/// Renders every diagnostic the logger holds, at the end: what its input
/// holds, which it hands over first, and what waits at its fallback.
pub(crate) fn render_unread(
    logger: &mut Machine<'_, '_>,
    renderer: &DefaultLogger,
) -> Result<(), Error> {
    hand_over(logger)?;
    drain_fallback(logger, renderer)
}

/// The logger `conf.log` selects, evaluated, or none.
fn select_logger(m: &mut Machine<'_, '_>) -> Result<Option<HandleValue>, Error> {
    m.list_pack(0)?;
    m.local_get("configuration")?;
    select_entry(m, "log", &["conf", "log"])?;
    m.eval()?;
    if selects_nothing(m)? {
        m.discard(1)?;
        return Ok(None);
    }
    glam::transition::pop_value(m).map(Some)
}

pub(crate) struct LoggerRun<'scope> {
    /// The configured logger's fork, where one runs.
    pub(crate) thread: Option<Fork<'scope>>,
    /// Stops the configured logger's run, where one runs.
    pub(crate) stop: Option<EffectRunStop>,
    /// What the configured logger reports, which `renderer` renders.
    pub(crate) diagnostics: DiagnosticBus,
    pub(crate) renderer: Arc<DefaultLogger>,
    _subscription: DiagnosticSubscription,
}

/// A runtime whose diagnostics wait at the logger, and probes of it.
#[cfg(test)]
pub(crate) mod test_support {
    use glam::{Assembler, Diagnostic, Runtime};

    use super::{DIAGNOSTICS, LOGGER, hand_over, take_fallback, wire_logger};

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

    /// Hands the logger's diagnostics to its fallback, as the CLI does
    /// when no logger is configured or the configured one ends.
    pub(crate) fn hand_over_logger(runtime: &mut Runtime) {
        runtime
            .run(LOGGER, hand_over)
            .expect("the logger hands over");
    }

    /// Takes the diagnostics waiting at the logger's fallback.
    pub(crate) fn take_from_fallback(runtime: &mut Runtime) -> Vec<Diagnostic> {
        let assembler = glam::transition::assembler(runtime).clone();
        runtime
            .run(LOGGER, |logger| take_fallback(logger, &assembler))
            .expect("the fallback holds diagnostics")
    }

    /// Takes every diagnostic the logger holds, handing them over first.
    pub(crate) fn take_unread(runtime: &mut Runtime) -> Vec<Diagnostic> {
        hand_over_logger(runtime);
        take_from_fallback(runtime)
    }
}

#[cfg(test)]
mod tests {
    use glam::{Assembler, Diagnostic, Severity};

    use super::test_support::{
        hand_over_logger, held_by_logger, logger_runtime, take_from_fallback, take_unread,
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
        let taken = take_from_fallback(&mut runtime);
        assert_eq!(
            taken
                .iter()
                .map(|diagnostic| rendered_text_for_test(&assembler, diagnostic))
                .collect::<Vec<_>>(),
            ["first", "second", "third"],
            "what the logger held, then what arrived after"
        );
        assert!(take_from_fallback(&mut runtime).is_empty());
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
            thread: _,
            stop: _,
            diagnostics: _,
            renderer: _,
            _subscription: _,
        } = run;
    }

    #[test]
    fn logger_run_owner_inventory_is_compile_exhaustive() {
        let _: fn(&LoggerRun<'_>) = assert_logger_run_owner_inventory;
    }
}

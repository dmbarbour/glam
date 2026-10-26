//! A host's interrupt reaches turns at their waits (`quiescence-is-analyzed`):
//! a turn with a run in progress at or below the machine it names fails its
//! next wait with the interrupt, and its result decides whether its machine
//! is in error.

use std::cell::Cell;
use std::sync::mpsc;

use glam::reflection::EffectRequestSpec;
use glam::{
    Answer, EffectProvider, Error, ErrorKind, Machine, MachinePath, Runtime, RuntimeConfig,
};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// A cut that waits until the heap defines `go`. It sends `name` as its
/// first attempt is undone, before it waits.
fn gate(
    m: &mut Machine<'_, '_>,
    waiting: &mpsc::Sender<&'static str>,
    name: &'static str,
) -> Result<(), Error> {
    let first = Cell::new(true);
    m.cut(|m| {
        if first.replace(false) {
            let waiting = waiting.clone();
            m.on_abort(0, move |_| {
                let _ = waiting.send(name);
            })?;
        }
        m.heap_try_get(["go"])?;
        m.discard(1)
    })
}

#[test]
fn an_interrupt_reaches_the_runs_in_progress_at_or_below_its_machine() {
    // A run in progress below the machine when the interrupt is posted is
    // reached, as is what it runs within itself; one elsewhere, or entered
    // after, waits on. A reached turn that handles the error leaves its
    // machine as it was; one that ends with it leaves its machine in error.
    let mut rt = runtime();
    let evaluation = glam::transition::assembler(&rt).evaluation_runtime();
    let values = glam::transition::assembler(&rt).values();
    let (waiting, waited) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    rt.run("p", |p| {
        let reached_waiting = waiting.clone();
        let reached = p.fork(["b", "c"], move |c| {
            let error = gate(c, &reached_waiting, "c").expect_err("the interrupt ends the wait");
            assert_eq!(error.kind(), ErrorKind::Interrupted);
            // Its signal goes nowhere: the parent waits on others.
            let (quiet, _) = mpsc::channel();
            let nested = c.run("x", |x| gate(x, &quiet, "x"));
            assert_eq!(
                nested
                    .expect_err("a run within a reached one is reached")
                    .kind(),
                ErrorKind::Interrupted
            );
            released.recv().expect("the parent releases the fork");
            Ok(())
        })?;
        let elsewhere_waiting = waiting.clone();
        let elsewhere = p.fork("e", move |e| gate(e, &elsewhere_waiting, "e"))?;
        let mut blocked = [waited.recv().unwrap(), waited.recv().unwrap()];
        blocked.sort_unstable();
        assert_eq!(blocked, ["c", "e"], "both wait before the interrupt");

        evaluation.interrupt(&MachinePath::from(["p", "b"]), values.text("stop b"))?;
        let later_waiting = waiting.clone();
        let later = p.fork(["b", "d"], move |d| gate(d, &later_waiting, "d"))?;
        assert_eq!(waited.recv().unwrap(), "d", "entered after, it waits");
        p.int_push(1);
        p.heap_set(["go"])?;
        // Each fork joins before any outcome is judged, so that a failure
        // leaves none waiting.
        let later = p.join(later);
        let elsewhere = p.join(elsewhere);
        release.send(()).expect("the fork listens");
        let reached = p.join(reached);
        later.expect("entered after the interrupt, it is not reached");
        elsewhere.expect("elsewhere, it is not reached");
        reached?;
        assert!(!p.failed(["b", "c"])?, "the reached turn handled the error");
        assert!(p.failed(["b", "c", "x"])?, "the nested turn ended with it");
        Ok(())
    })
    .expect("the turn should succeed");
}

/// No requests of its own: the effect's host families pass through.
struct NoRequests;

impl EffectProvider for NoRequests {
    type Request = ();

    fn requests(&self) -> Vec<EffectRequestSpec<()>> {
        Vec::new()
    }

    fn answer(&mut self, _: &mut Machine<'_, '_>, (): &()) -> Result<Answer, Error> {
        unreachable!("the effect makes no provider requests")
    }
}

#[test]
fn an_interrupt_ends_a_blocking_read_an_awaited_promise_and_an_effect_between_quanta() {
    let mut rt = runtime();
    let evaluation = glam::transition::assembler(&rt).evaluation_runtime();
    let values = glam::transition::assembler(&rt).values();
    rt.run("p", |p| {
        let reader = p.fork("r", |r| {
            let error = r.port_read("in", 1).expect_err("nothing arrives");
            assert_eq!(error.kind(), ErrorKind::Interrupted);
            Ok(())
        })?;
        let spinner = p.fork("s", |s| {
            s.dict_pack(Vec::<&str>::new())?;
            s.dict_pack(Vec::<&str>::new())?;
            s.script_object(
                ["spin"],
                "g",
                "language g0\nimport 'std\nspin n = spin n\neffect = seq (spin 0) (.r ())\n",
            )?;
            s.dict_get("effect")?;
            let error = s
                .effect_run(&mut NoRequests)
                .expect_err("the effect never ends");
            assert_eq!(error.kind(), ErrorKind::Interrupted);
            Ok(())
        })?;
        p.promise_new();
        p.swap()?;
        p.give("v", 1)?;
        let awaiting = p.fork("v", |v| {
            let error = v
                .cut(|v| {
                    v.dup(1)?;
                    v.eval()?;
                    v.discard(1)
                })
                .expect_err("nothing resolves the promise");
            assert_eq!(error.kind(), ErrorKind::Interrupted);
            Ok(())
        })?;
        for machine in ["r", "s", "v"] {
            evaluation.interrupt(&MachinePath::from(["p", machine]), values.text("stop"))?;
        }
        p.join(reader)?;
        p.join(spinner)?;
        p.join(awaiting)?;
        p.discard(1)
    })
    .expect("the turn should succeed");
}

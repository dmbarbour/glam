//! Ports: machines write and read named ports, wired from outside by their
//! ancestors, with buffers in the runtime's transactional state
//! (`machines-communicate-through-wired-ports`).

use std::cell::Cell;
use std::sync::mpsc;

use glam::{DiagnosticsWiring, Endpoint, Error, ErrorKind, Machine, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// Writes the integers `values` to the output `port` of the descendant at
/// `path`, in a turn on it.
fn send(m: &mut Machine<'_, '_>, path: &str, port: &str, values: &[i64]) -> Result<(), Error> {
    m.run(path, |w| {
        for value in values {
            w.push_int(*value);
        }
        w.write(port, values.len())
    })
}

/// Reads `n` integers from the input `port` of the descendant at `path`, in
/// a turn on it, in the order they arrived.
fn receive(m: &mut Machine<'_, '_>, path: &str, port: &str, n: usize) -> Result<Vec<i64>, Error> {
    m.run(path, |r| {
        r.read(port, n)?;
        let mut values = (0..n).map(|_| r.pop_int()).collect::<Result<Vec<_>, _>>()?;
        values.reverse();
        Ok(values)
    })
}

#[test]
fn wired_siblings_send_values_in_order() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1, 2])?;
        send(p, "a", "out", &[3])?;
        assert_eq!(receive(p, "b", "in", 3)?, [1, 2, 3]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_write_to_an_unconnected_output_is_an_error_and_pops_nothing() {
    let mut rt = runtime();
    rt.run("a", |a| {
        assert!(!a.output_connected("out"));
        a.push_int(1);
        let error = a.write("out", 1).expect_err("nothing is wired");
        assert_eq!(error.kind(), ErrorKind::Failure, "structure, not pushback");
        assert!(error.to_string().contains("not connected"), "{error}");
        assert_eq!(a.depth(), 1, "the write popped nothing");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn several_wires_from_an_output_duplicate_and_several_into_an_input_merge() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.wire(Endpoint::child("a", "out"), Endpoint::child("c", "in"))?;
        p.wire(Endpoint::child("d", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1])?;
        send(p, "d", "out", &[2])?;
        send(p, "a", "out", &[3])?;
        assert_eq!(
            receive(p, "b", "in", 3)?,
            [1, 2, 3],
            "merged in commit order"
        );
        assert_eq!(receive(p, "c", "in", 2)?, [1, 3], "each wire delivers");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_switchboard_passes_its_input_through_without_taking_turns() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.run("s", |s| {
            s.wire(Endpoint::input("in"), Endpoint::output("out"))
        })?;
        p.wire(Endpoint::child("a", "out"), Endpoint::child("s", "in"))?;
        p.wire(Endpoint::child("s", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[7, 8])?;
        let held = p.run("s", |s| Ok(s.available("in")))?;
        assert_eq!(held, 0, "the switchboard holds nothing");
        assert_eq!(receive(p, "b", "in", 2)?, [7, 8]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn connections_are_read_from_the_wiring() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        let (out, into) = p.run("a", |a| {
            Ok((a.output_connected("out"), a.input_connected("out")))
        })?;
        assert_eq!((out, into), (true, false), "a port name has two roles");
        assert!(p.run("b", |b| Ok(b.input_connected("in")))?);
        p.unwire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        assert!(!p.run("a", |a| Ok(a.output_connected("out")))?);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_wiring_loop_is_an_error() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.run("s", |s| {
            s.wire(Endpoint::input("in"), Endpoint::output("out"))
        })?;
        p.wire(Endpoint::child("s", "out"), Endpoint::child("s", "in"))?;
        p.wire(Endpoint::child("a", "out"), Endpoint::child("s", "in"))?;
        let error = send(p, "a", "out", &[1]).expect_err("the wiring loops");
        assert!(error.to_string().contains("loops"), "{error}");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn reading_too_few_is_pushback() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1])?;
        let error = p
            .run("b", |b| b.try_read("in", 2))
            .expect_err("one value is too few");
        assert_eq!(error.kind(), ErrorKind::Retry);
        p.recover("b", |_| Ok(()))?;
        assert_eq!(
            receive(p, "b", "in", 1)?,
            [1],
            "the short read took nothing"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_read_outside_a_cut_waits_for_its_values() {
    let mut rt = runtime();
    let (reading, ready) = mpsc::channel();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        let reader = p.fork("b", move |b| {
            assert_eq!(
                b.try_read("in", 1).map_err(|error| error.kind()),
                Err(ErrorKind::Retry),
                "nothing is there yet"
            );
            reading.send(()).expect("the writer listens");
            b.read("in", 1)
        })?;
        ready.recv().expect("the reader found nothing first");
        send(p, "a", "out", &[5])?;
        p.join(reader)?;
        p.take("b", 1)?;
        assert_eq!(p.pop_int()?, 5);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_waits_out_pushback_until_values_arrive() {
    let mut rt = runtime();
    let (waiting, blocked) = mpsc::channel::<()>();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        let reader = p.fork("b", move |b| {
            let attempts = Cell::new(0);
            b.cut(|b| {
                attempts.set(attempts.get() + 1);
                // Signals the writer as the pushback undoes the cut, before
                // the cut waits, forcing the write after the read.
                let waiting = waiting.clone();
                b.on_abort(0, move |_| waiting.send(()).expect("the writer listens"))?;
                b.read("in", 1)
            })?;
            b.push_int(attempts.get());
            Ok(())
        })?;
        blocked.recv().expect("the reader met pushback first");
        send(p, "a", "out", &[9])?;
        p.join(reader)?;
        p.take("b", 2)?;
        let attempts = p.pop_int()?;
        assert_eq!((p.pop_int()?, attempts), (9, 2), "the cut ran again once");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_sends_when_it_commits_and_a_failed_cut_sends_nothing() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.run("a", |a| {
            let failed: Result<(), Error> = a.cut(|a| {
                a.push_int(1);
                a.write("out", 1)?;
                Err(Error::new("the cut errs"))
            });
            assert!(failed.is_err());
            a.cut(|a| {
                a.push_int(2);
                a.write("out", 1)
            })
        })?;
        assert_eq!(p.run("b", |b| Ok(b.available("in")))?, 1);
        assert_eq!(receive(p, "b", "in", 1)?, [2]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_read_sees_committed_values_only() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("a", "in"))?;
        let attempted = p.run("a", |a| -> Result<(), Error> {
            a.cut(|a| {
                a.push_int(4);
                a.write("out", 1)?;
                let error = a
                    .try_read("in", 1)
                    .expect_err("its own write is not there yet");
                assert_eq!(error.kind(), ErrorKind::Retry);
                Ok(())
            })
        });
        attempted?;
        assert_eq!(receive(p, "a", "in", 1)?, [4], "after the commit it is");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn rewiring_within_a_failed_cut_is_undone() {
    let mut rt = runtime();
    rt.run("p", |p| {
        let failed: Result<(), Error> = p.cut(|p| {
            p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
            Err(Error::new("the cut errs"))
        });
        assert!(failed.is_err());
        assert!(!p.run("a", |a| Ok(a.output_connected("out")))?);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_fork_routing_by_wiring_its_cut_changed_after_the_fork_runs_again() {
    let mut rt = runtime();
    let attempts = Cell::new(0);
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.cut(|p| {
            attempts.set(attempts.get() + 1);
            let (sent, written) = mpsc::channel();
            let writer = p.fork("a", move |a| {
                a.push_int(42);
                a.write("out", 1)?;
                sent.send(()).expect("the cut listens");
                Ok(())
            })?;
            written.recv().expect("the fork routed its write");
            if attempts.get() == 1 {
                // After the fork routed by it, the cut changes the wiring.
                p.wire(Endpoint::child("a", "out"), Endpoint::child("c", "in"))?;
            }
            p.join(writer)
        })?;
        assert_eq!(attempts.get(), 2, "the inconsistent attempt ran again");
        assert_eq!(receive(p, "b", "in", 1)?, [42]);
        assert_eq!(
            p.run("c", |c| Ok(c.available("in")))?,
            0,
            "the first attempt's wire went with it"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn retry_is_pushback_that_a_cut_waits_out() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let unwakeable: Result<(), Error> = m.cut(|m| Err(m.retry()));
        let error = unwakeable.expect_err("nothing could wake the cut");
        assert!(error.to_string().contains("without observing"), "{error}");
        assert_eq!(m.retry().kind(), ErrorKind::Retry);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_machine_feeds_a_child_and_collects_from_one_through_its_own_ports() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::output("events"), Endpoint::child("b", "in"))?;
        p.wire(Endpoint::child("a", "out"), Endpoint::input("collected"))?;
        p.push_int(1);
        p.write("events", 1)?;
        assert_eq!(
            receive(p, "b", "in", 1)?,
            [1],
            "its own output fed the child"
        );
        send(p, "a", "out", &[2])?;
        p.read("collected", 1)?;
        assert_eq!(p.pop_int()?, 2, "its own input collected from the child");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn pending_lists_the_inputs_that_hold_values() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::input("x"))?;
        p.wire(Endpoint::child("b", "out"), Endpoint::input("y"))?;
        assert!(p.pending().is_empty());
        send(p, "b", "out", &[1, 2])?;
        send(p, "a", "out", &[3])?;
        let pending = p.pending();
        assert_eq!(pending, [("x".into(), 1), ("y".into(), 2)], "in port order");
        p.read("y", 2)?;
        p.discard(2)?;
        assert_eq!(p.pending(), [("x".into(), 1)]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_that_finds_nothing_pending_wakes_when_any_input_receives() {
    let mut rt = runtime();
    let (waiting, blocked) = mpsc::channel::<()>();
    rt.run("p", |p| {
        p.wire(
            Endpoint::child("a", "out"),
            Endpoint::child("server", "requests"),
        )?;
        let server = p.fork("server", move |s| {
            let port = s.cut(|s| {
                let Some((port, _)) = s.pending().into_iter().next() else {
                    let waiting = waiting.clone();
                    s.on_abort(0, move |_| waiting.send(()).expect("the client listens"))?;
                    return Err(s.retry());
                };
                Ok(port)
            })?;
            s.read(port, 1)
        })?;
        blocked.recv().expect("the server found nothing first");
        // A port that has never held a value.
        send(p, "a", "out", &[6])?;
        p.join(server)?;
        p.take("server", 1)?;
        assert_eq!(p.pop_int()?, 6);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn diagnostics_gather_at_the_root_by_default_and_a_machine_may_intervene() {
    let mut rt = runtime();
    rt.run("p", |p| send(p, "c", "diagnostics", &[1]))
        .expect("the default wiring connects every diagnostics output");
    let gathered = rt
        .run((), |root| {
            root.read("diagnostics", 1)?;
            root.pop_int()
        })
        .expect("the root's input holds the diagnostics");
    assert_eq!(gathered, 1);

    rt.run("p", |p| {
        // The machine keeps its children's diagnostics, to read itself.
        p.unwire(
            Endpoint::input("diagnostics"),
            Endpoint::output("diagnostics"),
        )?;
        send(p, "c", "diagnostics", &[2])?;
        p.read("diagnostics", 1)?;
        assert_eq!(p.pop_int()?, 2);
        Ok(())
    })
    .expect("the turn should succeed");
    let at_root = rt
        .run((), |root| Ok(root.available("diagnostics")))
        .expect("a turn");
    assert_eq!(at_root, 0, "nothing passed through");

    let mut quiet = Runtime::open(RuntimeConfig::new().diagnostics_wiring(DiagnosticsWiring::None))
        .expect("a runtime should open");
    quiet
        .run("c", |c| {
            c.push_int(3);
            let error = c.write("diagnostics", 1).expect_err("nothing is wired");
            assert!(error.to_string().contains("not connected"), "{error}");
            Ok(())
        })
        .expect("the turn should succeed");
}

#[test]
fn clearing_a_machine_resets_its_ports_and_its_descendants() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.run("b", |b| b.wire(Endpoint::input("x"), Endpoint::output("y")))?;
        send(p, "a", "out", &[1])?;
        p.run("b", |b| {
            b.push_int(9);
            b.clear()?;
            assert_eq!(b.available("in"), 0, "its input is empty");
            assert!(
                !b.output_connected("y"),
                "its wiring is back to the defaults"
            );
            Ok(())
        })?;
        send(p, "a", "out", &[2])?;
        assert_eq!(
            receive(p, "b", "in", 1)?,
            [2],
            "the parent's wire to it stays, and later arrivals stay"
        );
        send(p, "a", "out", &[3])?;
        p.clear()?;
        let error = send(p, "a", "out", &[4]).expect_err("the machine's own wires went");
        assert!(error.to_string().contains("not connected"), "{error}");
        assert_eq!(
            p.run("b", |b| Ok(b.available("in")))?,
            0,
            "so did its children's inputs"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_failed_cut_restores_the_ports_it_cleared() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[5])?;
        let failed: Result<(), Error> = p.cut(|p| {
            p.clear()?;
            Err(Error::new("the cut errs"))
        });
        assert!(failed.is_err());
        assert_eq!(
            receive(p, "b", "in", 1)?,
            [5],
            "the input and the wire are back"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn the_walk_counts_port_state_as_presence() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1])?;
        let below: Vec<_> = p.machines(())?.map(|(path, _)| path).collect();
        assert_eq!(
            below,
            [glam::MachinePath::from(["b"])],
            "b waits with input though it never took a turn; a holds nothing"
        );
        Ok(())
    })
    .expect("the turn should succeed");
    let walked: Vec<_> = rt.machines(()).map(|(path, _)| path).collect();
    assert_eq!(
        walked,
        [
            glam::MachinePath::from(["p"]),
            glam::MachinePath::from(["p", "b"])
        ],
        "p owns wiring, though its state is empty"
    );
}

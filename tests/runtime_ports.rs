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
            w.int_push(*value);
        }
        w.port_write(port, values.len())
    })
}

/// Reads `n` integers from the input `port` of the descendant at `path`, in
/// a turn on it, in the order they arrived.
fn receive(m: &mut Machine<'_, '_>, path: &str, port: &str, n: usize) -> Result<Vec<i64>, Error> {
    m.run(path, |r| {
        r.port_read(port, n)?;
        let mut values = (0..n).map(|_| r.int_pop()).collect::<Result<Vec<_>, _>>()?;
        values.reverse();
        Ok(values)
    })
}

#[test]
fn wired_siblings_send_values_in_order() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
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
        assert!(!a.port_output_connected("out"));
        a.int_push(1);
        let error = a.port_write("out", 1).expect_err("nothing is wired");
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
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("c", "in"))?;
        p.wire_add(Endpoint::child("d", "out"), Endpoint::child("b", "in"))?;
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
            s.wire_add(Endpoint::input("in"), Endpoint::output("out"))
        })?;
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("s", "in"))?;
        p.wire_add(Endpoint::child("s", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[7, 8])?;
        let held = p.run("s", |s| Ok(s.port_available("in")))?;
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
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        let (out, into) = p.run("a", |a| {
            Ok((
                a.port_output_connected("out"),
                a.port_input_connected("out"),
            ))
        })?;
        assert_eq!((out, into), (true, false), "a port name has two roles");
        assert!(p.run("b", |b| Ok(b.port_input_connected("in")))?);
        p.wire_del(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        assert!(!p.run("a", |a| Ok(a.port_output_connected("out")))?);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_wiring_loop_is_an_error() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.run("s", |s| {
            s.wire_add(Endpoint::input("in"), Endpoint::output("out"))
        })?;
        p.wire_add(Endpoint::child("s", "out"), Endpoint::child("s", "in"))?;
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("s", "in"))?;
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
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1])?;
        let error = p
            .run("b", |b| b.port_try_read("in", 2))
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
fn reading_none_succeeds_at_any_input() {
    let mut rt = runtime();
    rt.run("p", |p| {
        assert_eq!(
            receive(p, "b", "never", 0)?,
            [] as [i64; 0],
            "an input that never held a value"
        );
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1])?;
        assert_eq!(receive(p, "b", "in", 1)?, [1]);
        p.run("b", |b| b.cut(|b| b.port_read("in", 0)))?;
        assert_eq!(
            receive(p, "b", "in", 0)?,
            [] as [i64; 0],
            "an input it emptied, in a cut and outside one"
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
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        let reader = p.fork("b", move |b| {
            assert_eq!(
                b.port_try_read("in", 1).map_err(|error| error.kind()),
                Err(ErrorKind::Retry),
                "nothing is there yet"
            );
            reading.send(()).expect("the writer listens");
            b.port_read("in", 1)
        })?;
        ready.recv().expect("the reader found nothing first");
        send(p, "a", "out", &[5])?;
        p.join(reader)?;
        p.take("b", 1)?;
        assert_eq!(p.int_pop()?, 5);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_waits_out_pushback_until_values_arrive() {
    let mut rt = runtime();
    let (waiting, blocked) = mpsc::channel::<()>();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        let reader = p.fork("b", move |b| {
            let attempts = Cell::new(0);
            b.cut(|b| {
                attempts.set(attempts.get() + 1);
                // Signals the writer as the pushback undoes the cut, before
                // the cut waits, forcing the write after the read.
                let waiting = waiting.clone();
                b.on_abort(0, move |_| waiting.send(()).expect("the writer listens"))?;
                b.port_read("in", 1)
            })?;
            b.int_push(attempts.get());
            Ok(())
        })?;
        blocked.recv().expect("the reader met pushback first");
        send(p, "a", "out", &[9])?;
        p.join(reader)?;
        p.take("b", 2)?;
        let attempts = p.int_pop()?;
        assert_eq!((p.int_pop()?, attempts), (9, 2), "the cut ran again once");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_sends_when_it_commits_and_a_failed_cut_sends_nothing() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.run("a", |a| {
            let failed: Result<(), Error> = a.cut(|a| {
                a.int_push(1);
                a.port_write("out", 1)?;
                Err(Error::new("the cut errs"))
            });
            assert!(failed.is_err());
            a.cut(|a| {
                a.int_push(2);
                a.port_write("out", 1)
            })
        })?;
        assert_eq!(p.run("b", |b| Ok(b.port_available("in")))?, 1);
        assert_eq!(receive(p, "b", "in", 1)?, [2]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_read_sees_committed_values_only() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("a", "in"))?;
        let attempted = p.run("a", |a| -> Result<(), Error> {
            a.cut(|a| {
                a.int_push(4);
                a.port_write("out", 1)?;
                let error = a
                    .port_try_read("in", 1)
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
fn a_cut_may_enter_a_child_it_sent_input_before_the_input_arrives() {
    // Input a cut sends a child waits in a buffer, which arrives as the cut
    // commits, for a turn to read: the child has no driver of its own, so
    // an entry within the cut overtakes nothing. A read there of what the
    // cut sent is pushback, as a machine's read of its own write is.
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::output("events"), Endpoint::child("b", "in"))?;
        p.cut(|p| {
            p.int_push(1);
            p.port_write("events", 1)?;
            p.int_push(2);
            p.give("b", 1)?;
            p.run("b", |b| {
                let error = b.port_try_read("in", 1).expect_err("not there yet");
                assert_eq!(error.kind(), ErrorKind::Retry);
                assert_eq!(b.int_pop()?, 2, "the value given within the cut");
                Ok(())
            })
        })?;
        assert_eq!(receive(p, "b", "in", 1)?, [1], "after the commit it is");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn rewiring_within_a_failed_cut_is_undone() {
    let mut rt = runtime();
    rt.run("p", |p| {
        let failed: Result<(), Error> = p.cut(|p| {
            p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
            Err(Error::new("the cut errs"))
        });
        assert!(failed.is_err());
        assert!(!p.run("a", |a| Ok(a.port_output_connected("out")))?);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_fork_routing_by_wiring_its_cut_changed_after_the_fork_runs_again() {
    let mut rt = runtime();
    let attempts = Cell::new(0);
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.cut(|p| {
            attempts.set(attempts.get() + 1);
            let (sent, written) = mpsc::channel();
            let writer = p.fork("a", move |a| {
                a.int_push(42);
                a.port_write("out", 1)?;
                sent.send(()).expect("the cut listens");
                Ok(())
            })?;
            written.recv().expect("the fork routed its write");
            if attempts.get() == 1 {
                // After the fork routed by it, the cut changes the wiring.
                p.wire_add(Endpoint::child("a", "out"), Endpoint::child("c", "in"))?;
            }
            p.join(writer)
        })?;
        assert_eq!(attempts.get(), 2, "the inconsistent attempt ran again");
        assert_eq!(receive(p, "b", "in", 1)?, [42]);
        assert_eq!(
            p.run("c", |c| Ok(c.port_available("in")))?,
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
        p.wire_add(Endpoint::output("events"), Endpoint::child("b", "in"))?;
        p.wire_add(Endpoint::child("a", "out"), Endpoint::input("collected"))?;
        p.int_push(1);
        p.port_write("events", 1)?;
        assert_eq!(
            receive(p, "b", "in", 1)?,
            [1],
            "its own output fed the child"
        );
        send(p, "a", "out", &[2])?;
        p.port_read("collected", 1)?;
        assert_eq!(p.int_pop()?, 2, "its own input collected from the child");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn pending_lists_the_inputs_that_hold_values() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::input("x"))?;
        p.wire_add(Endpoint::child("b", "out"), Endpoint::input("y"))?;
        assert!(p.port_pending().is_empty());
        send(p, "b", "out", &[1, 2])?;
        send(p, "a", "out", &[3])?;
        let pending = p.port_pending();
        assert_eq!(pending, [("x".into(), 1), ("y".into(), 2)], "in port order");
        p.port_read("y", 2)?;
        p.discard(2)?;
        assert_eq!(p.port_pending(), [("x".into(), 1)]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_that_finds_nothing_pending_wakes_when_any_input_receives() {
    let mut rt = runtime();
    let (waiting, blocked) = mpsc::channel::<()>();
    rt.run("p", |p| {
        p.wire_add(
            Endpoint::child("a", "out"),
            Endpoint::child("server", "requests"),
        )?;
        let server = p.fork("server", move |s| {
            let port = s.cut(|s| {
                let Some((port, _)) = s.port_pending().into_iter().next() else {
                    let waiting = waiting.clone();
                    s.on_abort(0, move |_| waiting.send(()).expect("the client listens"))?;
                    return Err(s.retry());
                };
                Ok(port)
            })?;
            s.port_read(port, 1)
        })?;
        blocked.recv().expect("the server found nothing first");
        // A port that has never held a value.
        send(p, "a", "out", &[6])?;
        p.join(server)?;
        p.take("server", 1)?;
        assert_eq!(p.int_pop()?, 6);
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
            root.port_read("diagnostics", 1)?;
            root.int_pop()
        })
        .expect("the root's input holds the diagnostics");
    assert_eq!(gathered, 1);

    rt.run("p", |p| {
        // The machine keeps its children's diagnostics, to read itself.
        p.wire_del(
            Endpoint::input("diagnostics"),
            Endpoint::output("diagnostics"),
        )?;
        send(p, "c", "diagnostics", &[2])?;
        p.port_read("diagnostics", 1)?;
        assert_eq!(p.int_pop()?, 2);
        Ok(())
    })
    .expect("the turn should succeed");
    let at_root = rt
        .run((), |root| Ok(root.port_available("diagnostics")))
        .expect("a turn");
    assert_eq!(at_root, 0, "nothing passed through");

    let mut quiet = Runtime::open(RuntimeConfig::new().diagnostics_wiring(DiagnosticsWiring::None))
        .expect("a runtime should open");
    quiet
        .run("c", |c| {
            c.int_push(3);
            let error = c
                .port_write("diagnostics", 1)
                .expect_err("nothing is wired");
            assert!(error.to_string().contains("not connected"), "{error}");
            Ok(())
        })
        .expect("the turn should succeed");
}

#[test]
fn a_machine_reports_its_own_wiring_with_the_default_rule() {
    let mut rt = runtime();
    let wiring = rt
        .run("p", |p| {
            p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
            p.wire_del(
                Endpoint::input("diagnostics"),
                Endpoint::output("diagnostics"),
            )?;
            p.run("b", |b| {
                b.wire_add(Endpoint::input("in"), Endpoint::output("out"))
            })?;
            Ok(p.wire_list())
        })
        .expect("the turn should succeed");
    assert_eq!(
        wiring.wires(),
        [(Endpoint::child("a", "out"), Endpoint::child("b", "in"))],
        "its own wires only, not its child's"
    );
    assert_eq!(wiring.default_wiring(), DiagnosticsWiring::ToRoot);
    assert_eq!(
        wiring.unwired_defaults(),
        [(
            Endpoint::input("diagnostics"),
            Endpoint::output("diagnostics")
        )]
    );

    let mut quiet = Runtime::open(RuntimeConfig::new().diagnostics_wiring(DiagnosticsWiring::None))
        .expect("a runtime should open");
    let wiring = quiet.run("p", |p| Ok(p.wire_list())).expect("a turn");
    assert_eq!(wiring.default_wiring(), DiagnosticsWiring::None);
    assert!(wiring.wires().is_empty() && wiring.unwired_defaults().is_empty());
}

#[test]
fn a_machine_may_turn_its_default_wiring_off_and_on() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_defaults(false)?;
        assert_eq!(p.wire_list().default_wiring(), DiagnosticsWiring::None);
        assert!(
            !p.port_input_connected("diagnostics"),
            "no child's reaches it"
        );
        let error = send(p, "c", "diagnostics", &[1]).expect_err("nothing collects it");
        assert!(error.to_string().contains("not connected"), "{error}");
        p.recover("c", |c| c.discard(1))?;

        // A wire of the rule's shape is now the machine's own.
        p.wire_add(
            Endpoint::child("c", "diagnostics"),
            Endpoint::input("diagnostics"),
        )?;
        send(p, "c", "diagnostics", &[2])?;
        assert_eq!(
            p.port_available("diagnostics"),
            1,
            "kept, with no pass-through"
        );
        p.wire_defaults(true)?;
        send(p, "c", "diagnostics", &[3])?;
        Ok(())
    })
    .expect("the turn should succeed");
    let at_root = rt
        .run((), |root| {
            let count = root.port_available("diagnostics");
            root.port_read("diagnostics", count)?;
            Ok((count, root.int_pop()?))
        })
        .expect("a turn");
    assert_eq!(
        at_root,
        (1, 3),
        "on again, the rule passes it through once, though the machine owns a like wire"
    );

    rt.run("p", |p| {
        p.wire_defaults(false)?;
        p.clear()?;
        assert_eq!(p.wire_list().default_wiring(), DiagnosticsWiring::ToRoot);
        assert!(p.wire_list().wires().is_empty(), "clear resets the wiring");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn clearing_a_machine_resets_its_ports_and_its_descendants() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        p.run("b", |b| {
            b.wire_add(Endpoint::input("x"), Endpoint::output("y"))
        })?;
        send(p, "a", "out", &[1])?;
        p.run("b", |b| {
            b.int_push(9);
            b.clear()?;
            assert_eq!(b.port_available("in"), 0, "its input is empty");
            assert!(
                !b.port_output_connected("y"),
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
            p.run("b", |b| Ok(b.port_available("in")))?,
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
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
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
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
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

/// Fails a cut's attempt past the third, so a cut that would run again
/// without end fails instead.
fn bounded(attempts: &Cell<usize>) -> Result<(), Error> {
    attempts.set(attempts.get() + 1);
    if attempts.get() > 3 {
        return Err(Error::new("the cut ran again with nothing changed"));
    }
    Ok(())
}

#[test]
fn a_cut_that_counts_items_it_leaves_commits() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1, 2])?;
        let attempts = Cell::new(0);
        let count = p.run("b", |b| {
            b.cut(|b| {
                bounded(&attempts)?;
                b.port_read("in", 1)?;
                Ok(b.port_available("in"))
            })
        })?;
        assert_eq!(
            (count, attempts.get()),
            (1, 1),
            "the count stands while nothing joins the items it left"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_putback_stands_at_the_head_of_its_input() {
    // A putback puts values back at the head of the machine's own input, in
    // order, ahead of what it holds, so the next read takes them first.
    // Within a cut, the cut's own reads see what it put back, and a failed
    // cut puts nothing back.
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1, 2, 3])?;
        p.run("b", |b| {
            b.port_read("in", 1)?;
            b.int_push(10);
            b.port_unread("in", 2)?;
            assert_eq!(b.depth(), 0, "the putback popped its values");
            Ok(())
        })?;
        assert_eq!(
            receive(p, "b", "in", 2)?,
            [1, 10],
            "in order, ahead of what the input held"
        );
        p.run("b", |b| {
            b.cut(|b| {
                b.int_push(20);
                b.port_unread("in", 1)?;
                assert_eq!(b.port_available("in"), 3, "counted with what it held");
                assert_eq!(b.port_pending(), [("in".into(), 3)]);
                b.port_read("in", 1)?;
                assert_eq!(b.int_pop()?, 20, "the cut's read takes its own putback");
                b.int_push(21);
                b.port_unread("in", 1)
            })?;
            let failed: Result<(), Error> = b.cut(|b| {
                b.int_push(30);
                b.port_unread("in", 1)?;
                Err(Error::new("the cut errs"))
            });
            assert!(failed.is_err());
            Ok(())
        })?;
        assert_eq!(
            receive(p, "b", "in", 3)?,
            [21, 2, 3],
            "the committed cut's putback, not the failed one's"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_fork_s_putbacks_merge_into_its_cut() {
    // A fork works on a branch of its cut's transaction, which sees what the
    // cut put back before the fork; what the fork takes or puts back merges
    // at the join. A machine holding a putback is present.
    let mut rt = runtime();
    rt.run("p", |p| {
        p.cut(|p| {
            p.run("b", |b| {
                b.int_push(5);
                b.port_unread("in", 1)
            })?;
            let present: Vec<_> = p.machines(())?.map(|(path, _)| path).collect();
            assert_eq!(present, [glam::MachinePath::from(["b"])]);
            let reader = p.fork("b", |b| b.port_read("in", 1))?;
            p.join(reader)?;
            p.take("b", 1)?;
            assert_eq!(p.int_pop()?, 5);
            assert_eq!(
                p.run("b", |b| Ok(b.port_available("in")))?,
                0,
                "the fork took it"
            );
            let putter = p.fork("b", |b| {
                b.int_push(6);
                b.port_unread("in", 1)
            })?;
            p.join(putter)?;
            assert_eq!(
                p.run("b", |b| Ok(b.port_available("in")))?,
                1,
                "the fork put it back"
            );
            Ok(())
        })?;
        assert_eq!(receive(p, "b", "in", 1)?, [6], "once the cut commits");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_clear_drops_what_its_cut_put_back_and_what_its_input_held() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.wire_add(Endpoint::child("a", "out"), Endpoint::child("b", "in"))?;
        send(p, "a", "out", &[1])?;
        p.run("b", |b| {
            let available = b.cut(|b| {
                b.int_push(5);
                b.port_unread("in", 1)?;
                b.clear()?;
                Ok(b.port_available("in"))
            })?;
            assert_eq!(available, 0);
            assert_eq!(b.port_available("in"), 0, "once the cut commits");
            Ok(())
        })
    })
    .expect("the turn should succeed");
}

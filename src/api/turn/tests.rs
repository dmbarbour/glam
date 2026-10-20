//! The runtime's own diagnostics reach the root's diagnostics input
//! (`machines-communicate-through-wired-ports`).

use super::*;
use crate::diagnostic::Severity;

#[test]
fn the_runtime_s_diagnostics_join_the_root_s_diagnostics_input() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let bus = rt.assembler().diagnostic_bus();
    for message in ["first", "second"] {
        bus.publish(Diagnostic::new(&values, Severity::Warning, message))
            .expect("the diagnostic should publish");
    }
    let decoded = rt
        .run((), |root| {
            root.read(DIAGNOSTICS.clone(), 2)?;
            let second = root.pop()?;
            let first = root.pop()?;
            Ok([first, second].map(|value| {
                Diagnostic::from_transport_value(&values, &value)
                    .expect("the port holds transport values")
            }))
        })
        .expect("the root reads its diagnostics input");
    assert_eq!(
        decoded.each_ref().map(|diagnostic| diagnostic.message()),
        ["first", "second"],
        "in the bus's order"
    );
    assert_eq!(decoded[0].severity(), Severity::Warning);
    assert_eq!(bus.counts().warnings(), 2, "the bus still counts them");
}

#[test]
fn a_runtime_without_the_default_wiring_keeps_its_diagnostics_on_the_bus() {
    let mut rt = Runtime::open(RuntimeConfig::new().diagnostics_wiring(DiagnosticsWiring::None))
        .expect("a runtime should open");
    let values = rt.assembler().values();
    rt.assembler()
        .diagnostic_bus()
        .publish(Diagnostic::new(&values, Severity::Warning, "kept"))
        .expect("the diagnostic should publish");
    let held = rt
        .run((), |root| Ok(root.available(DIAGNOSTICS.clone())))
        .expect("a turn");
    assert_eq!(held, 0);
}

#[test]
fn a_read_of_the_root_s_diagnostics_waits_for_one_to_arrive() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let bus = rt.assembler().diagnostic_bus().clone();
    let message = rt
        .run((), |root| {
            let publisher = {
                let values = values.clone();
                std::thread::spawn(move || {
                    bus.publish(Diagnostic::new(&values, Severity::Info, "late"))
                        .expect("the diagnostic should publish");
                })
            };
            root.read(DIAGNOSTICS.clone(), 1)?;
            publisher.join().expect("the publisher should finish");
            let value = root.pop()?;
            Ok(Diagnostic::from_transport_value(&values, &value)?
                .message()
                .to_owned())
        })
        .expect("the read waits for the diagnostic");
    assert_eq!(message, "late");
}

#[test]
fn the_runtime_s_diagnostics_follow_the_root_s_wiring() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let bus = rt.assembler().diagnostic_bus();
    let diagnostics = || DIAGNOSTICS.clone();
    rt.run((), |root| {
        root.wire(
            Endpoint::input(diagnostics()),
            Endpoint::child("sink", diagnostics()),
        )
    })
    .expect("the root wires its input on");
    bus.publish(Diagnostic::new(&values, Severity::Warning, "looped"))
        .expect("the diagnostic should publish");
    let held = |rt: &mut Runtime, path: MachinePath| {
        rt.run(path, |m| Ok(m.available(diagnostics())))
            .expect("a turn")
    };
    assert_eq!(
        (
            held(&mut rt, "refl".into()),
            held(&mut rt, ().into()),
            held(&mut rt, "sink".into())
        ),
        (1, 0, 0),
        "the sink's default pass-through loops back, so it waits where it entered, at `refl`"
    );

    rt.run("sink", |sink| {
        sink.unwire(
            Endpoint::input(diagnostics()),
            Endpoint::output(diagnostics()),
        )
    })
    .expect("the sink keeps what reaches it");
    bus.publish(Diagnostic::new(&values, Severity::Warning, "routed"))
        .expect("the diagnostic should publish");
    assert_eq!(
        (
            held(&mut rt, "refl".into()),
            held(&mut rt, ().into()),
            held(&mut rt, "sink".into())
        ),
        (1, 0, 1),
        "it passes through `refl` and the root to the sink"
    );
}

#[test]
fn a_hand_over_cut_reruns_when_a_diagnostic_arrives_and_keeps_their_order() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let bus = rt.assembler().diagnostic_bus();
    let publish = |message: &str| {
        bus.publish(Diagnostic::new(&values, Severity::Info, message))
            .expect("the diagnostic should publish");
    };
    let diagnostics = || DIAGNOSTICS.clone();
    let keep = |machine: &mut Machine<'_, '_>| {
        machine.unwire(
            Endpoint::input(diagnostics()),
            Endpoint::output(diagnostics()),
        )
    };
    // As the CLI wires its logger: the root's diagnostics to `logger`, which
    // keeps them, and its `unread` output to its `fallback`, which keeps them.
    rt.run(["logger", "fallback"], keep)
        .expect("the fallback keeps");
    rt.run("logger", |logger| {
        keep(logger)?;
        logger.wire(
            Endpoint::output("unread"),
            Endpoint::child("fallback", diagnostics()),
        )
    })
    .expect("the logger keeps");
    rt.run((), |root| {
        root.wire(
            Endpoint::input(diagnostics()),
            Endpoint::child("logger", diagnostics()),
        )
    })
    .expect("the root sends its diagnostics to the logger");
    publish("held");

    let attempts = std::cell::Cell::new(0);
    rt.run("logger", |logger| {
        logger.cut(|logger| {
            logger.wire(
                Endpoint::input(diagnostics()),
                Endpoint::child("fallback", diagnostics()),
            )?;
            let held = logger.available(diagnostics());
            logger.read(diagnostics(), held)?;
            logger.write("unread", held)?;
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                // It reaches the logger's input, since the rewiring has not
                // committed, after this attempt counted what the input held.
                publish("during");
            }
            Ok(())
        })
    })
    .expect("the logger hands over");
    assert_eq!(attempts.get(), 2, "the arrival disturbed the first attempt");
    publish("after");

    let messages = rt
        .run(["logger", "fallback"], |fallback| {
            let count = fallback.available(diagnostics());
            fallback.read(diagnostics(), count)?;
            let mut messages = (0..count)
                .map(|_| {
                    let value = fallback.pop()?;
                    Ok(Diagnostic::from_transport_value(&values, &value)?
                        .message()
                        .to_owned())
                })
                .collect::<Result<Vec<_>, Error>>()?;
            messages.reverse();
            Ok(messages)
        })
        .expect("the fallback holds them");
    assert_eq!(messages, ["held", "during", "after"], "each once, in order");
    let left = rt
        .run("logger", |logger| Ok(logger.available(diagnostics())))
        .expect("a turn");
    assert_eq!(left, 0);
}

#[test]
fn reflection_reads_the_refl_machine_s_environment_over_its_sealed_one() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let shares = "language g0\nobject root_share with\n    greeting = \"hello\"\n";
    rt.run((), |root| {
        root.compile(["shares"], [ModuleInput::script("g", shares)])?;
        root.access("root_share")?;
        root.set_env_mixin()
    })
    .expect("the root binds its share");
    let assembler = rt.assembler();
    let values = assembler.values();
    let environment = assembler.reflection_environment();
    let text = |path: &[&str]| -> String {
        let value = values
            .access_names(&environment, path.iter().copied())
            .expect("the path composes");
        let value = assembler.evaluator().eval(&value).expect("it evaluates");
        String::from_utf8(value.as_bytes().unwrap().unwrap().to_vec()).unwrap()
    };
    assert_eq!(
        text(&["greeting"]),
        "hello",
        "the root's share, through `refl`"
    );
    assert!(
        !text(&["glam", "version"]).is_empty(),
        "the sealed environment lies beneath"
    );
}

#[test]
fn a_broken_promise_fails_its_readers_with_its_message() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let error = rt
        .run("m", |m| {
            m.promise();
            m.push_text("the reason it broke");
            m.break_promise()?;
            Ok(m.pop_int().expect_err("the promise was broken"))
        })
        .expect("the turn should succeed");
    let values = rt.assembler().values();
    let diagnostic = error
        .diagnostic(&values)
        .expect("a failure has a diagnostic");
    let text = values
        .access_names(diagnostic.emission(), ["msg", "text"])
        .expect("the path composes");
    let text = rt
        .assembler()
        .evaluator()
        .eval(&text)
        .expect("the message evaluates");
    assert_eq!(
        text.as_bytes().unwrap().as_deref(),
        Some(b"the reason it broke".as_slice())
    );
}

/// Collects as runtime maintenance does, acting on the death notices
/// (`opaque-resources-reclaimed-when-unreachable`).
fn collect(m: &Machine<'_, '_>) {
    m.runtime
        .assembler
        .evaluation_runtime()
        .service_managed_collection()
        .expect("the runtime collects");
}

/// Whether the store holds `volume`.
fn holds(m: &Machine<'_, '_>, volume: crate::reflection::VolumeId) -> bool {
    m.resources()
        .reflection_snapshot()
        .1
        .volume_value(volume, &[])
        .is_some()
}

#[test]
fn a_lost_resolver_breaks_its_promise_and_wakes_its_reader() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (waiting, waited) = std::sync::mpsc::channel();
    let error = rt
        .run("p", |p| {
            p.promise();
            p.swap()?;
            p.give("reader", 1)?;
            let fork = p.fork("reader", move |r| {
                let mut attempts = 0;
                let read = r.cut(|r| {
                    r.dup()?;
                    let read = r.pop_int();
                    if read
                        .as_ref()
                        .is_err_and(|error| error.kind() == ErrorKind::Retry)
                    {
                        attempts += 1;
                        if attempts == 1 {
                            waiting
                                .send(())
                                .expect("the main turn waits for the attempt");
                        }
                    }
                    read
                });
                let error = read.expect_err("the promise broke");
                assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
                Ok(())
            })?;
            waited.recv().expect("the reader's first attempt waits");
            p.discard(1)?;
            collect(p);
            p.join(fork)?;
            p.take("reader", 1)?;
            Ok(p.pop_int().expect_err("the promise was broken"))
        })
        .expect("the turn should succeed");
    let values = rt.assembler().values();
    let diagnostic = error
        .diagnostic(&values)
        .expect("a failure has a diagnostic");
    let text = values
        .access_names(diagnostic.emission(), ["msg", "text"])
        .expect("the path composes");
    let text = rt
        .assembler()
        .evaluator()
        .eval(&text)
        .expect("the message evaluates");
    assert_eq!(
        text.as_bytes().unwrap().as_deref(),
        Some(b"resolver lost".as_slice())
    );
}

#[test]
fn a_resolved_promise_s_record_goes_with_its_resolver() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        let records = |m: &Machine<'_, '_>| {
            m.resources()
                .reflection_snapshot()
                .1
                .promises()
                .len_for_test()
        };
        m.promise();
        m.push_int(1);
        m.fulfill()?;
        assert_eq!(records(m), 1, "the record keeps how it resolved");
        collect(m);
        assert_eq!(records(m), 0, "nothing can ask it any more");
        assert_eq!(m.pop_int()?, 1, "the promised value stands");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_volume_lives_while_any_copy_of_its_handle_does() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        m.push_int(7);
        m.volume_new()?;
        let volume = m.volume_at(1)?;
        m.dup()?;
        m.set("copy")?;
        m.discard(1)?;
        collect(m);
        assert!(holds(m, volume), "a copy of its handle remains");
        m.push_unit();
        m.set("copy")?;
        collect(m);
        assert!(!holds(m, volume), "its last handle is gone");

        m.push_int(3);
        m.heap_set(["x"])?;
        m.heap_volume();
        m.discard(1)?;
        collect(m);
        m.heap_get(["x"]);
        assert_eq!(m.pop_int()?, 3, "the shared heap outlives its handles");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn settlement_collects_and_acts_on_death_notices_first() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let volume = rt
        .run("m", |m| {
            m.push_int(7);
            m.volume_new()?;
            let volume = m.volume_at(1)?;
            m.discard(1)?;
            assert!(holds(m, volume), "nothing has collected yet");
            Ok(volume)
        })
        .expect("the turn should succeed");
    let runtime = rt.assembler().evaluation_runtime();
    loop {
        runtime.pump_until_stable();
        match runtime.readiness() {
            crate::api::RuntimeReadiness::Ready(snapshot) => {
                if snapshot.settle().is_ok() {
                    break;
                }
            }
            crate::api::RuntimeReadiness::Busy => {}
            crate::api::RuntimeReadiness::MaintenanceRequired(maintenance) => {
                let _ = maintenance.service();
            }
            _ => panic!("the runtime should settle"),
        }
    }
    rt.run("m", |m| {
        assert!(!holds(m, volume), "settlement reclaimed it");
        Ok(())
    })
    .expect("the turn should succeed");
}

/// Whether a cut waiting on a promised value wakes to its fulfillment alone.
/// The waiter's cut reads the value with `read` and finds it pending; the
/// parent's resolving commit wakes it before the value publishes, and it
/// reads it pending again and waits again; only then does the value
/// publish. Should the waiter miss that, an unrelated commit wakes it after
/// a while, so the test ends either way.
fn waiter_wakes_on_fulfillment(
    read: impl Fn(&mut Machine<'_, '_>) -> Result<i64, Error> + Send + Sync + 'static,
) -> bool {
    use std::sync::mpsc;
    use std::time::Duration;
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (attempted, attempts) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    rt.run("p", move |p| {
        p.promise();
        p.set("resolver")?;
        p.give("waiter", 1)?;
        let fork = p.fork("waiter", move |w| {
            w.set("promised")?;
            let value = w.cut(|w| {
                let value = read(w);
                if value
                    .as_ref()
                    .is_err_and(|error| error.kind() == ErrorKind::Retry)
                {
                    attempted.send(()).expect("the parent counts the attempts");
                }
                value
            })?;
            done.send(value).expect("the parent awaits the value");
            Ok(())
        })?;
        attempts.recv().expect("the waiter's first attempt waits");
        p.resources().before_fulfillment_for_test(move || {
            attempts
                .recv_timeout(Duration::from_secs(10))
                .expect("the resolving commit wakes the waiter, which reads it pending again");
        });
        p.get("resolver")?;
        p.push_int(7);
        p.fulfill()?;
        let woke = finished.recv_timeout(Duration::from_secs(5));
        if woke.is_err() {
            p.push_int(0);
            p.heap_set(["nudge"])?;
        }
        p.join(fork)?;
        assert!(woke.as_ref().is_err() || *woke.as_ref().unwrap() == 7);
        Ok(woke.is_ok())
    })
    .expect("the turn should succeed")
}

#[test]
fn a_cut_waiting_on_a_promised_value_wakes_to_its_fulfillment() {
    assert!(
        waiter_wakes_on_fulfillment(|w| {
            w.get("promised")?;
            w.pop_int()
        }),
        "the waiter missed the fulfillment"
    );
}

#[test]
fn a_cut_waiting_on_a_lazy_value_wakes_to_the_fulfillment_it_needs() {
    assert!(
        waiter_wakes_on_fulfillment(|w| {
            w.get("promised")?;
            w.make_dict(["value"])?;
            w.access("value")?;
            w.pop_int()
        }),
        "the waiter missed the fulfillment"
    );
}

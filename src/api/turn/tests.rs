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
            root.port_read(DIAGNOSTICS.clone(), 2)?;
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
        .run((), |root| Ok(root.port_available(DIAGNOSTICS.clone())))
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
            root.port_read(DIAGNOSTICS.clone(), 1)?;
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
        root.wire_add(
            Endpoint::input(diagnostics()),
            Endpoint::child("sink", diagnostics()),
        )
    })
    .expect("the root wires its input on");
    bus.publish(Diagnostic::new(&values, Severity::Warning, "looped"))
        .expect("the diagnostic should publish");
    let held = |rt: &mut Runtime, path: MachinePath| {
        rt.run(path, |m| Ok(m.port_available(diagnostics())))
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
        sink.wire_del(
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
        machine.wire_del(
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
        logger.wire_add(
            Endpoint::output("unread"),
            Endpoint::child("fallback", diagnostics()),
        )
    })
    .expect("the logger keeps");
    rt.run((), |root| {
        root.wire_add(
            Endpoint::input(diagnostics()),
            Endpoint::child("logger", diagnostics()),
        )
    })
    .expect("the root sends its diagnostics to the logger");
    publish("held");

    let attempts = std::cell::Cell::new(0);
    rt.run("logger", |logger| {
        logger.cut(|logger| {
            logger.wire_add(
                Endpoint::input(diagnostics()),
                Endpoint::child("fallback", diagnostics()),
            )?;
            let held = logger.port_available(diagnostics());
            logger.port_read(diagnostics(), held)?;
            logger.port_write("unread", held)?;
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
            let count = fallback.port_available(diagnostics());
            fallback.port_read(diagnostics(), count)?;
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
        .run("logger", |logger| Ok(logger.port_available(diagnostics())))
        .expect("a turn");
    assert_eq!(left, 0);
}

#[test]
fn reflection_reads_the_refl_machine_s_environment_over_its_sealed_one() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let shares = "language g0\nobject root_share with\n    greeting = \"hello\"\n";
    rt.run((), |root| {
        root.dict_pack(Vec::<&str>::new())?;
        root.script_object(["shares"], "g", shares)?;
        root.dict_get("root_share")?;
        root.env_mixin_set()
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
            m.promise_new();
            m.text_push("the reason it broke");
            m.promise_break()?;
            Ok(m.int_pop().expect_err("the promise was broken"))
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
        .holds_volume_for_test(volume)
}

#[test]
fn a_lost_resolver_breaks_its_promise_and_wakes_its_reader() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let (waiting, waited) = std::sync::mpsc::channel();
    let error = rt
        .run("p", |p| {
            p.promise_new();
            p.swap()?;
            p.give("reader", 1)?;
            let fork = p.fork("reader", move |r| {
                let mut attempts = 0;
                let read = r.cut(|r| {
                    r.dup(1)?;
                    let read = r.int_pop();
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
            Ok(p.int_pop().expect_err("the promise was broken"))
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
fn a_promise_leaves_the_store_as_it_resolves_and_its_resolver_keeps_its_status() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        let pending = |m: &Machine<'_, '_>| {
            m.resources()
                .reflection_snapshot()
                .1
                .promises()
                .len_for_test()
        };
        m.promise_new();
        assert_eq!(pending(m), 1);
        m.dup(1)?;
        m.int_push(1);
        m.promise_fulfill()?;
        assert_eq!(pending(m), 0, "the store holds only pending promises");
        collect(m);
        assert_eq!(
            m.promise_status()?,
            PromiseStatus::Fulfilled,
            "the resolver answers"
        );
        m.discard(1)?;
        assert_eq!(m.int_pop()?, 1, "the promised value stands");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_volume_lives_while_any_copy_of_its_handle_does() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        m.int_push(7);
        m.volume_new()?;
        let volume = m.volume_at(1)?;
        m.dup(1)?;
        m.local_set("copy")?;
        m.discard(1)?;
        collect(m);
        assert!(holds(m, volume), "a copy of its handle remains");
        m.unit_push();
        m.local_set("copy")?;
        collect(m);
        assert!(!holds(m, volume), "its last handle is gone");

        m.int_push(3);
        m.heap_set(["x"])?;
        m.heap_volume();
        m.discard(1)?;
        collect(m);
        m.heap_get(["x"])?;
        assert_eq!(m.int_pop()?, 3, "the shared heap outlives its handles");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_volume_reaching_its_own_handle_is_collected() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("m", |m| {
        m.int_push(7);
        m.volume_new()?;
        let volume = m.volume_at(1)?;
        m.dup(1)?;
        m.volume_set(["self"])?;
        assert_eq!(m.depth(), 0, "only the volume holds its handle");
        collect(m);
        assert!(
            !holds(m, volume),
            "its value reaches its cell only through its version (`api-volume-cells`)"
        );
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
        p.promise_new();
        p.local_set("resolver")?;
        p.give("waiter", 1)?;
        let fork = p.fork("waiter", move |w| {
            w.local_set("promised")?;
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
        p.local_get("resolver")?;
        p.int_push(7);
        p.promise_fulfill()?;
        let woke = finished.recv_timeout(Duration::from_secs(5));
        if woke.is_err() {
            p.int_push(0);
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
            w.local_get("promised")?;
            w.int_pop()
        }),
        "the waiter missed the fulfillment"
    );
}

#[test]
fn a_cut_waiting_on_a_lazy_value_wakes_to_the_fulfillment_it_needs() {
    assert!(
        waiter_wakes_on_fulfillment(|w| {
            w.local_get("promised")?;
            w.dict_pack(["value"])?;
            w.dict_get("value")?;
            w.int_pop()
        }),
        "the waiter missed the fulfillment"
    );
}

#[test]
fn undefined_reads_are_errors_naming_their_path() {
    // `dict-empty-is-a-value`: a turn's reads mirror `.get`: an error where
    // the path is undefined, whose diagnostic names the path, in its text and
    // as data, and the read pushes nothing.
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let errors = rt
        .run("m", |m| {
            m.dict_pack(Vec::<&str>::new())?;
            m.heap_set(["a"])?;
            let heap = m.heap_get(["a", "b"]).expect_err("`a.b` is undefined");
            let local = m.local_get("missing").expect_err("`missing` is undefined");
            m.dict_pack(Vec::<&str>::new())?;
            let unpacked = m.dict_unpack(["k"]).expect_err("`k` is undefined");
            m.discard(1)?;
            assert_eq!(m.depth(), 0, "the failed reads pushed nothing");
            Ok([
                (heap, "`a.b`", vec!["a", "b"]),
                (local, "`missing`", vec!["missing"]),
                (unpacked, "`k`", vec!["k"]),
            ])
        })
        .expect("the turn should succeed");
    for (error, path, keys) in errors {
        let diagnostic = error
            .diagnostic(&values)
            .expect("the error has a diagnostic");
        assert_eq!(
            diagnostic.rendered_text_for_test(&values),
            format!("{path} is undefined")
        );
        let named = values
            .access_names(diagnostic.emission(), ["undefined"])
            .and_then(|path| rt.assembler().evaluator().eval(&path))
            .expect("the diagnostic names the path as data");
        let keys = values
            .list(keys.into_iter().map(|key| values.atom_from_text(key)))
            .expect("the path should construct");
        assert!(named.same_representation(&keys).unwrap(), "{path}");
    }
}

#[test]
fn an_analysis_lists_input_a_machine_left_unread() {
    // `quiescence-is-analyzed`: input in a machine's buffer that no turn
    // has read is a backlog, which reading clears.
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    rt.run("p", |p| {
        p.wire_add(Endpoint::output("events"), Endpoint::child("b", "in"))?;
        p.int_push(1);
        p.int_push(2);
        p.port_write("events", 2)
    })
    .expect("the turn should succeed");
    let runtime = rt.assembler().evaluation_runtime();
    let analysis = runtime.analysis().expect("the runtime is not poisoned");
    let backlogs: Vec<_> = analysis
        .backlogs()
        .iter()
        .map(|backlog| {
            (
                backlog.machine().keys().to_vec(),
                backlog.port().cloned(),
                backlog.count(),
            )
        })
        .collect();
    assert_eq!(
        backlogs,
        [(
            vec![Key::atom_from_text("p"), Key::atom_from_text("b")],
            Some(Key::atom_from_text("in")),
            2
        )]
    );
    assert!(
        analysis
            .issues()
            .iter()
            .any(|issue| issue.family() == crate::api::RuntimeIssueFamily::Backlog)
    );
    rt.run("p", |p| p.run("b", |b| b.port_read("in", 2)))
        .expect("the turn should succeed");
    assert!(runtime.analysis().unwrap().backlogs().is_empty());
}

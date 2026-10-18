//! Cuts: a turn's transactions over the shared heap, which also bound a
//! backtracking search (`turn-cuts-bound-transactions-and-search`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use glam::{Error, ErrorKind, Machine, ModuleInput, Runtime, RuntimeConfig, ValueKind};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// The integer at `path` in the shared heap, read outside any cut.
fn heap_int(rt: &Runtime, path: &[&str]) -> i64 {
    rt.run("reader", |m| {
        m.heap_get(path.iter().copied());
        m.pop_int()
    })
    .expect("the heap should hold an integer there")
}

/// Whether the value at `path` in the shared heap is undefined, read
/// outside any cut.
fn heap_undefined(rt: &Runtime, path: &[&str]) -> bool {
    rt.run("reader", |m| {
        m.heap_get(path.iter().copied());
        m.eval()?;
        undefined(m)
    })
    .expect("the read should succeed")
}

type Log = Rc<RefCell<Vec<&'static str>>>;

/// A hook that records `entry` in `log`.
fn record(
    log: &Log,
    entry: &'static str,
) -> impl FnOnce(&mut Machine<'_>) -> Result<(), Error> + use<> {
    let log = log.clone();
    move |_| {
        log.borrow_mut().push(entry);
        Ok(())
    }
}

/// Whether the top value, evaluated, is `{}`; it is popped.
fn undefined(m: &mut Machine<'_>) -> Result<bool, Error> {
    if m.kind()? != ValueKind::Dict {
        m.discard(1)?;
        return Ok(false);
    }
    let entries = m.unpack_dict()?;
    m.discard(2 * entries)?;
    Ok(entries == 0)
}

#[test]
fn heap_actions_outside_a_cut_commit_at_once() {
    let rt = runtime();
    assert!(
        heap_undefined(&rt, &["counter"]),
        "a missing key reads {{}}"
    );
    rt.run("writer", |m| {
        m.push_int(41);
        m.heap_set(["counter"])?;
        m.compile(
            ["inc"],
            [ModuleInput::script("g", "language g0\ninc n = n + 1\n")],
        )?;
        m.access("inc")?;
        m.heap_rewrite(["counter"])
    })
    .expect("the writes should commit");
    assert_eq!(heap_int(&rt, &["counter"]), 42);
}

#[test]
fn a_cut_commits_its_writes_together_and_reads_its_own() {
    let rt = runtime();
    let seen = rt
        .run("writer", |m| {
            m.cut(|m| {
                m.push_int(1);
                m.heap_set(["a"])?;
                m.push_int(2);
                m.heap_set(["b"])?;
                m.heap_get(["a"]);
                m.pop_int()
            })
        })
        .expect("the cut should commit");
    assert_eq!(seen, 1, "a cut reads its own writes");
    assert_eq!((heap_int(&rt, &["a"]), heap_int(&rt, &["b"])), (1, 2));
}

#[test]
fn a_failed_cut_undoes_its_writes_and_the_machine_s_state() {
    let rt = runtime();
    rt.run("writer", |m| {
        m.push_int(7);
        m.set("kept")?;
        let failed: Result<(), Error> = m.cut(|m| {
            m.push_int(1);
            m.heap_set(["a"])?;
            m.push_int(99);
            m.set("kept")?;
            m.push_text("left behind");
            Err(Error::new("this alternative fails"))
        });
        assert!(failed.is_err());
        assert_eq!(m.depth(), 0, "the cut's pushes are undone");
        m.get("kept")?;
        assert_eq!(m.pop_int()?, 7, "the cut's state changes are undone");
        Ok(())
    })
    .expect("the turn should succeed");
    assert!(heap_undefined(&rt, &["a"]), "the cut's write is undone");
    let attempts = Cell::new(0);
    let error = rt
        .run("writer", |m| -> Result<(), Error> {
            m.cut(|m| {
                attempts.set(attempts.get() + 1);
                m.heap_get(["a"]);
                Err(Error::new("an error, not a failure"))
            })
        })
        .expect_err("the error ends the cut");
    assert_eq!(error.kind(), ErrorKind::Failure);
    assert_eq!(attempts.get(), 1, "an error ends the cut without waiting");
}

#[test]
fn nested_cuts_backtrack_alone_and_merge_on_success() {
    let rt = runtime();
    rt.run("writer", |m| {
        m.cut(|m| {
            m.push_int(1);
            m.heap_set(["outer"])?;
            let first: Result<(), Error> = m.cut(|m| {
                m.push_int(2);
                m.heap_set(["first"])?;
                Err(Error::new("the first alternative fails"))
            });
            assert!(first.is_err());
            m.heap_get(["first"]);
            m.eval()?;
            assert!(undefined(m)?, "the failed alternative's write is undone");
            let failed: Result<(), Error> = m.cut(|m| {
                m.heap_get(["absent"]);
                Err(m.fail())
            });
            assert_eq!(
                failed.expect_err("a nested cut returns a failure").kind(),
                ErrorKind::Backtrack,
                "the caller may try another alternative"
            );
            m.cut(|m| {
                m.push_int(3);
                m.heap_set(["second"])
            })
        })
    })
    .expect("the outer cut should commit");
    assert_eq!(heap_int(&rt, &["outer"]), 1);
    assert!(heap_undefined(&rt, &["first"]));
    assert_eq!(heap_int(&rt, &["second"]), 3);
}

#[test]
fn hooks_run_in_order_on_commit_and_in_reverse_on_abort() {
    let rt = runtime();
    let log = Log::default();
    rt.run("hooks", |m| {
        m.cut(|m| {
            m.on_commit(0, record(&log, "commit 1"))?;
            m.on_abort(0, record(&log, "abort 1"))?;
            let failed: Result<(), Error> = m.cut(|m| {
                m.on_commit(0, record(&log, "dropped commit"))?;
                m.on_abort(0, record(&log, "nested abort 1"))?;
                m.on_abort(0, record(&log, "nested abort 2"))?;
                Err(m.fail())
            });
            assert!(failed.is_err());
            m.cut(|m| m.on_commit(0, record(&log, "commit 2")))
        })?;
        assert_eq!(
            *log.borrow(),
            ["nested abort 2", "nested abort 1", "commit 1", "commit 2"]
        );
        log.borrow_mut().clear();

        let failed: Result<(), Error> = m.cut(|m| {
            m.on_abort(0, record(&log, "outer abort"))?;
            m.cut(|m| m.on_abort(0, record(&log, "committed nested abort")))?;
            Err(Error::new("the outer cut errs"))
        });
        assert!(failed.is_err());
        assert_eq!(
            *log.borrow(),
            ["committed nested abort", "outer abort"],
            "an abort backtracks through the hooks of nested cuts that committed"
        );
        log.borrow_mut().clear();

        m.on_commit(0, record(&log, "outside a cut"))?;
        m.on_abort(0, record(&log, "never"))?;
        Ok(())
    })
    .expect("the turn should succeed");
    assert_eq!(*log.borrow(), ["outside a cut"]);
}

#[test]
fn a_hook_runs_on_the_machine_with_the_items_it_captured() {
    let rt = runtime();
    rt.run("hooks", |m| {
        m.push_int(1);
        m.set("count")?;
        m.cut(|m| {
            m.push_text("stashed");
            m.stash(1)?;
            m.push_int(10);
            m.push_int(20);
            m.push_int(30);
            m.on_commit(2, |m| {
                assert_eq!(
                    (m.depth(), m.stash_depth()),
                    (2, 0),
                    "a hook's stack holds only its items"
                );
                let (top, below) = (m.pop_int()?, m.pop_int()?);
                assert_eq!((below, top), (20, 30), "the items keep their order");
                m.get("count")?;
                let count = m.pop_int()?;
                m.push_int(count + below + top);
                m.heap_set(["total"])?;
                m.push_int(count + 1);
                m.set("count")?;
                m.push_text("left behind");
                Ok(())
            })?;
            m.push_int(2);
            m.set("count")
        })?;
        assert_eq!(
            (m.depth(), m.stash_depth()),
            (1, 1),
            "the machine's stack and stash return after the hook"
        );
        assert_eq!(m.pop_int()?, 10);
        m.get("count")?;
        assert_eq!(m.pop_int()?, 3, "the hook's state change stays");

        m.cut(|m| {
            let failed: Result<(), Error> = m.cut(|m| {
                m.push_text("compensation");
                m.on_abort(1, |m| m.heap_set(["undone"]))?;
                m.push_text("attempt");
                m.heap_set(["undone"])?;
                Err(m.fail())
            });
            assert!(failed.is_err());
            m.heap_get(["undone"]);
            assert_eq!(
                m.pop_text()?,
                "compensation",
                "an abort hook runs within the enclosing cut, after the undo"
            );
            Ok(())
        })
    })
    .expect("the turn should succeed");
    assert_eq!(
        heap_int(&rt, &["total"]),
        52,
        "a hook sees the state as it stands when the hook runs"
    );
    let undone = rt
        .run("reader", |m| {
            m.heap_get(["undone"]);
            m.pop_text()
        })
        .expect("the heap should hold a text there");
    assert_eq!(undone, "compensation", "the abort hook's write commits");
}

#[test]
fn a_hook_s_error_replaces_a_result_or_a_failure_but_not_another_error() {
    let rt = runtime();
    let log = Log::default();
    rt.run("hooks", |m| {
        let committed: Result<i64, Error> = m.cut(|m| {
            m.on_commit(0, |_| Err(Error::new("the first hook errs")))?;
            m.on_commit(0, record(&log, "the second hook runs"))?;
            Ok(1)
        });
        assert_eq!(
            committed
                .expect_err("the hook's error replaces the result")
                .to_string(),
            "the first hook errs"
        );
        assert_eq!(*log.borrow(), ["the second hook runs"]);
        m.cut(|m| {
            let failed: Result<(), Error> = m.cut(|m| {
                m.on_abort(0, |_| Err(Error::new("the abort hook errs")))?;
                Err(m.fail())
            });
            let error = failed.expect_err("the cut fails");
            assert_eq!(
                (error.kind(), error.to_string().as_str()),
                (ErrorKind::Failure, "the abort hook errs"),
                "the hook's error replaces the failure"
            );
            let errored: Result<(), Error> = m.cut(|m| {
                m.on_abort(0, |_| Err(Error::new("the abort hook errs")))?;
                Err(Error::new("the body errs"))
            });
            assert_eq!(
                errored.expect_err("the cut errs").to_string(),
                "the body errs",
                "the body's error came first"
            );
            Ok(())
        })
    })
    .expect("the turn should succeed");
}

#[test]
fn a_failed_cut_waits_until_something_it_observed_changes() {
    let rt = runtime();
    let (waiting, blocked) = mpsc::channel();
    let attempts = thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let attempts = Cell::new(0);
            let value = rt
                .run("reader", |m| {
                    m.cut(|m| {
                        attempts.set(attempts.get() + 1);
                        m.heap_get(["ready"]);
                        m.eval()?;
                        if m.kind()? == ValueKind::Number {
                            return m.pop_int();
                        }
                        // Signals the writer as the failure backtracks,
                        // before the cut waits, forcing the write after the
                        // read.
                        let waiting = waiting.clone();
                        m.on_abort(0, move |_| {
                            waiting.send(()).expect("the writer listens");
                            Ok(())
                        })?;
                        Err(m.fail())
                    })
                })
                .expect("the cut should commit once the value is ready");
            assert_eq!(value, 5);
            attempts.get()
        });
        blocked.recv().expect("the reader should fail first");
        rt.run("writer", |m| {
            m.push_int(5);
            m.heap_set(["ready"])
        })
        .expect("the write should commit");
        reader.join().expect("the reader should not panic")
    });
    assert_eq!(attempts, 2, "the cut runs again once, after the write");
}

#[test]
fn a_conflicting_commit_reruns_the_cut_at_once() {
    let rt = runtime();
    let (read, proceed) = (mpsc::channel(), mpsc::channel());
    let (read_sender, read_receiver) = read;
    let (written_sender, written_receiver) = proceed;
    rt.run("setup", |m| {
        m.push_int(1);
        m.heap_set(["x"])
    })
    .expect("the setup should commit");
    let rt = &rt;
    let attempts = thread::scope(|scope| {
        let doubler = scope.spawn(move || {
            let attempts = Cell::new(0);
            rt.run("doubler", |m| {
                m.cut(|m| {
                    attempts.set(attempts.get() + 1);
                    m.heap_get(["x"]);
                    let x = m.pop_int()?;
                    if attempts.get() == 1 {
                        // Forces another commit between this read and this
                        // cut's commit.
                        read_sender.send(()).expect("the writer listens");
                        written_receiver.recv().expect("the writer commits");
                    }
                    m.push_int(x * 2);
                    m.heap_set(["doubled"])
                })
            })
            .expect("the cut should commit");
            attempts.get()
        });
        read_receiver.recv().expect("the doubler reads first");
        rt.run("writer", |m| {
            m.push_int(10);
            m.heap_set(["x"])
        })
        .expect("the write should commit");
        written_sender.send(()).expect("the doubler waits");
        doubler.join().expect("the doubler should not panic")
    });
    assert_eq!(attempts, 2, "the conflict reruns the cut");
    assert_eq!(
        heap_int(rt, &["doubled"]),
        20,
        "the rerun reads the new value"
    );
}

#[test]
fn a_failure_without_observations_or_outside_a_cut_ends_the_turn() {
    let rt = runtime();
    rt.run("m", |m| {
        let unwakeable: Result<(), Error> = m.cut(|m| Err(m.fail()));
        let error = unwakeable.expect_err("nothing could wake the cut");
        assert_eq!(error.kind(), ErrorKind::Failure);
        assert_eq!(m.fail().kind(), ErrorKind::Backtrack);
        Ok(())
    })
    .expect("the turn should succeed");
    let error = rt
        .run("m", |m| -> Result<(), Error> { Err(m.fail()) })
        .expect_err("a failure outside a cut ends the turn");
    assert_eq!(error.kind(), ErrorKind::Backtrack);
}

//! Cuts: a turn's transactions over the shared heap, which also bound a
//! backtracking search (`turn-cuts-bound-transactions-and-search`).

use std::cell::Cell;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use glam::{Error, ErrorKind, Machine, Runtime, RuntimeConfig, ValueKind};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// The integer at `path` in the shared heap, read outside any cut.
fn heap_int(rt: &mut Runtime, path: &[&str]) -> i64 {
    rt.run("reader", |m| {
        m.heap_get(path.iter().copied());
        m.pop_int()
    })
    .expect("the heap should hold an integer there")
}

/// Whether the value at `path` in the shared heap is undefined, read
/// outside any cut.
fn heap_undefined(rt: &mut Runtime, path: &[&str]) -> bool {
    rt.run("reader", |m| {
        m.heap_get(path.iter().copied());
        m.eval()?;
        undefined(m)
    })
    .expect("the read should succeed")
}

type Log = Arc<Mutex<Vec<&'static str>>>;

/// A hook that records `entry` in `log`.
fn record(log: &Log, entry: &'static str) -> impl FnOnce(&mut Machine<'_, '_>) + use<> {
    let log = log.clone();
    move |_| log.lock().unwrap().push(entry)
}

/// A hook's work, which handles its own errors by failing the test: it
/// pops two captured items, and writes their sum with the machine's count.
fn tally(m: &mut Machine<'_, '_>) -> Result<(), Error> {
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
}

/// Whether the top value, evaluated, is `{}`; it is popped.
fn undefined(m: &mut Machine<'_, '_>) -> Result<bool, Error> {
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
    let mut rt = runtime();
    assert!(
        heap_undefined(&mut rt, &["counter"]),
        "a missing key reads {{}}"
    );
    rt.run("writer", |m| {
        m.push_int(41);
        m.heap_set(["counter"])?;
        m.make_dict(Vec::<&str>::new())?;
        m.script_object(["inc"], "g", "language g0\ninc n = n + 1\n")?;
        m.access("inc")?;
        m.heap_rewrite(["counter"])
    })
    .expect("the writes should commit");
    assert_eq!(heap_int(&mut rt, &["counter"]), 42);
}

#[test]
fn a_cut_commits_its_writes_together_and_reads_its_own() {
    let mut rt = runtime();
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
    assert_eq!(
        (heap_int(&mut rt, &["a"]), heap_int(&mut rt, &["b"])),
        (1, 2)
    );
}

#[test]
fn a_failed_cut_undoes_its_writes_and_the_machine_s_state() {
    let mut rt = runtime();
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
    assert!(heap_undefined(&mut rt, &["a"]), "the cut's write is undone");
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
    let mut rt = runtime();
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
    assert_eq!(heap_int(&mut rt, &["outer"]), 1);
    assert!(heap_undefined(&mut rt, &["first"]));
    assert_eq!(heap_int(&mut rt, &["second"]), 3);
}

#[test]
fn hooks_run_in_order_on_commit_and_in_reverse_on_abort() {
    let mut rt = runtime();
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
            *log.lock().unwrap(),
            ["nested abort 2", "nested abort 1", "commit 1", "commit 2"]
        );
        log.lock().unwrap().clear();

        let failed: Result<(), Error> = m.cut(|m| {
            m.on_abort(0, record(&log, "outer abort"))?;
            m.cut(|m| m.on_abort(0, record(&log, "committed nested abort")))?;
            Err(Error::new("the outer cut errs"))
        });
        assert!(failed.is_err());
        assert_eq!(
            *log.lock().unwrap(),
            ["committed nested abort", "outer abort"],
            "an abort backtracks through the hooks of nested cuts that committed"
        );
        log.lock().unwrap().clear();

        m.on_commit(0, record(&log, "outside a cut"))?;
        m.on_abort(0, record(&log, "never"))?;
        Ok(())
    })
    .expect("the turn should succeed");
    assert_eq!(*log.lock().unwrap(), ["outside a cut"]);
}

#[test]
fn a_hook_runs_on_the_machine_with_the_items_it_captured() {
    let mut rt = runtime();
    rt.run("hooks", |m| {
        m.push_int(1);
        m.set("count")?;
        m.cut(|m| {
            m.push_text("stashed");
            m.stash(1)?;
            m.push_int(10);
            m.push_int(20);
            m.push_int(30);
            m.on_commit(2, |m| tally(m).expect("the hook's work should succeed"))?;
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
                m.on_abort(1, |m| {
                    m.heap_set(["undone"])
                        .expect("the compensation should write")
                })?;
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
        heap_int(&mut rt, &["total"]),
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
fn a_hook_handles_its_own_failures_and_a_cut_s_outcome_is_its_own() {
    let mut rt = runtime();
    let log = Log::default();
    let committed = rt
        .run("hooks", |m| {
            m.cut(|m| {
                let log = log.clone();
                m.on_commit(0, move |m| {
                    // Nothing is on the hook's stack to pop.
                    if m.pop_int().is_err() {
                        log.lock().unwrap().push("the hook handled its failure");
                    }
                })?;
                Ok(1)
            })
        })
        .expect("a hook's failure is no cut's");
    assert_eq!(committed, 1);
    assert_eq!(*log.lock().unwrap(), ["the hook handled its failure"]);
}

#[test]
fn a_failed_cut_waits_until_something_it_observed_changes() {
    let mut rt = runtime();
    let (waiting, blocked) = mpsc::channel();
    let attempts = rt
        .run((), |root| {
            let reader = root.fork("reader", move |m| {
                let attempts = Cell::new(0);
                let value = m.cut(|m| {
                    attempts.set(attempts.get() + 1);
                    m.heap_get(["ready"]);
                    m.eval()?;
                    if m.kind()? == ValueKind::Number {
                        return m.pop_int();
                    }
                    // Signals the writer as the failure backtracks, before
                    // the cut waits, forcing the write after the read.
                    let waiting = waiting.clone();
                    m.on_abort(0, move |_| waiting.send(()).expect("the writer listens"))?;
                    Err(m.fail())
                })?;
                assert_eq!(value, 5);
                m.push_int(attempts.get());
                Ok(())
            })?;
            blocked.recv().expect("the reader should fail first");
            root.push_int(5);
            root.heap_set(["ready"])?;
            root.join(reader)?;
            root.take("reader", 1)?;
            root.pop_int()
        })
        .expect("the cut should commit once the value is ready");
    assert_eq!(attempts, 2, "the cut runs again once, after the write");
}

#[test]
fn a_conflicting_commit_reruns_the_cut_at_once() {
    let mut rt = runtime();
    let (read_sender, read_receiver) = mpsc::channel();
    let (written_sender, written_receiver) = mpsc::channel();
    rt.run("setup", |m| {
        m.push_int(1);
        m.heap_set(["x"])
    })
    .expect("the setup should commit");
    let attempts = rt
        .run((), |root| {
            let doubler = root.fork("doubler", move |m| {
                let attempts = Cell::new(0);
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
                })?;
                m.push_int(attempts.get());
                Ok(())
            })?;
            read_receiver.recv().expect("the doubler reads first");
            root.push_int(10);
            root.heap_set(["x"])?;
            written_sender.send(()).expect("the doubler waits");
            root.join(doubler)?;
            root.take("doubler", 1)?;
            root.pop_int()
        })
        .expect("the cut should commit");
    assert_eq!(attempts, 2, "the conflict reruns the cut");
    assert_eq!(
        heap_int(&mut rt, &["doubled"]),
        20,
        "the rerun reads the new value"
    );
}

#[test]
fn a_child_turn_joins_its_parent_s_cut() {
    let mut rt = runtime();
    rt.run("parent", |m| {
        let failed: Result<(), Error> = m.cut(|m| {
            m.run("child", |c| {
                c.push_int(1);
                c.heap_set(["by_child"])?;
                c.push_int(7);
                c.set("mark")
            })?;
            Err(m.fail())
        });
        assert!(failed.is_err());
        m.run("child", |c| {
            assert!(c.get("mark").is_err(), "the cut undid the child's state");
            Ok(())
        })?;
        m.cut(|m| {
            m.push_int(3);
            m.heap_set(["by_parent"])?;
            m.run("child", |c| {
                c.heap_get(["by_parent"]);
                assert_eq!(c.pop_int()?, 3, "a child reads its parent's staged write");
                c.push_int(2);
                c.heap_set(["by_child"])
            })
        })
    })
    .expect("the turn should succeed");
    assert_eq!(
        (
            heap_int(&mut rt, &["by_parent"]),
            heap_int(&mut rt, &["by_child"])
        ),
        (3, 2),
        "the cut commits the parent's and the child's writes together"
    );
}

#[test]
fn hooks_run_on_the_machine_that_registered_them() {
    let mut rt = runtime();
    let log = Log::default();
    rt.run("parent", |m| {
        let failed: Result<(), Error> = m.cut(|m| {
            m.run("child", |c| {
                c.on_abort(0, |c| {
                    c.push_int(1);
                    c.set("compensated").expect("the compensation should set");
                })
            })?;
            Err(Error::new("the cut errs"))
        });
        assert!(failed.is_err());
        m.cut(|m| {
            m.run("child", |c| {
                c.push_text("child");
                c.set("name")?;
                let log = log.clone();
                c.on_commit(0, move |c| {
                    c.get("name").expect("the child holds its name");
                    let name = c.pop_text().expect("the name is text");
                    log.lock().unwrap().push(if name == "child" {
                        "the child's hook ran on the child"
                    } else {
                        "elsewhere"
                    });
                })
            })
        })
    })
    .expect("the turn should succeed");
    assert_eq!(*log.lock().unwrap(), ["the child's hook ran on the child"]);
    let compensated = rt
        .run(["parent", "child"], |c| {
            c.get("compensated")?;
            c.pop_int()
        })
        .expect("the abort hook ran on the child, after the undo");
    assert_eq!(compensated, 1);
}

#[test]
fn a_failure_without_observations_or_outside_a_cut_ends_the_turn() {
    let mut rt = runtime();
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

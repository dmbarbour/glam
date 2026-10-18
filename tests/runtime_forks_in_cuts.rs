//! Forks within a cut: each works on a branch of the cut's transaction that
//! merges at its join, and siblings serialize in join order
//! (`machines-form-a-tree`).

use std::cell::Cell;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use glam::{Error, Machine, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

fn heap_int(rt: &mut Runtime, key: &str) -> i64 {
    rt.run("reader", |m| {
        m.heap_get([key]);
        m.pop_int()
    })
    .expect("the heap should hold an integer there")
}

fn set_ints(rt: &mut Runtime, entries: &[(&str, i64)]) {
    rt.run("setup", |m| {
        for (key, value) in entries {
            m.push_int(*value);
            m.heap_set([*key])?;
        }
        Ok(())
    })
    .expect("the setup should commit");
}

/// A fork that reads `from`, signals that it has read, and writes `to` as
/// what it read plus `add`.
fn copy_add(
    from: &'static str,
    to: &'static str,
    add: i64,
    read: mpsc::Sender<()>,
) -> impl FnOnce(&mut Machine<'_, '_>) -> Result<(), Error> + Send + 'static {
    move |f| {
        f.heap_get([from]);
        let value = f.pop_int()?;
        read.send(()).expect("the cut listens");
        f.push_int(value + add);
        f.heap_set([to])
    }
}

#[test]
fn forks_within_a_cut_join_its_transaction() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.cut(|m| {
            m.push_int(1);
            m.heap_set(["before"])?;
            let left = m.fork("left", |l| {
                l.heap_get(["before"]);
                let before = l.pop_int()?;
                l.push_int(before + 10);
                l.heap_set(["left"])?;
                let inner = l.fork("inner", |i| {
                    i.push_int(3);
                    i.heap_set(["inner"])
                })?;
                l.join(inner)
            })?;
            let right = m.fork("right", |r| {
                r.push_int(20);
                r.heap_set(["right"])?;
                r.push_int(5);
                r.set("mark")
            })?;
            m.join(left)?;
            m.join(right)?;
            m.heap_get(["left"]);
            assert_eq!(m.pop_int()?, 11, "the cut sees what its forks merged");
            Ok(())
        })
    })
    .expect("the cut should commit");
    assert_eq!(
        [
            heap_int(&mut rt, "left"),
            heap_int(&mut rt, "right"),
            heap_int(&mut rt, "inner"),
        ],
        [11, 20, 3],
        "a fork sees the cut's writes before it, and its writes commit with the cut"
    );
    let mark = rt
        .run(["m", "right"], |r| {
            r.get("mark")?;
            r.pop_int()
        })
        .expect("the joined machine keeps its state");
    assert_eq!(mark, 5);
}

#[test]
fn a_failed_cut_undoes_its_forks_which_join_at_its_end() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let failed: Result<(), Error> = m.cut(|m| {
            m.fork("child", |c| {
                c.push_int(1);
                c.heap_set(["x"])?;
                c.push_int(2);
                c.set("mark")
            })?;
            Err(Error::new("the cut errs"))
        });
        assert!(failed.is_err());
        m.run("child", |c| {
            assert!(c.get("mark").is_err(), "the cut undid the fork's machine");
            Ok(())
        })
    })
    .expect("the turn should succeed");
    let x = rt
        .run("reader", |m| {
            m.heap_get(["x"]);
            m.eval()?;
            Ok(m.kind()? == glam::ValueKind::Dict)
        })
        .expect("the read should succeed");
    assert!(x, "the cut undid the fork's write");
}

#[test]
fn sibling_forks_serialize_in_join_order() {
    let mut rt = runtime();
    set_ints(&mut rt, &[("x", 1), ("y", 1)]);
    let attempts = Cell::new(0);
    rt.run("m", |m| {
        m.cut(|m| {
            attempts.set(attempts.get() + 1);
            let (read, have_read) = mpsc::channel();
            let a = m.fork("a", copy_add("x", "y", 10, read.clone()))?;
            if attempts.get() > 1 {
                have_read.recv().expect("a reads");
                m.join(a)?;
                let b = m.fork("b", copy_add("y", "x", 100, read))?;
                have_read.recv().expect("b reads");
                return m.join(b);
            }
            // Both read before either joins: write skew, which no serial
            // order explains, unless the cut runs again.
            let b = m.fork("b", copy_add("y", "x", 100, read))?;
            have_read.recv().expect("a fork reads");
            have_read.recv().expect("the other reads");
            m.join(a)?;
            let error = m.join(b).expect_err("b read what a wrote after b's fork");
            assert!(error.to_string().contains("runs again"), "{error}");
            Err(error)
        })
    })
    .expect("the cut should commit");
    assert_eq!(attempts.get(), 2, "the inconsistent attempt runs again");
    assert_eq!(
        (heap_int(&mut rt, "x"), heap_int(&mut rt, "y")),
        (111, 11),
        "a, then b"
    );
}

#[test]
fn a_fork_conflicts_with_what_its_cut_wrote_after_the_fork() {
    let mut rt = runtime();
    set_ints(&mut rt, &[("x", 1)]);
    let attempts = Cell::new(0);
    rt.run("m", |m| {
        m.cut(|m| {
            attempts.set(attempts.get() + 1);
            // The cut's own write before the fork covers its later reads,
            // but not the fork's: the fork depends on it.
            m.push_int(2);
            m.heap_set(["x"])?;
            let (read, have_read) = mpsc::channel();
            let fork = m.fork("f", copy_add("x", "seen", 0, read))?;
            have_read.recv().expect("the fork reads");
            if attempts.get() == 1 {
                m.push_int(3);
                m.heap_set(["x"])?;
            }
            // An error that the body swallows still runs the cut again.
            let _ = m.join(fork);
            Ok(())
        })
    })
    .expect("the cut should commit");
    assert_eq!(attempts.get(), 2);
    assert_eq!(heap_int(&mut rt, "seen"), 2);
}

#[test]
fn a_fork_s_hooks_run_on_its_machine() {
    let mut rt = runtime();
    let log = Arc::new(Mutex::new(Vec::new()));
    rt.run("m", |m| {
        m.cut(|m| {
            let log = Arc::clone(&log);
            let fork = m.fork("f", move |f| {
                f.push_text("forked");
                f.set("name")?;
                f.on_commit(0, move |f| {
                    f.get("name")?;
                    log.lock().unwrap().push(f.pop_text()?);
                    Ok(())
                })
            })?;
            m.join(fork)
        })
    })
    .expect("the cut should commit");
    assert_eq!(*log.lock().unwrap(), ["forked"]);
}

#[test]
fn a_failed_cut_waits_on_what_its_forks_read() {
    let mut rt = runtime();
    let attempts = Cell::new(0);
    let (waiting, blocked) = mpsc::channel::<()>();
    rt.run((), |root| {
        // Outside the cut, a writer waits for the cut to fail.
        let writer = root.fork("writer", move |w| {
            blocked.recv().expect("the cut fails first");
            w.push_int(5);
            w.heap_set(["ready"])
        })?;
        let ready = root.cut(|m| {
            attempts.set(attempts.get() + 1);
            let reader = m.fork("reader", |r| {
                r.heap_get(["ready"]);
                r.eval()?;
                Ok(r.kind()? == glam::ValueKind::Number)
            })?;
            if m.join(reader)? {
                return Ok(());
            }
            let waiting = waiting.clone();
            m.on_abort(0, move |_| {
                waiting.send(()).expect("the writer listens");
                Ok(())
            })?;
            Err(m.fail())
        });
        root.join(writer)?;
        ready
    })
    .expect("the cut should commit once the fork's read changes");
    assert_eq!(attempts.get(), 2);
}

#[test]
fn a_fork_within_a_cut_borrows_what_the_turn_declared_before_it() {
    let mut rt = runtime();
    let total = rt
        .run("m", |m| {
            let words = ["declared", "in", "the", "turn"];
            // Made at the turn's level, it joins there, after the cut.
            let outside = m.fork("outside", |_| Ok(1))?;
            let inside = m.cut(|m| {
                let fork = m.fork("inside", |_| Ok(words.len()))?;
                m.join(fork)
            })?;
            Ok(inside + m.join(outside)?)
        })
        .expect("the turn should succeed");
    assert_eq!(total, 5);
}

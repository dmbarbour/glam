//! Forks: a turn takes descendants to threads that join by its end, so
//! concurrency stays within the tree of machines (`machines-form-a-tree`).
//! What a fork leaves is on its machine's stack, and an error it ends in
//! leaves its machine in error (`machine-errors-mark-and-recover`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use glam::{Error, ErrorKind, Machine, MachinePath, MachineStatus, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

#[test]
fn a_turn_forks_descendants_and_joins_them() {
    let mut rt = runtime();
    let doubled = rt
        .run("parent", |m| {
            let forks = (0..4)
                .map(|index| {
                    m.fork(format!("worker{index}"), move |w| {
                        w.int_push(index);
                        w.local_set("index")?;
                        w.int_push(index * 2);
                        Ok(())
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            for fork in forks {
                m.join(fork)?;
            }
            let mut doubled = 0;
            for index in 0..4 {
                m.take(format!("worker{index}"), 1)?;
                doubled += m.int_pop()?;
            }
            Ok(doubled)
        })
        .expect("the turn should succeed");
    assert_eq!(doubled, 12);
    for index in 0..4 {
        let kept = rt
            .run(["parent".to_owned(), format!("worker{index}")], |w| {
                w.local_get("index");
                w.int_pop()
            })
            .expect("a joined machine keeps what its fork did");
        assert_eq!(kept, index);
    }
}

#[test]
fn a_fork_may_borrow_what_outlives_the_turn() {
    let mut rt = runtime();
    let words = ["borrowed", "by", "a", "fork"];
    let count = rt
        .run("m", |m| {
            let fork = m.fork("counter", |c| {
                c.int_push(words.len() as i64);
                Ok(())
            })?;
            m.join(fork)?;
            m.take("counter", 1)?;
            m.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(count, 4);
}

#[test]
fn a_forked_machine_is_unreachable_until_its_join() {
    let mut rt = runtime();
    let (release, released) = mpsc::channel::<()>();
    rt.run("m", |m| {
        let fork = m.fork("busy", move |b| {
            released.recv().expect("the turn releases the fork");
            b.int_push(1);
            Ok(())
        })?;
        for error in [
            m.run("busy", |_| Ok(())).expect_err("busy is forked"),
            m.run(["busy", "below"], |_| Ok(()))
                .expect_err("so is below it"),
            m.give("busy", 0).expect_err("busy is forked"),
            m.take(["busy", "below"], 0).expect_err("so is below it"),
            m.fork("busy", |_| Ok(()))
                .map(|_| ())
                .expect_err("busy is forked"),
        ] {
            assert!(error.to_string().contains("on a fork"), "{error}");
        }
        let error = m.clear().expect_err("a forked descendant stays");
        assert!(error.to_string().contains("joined"), "{error}");
        release.send(()).expect("the fork waits");
        m.join(fork)?;
        m.take("busy", 1)?;
        assert_eq!(m.int_pop()?, 1, "the joined machine is back");

        let below = m.fork(["outer", "inner"], |_| Ok(()))?;
        let error = m
            .fork("outer", |_| Ok(()))
            .map(|_| ())
            .expect_err("a machine with a fork below it stays");
        assert!(error.to_string().contains("below it"), "{error}");
        m.join(below)
    })
    .expect("the turn should succeed");
}

#[test]
fn unjoined_forks_join_at_the_end_of_their_scope_or_turn() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.scope(|m| {
            let _ = m.fork("scoped", |s| {
                s.int_push(1);
                Ok(())
            })?;
            Ok(())
        })?;
        m.take("scoped", 1)?;
        assert_eq!(m.int_pop()?, 1, "the scope joined its fork");
        let _ = m.fork("unjoined", |u| {
            u.int_push(2);
            Ok(())
        })?;
        Ok(())
    })
    .expect("the turn should succeed");
    let left = rt
        .run(["m", "unjoined"], |u| u.int_pop())
        .expect("the turn joined its fork");
    assert_eq!(left, 2);
}

#[test]
fn a_panic_in_a_fork_resumes_at_its_join_and_marks_each_machine_it_unwinds() {
    let mut rt = runtime();
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        rt.run("m", |m| {
            let fork = m.fork("p", |p| -> Result<(), Error> {
                p.int_push(5);
                p.local_set("before")?;
                panic!("the fork panics");
            })?;
            m.join(fork)
        })
    }));
    assert!(panicked.is_err(), "the panic resumes through the turn");
    assert!(
        rt.failed("m") && rt.failed(["m", "p"]),
        "the panic left each machine it unwound in error"
    );
    let kept = rt
        .recover(["m", "p"], |p| {
            p.local_get("before");
            p.int_pop()
        })
        .expect("the machine recovers after a panic");
    assert_eq!(kept, 5, "a fork's changes stay when it unwinds");

    let unjoined = catch_unwind(AssertUnwindSafe(|| {
        rt.run("n", |m| {
            let _ = m.fork("q", |_| -> Result<(), Error> {
                panic!("an unjoined fork panics")
            })?;
            Ok(())
        })
    }));
    assert!(
        unjoined.is_err(),
        "an unjoined fork's panic resumes at the turn's end"
    );
    assert!(
        rt.failed(["n", "q"]),
        "the panic left the fork's machine in error"
    );
    assert!(
        !rt.failed("n"),
        "but not the turn's, whose body had returned"
    );
}

#[test]
fn a_join_returns_the_fork_s_outcome_and_its_machine_stays_in_error() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let fork = m.fork("f", |f| {
            f.int_push(7);
            Err(Error::new("the fork errs"))
        })?;
        let error = m.join(fork).expect_err("the join returns the fork's error");
        assert_eq!(error.to_string(), "the fork errs");
        for error in [
            m.run("f", |_| Ok(())).expect_err("f is in error"),
            m.give("f", 0).expect_err("f is in error"),
            m.take("f", 0).expect_err("f is in error"),
            m.fork("f", |_| Ok(()))
                .map(|_| ())
                .expect_err("f is in error"),
        ] {
            assert!(error.to_string().contains("in error"), "{error}");
        }
        assert!(m.failed("f")?, "observing the error recovered nothing");
        let left = m.recover("f", |f| f.int_pop())?;
        assert_eq!(left, 7, "the fork's machine is as its turn left it");
        m.run("f", |_| Ok(()))?;

        let failing = m.fork("g", |g| Err(g.fail()))?;
        let failure = m.join(failing).expect_err("the fork failed");
        assert_eq!(failure.kind(), ErrorKind::Backtrack, "a failure stays one");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn implicit_joins_propagate_the_first_error_in_join_order() {
    let mut rt = runtime();
    let erring = |text: &'static str| {
        move |_: &mut Machine<'_, '_>| -> Result<(), Error> { Err(Error::new(text)) }
    };
    rt.run("m", |m| {
        let first = m.scope(|m| {
            let _ = m.fork("a", |_| Ok(()))?;
            let _ = m.fork("b", erring("b errs"))?;
            let _ = m.fork("c", erring("c errs"))?;
            Ok(())
        });
        assert_eq!(
            first.expect_err("a fork erred").to_string(),
            "b errs",
            "the first in join order, the order the forks were made"
        );
        let body = m.scope(|m| -> Result<(), Error> {
            let _ = m.fork("d", erring("d errs"))?;
            Err(Error::new("the body errs"))
        });
        assert_eq!(
            body.expect_err("the body erred").to_string(),
            "the body errs",
            "the body's own outcome comes first"
        );
        Ok(())
    })
    .expect("the scopes returned their errors to the turn");

    rt.run("n", |m| {
        let _ = m.fork("e", erring("e errs"))?;
        Ok(())
    })
    .expect("a fork left to the turn's end is no early exit of the turn");
    assert!(
        rt.failed(["n", "e"]) && !rt.failed("n"),
        "the fork's error stays in its own machine"
    );
}

#[test]
fn the_walk_reports_each_machine_below_a_path_with_its_status() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.int_push(1);
        m.give("b", 1)?;
        let _ = m.fork(["a", "x"], |_| -> Result<(), Error> {
            Err(Error::new("x errs"))
        })?;
        Ok(())
    })
    .expect("the turn should succeed");
    let walked: Vec<_> = rt
        .machines("m")
        .map(|(path, status)| (path.keys().len(), status))
        .collect();
    assert_eq!(
        walked,
        [
            (2, MachineStatus::Idle),
            (3, MachineStatus::Failed),
            (2, MachineStatus::Idle),
        ],
        "m/a, m/a/x, m/b: depth first, in key order, from the root"
    );
    let failures: Vec<_> = rt
        .machines(())
        .filter(|(_, status)| *status == MachineStatus::Failed)
        .map(|(path, _)| path)
        .collect();
    assert_eq!(failures, [MachinePath::from(["m", "a", "x"])]);

    let (release, released) = mpsc::channel::<()>();
    rt.run("m", |m| {
        let fork = m.fork("b", move |_| {
            released.recv().expect("the turn releases the fork");
            Ok(())
        })?;
        let walked: Vec<_> = m.machines(())?.collect();
        assert_eq!(
            walked,
            [
                (MachinePath::from(["a"]), MachineStatus::Idle),
                (MachinePath::from(["a", "x"]), MachineStatus::Failed),
                (MachinePath::from(["b"]), MachineStatus::Forked),
            ],
            "from the machine, relative to it"
        );
        assert!(
            m.machines("b").is_err(),
            "a forked machine's subtree is on its thread"
        );
        release.send(()).expect("the fork waits");
        m.join(fork)
    })
    .expect("the turn should succeed");
}

#[test]
fn a_fork_s_thread_may_be_configured() {
    let mut rt = runtime();
    let name = rt
        .run("m", |m| {
            let fork = m.fork_with(thread::Builder::new().name("named".to_owned()), "c", |c| {
                c.text_push(thread::current().name().unwrap_or("unnamed"));
                Ok(())
            })?;
            m.join(fork)?;
            m.take("c", 1)?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(name, "named");
}

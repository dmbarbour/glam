//! Forks: a turn takes descendants to threads that join by its end, so
//! concurrency stays within the tree of machines (`machines-form-a-tree`).
//! What a fork leaves is on its machine's stack, and an error it ends in
//! leaves its machine in error (`machine-errors-mark-and-recover`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use glam::{Error, ErrorKind, Machine, Runtime, RuntimeConfig};

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
                        w.push_int(index);
                        w.set("index")?;
                        w.push_int(index * 2);
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
                doubled += m.pop_int()?;
            }
            Ok(doubled)
        })
        .expect("the turn should succeed");
    assert_eq!(doubled, 12);
    for index in 0..4 {
        let kept = rt
            .run(["parent".to_owned(), format!("worker{index}")], |w| {
                w.get("index")?;
                w.pop_int()
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
                c.push_int(words.len() as i64);
                Ok(())
            })?;
            m.join(fork)?;
            m.take("counter", 1)?;
            m.pop_int()
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
            b.push_int(1);
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
        assert_eq!(m.pop_int()?, 1, "the joined machine is back");

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
            m.fork("scoped", |s| {
                s.push_int(1);
                Ok(())
            })?;
            Ok(())
        })?;
        m.take("scoped", 1)?;
        assert_eq!(m.pop_int()?, 1, "the scope joined its fork");
        m.fork("unjoined", |u| {
            u.push_int(2);
            Ok(())
        })?;
        Ok(())
    })
    .expect("the turn should succeed");
    let left = rt
        .run(["m", "unjoined"], |u| u.pop_int())
        .expect("the turn joined its fork");
    assert_eq!(left, 2);
}

#[test]
fn a_panic_in_a_fork_resumes_at_its_join_and_marks_each_machine_it_unwinds() {
    let mut rt = runtime();
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        rt.run("m", |m| {
            let fork = m.fork("p", |p| -> Result<(), Error> {
                p.push_int(5);
                p.set("before")?;
                panic!("the fork panics");
            })?;
            m.join(fork)
        })
    }));
    assert!(panicked.is_err(), "the panic resumes through the turn");
    for path in [vec!["m"], vec!["m", "p"]] {
        let error = rt
            .run(path, |_| Ok(()))
            .expect_err("the panic left the machine in error");
        assert!(error.to_string().contains("in error"), "{error}");
    }
    let (errors, kept) = rt
        .recover(["m", "p"], |p| {
            let errors = p.unpack_list()?;
            p.discard(errors)?;
            p.get("before")?;
            Ok((errors, p.pop_int()?))
        })
        .expect("the machine recovers after a panic");
    assert_eq!(errors, 1, "the panic's diagnostic");
    assert_eq!(kept, 5, "a fork's changes stay when it unwinds");

    let unjoined = catch_unwind(AssertUnwindSafe(|| {
        rt.run("n", |m| {
            m.fork("q", |_| -> Result<(), Error> {
                panic!("an unjoined fork panics")
            })?;
            Ok(())
        })
    }));
    assert!(
        unjoined.is_err(),
        "an unjoined fork's panic resumes at the turn's end"
    );
}

#[test]
fn a_join_returns_the_fork_s_outcome_and_its_machine_stays_in_error() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let fork = m.fork("f", |f| {
            f.push_int(7);
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
        let (errors, left) = m.recover("f", |f| {
            let errors = f.unpack_list()?;
            f.discard(errors)?;
            Ok((errors, f.pop_int()?))
        })?;
        assert_eq!(
            (errors, left),
            (1, 7),
            "the error's diagnostic sits above what the fork left"
        );
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
            m.fork("a", |_| Ok(()))?;
            m.fork("b", erring("b errs"))?;
            m.fork("c", erring("c errs"))?;
            Ok(())
        });
        assert_eq!(
            first.expect_err("a fork erred").to_string(),
            "b errs",
            "the first in join order, the order the forks were made"
        );
        let body = m.scope(|m| -> Result<(), Error> {
            m.fork("d", erring("d errs"))?;
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

    let error = rt
        .run("n", |m| {
            m.fork("e", erring("e errs"))?;
            Ok(())
        })
        .expect_err("an unjoined fork's error ends the turn");
    assert_eq!(error.to_string(), "e errs");
    let errors = rt
        .recover("n", |n| n.unpack_list())
        .expect("the turn's machine recovers");
    assert_eq!(
        errors, 1,
        "the fork's error left the turn's machine in error too"
    );
}

#[test]
fn a_fork_s_thread_may_be_configured() {
    let mut rt = runtime();
    let name = rt
        .run("m", |m| {
            let fork = m.fork_with(thread::Builder::new().name("named".to_owned()), "c", |c| {
                c.push_text(thread::current().name().unwrap_or("unnamed"));
                Ok(())
            })?;
            m.join(fork)?;
            m.take("c", 1)?;
            m.pop_text()
        })
        .expect("the turn should succeed");
    assert_eq!(name, "named");
}

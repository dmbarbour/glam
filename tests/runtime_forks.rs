//! Forks: a turn takes descendants to threads that join by its end, so
//! concurrency stays within the tree of machines (`machines-form-a-tree`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use glam::{Error, Runtime, RuntimeConfig};

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
                        Ok(index * 2)
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;
            forks
                .into_iter()
                .map(|fork| m.join(fork))
                .sum::<Result<i64, Error>>()
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
            let fork = m.fork("counter", |_| Ok(words.len()))?;
            m.join(fork)
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
            })
            .map(|_| ())
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
fn a_panic_in_a_fork_resumes_at_its_join_and_its_machine_stays() {
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
    let kept = rt
        .run(["m", "p"], |p| {
            p.get("before")?;
            p.pop_int()
        })
        .expect("the machine takes turns after a panic");
    assert_eq!(kept, 5, "a fork's changes stay when it unwinds");

    let unjoined = catch_unwind(AssertUnwindSafe(|| {
        rt.run("m", |m| {
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
fn a_fork_joins_on_the_machine_that_made_it() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let fork = m.fork("c", |_| Ok(()))?;
        let error = m
            .run("other", move |o| o.join(fork))
            .expect_err("another machine cannot join it");
        assert!(error.to_string().contains("another machine"), "{error}");
        Ok(())
    })
    .expect("the turn joins the fork at its end");
}

#[test]
fn forks_within_a_cut_are_not_supported_yet() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let mut fork = Some(m.fork("before", |_| Ok(()))?);
        let (forked, joined) = m.cut(|m| {
            let forked = m.fork("within", |_| Ok(())).map(|_| ()).unwrap_err();
            let joined = m.join(fork.take().expect("one attempt")).unwrap_err();
            Ok((forked, joined))
        })?;
        assert!(forked.to_string().contains("not supported"), "{forked}");
        assert!(joined.to_string().contains("not supported"), "{joined}");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_fork_s_thread_may_be_configured() {
    let mut rt = runtime();
    let name = rt
        .run("m", |m| {
            let fork = m.fork_with(thread::Builder::new().name("named".to_owned()), "c", |_| {
                Ok(thread::current().name().map(str::to_owned))
            })?;
            m.join(fork)
        })
        .expect("the turn should succeed");
    assert_eq!(name.as_deref(), Some("named"));
}

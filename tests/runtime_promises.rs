//! Promises that machines allocate and resolve, once
//! (`machines-inherit-an-environment`), within cuts as part of their
//! transactions (`promise-resolution-commits-with-its-cut`).

use std::sync::mpsc;

use glam::{Error, ErrorKind, Machine, ModuleInput, PromiseStatus, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

#[test]
fn a_fulfilled_promise_reads_its_value() {
    let mut rt = runtime();
    let read = rt
        .run("m", |m| {
            m.promise();
            m.set("resolver")?;
            m.set("promised")?;
            m.get("resolver")?;
            m.push_int(42);
            m.fulfill()?;
            m.get("promised")?;
            m.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(read, 42);
}

/// Reads, in a cut, the integer that `read` pushes, sending on `waiting`
/// when its first attempt finds it not known yet, so the cut waits.
fn await_int(
    m: &mut Machine<'_, '_>,
    waiting: mpsc::Sender<()>,
    read: impl Fn(&mut Machine<'_, '_>) -> Result<(), Error>,
) -> Result<i64, Error> {
    let mut attempts = 0;
    m.cut(|m| {
        read(m)?;
        let value = m.pop_int();
        if let Err(error) = &value {
            assert_eq!(error.kind(), ErrorKind::Retry, "{error}");
            attempts += 1;
            if attempts == 1 {
                waiting
                    .send(())
                    .expect("the resolver waits for the attempt");
            }
        }
        value
    })
}

#[test]
fn a_cut_waits_until_the_promise_is_fulfilled() {
    let mut rt = runtime();
    let (waiting, waited) = mpsc::channel();
    let value = rt
        .run("p", |p| {
            p.promise();
            p.set("resolver")?;
            p.give("reader", 1)?;
            let fork = p.fork("reader", move |r| {
                r.set("promised")?;
                let value = await_int(r, waiting, |r| r.get("promised"))?;
                r.push_int(value);
                Ok(())
            })?;
            waited.recv().expect("the reader's first attempt waits");
            p.get("resolver")?;
            p.push_int(7);
            p.fulfill()?;
            p.join(fork)?;
            p.take("reader", 1)?;
            p.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(value, 7, "the reader read what the parent fulfilled");
}

#[test]
fn outside_a_cut_an_unresolved_promise_is_pushback_that_pops_nothing() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.promise();
        // Kept, for a resolver lost would break the promise once the
        // runtime collects (`opaque-resources-reclaimed-when-unreachable`).
        m.set("resolver")?;
        let error = m.pop_int().expect_err("nothing resolved it");
        assert_eq!(error.kind(), ErrorKind::Retry);
        assert_eq!(m.depth(), 1, "the promised value stays");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_promise_resolves_once_and_a_broken_one_fails_its_readers() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.promise();
        m.set("resolver")?;
        m.set("promised")?;

        m.get("resolver")?;
        m.push_text("broken by the test");
        m.break_promise()?;
        m.get("resolver")?;
        m.push_text("again");
        let error = m.break_promise().expect_err("it resolved already");
        assert!(error.to_string().contains("already resolved"), "{error}");
        assert_eq!(m.depth(), 2, "the error popped nothing");
        m.discard(2)?;

        m.get("promised")?;
        let error = m.pop_int().expect_err("the promise was broken");
        assert_eq!(error.kind(), ErrorKind::Failure, "a broken promise fails");
        m.discard(1)?;

        m.push_int(3);
        m.push_int(4);
        let error = m.fulfill().expect_err("an integer resolves nothing");
        assert!(error.to_string().contains("not a resolver"), "{error}");
        Ok(())
    })
    .expect("the turn should succeed");
}

/// Binds as the machine's share an object whose `args` is the value on top.
fn bind_args(m: &mut Machine<'_, '_>) -> Result<(), Error> {
    m.stash(1)?;
    m.compile(
        ["share"],
        [ModuleInput::script(
            "g",
            "language g0\nimport 'std\nshare args = object_from_dict {args:args}\n",
        )],
    )?;
    m.access("share")?;
    m.stash_pop(1)?;
    m.apply(1)?;
    m.set_env_mixin()
}

#[test]
fn a_promised_value_in_a_share_waits_alone_until_fulfilled() {
    let mut rt = runtime();
    let (waiting, waited) = mpsc::channel();
    let args = rt
        .run((), |root| {
            root.promise();
            root.set("resolver")?;
            bind_args(root)?;
            let fork = root.fork("reader", move |r| {
                r.env();
                r.access("spec")?;
                r.eval()?;
                r.discard(1)?;
                let args = await_int(r, waiting, |r| {
                    r.env();
                    r.access("args")
                })?;
                r.push_int(args);
                Ok(())
            })?;
            waited.recv().expect("the reader's first attempt waits");
            root.get("resolver")?;
            root.push_int(5);
            root.fulfill()?;
            root.join(fork)?;
            root.take("reader", 1)?;
            root.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(
        args, 5,
        "the environment read at once, its promised `args` alone waited"
    );
}

#[test]
fn a_cut_s_resolution_commits_with_it() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.promise();
        m.set("resolver")?;
        m.set("promised")?;

        let abandoned = m.cut(|m| -> Result<(), Error> {
            m.get("resolver")?;
            m.push_int(1);
            m.fulfill()?;
            m.get("resolver")?;
            assert_eq!(
                m.promise_status()?,
                PromiseStatus::Fulfilled,
                "the cut sees its own resolution"
            );
            Err(Error::new("abandoned"))
        });
        assert!(abandoned.is_err());
        m.get("resolver")?;
        assert_eq!(
            m.promise_status()?,
            PromiseStatus::Pending,
            "the failed cut resolved nothing"
        );
        m.discard(1)?;

        m.cut(|m| {
            m.get("resolver")?;
            m.push_int(2);
            m.fulfill()
        })?;
        m.get("promised")?;
        assert_eq!(m.pop_int()?, 2, "the committed cut resolved it");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_resolution_beaten_to_the_commit_fails_when_its_cut_runs_again() {
    let mut rt = runtime();
    let (staged, staging) = mpsc::channel();
    let (raced, racing) = mpsc::channel();
    rt.run("p", |p| {
        p.promise();
        p.set("resolver")?;
        p.set("promised")?;
        p.get("resolver")?;
        p.give("server", 1)?;
        let fork = p.fork("server", move |s| {
            s.set("reply")?;
            let mut attempts = 0;
            let outcome = s.cut(|s| {
                attempts += 1;
                s.get("reply")?;
                s.push_int(1);
                s.fulfill()?;
                if attempts == 1 {
                    staged.send(()).expect("the parent waits for the staging");
                    racing.recv().expect("the parent resolves first");
                }
                Ok(())
            });
            let error = outcome.expect_err("running again, it finds the promise resolved");
            assert!(error.to_string().contains("already resolved"), "{error}");
            assert_eq!(attempts, 2, "the first commit conflicted");
            Ok(())
        })?;
        staging.recv().expect("the server stages its resolution");
        p.get("resolver")?;
        p.push_int(2);
        p.fulfill()?;
        raced.send(()).expect("the server awaits the race");
        p.join(fork)?;
        p.get("promised")?;
        assert_eq!(p.pop_int()?, 2, "the first resolution stands");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn forks_resolving_one_promise_within_one_cut_fail_the_second_join() {
    let mut rt = runtime();
    rt.run("p", |p| {
        p.promise();
        p.set("resolver")?;
        p.set("promised")?;
        let error = p
            .cut(|p| {
                for server in ["first", "second"] {
                    p.get("resolver")?;
                    p.give(server, 1)?;
                }
                let first = p.fork("first", |s| {
                    s.push_int(1);
                    s.fulfill()
                })?;
                let second = p.fork("second", |s| {
                    s.push_int(2);
                    s.fulfill()
                })?;
                p.join(first)?;
                p.join(second)
            })
            .expect_err("the second resolution fails");
        assert!(
            error
                .to_string()
                .contains("resolved a promise that its cut resolved too"),
            "{error}"
        );
        p.get("resolver")?;
        assert_eq!(
            p.promise_status()?,
            PromiseStatus::Pending,
            "the failed cut resolved nothing"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_that_reads_the_value_of_a_promise_it_resolves_fails() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.promise();
        m.set("resolver")?;
        m.set("promised")?;
        let error = m
            .cut(|m| {
                m.get("resolver")?;
                m.push_int(1);
                m.fulfill()?;
                m.get("promised")?;
                m.pop_int()
            })
            .expect_err("only the cut's commit would settle the promise");
        assert!(
            error.to_string().contains("a promise it resolves"),
            "{error}"
        );
        m.get("resolver")?;
        assert_eq!(
            m.promise_status()?,
            PromiseStatus::Pending,
            "the failed cut resolved nothing"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_that_reads_a_promise_it_resolves_through_a_lazy_value_fails() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.promise();
        m.set("resolver")?;
        m.make_dict(["value"])?;
        m.set("record")?;
        let error = m
            .cut(|m| {
                m.get("resolver")?;
                m.push_int(1);
                m.fulfill()?;
                m.get("record")?;
                m.access("value")?;
                m.pop_int()
            })
            .expect_err("only the cut's commit would settle the promise");
        assert!(
            error.to_string().contains("a promise it resolves"),
            "{error}"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

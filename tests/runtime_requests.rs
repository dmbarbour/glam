//! Requests travel on ports, their answers through promises
//! (`requests-travel-on-ports`).

use std::sync::mpsc;

use glam::{Endpoint, Error, ErrorKind, Machine, PromiseStatus, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// Wires the requester `client`'s output `out` to each server's input `in`.
fn wire(p: &mut Machine<'_, '_>, servers: &[&str]) -> Result<(), Error> {
    for server in servers {
        p.wire_add(
            Endpoint::child("client", "out"),
            Endpoint::child(*server, "in"),
        )?;
    }
    Ok(())
}

/// Serves one request at `server`: reads it, and fulfills its reply with
/// the request doubled.
fn serve(p: &mut Machine<'_, '_>, server: &str) -> Result<(), Error> {
    p.run(server, |s| {
        s.port_read("in", 1)?;
        s.local_set("received")?;
        s.local_get("received")?;
        s.dict_get("reply")?;
        s.local_get("received")?;
        s.dict_get("request")?;
        let request = s.int_pop()?;
        s.int_push(request * 2);
        s.promise_fulfill()
    })
}

#[test]
fn a_server_answers_what_a_requester_asks() {
    let mut rt = runtime();
    let answer = rt
        .run("p", |p| {
            wire(p, &["server"])?;
            p.run("client", |c| {
                c.int_push(21);
                c.port_ask("out")
            })?;
            serve(p, "server")?;
            p.run("client", |c| c.int_pop())
        })
        .expect("the turn should succeed");
    assert_eq!(answer, 42);
}

#[test]
fn a_requester_waits_in_a_cut_for_its_answer() {
    let mut rt = runtime();
    let (waiting, waited) = mpsc::channel();
    let answer = rt
        .run("p", |p| {
            wire(p, &["server"])?;
            p.run("client", |c| {
                c.int_push(4);
                c.port_ask("out")?;
                c.local_set("answer")
            })?;
            let fork = p.fork("client", move |c| {
                let mut attempts = 0;
                let answer = c.cut(|c| {
                    c.local_get("answer")?;
                    let answer = c.int_pop();
                    if let Err(error) = &answer {
                        assert_eq!(error.kind(), ErrorKind::Retry, "{error}");
                        attempts += 1;
                        if attempts == 1 {
                            waiting.send(()).expect("the server waits for the attempt");
                        }
                    }
                    answer
                })?;
                c.int_push(answer);
                Ok(())
            })?;
            waited.recv().expect("the requester's first attempt waits");
            serve(p, "server")?;
            p.join(fork)?;
            p.take("client", 1)?;
            p.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(answer, 8);
}

#[test]
fn a_failed_cut_sends_no_request_and_a_channel_keeps_commit_order() {
    let mut rt = runtime();
    rt.run("p", |p| {
        wire(p, &["server"])?;
        p.run("client", |c| {
            let abandoned = c.cut(|c| -> Result<(), Error> {
                c.int_push(1);
                c.port_ask("out")?;
                Err(Error::new("abandoned"))
            });
            assert!(abandoned.is_err());
            for request in [2, 3] {
                c.int_push(request);
                c.port_ask("out")?;
                c.discard(1)?;
            }
            Ok(())
        })?;
        let requests = p.run("server", |s| {
            let count = s.port_available("in");
            s.port_read("in", count)?;
            (0..count)
                .map(|_| {
                    s.dict_get("request")?;
                    s.int_pop()
                })
                .collect::<Result<Vec<_>, Error>>()
        })?;
        assert_eq!(
            requests,
            [3, 2],
            "the abandoned request never left; read top first"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn duplicate_servers_race_and_the_first_resolution_wins() {
    let mut rt = runtime();
    rt.run("p", |p| {
        wire(p, &["first", "second"])?;
        p.run("client", |c| {
            c.int_push(5);
            c.port_ask("out")
        })?;
        serve(p, "first")?;
        p.run("second", |s| {
            s.port_read("in", 1)?;
            s.dict_get("reply")?;
            assert_eq!(
                s.promise_status()?,
                PromiseStatus::Fulfilled,
                "the second server sees it answered"
            );
            s.int_push(0);
            let error = s.promise_fulfill().expect_err("it resolved already");
            assert!(error.to_string().contains("already resolved"), "{error}");
            Ok(())
        })?;
        assert_eq!(
            p.run("client", |c| c.int_pop())?,
            10,
            "the first answer stands"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_resolver_reads_its_promise_s_status() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.promise_new();
        m.local_set("resolver")?;
        let error = m
            .promise_status()
            .expect_err("a promised value stands for its value, which waits");
        assert_eq!(error.kind(), ErrorKind::Retry, "{error}");

        m.local_get("resolver")?;
        assert_eq!(m.promise_status()?, PromiseStatus::Pending);
        m.int_push(7);
        m.promise_fulfill()?;
        m.local_get("resolver")?;
        assert_eq!(m.promise_status()?, PromiseStatus::Fulfilled);
        m.discard(1)?;
        let error = m
            .promise_status()
            .expect_err("the promised value now stands for 7, which has no status");
        assert!(error.to_string().contains("not a resolver"), "{error}");
        m.discard(1)?;

        m.promise_new();
        m.local_set("broken")?;
        m.local_get("broken")?;
        m.text_push("why");
        m.promise_break()?;
        m.local_get("broken")?;
        assert_eq!(m.promise_status()?, PromiseStatus::Broken);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_waiting_on_a_status_wakes_when_the_promise_resolves() {
    let mut rt = runtime();
    let (waiting, waited) = mpsc::channel();
    let status = rt
        .run("p", |p| {
            p.promise_new();
            p.local_set("resolver")?;
            p.discard(1)?;
            p.local_get("resolver")?;
            p.give("watcher", 1)?;
            let fork = p.fork("watcher", move |w| {
                let mut attempts = 0;
                w.cut(|w| {
                    let status = w.promise_status()?;
                    if status == PromiseStatus::Pending {
                        attempts += 1;
                        if attempts == 1 {
                            waiting
                                .send(())
                                .expect("the resolver waits for the attempt");
                        }
                        return Err(w.retry());
                    }
                    Ok(())
                })
            })?;
            waited.recv().expect("the watcher's first attempt waits");
            p.local_get("resolver")?;
            p.int_push(1);
            p.promise_fulfill()?;
            p.join(fork)?;
            p.run("watcher", |w| w.promise_status())
        })
        .expect("the turn should succeed");
    assert_eq!(status, PromiseStatus::Fulfilled);
}

#[test]
fn a_cut_that_read_a_promise_pending_conflicts_with_its_resolution() {
    let mut rt = runtime();
    let (read, resolving) = mpsc::channel();
    let (resolved, awaiting) = mpsc::channel();
    let attempts = rt
        .run("p", |p| {
            p.promise_new();
            p.local_set("resolver")?;
            p.discard(1)?;
            p.local_get("resolver")?;
            p.give("watcher", 1)?;
            let fork = p.fork("watcher", move |w| {
                let mut attempts = 0;
                let status = w.cut(|w| {
                    attempts += 1;
                    let status = w.promise_status()?;
                    if attempts == 1 {
                        read.send(()).expect("the resolver waits for the read");
                        awaiting.recv().expect("the resolution arrives");
                    }
                    Ok(status)
                })?;
                assert_eq!(status, PromiseStatus::Fulfilled, "the rerun sees it");
                w.int_push(attempts);
                Ok(())
            })?;
            resolving
                .recv()
                .expect("the watcher reads the promise pending");
            p.local_get("resolver")?;
            p.int_push(1);
            p.promise_fulfill()?;
            resolved
                .send(())
                .expect("the watcher awaits the resolution");
            p.join(fork)?;
            p.take("watcher", 1)?;
            p.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(
        attempts, 2,
        "the first commit conflicted with the resolution"
    );
}

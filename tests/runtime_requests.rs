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
        p.wire(
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
        s.read("in", 1)?;
        s.set("received")?;
        s.get("received")?;
        s.access("reply")?;
        s.get("received")?;
        s.access("request")?;
        let request = s.pop_int()?;
        s.push_int(request * 2);
        s.fulfill()
    })
}

#[test]
fn a_server_answers_what_a_requester_asks() {
    let mut rt = runtime();
    let answer = rt
        .run("p", |p| {
            wire(p, &["server"])?;
            p.run("client", |c| {
                c.push_int(21);
                c.ask("out")
            })?;
            serve(p, "server")?;
            p.run("client", |c| c.pop_int())
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
                c.push_int(4);
                c.ask("out")?;
                c.set("answer")
            })?;
            let fork = p.fork("client", move |c| {
                let mut attempts = 0;
                let answer = c.cut(|c| {
                    c.get("answer")?;
                    let answer = c.pop_int();
                    if let Err(error) = &answer {
                        assert_eq!(error.kind(), ErrorKind::Retry, "{error}");
                        attempts += 1;
                        if attempts == 1 {
                            waiting.send(()).expect("the server waits for the attempt");
                        }
                    }
                    answer
                })?;
                c.push_int(answer);
                Ok(())
            })?;
            waited.recv().expect("the requester's first attempt waits");
            serve(p, "server")?;
            p.join(fork)?;
            p.take("client", 1)?;
            p.pop_int()
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
                c.push_int(1);
                c.ask("out")?;
                Err(Error::new("abandoned"))
            });
            assert!(abandoned.is_err());
            for request in [2, 3] {
                c.push_int(request);
                c.ask("out")?;
                c.discard(1)?;
            }
            Ok(())
        })?;
        let requests = p.run("server", |s| {
            let count = s.available("in");
            s.read("in", count)?;
            (0..count)
                .map(|_| {
                    s.access("request")?;
                    s.pop_int()
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
            c.push_int(5);
            c.ask("out")
        })?;
        serve(p, "first")?;
        p.run("second", |s| {
            s.read("in", 1)?;
            s.access("reply")?;
            assert_eq!(
                s.promise_status()?,
                PromiseStatus::Fulfilled,
                "the second server sees it answered"
            );
            s.push_int(0);
            let error = s.fulfill().expect_err("it resolved already");
            assert!(error.to_string().contains("already resolved"), "{error}");
            Ok(())
        })?;
        assert_eq!(
            p.run("client", |c| c.pop_int())?,
            10,
            "the first answer stands"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_promise_s_status_follows_its_resolution_without_forcing_it() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.promise();
        m.set("resolver")?;
        assert_eq!(
            m.promise_status()?,
            PromiseStatus::Pending,
            "the promised value"
        );
        m.get("resolver")?;
        assert_eq!(m.promise_status()?, PromiseStatus::Pending, "its resolver");
        m.push_text("why");
        m.break_promise()?;
        assert_eq!(m.promise_status()?, PromiseStatus::Broken);
        m.get("resolver")?;
        assert_eq!(m.promise_status()?, PromiseStatus::Broken);
        m.discard(2)?;

        m.push_int(9);
        let error = m.promise_status().expect_err("an integer is no promise");
        assert!(error.to_string().contains("neither"), "{error}");
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
            p.promise();
            p.set("resolver")?;
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
            p.get("resolver")?;
            p.push_int(1);
            p.fulfill()?;
            p.join(fork)?;
            p.run("watcher", |w| w.promise_status())
        })
        .expect("the turn should succeed");
    assert_eq!(status, PromiseStatus::Fulfilled);
}

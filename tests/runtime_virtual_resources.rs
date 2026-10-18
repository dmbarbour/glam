//! Virtual resources: proxies, named by paths of keys, for host state that
//! the runtime does not see. Cuts get, set and rewrite them as they do the
//! heap, for conflict analysis and wakes
//! (`turn-cuts-bound-transactions-and-search`).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use glam::{Error, ErrorKind, Machine, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// How many times a cut runs `reads`, when `interleaved` runs between them
/// and the cut's first commit.
fn attempts_around(
    reads: impl Fn(&mut Machine<'_>) -> Result<(), Error> + Send,
    interleaved: impl FnOnce(&Runtime),
) -> usize {
    let rt = runtime();
    let rt = &rt;
    let (read_sender, read_receiver) = mpsc::channel();
    let (done_sender, done_receiver) = mpsc::channel();
    thread::scope(|scope| {
        let reader = scope.spawn(move || {
            let attempts = Cell::new(0);
            rt.run("reader", |m| {
                m.cut(|m| {
                    attempts.set(attempts.get() + 1);
                    reads(m)?;
                    if attempts.get() == 1 {
                        read_sender.send(()).expect("the writer listens");
                        done_receiver.recv().expect("the writer finishes");
                    }
                    Ok(())
                })
            })
            .expect("the cut should commit");
            attempts.get()
        });
        read_receiver.recv().expect("the reader reads first");
        interleaved(rt);
        done_sender.send(()).expect("the reader waits");
        reader.join().expect("the reader should not panic")
    })
}

/// Reads the virtual resource at `path`.
fn get(path: &'static [&'static str]) -> impl Fn(&mut Machine<'_>) -> Result<(), Error> + Send {
    move |m| {
        m.virtual_get(path.iter().copied());
        Ok(())
    }
}

/// Sets the virtual resource at `path` outside any cut.
fn set(path: &'static [&'static str]) -> impl FnOnce(&Runtime) {
    move |rt| {
        rt.run("writer", |m| m.virtual_set(path.iter().copied()))
            .expect("the write should commit");
    }
}

#[test]
fn a_write_conflicts_with_cuts_that_read_the_resource_or_a_path_around_it() {
    assert_eq!(
        attempts_around(get(&["fs"]), set(&["fs", "a"])),
        2,
        "a write below the read conflicts, and the cut runs again"
    );
    assert_eq!(
        attempts_around(get(&["fs", "a"]), set(&["fs"])),
        2,
        "a write above the read conflicts"
    );
    let rewrite = |rt: &Runtime| {
        rt.run("writer", |m| m.virtual_rewrite(["fs"]))
            .expect("the write should commit");
    };
    assert_eq!(
        attempts_around(get(&["fs"]), rewrite),
        2,
        "a rewrite conflicts as a set does"
    );
    assert_eq!(
        attempts_around(get(&["fs"]), set(&["net"])),
        1,
        "an unrelated write does not conflict"
    );
    let heap_write = |rt: &Runtime| {
        rt.run("writer", |m| {
            m.push_int(1);
            m.heap_set(["fs"])
        })
        .expect("the write should commit");
    };
    assert_eq!(
        attempts_around(get(&["fs"]), heap_write),
        1,
        "virtual resources lie apart from the heap"
    );
}

#[test]
fn a_set_covers_a_later_read_and_a_rewrite_widens_it() {
    let set_then_get = |m: &mut Machine<'_>| {
        m.virtual_set(["fs"])?;
        m.virtual_get(["fs", "a"]);
        Ok(())
    };
    assert_eq!(
        attempts_around(set_then_get, set(&["fs", "a"])),
        1,
        "the cut's own set covers its read"
    );
    let rewrite_then_get = |m: &mut Machine<'_>| {
        m.virtual_rewrite(["fs"])?;
        m.virtual_get(["fs", "a"]);
        Ok(())
    };
    assert_eq!(
        attempts_around(rewrite_then_get, set(&["fs", "b"])),
        2,
        "a read after the cut's own rewrite depends on the whole rewritten path"
    );
}

#[test]
fn a_cut_s_virtual_write_takes_effect_when_it_commits() {
    let committed = |rt: &Runtime| {
        rt.run("writer", |m| m.cut(|m| m.virtual_set(["fs"])))
            .expect("the cut should commit");
    };
    assert_eq!(attempts_around(get(&["fs"]), committed), 2);
    let failed = |rt: &Runtime| {
        rt.run("writer", |m| -> Result<(), Error> {
            m.cut(|m| {
                m.virtual_set(["fs"])?;
                Err(Error::new("the cut errs"))
            })
        })
        .expect_err("the cut should err");
    };
    assert_eq!(
        attempts_around(get(&["fs"]), failed),
        1,
        "a failed cut writes nothing"
    );
    let nested = |rt: &Runtime| {
        rt.run("writer", |m| {
            m.cut(|m| {
                let failed: Result<(), Error> = m.cut(|m| {
                    m.virtual_rewrite(["fs"])?;
                    Err(m.fail())
                });
                assert!(failed.is_err());
                Ok(())
            })
        })
        .expect("the outer cut should commit");
    };
    assert_eq!(
        attempts_around(get(&["fs"]), nested),
        1,
        "a failed nested cut's write is undone"
    );
}

#[test]
fn a_failed_cut_waits_until_a_virtual_resource_it_read_is_written() {
    let rt = runtime();
    // The host state that the virtual resource stands for.
    let host_ready = AtomicBool::new(false);
    let (waiting, blocked) = mpsc::channel();
    let attempts = thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let attempts = Cell::new(0);
            rt.run("reader", |m| {
                m.cut(|m| {
                    attempts.set(attempts.get() + 1);
                    m.virtual_get(["host", "ready"]);
                    if host_ready.load(Ordering::SeqCst) {
                        return Ok(());
                    }
                    // Signals the watcher as the failure backtracks, before
                    // the cut waits, forcing the write after the read.
                    let waiting = waiting.clone();
                    m.on_abort(0, move |_| {
                        waiting.send(()).expect("the watcher listens");
                        Ok(())
                    })?;
                    Err(m.fail())
                })
            })
            .expect("the cut should commit once the host is ready");
            attempts.get()
        });
        blocked.recv().expect("the reader should fail first");
        host_ready.store(true, Ordering::SeqCst);
        rt.run("watcher", |m| m.virtual_set(["host", "ready"]))
            .expect("the write should commit");
        reader.join().expect("the reader should not panic")
    });
    assert_eq!(
        attempts, 2,
        "the write wakes the cut, which runs again once"
    );
}

#[test]
fn a_failed_cut_whose_only_read_was_covered_cannot_wait() {
    let rt = runtime();
    let error = rt
        .run("m", |m| -> Result<(), Error> {
            m.cut(|m| {
                m.virtual_set(["fs"])?;
                m.virtual_get(["fs"]);
                Err(m.fail())
            })
        })
        .expect_err("nothing could wake the cut");
    assert_eq!(error.kind(), ErrorKind::Failure);
}

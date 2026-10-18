//! Orderings of turns that wait for a machine, forced by watching the
//! runtime's record of waiting threads.

use std::sync::mpsc;
use std::thread;

use super::*;

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// Yields until `condition` holds.
fn wait_until(condition: impl Fn() -> bool) {
    while !condition() {
        thread::yield_now();
    }
}

fn waiting_threads(rt: &Runtime) -> usize {
    rt.machines().waiting.len()
}

#[test]
fn a_turn_waits_for_another_thread_s_turn_on_its_machine() {
    let rt = runtime();
    let rt = &rt;
    let (in_turn, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let holder = scope.spawn(move || {
            rt.run("m", |m| {
                m.push_int(1);
                m.set("left")?;
                in_turn.send(()).expect("the test listens");
                released.recv().expect("the test releases the turn");
                Ok(())
            })
        });
        entered.recv().expect("the holder takes its turn");
        let waiter = scope.spawn(move || {
            rt.run("m", |m| {
                m.get("left")?;
                m.pop_int()
            })
        });
        // The waiter waits before the holder's turn ends.
        wait_until(|| waiting_threads(rt) == 1);
        release.send(()).expect("the holder waits");
        holder
            .join()
            .expect("the holder should not panic")
            .expect("the holder's turn should succeed");
        let left = waiter
            .join()
            .expect("the waiter should not panic")
            .expect("the waiter's turn should follow");
        assert_eq!(left, 1, "the waiter's turn sees what the holder's left");
    });
    assert_eq!(waiting_threads(rt), 0);
}

#[test]
fn a_wait_that_would_close_a_cycle_fails() {
    let rt = runtime();
    let rt = &rt;
    let (a_held, a_taken) = mpsc::channel();
    let (b_held, b_taken) = mpsc::channel();
    let (go_first, first_goes) = mpsc::channel::<()>();
    let (go_second, second_goes) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            rt.run("a", |_| {
                a_held.send(()).expect("the test listens");
                first_goes.recv().expect("the test lets it wait");
                rt.run("b", |m| {
                    m.push_int(2);
                    m.set("b")
                })
            })
        });
        let second = scope.spawn(move || {
            rt.run("b", |_| {
                b_held.send(()).expect("the test listens");
                second_goes.recv().expect("the test lets it wait");
                let error = rt
                    .run("a", |_| Ok(()))
                    .expect_err("waiting for a would deadlock");
                assert!(error.to_string().contains("would deadlock"), "{error}");
                Ok(())
            })
        });
        a_taken.recv().expect("the first takes a");
        b_taken.recv().expect("the second takes b");
        go_first.send(()).expect("the first waits");
        // The first waits for b before the second asks for a.
        wait_until(|| waiting_threads(rt) == 1);
        go_second.send(()).expect("the second waits");
        second
            .join()
            .expect("the second should not panic")
            .expect("the second's turn should succeed");
        first
            .join()
            .expect("the first should not panic")
            .expect("the first's turn on b should follow");
    });
}

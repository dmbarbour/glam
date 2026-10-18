//! Threads that borrow a runtime: turns that wait for a machine or decline,
//! and scopes that fork and join turns (`runtime-api-turns-on-machines`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;

use glam::{Error, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

#[test]
fn try_run_declines_a_machine_in_a_turn() {
    let rt = runtime();
    let (in_turn, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    rt.scope(|s| {
        let holder = s.run("m", move |_| {
            in_turn.send(()).expect("the test listens");
            released.recv().expect("the test releases the turn");
            Ok(())
        });
        entered.recv().expect("the holder takes its turn");
        let declined = rt
            .try_run("m", |_| Ok(()))
            .expect("a declined turn is no error");
        assert_eq!(declined, None, "the machine is in a turn on another thread");
        release.send(()).expect("the holder waits");
        holder.join().expect("the holder's turn should succeed");
    });
    let taken = rt
        .try_run("m", |m| {
            m.push_int(3);
            m.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(taken, Some(3));
    rt.run("m", |_| {
        assert_eq!(
            rt.try_run("m", |_| Ok(())).expect("no error"),
            None,
            "the machine is in a turn on this thread"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_scope_forks_turns_and_joins_them() {
    let rt = runtime();
    let doubled: i64 = rt.scope(|s| {
        let turns: Vec<_> = (0..4)
            .map(|index| {
                s.run(format!("worker{index}"), move |m| {
                    m.push_int(index);
                    m.set("index")?;
                    Ok(index * 2)
                })
            })
            .collect();
        turns
            .into_iter()
            .map(|turn| turn.join().expect("the turn should succeed"))
            .sum()
    });
    assert_eq!(doubled, 12);
    for index in 0..4 {
        let kept = rt
            .run(format!("worker{index}"), |m| {
                m.get("index")?;
                m.pop_int()
            })
            .expect("each machine keeps its state");
        assert_eq!(kept, index);
    }
}

#[test]
fn a_turn_forks_turns_on_other_machines() {
    let rt = runtime();
    let total = rt
        .run("parent", |m| {
            let sum = rt.scope(|s| -> Result<i64, Error> {
                let left = s.run("left", |m| {
                    m.push_int(20);
                    m.pop_int()
                });
                // A scope's threads may spawn others in it.
                let right = s.spawn(move |_| {
                    s.run("right", |m| {
                        m.push_int(22);
                        m.pop_int()
                    })
                    .join()
                });
                Ok(left.join()? + right.join().expect("no panic")?)
            })?;
            m.push_int(sum);
            m.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(total, 42);
}

#[test]
fn a_panicking_turn_resumes_its_panic_at_the_join_and_keeps_its_machine() {
    let rt = runtime();
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        rt.scope(|s| {
            s.run("m", |m| -> Result<(), Error> {
                m.push_int(5);
                m.set("before")?;
                panic!("the turn panics");
            })
            .join()
        })
    }));
    assert!(panicked.is_err(), "the panic resumes at the join");
    let kept = rt
        .run("m", |m| {
            m.get("before")?;
            m.pop_int()
        })
        .expect("the machine takes turns after a panic");
    assert_eq!(kept, 5, "a turn's changes stay when it unwinds");
}

#[test]
fn a_scope_thread_may_be_configured() {
    let rt = runtime();
    let name = rt.scope(|s| {
        s.spawn_with(thread::Builder::new().name("named".to_owned()), |rt| {
            rt.run("m", |_| Ok(())).expect("the turn should succeed");
            thread::current().name().map(str::to_owned)
        })
        .expect("the thread should start")
        .join()
        .expect("no panic")
    });
    assert_eq!(name.as_deref(), Some("named"));
}

//! The runtime as an environment: turns on keyed machines
//! (`runtime-api-turns-on-machines`).

use glam::{Key, ModuleInput, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

#[test]
fn a_turn_compiles_a_module_and_takes_its_bytes() {
    let rt = runtime();
    let text = rt
        .run("cli", |m| {
            let warnings = m.compile(
                ["example"],
                ModuleInput::script("g", "language g0\nasm.result = \"Hello, \" ++ \"turns!\"\n"),
            )?;
            assert!(warnings.is_empty());
            m.access("asm.result")?;
            m.pop_text()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "Hello, turns!");
}

#[test]
fn a_machine_keeps_its_stack_and_state_between_turns() {
    let rt = runtime();
    rt.run("counter", |m| {
        m.push_int(41);
        m.set("count")?;
        m.push_text("left on the stack");
        Ok(())
    })
    .expect("the first turn should succeed");
    let (count, left) = rt
        .run("counter", |m| {
            let left = m.pop_text()?;
            m.get("count")?;
            Ok((m.pop_int()?, left))
        })
        .expect("the second turn should succeed");
    assert_eq!((count, left.as_str()), (41, "left on the stack"));
}

#[test]
fn machines_with_other_keys_are_separate_and_start_empty() {
    let rt = runtime();
    rt.run("a", |m| {
        m.push_int(1);
        Ok(())
    })
    .expect("the turn should succeed");
    let depth = rt
        .run(
            Key::abstract_global_path(["client", "b"]),
            |m| Ok(m.depth()),
        )
        .expect("the turn should succeed");
    assert_eq!(depth, 0);
}

#[test]
fn clear_returns_a_machine_to_its_initial_state() {
    let rt = runtime();
    rt.run("m", |m| {
        m.push_int(1);
        m.push_int(2);
        m.set("x")?;
        m.stash(1)?;
        m.clear();
        Ok(())
    })
    .expect("the turn should succeed");
    let missing = rt
        .run("m", |m| {
            assert_eq!((m.depth(), m.stash_depth()), (0, 0));
            m.get("x")
        })
        .expect_err("cleared state is gone");
    assert!(missing.to_string().contains("no state"), "{missing}");
}

#[test]
fn stash_moves_values_as_a_block_and_back() {
    let rt = runtime();
    let popped = rt
        .run("m", |m| {
            for value in 1..=4 {
                m.push_int(value);
            }
            m.stash(3)?;
            assert_eq!((m.depth(), m.stash_depth()), (1, 3));
            m.push_int(10);
            m.stash_pop(3)?;
            let mut popped = Vec::new();
            while m.depth() > 0 {
                popped.push(m.pop_int()?);
            }
            Ok(popped)
        })
        .expect("the turn should succeed");
    assert_eq!(popped, [4, 3, 2, 10, 1]);
}

#[test]
fn stack_operations_and_application_compose() {
    let rt = runtime();
    let result = rt
        .run("m", |m| {
            m.compile(
                ["math"],
                ModuleInput::script("g", "language g0\nsub a b = a - b\n"),
            )?;
            m.access("sub")?;
            m.push_int(2);
            m.push_int(44);
            m.swap()?;
            m.apply(2)?;
            m.dup()?;
            m.discard(1)?;
            m.eval()?;
            m.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(result, 42);
}

#[test]
fn an_operation_without_its_values_fails_and_the_turn_keeps_what_it_did() {
    let rt = runtime();
    let underflow = rt
        .run("m", |m| {
            m.push_int(1);
            m.apply(1)
        })
        .expect_err("apply needs a function and an argument");
    assert!(underflow.to_string().contains("needs 2"), "{underflow}");
    let depth = rt
        .run("m", |m| Ok(m.depth()))
        .expect("the turn should succeed");
    assert_eq!(depth, 1, "a turn is not atomic");
    rt.run("m", |m| m.pop_bytes())
        .expect_err("an integer is not binary data");
}

#[test]
fn a_machine_takes_one_turn_at_a_time() {
    let rt = runtime();
    rt.run("outer", |_| {
        let nested = rt
            .run("outer", |_| Ok(()))
            .expect_err("a machine in a turn takes no other");
        assert!(nested.to_string().contains("already taking a turn"));
        rt.run("other", |m| {
            m.push_int(1);
            Ok(())
        })
    })
    .expect("a turn may take a turn on another machine");
}

#[test]
fn scoped_threads_take_turns_on_one_runtime() {
    let rt = runtime();
    std::thread::scope(|scope| {
        for index in 0..4 {
            let rt = &rt;
            scope.spawn(move || {
                rt.run(format!("worker{index}"), |m| {
                    m.push_int(index);
                    m.set("index")
                })
                .expect("the turn should succeed");
            });
        }
    });
    for index in 0..4 {
        let value = rt
            .run(format!("worker{index}"), |m| {
                m.get("index")?;
                m.pop_int()
            })
            .expect("the turn should succeed");
        assert_eq!(value, index);
    }
}

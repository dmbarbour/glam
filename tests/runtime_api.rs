//! The runtime as an environment: turns on keyed machines
//! (`runtime-api-turns-on-machines`).

use glam::{Key, ModuleInput, Runtime, RuntimeConfig, ValueKind};

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
                [ModuleInput::script(
                    "g",
                    "language g0\nasm.result = \"Hello, \" ++ \"turns!\"\n",
                )],
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
                [ModuleInput::script("g", "language g0\nsub a b = a - b\n")],
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

#[test]
fn workers_activate_once_in_a_runtime_opened_without_them() {
    let rt = runtime();
    rt.activate_workers(2).expect("workers should activate");
    assert!(
        rt.activate_workers(2).is_err(),
        "workers activate only once"
    );
    let text = rt
        .run("cli", |m| {
            m.compile(
                ["example"],
                [ModuleInput::script(
                    "g",
                    "language g0\nasm.result = \"with workers\"\n",
                )],
            )?;
            m.access("asm.result")?;
            m.pop_text()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "with workers");

    let opened_with_workers = Runtime::open(RuntimeConfig::new().worker_threads(1))
        .expect("a runtime with a worker should open");
    assert!(
        opened_with_workers.activate_workers(1).is_err(),
        "a runtime opened with workers has activated them"
    );
}

#[test]
fn lists_and_dicts_build_from_the_stack_and_unpack_onto_it() {
    let rt = runtime();
    rt.run("data", |m| {
        m.push_int(1);
        m.push_int(2);
        m.push_int(3);
        m.make_list(3)?;
        m.push_text("numbers");
        m.make_dict(["items", "name"])?;
        m.dup()?;
        m.access("name")?;
        assert_eq!(m.pop_text()?, "numbers");
        m.access("items")?;
        assert_eq!(m.unpack_list()?, 3);
        assert_eq!((m.pop_int()?, m.pop_int()?, m.pop_int()?), (3, 2, 1));

        m.push_int(2);
        m.push_int(1);
        m.make_dict(["b", "a"])?;
        assert_eq!(m.unpack_dict()?, 2);
        let mut entries = Vec::new();
        for _ in 0..2 {
            let value = m.pop_int()?;
            entries.push((m.pop_atom_name()?, value));
        }
        entries.sort();
        assert_eq!(entries, [("a".to_owned(), 1), ("b".to_owned(), 2)]);
        assert_eq!(m.depth(), 0);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_dict_rejects_repeated_names_and_reading_the_wrong_kind_fails() {
    let rt = runtime();
    rt.run("data", |m| {
        m.push_int(1);
        m.push_int(2);
        assert!(m.make_dict(["x", "x"]).is_err());
        assert_eq!(m.depth(), 2, "a rejected dict pops nothing");
        m.discard(2)?;
        m.push_int(7);
        assert!(m.unpack_list().is_err());
        m.push_text("text");
        assert!(m.unpack_dict().is_err());
        m.push_int(7);
        assert!(m.pop_atom_name().is_err());
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn kinds_and_representations_are_observed_without_evaluation() {
    let rt = runtime();
    rt.run("data", |m| {
        m.compile(
            ["kinds"],
            [ModuleInput::script("g", "language g0\nanswer = 6 * 7\n")],
        )?;
        m.access("answer")?;
        assert_eq!(m.kind()?, ValueKind::Lazy);
        m.eval()?;
        assert_eq!(m.kind()?, ValueKind::Number);
        assert_eq!(m.depth(), 1, "observation pops nothing");

        m.push_atom("same");
        m.push_atom("same");
        assert!(m.same_representation()?);
        m.discard(1)?;
        m.push_atom("other");
        assert!(!m.same_representation()?);
        m.discard(2)?;

        m.push_abstract_path(["runtime_api", "path"]);
        assert_eq!(m.kind()?, ValueKind::Atom);
        assert!(
            m.pop_atom_name().is_err(),
            "a path atom is not named by text"
        );
        assert_eq!(m.pop_int()?, 42);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn annotations_and_initial_definitions_shape_what_compiles() {
    let rt = runtime();
    let text = rt
        .run("cli", |m| {
            m.push_text("Hello");
            m.make_dict(["greeting"])?;
            m.compile_over(
                ["over"],
                [ModuleInput::script(
                    "g",
                    "language g0\nasm.result = greeting ++ \", over\"\n",
                )],
            )?;
            m.access("asm.result")?;
            m.pop_text()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "Hello, over");

    rt.run("cli", |m| {
        m.push_atom("binary");
        m.push_text("bytes");
        m.anno()?;
        m.eval()?;
        assert_eq!(m.kind()?, ValueKind::Binary);
        assert_eq!(m.pop_bytes()?, "bytes");
        m.push_atom("binary");
        m.push_int(7);
        m.anno()?;
        assert!(m.eval().is_err(), "a number is no binary");
        Ok(())
    })
    .expect("the turn should succeed");
}

//! The runtime as an environment: turns on a tree of machines
//! (`runtime-api-turns-on-machines`, `machines-form-a-tree`).

use glam::{Error, Key, ModuleInput, Runtime, RuntimeConfig, ValueKind};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

#[test]
fn a_turn_compiles_a_module_and_takes_its_bytes() {
    let mut rt = runtime();
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
    let mut rt = runtime();
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
    let mut rt = runtime();
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
    let mut rt = runtime();
    rt.run("m", |m| {
        m.push_int(1);
        m.push_int(2);
        m.set("x")?;
        m.stash(1)?;
        m.push_int(3);
        m.give("child", 1)?;
        m.clear()?;
        Ok(())
    })
    .expect("the turn should succeed");
    let missing = rt
        .run("m", |m| {
            assert_eq!((m.depth(), m.stash_depth()), (0, 0));
            assert!(m.take("child", 1).is_err(), "cleared children are gone");
            m.get("x")
        })
        .expect_err("cleared state is gone");
    assert!(missing.to_string().contains("no state"), "{missing}");
}

#[test]
fn stash_moves_values_as_a_block_and_back() {
    let mut rt = runtime();
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
    let mut rt = runtime();
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
    let mut rt = runtime();
    let underflow = rt
        .run("m", |m| {
            m.push_int(1);
            m.apply(1)
        })
        .expect_err("apply needs a function and an argument");
    assert!(underflow.to_string().contains("needs 2"), "{underflow}");
    let kept = rt
        .recover("m", |m| {
            let errors = m.unpack_list()?;
            m.discard(errors)?;
            m.pop_int()
        })
        .expect("the recovering turn should succeed");
    assert_eq!(kept, 1, "a turn is not atomic");
}

#[test]
fn a_turn_that_ends_in_error_leaves_its_machine_in_error_until_recovered() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.push_int(1);
        Err::<(), _>(Error::new("the turn errs"))
    })
    .expect_err("the turn erred");
    let error = rt
        .run("m", |_| Ok(()))
        .expect_err("returning the error recovered nothing");
    assert!(error.to_string().contains("in error"), "{error}");
    rt.run(["m", "child"], |c| {
        c.push_int(2);
        Ok(())
    })
    .expect("only a machine's own error bars it, not its parent's");

    let (errors, below) = rt
        .recover("m", |m| {
            let errors = m.unpack_list()?;
            m.discard(errors)?;
            Ok((errors, m.pop_int()?))
        })
        .expect("a recovering turn enters");
    assert_eq!(
        (errors, below),
        (1, 1),
        "the list of diagnostics sits on the stack as the turn left it"
    );
    let errors = rt
        .recover("m", |m| m.unpack_list())
        .expect("a recovering turn enters a machine in no error too");
    assert_eq!(errors, 0, "with an empty list");
    rt.run("m", |_| Ok(()))
        .expect("the machine takes turns again");
}

#[test]
fn a_registered_error_marks_the_machine_without_doubling() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.register_error(&Error::new("the state is spoiled"));
        Ok(())
    })
    .expect("the turn itself succeeds");
    rt.run("m", |_| Ok(()))
        .expect_err("the registered error left the machine in error");
    let errors = rt
        .recover("m", |m| m.unpack_list())
        .expect("the machine recovers");
    assert_eq!(errors, 1);

    rt.run("m", |m| -> Result<(), Error> {
        let error = Error::new("returned early");
        m.register_error(&error);
        Err(error)
    })
    .expect_err("the turn returns its error");
    let errors = rt
        .recover("m", |m| m.unpack_list())
        .expect("the machine recovers");
    assert_eq!(errors, 1, "what the turn registered describes it");
}

#[test]
fn a_panicking_turn_leaves_its_machine_in_error() {
    let mut rt = runtime();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.run("m", |m| -> Result<(), Error> {
            m.push_int(3);
            panic!("the turn panics");
        })
    }));
    assert!(panicked.is_err());
    let (errors, kept) = rt
        .recover("m", |m| {
            let errors = m.unpack_list()?;
            m.discard(errors)?;
            Ok((errors, m.pop_int()?))
        })
        .expect("the machine recovers after a panic");
    assert_eq!((errors, kept), (1, 3));
}

#[test]
fn a_descendant_in_error_bars_its_parent_s_operations_until_recovered() {
    let mut rt = runtime();
    rt.run("parent", |m| {
        m.push_int(9);
        m.give("child", 1)?;
        let error = m
            .run("child", |_| -> Result<(), Error> {
                Err(Error::new("the child errs"))
            })
            .expect_err("the child's turn erred");
        assert_eq!(error.to_string(), "the child errs");
        for error in [
            m.run("child", |_| Ok(()))
                .expect_err("the child is in error"),
            m.give("child", 0).expect_err("the child is in error"),
            m.take("child", 1).expect_err("the child is in error"),
        ] {
            assert!(error.to_string().contains("in error"), "{error}");
        }
        m.recover("child", |_| Ok(()))?;
        m.take("child", 2)?;
        let errors = m.unpack_list()?;
        m.discard(errors)?;
        assert_eq!((errors, m.pop_int()?), (1, 9));
        Ok(())
    })
    .expect("the parent's turn should succeed");
}

#[test]
fn a_turn_reaches_its_descendants_and_moves_values_to_and_from_them() {
    let mut rt = runtime();
    rt.run("parent", |m| {
        m.push_int(1);
        m.push_int(2);
        m.give("child", 2)?;
        assert_eq!(m.depth(), 0, "given values leave");
        m.run("child", |c| {
            let (top, below) = (c.pop_int()?, c.pop_int()?);
            assert_eq!((below, top), (1, 2), "given values keep their order");
            c.push_int(below + top);
            c.set("sum")?;
            c.push_int(below + top);
            c.run("grandchild", |g| {
                g.push_text("deep");
                Ok(())
            })
        })?;
        m.take("child", 1)?;
        assert_eq!(m.pop_int()?, 3, "taken values arrive");
        Ok(())
    })
    .expect("the turn should succeed");
    let (sum, deep) = rt
        .run(["parent", "child"], |c| {
            c.get("sum")?;
            let sum = c.pop_int()?;
            c.take("grandchild", 1)?;
            Ok((sum, c.pop_text()?))
        })
        .expect("a path from the root reaches the same machine");
    assert_eq!((sum, deep.as_str()), (3, "deep"));
    let sum = rt
        .run((), |root| {
            root.run(["parent", "child"], |c| {
                c.get("sum")?;
                c.pop_int()
            })
        })
        .expect("the root's children are the machines that keys name");
    assert_eq!(sum, 3);
}

#[test]
fn moving_values_needs_them_and_a_descendant() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let error = m.give("child", 1).expect_err("nothing to give");
        assert!(error.to_string().contains("needs 1"), "{error}");
        let error = m.take("child", 1).expect_err("nothing to take");
        assert!(error.to_string().contains("needs 1"), "{error}");
        m.push_int(1);
        assert!(m.give((), 1).is_err(), "values move to descendants");
        assert!(
            m.run((), |_| Ok(())).is_err(),
            "a machine takes turns on its descendants"
        );
        assert_eq!(m.depth(), 1, "a failed move leaves the values");
        m.give("absent", 0)?;
        m.take("absent", 0)?;
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn workers_activate_once_in_a_runtime_opened_without_them() {
    let mut rt = runtime();
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
    let mut rt = runtime();
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
    let mut rt = runtime();
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
    let mut rt = runtime();
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
    let mut rt = runtime();
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

#[test]
fn a_runtime_moves_to_a_thread_that_takes_turns_and_retires_it() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.push_int(7);
        m.set("x")
    })
    .expect("the turn should succeed");
    let kept = std::thread::spawn(move || {
        let kept = rt
            .run("m", |m| {
                m.get("x")?;
                m.pop_int()
            })
            .expect("the turn should succeed on the new thread");
        drop(rt);
        kept
    })
    .join()
    .expect("the thread should not panic");
    assert_eq!(kept, 7, "the machines move with the runtime");
}

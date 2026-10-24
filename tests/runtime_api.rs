//! The runtime as an environment: turns on a tree of machines
//! (`runtime-api-turns-on-machines`, `machines-form-a-tree`).

use glam::{Error, ErrorKind, Key, Machine, Runtime, RuntimeConfig, ValueKind};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

#[test]
fn a_turn_compiles_a_module_and_takes_its_bytes() {
    let mut rt = runtime();
    let text = rt
        .run("cli", |m| {
            m.dict_pack(Vec::<&str>::new())?;
            m.script_object(
                ["example"],
                "g",
                "language g0\nasm.result = \"Hello, \" ++ \"turns!\"\n",
            )?;
            m.dict_get_path(["asm", "result"])?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "Hello, turns!");
}

#[test]
fn a_machine_keeps_its_stack_and_state_between_turns() {
    let mut rt = runtime();
    rt.run("counter", |m| {
        m.int_push(41);
        m.local_set("count")?;
        m.text_push("left on the stack");
        Ok(())
    })
    .expect("the first turn should succeed");
    let (count, left) = rt
        .run("counter", |m| {
            let left = m.text_pop()?;
            m.local_get("count")?;
            Ok((m.int_pop()?, left))
        })
        .expect("the second turn should succeed");
    assert_eq!((count, left.as_str()), (41, "left on the stack"));
}

#[test]
fn machines_with_other_keys_are_separate_and_start_empty() {
    let mut rt = runtime();
    rt.run("a", |m| {
        m.int_push(1);
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
        m.int_push(1);
        m.int_push(2);
        m.local_set("x")?;
        m.stash(1)?;
        m.int_push(3);
        m.give("child", 1)?;
        m.clear()?;
        Ok(())
    })
    .expect("the turn should succeed");
    rt.run("m", |m| {
        assert_eq!((m.depth(), m.stash_depth()), (0, 0));
        assert!(m.take("child", 1).is_err(), "cleared children are gone");
        assert!(!m.local_defines("x"), "cleared state is gone");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn stash_moves_values_aside_and_back() {
    let mut rt = runtime();
    let popped = rt
        .run("m", |m| {
            for value in 1..=4 {
                m.int_push(value);
            }
            m.stash(3)?;
            assert_eq!((m.depth(), m.stash_depth()), (1, 3));
            m.int_push(10);
            m.stash_pop(3)?;
            let mut popped = Vec::new();
            while m.depth() > 0 {
                popped.push(m.int_pop()?);
            }
            Ok(popped)
        })
        .expect("the turn should succeed");
    assert_eq!(popped, [4, 3, 2, 10, 1]);
}

/// Pops every value of the stack, top first.
fn pop_all(m: &mut Machine<'_, '_>) -> Result<Vec<i64>, Error> {
    let mut popped = Vec::new();
    while m.depth() > 0 {
        popped.push(m.int_pop()?);
    }
    Ok(popped)
}

#[test]
fn the_stash_is_the_stack_s_other_side() {
    let mut rt = runtime();
    let (one_at_a_time, flipped) = rt
        .run("m", |m| {
            for value in 1..=3 {
                m.int_push(value);
            }
            m.stash(3)?;
            for _ in 0..3 {
                m.stash_pop(1)?;
            }
            let one_at_a_time = pop_all(m)?;
            for value in 1..=3 {
                m.int_push(value);
            }
            for _ in 0..3 {
                m.stash(1)?;
            }
            m.stash_pop(3)?;
            m.flip(3)?;
            Ok((one_at_a_time, pop_all(m)?))
        })
        .expect("the turn should succeed");
    assert_eq!(
        one_at_a_time,
        [3, 2, 1],
        "stash(3) is stash(1) three times, and so returns them as they were"
    );
    assert_eq!(flipped, [1, 2, 3], "flip(3) reverses the top three");
}

#[test]
fn flip_generalizes_swap() {
    let mut rt = runtime();
    let flipped = rt
        .run("m", |m| {
            for value in 1..=4 {
                m.int_push(value);
            }
            m.flip(2)?;
            m.flip(0)?;
            m.flip(1)?;
            assert!(m.flip(5).is_err(), "too few values");
            pop_all(m)
        })
        .expect("the turn should succeed");
    assert_eq!(flipped, [3, 4, 2, 1]);
}

#[test]
fn stack_operations_and_application_compose() {
    let mut rt = runtime();
    let result = rt
        .run("m", |m| {
            m.dict_pack(Vec::<&str>::new())?;
            m.script_object(["math"], "g", "language g0\nsub a b = a - b\n")?;
            m.dict_get("sub")?;
            m.int_push(2);
            m.int_push(44);
            m.swap()?;
            m.apply(2)?;
            m.dup(1)?;
            m.discard(1)?;
            m.eval()?;
            m.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(result, 42);
}

#[test]
fn an_operation_without_its_values_fails_and_the_turn_keeps_what_it_did() {
    let mut rt = runtime();
    let underflow = rt
        .run("m", |m| {
            m.int_push(1);
            m.apply(1)
        })
        .expect_err("apply needs a function and an argument");
    assert!(underflow.to_string().contains("needs 2"), "{underflow}");
    let kept = rt
        .recover("m", |m| m.int_pop())
        .expect("the recovering turn should succeed");
    assert_eq!(kept, 1, "a turn is not atomic");
}

#[test]
fn a_turn_that_ends_in_error_leaves_its_machine_in_error_until_recovered() {
    let mut rt = runtime();
    assert!(!rt.failed("m"), "an absent machine is in no error");
    rt.run("m", |m| {
        m.int_push(1);
        Err::<(), _>(Error::new("the turn errs"))
    })
    .expect_err("the turn erred");
    assert!(rt.failed("m"));
    let error = rt
        .run("m", |_| Ok(()))
        .expect_err("returning the error recovered nothing");
    assert!(error.to_string().contains("in error"), "{error}");
    assert!(rt.failed("m"), "asking, or being refused, recovers nothing");
    rt.run(["m", "child"], |c| {
        c.int_push(2);
        Ok(())
    })
    .expect("only a machine's own error bars it, not its parent's");

    let below = rt
        .recover("m", |m| m.int_pop())
        .expect("a recovering turn enters");
    assert_eq!(
        below, 1,
        "the turn finds the machine as the failed turn left it"
    );
    assert!(!rt.failed("m"));
    rt.recover("m", |_| Ok(()))
        .expect("a recovering turn enters a machine in no error too");
    rt.run("m", |_| Ok(()))
        .expect("the machine takes turns again");
}

#[test]
fn a_panicking_turn_leaves_its_machine_in_error() {
    let mut rt = runtime();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.run("m", |m| -> Result<(), Error> {
            m.int_push(3);
            panic!("the turn panics");
        })
    }));
    assert!(panicked.is_err());
    assert!(rt.failed("m"));
    let kept = rt
        .recover("m", |m| m.int_pop())
        .expect("the machine recovers after a panic");
    assert_eq!(kept, 3);
}

#[test]
fn a_descendant_in_error_bars_its_parent_s_operations_until_recovered() {
    let mut rt = runtime();
    rt.run("parent", |m| {
        m.int_push(9);
        m.give("child", 1)?;
        let error = m
            .run("child", |_| -> Result<(), Error> {
                Err(Error::new("the child errs"))
            })
            .expect_err("the child's turn erred");
        assert_eq!(error.to_string(), "the child errs");
        assert!(m.failed("child")?);
        for error in [
            m.run("child", |_| Ok(()))
                .expect_err("the child is in error"),
            m.give("child", 0).expect_err("the child is in error"),
            m.take("child", 1).expect_err("the child is in error"),
        ] {
            assert!(error.to_string().contains("in error"), "{error}");
        }
        m.recover("child", |_| Ok(()))?;
        assert!(!m.failed("child")?);
        m.take("child", 1)?;
        assert_eq!(m.int_pop()?, 9);
        Ok(())
    })
    .expect("the parent's turn should succeed");
}

#[test]
fn a_turn_reaches_its_descendants_and_moves_values_to_and_from_them() {
    let mut rt = runtime();
    rt.run("parent", |m| {
        m.int_push(1);
        m.int_push(2);
        m.give("child", 2)?;
        assert_eq!(m.depth(), 0, "given values leave");
        m.run("child", |c| {
            let (top, below) = (c.int_pop()?, c.int_pop()?);
            assert_eq!((below, top), (1, 2), "given values keep their order");
            c.int_push(below + top);
            c.local_set("sum")?;
            c.int_push(below + top);
            c.run("grandchild", |g| {
                g.text_push("deep");
                Ok(())
            })
        })?;
        m.take("child", 1)?;
        assert_eq!(m.int_pop()?, 3, "taken values arrive");
        Ok(())
    })
    .expect("the turn should succeed");
    let (sum, deep) = rt
        .run(["parent", "child"], |c| {
            c.local_get("sum")?;
            let sum = c.int_pop()?;
            c.take("grandchild", 1)?;
            Ok((sum, c.text_pop()?))
        })
        .expect("a path from the root reaches the same machine");
    assert_eq!((sum, deep.as_str()), (3, "deep"));
    let sum = rt
        .run((), |root| {
            root.run(["parent", "child"], |c| {
                c.local_get("sum")?;
                c.int_pop()
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
        m.int_push(1);
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
            m.dict_pack(Vec::<&str>::new())?;
            m.script_object(
                ["example"],
                "g",
                "language g0\nasm.result = \"with workers\"\n",
            )?;
            m.dict_get_path(["asm", "result"])?;
            m.text_pop()
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
        m.int_push(1);
        m.int_push(2);
        m.int_push(3);
        m.list_pack(3)?;
        m.text_push("numbers");
        m.dict_pack(["items", "name"])?;
        m.dup(1)?;
        m.dict_get("name")?;
        assert_eq!(m.text_pop()?, "numbers");
        m.dict_get("items")?;
        m.list_unpack(3)?;
        assert_eq!((m.int_pop()?, m.int_pop()?, m.int_pop()?), (3, 2, 1));

        m.int_push(2);
        m.int_push(1);
        m.dict_pack(["b", "a"])?;
        m.dict_unpack(["a", "b"])?;
        assert_eq!((m.int_pop()?, m.int_pop()?), (2, 1));
        assert_eq!(m.depth(), 0);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_dict_rejects_repeated_names_and_reading_the_wrong_kind_fails() {
    let mut rt = runtime();
    rt.run("data", |m| {
        m.int_push(1);
        m.int_push(2);
        assert!(m.dict_pack(["x", "x"]).is_err());
        assert_eq!(m.depth(), 2, "a rejected dict pops nothing");
        m.discard(2)?;
        m.int_push(7);
        assert!(m.list_unpack(1).is_err());
        m.text_push("text");
        assert!(m.dict_unpack(["text"]).is_err());
        m.int_push(7);
        assert!(m.atom_pop().is_err());
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn kinds_and_representations_are_observed_without_evaluation() {
    let mut rt = runtime();
    rt.run("data", |m| {
        m.dict_pack(Vec::<&str>::new())?;
        m.script_object(["kinds"], "g", "language g0\nanswer = 6 * 7\n")?;
        m.dict_get("answer")?;
        assert_eq!(m.kind()?, ValueKind::Lazy);
        m.eval()?;
        assert_eq!(m.kind()?, ValueKind::Number);
        assert_eq!(m.depth(), 1, "observation pops nothing");

        m.atom_push("same");
        m.atom_push("same");
        assert!(m.same_representation()?);
        m.discard(1)?;
        m.atom_push("other");
        assert!(!m.same_representation()?);
        m.discard(2)?;

        m.abstract_path_push(["runtime_api", "path"]);
        assert_eq!(m.kind()?, ValueKind::Atom);
        assert!(m.atom_pop().is_err(), "a path atom is not named by text");
        m.discard(1)?;
        assert_eq!(m.int_pop()?, 42);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn annotations_and_initial_definitions_shape_what_compiles() {
    let mut rt = runtime();
    let text = rt
        .run("cli", |m| {
            m.text_push("Hello");
            m.dict_pack(["greeting"])?;
            m.script_object(
                ["over"],
                "g",
                "language g0\nasm.result = greeting ++ \", over\"\n",
            )?;
            m.dict_get_path(["asm", "result"])?;
            m.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(text, "Hello, over");

    rt.run("cli", |m| {
        m.atom_push("binary");
        m.text_push("bytes");
        m.anno()?;
        m.eval()?;
        assert_eq!(m.kind()?, ValueKind::Binary);
        assert_eq!(m.bytes_pop()?, "bytes");
        m.atom_push("binary");
        m.int_push(7);
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
        m.int_push(7);
        m.local_set("x")
    })
    .expect("the turn should succeed");
    let kept = std::thread::spawn(move || {
        let kept = rt
            .run("m", |m| {
                m.local_get("x")?;
                m.int_pop()
            })
            .expect("the turn should succeed on the new thread");
        drop(rt);
        kept
    })
    .join()
    .expect("the thread should not panic");
    assert_eq!(kept, 7, "the machines move with the runtime");
}

#[test]
fn an_operation_that_fails_does_nothing() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.text_push("not an integer");
        m.int_pop().expect_err("text is no integer");
        m.bytes_push(vec![0xff]);
        m.text_pop().expect_err("these bytes are no UTF-8");
        assert_eq!(m.depth(), 2, "both values stay");
        m.discard(2)?;

        m.int_push(1);
        m.anno().expect_err("an annotation needs a target");
        assert_eq!(m.depth(), 1, "the target stays");
        m.discard(1)?;

        m.dict_pack(Vec::<&str>::new())?;
        m.script_object(["broken"], "g", "language g0\nx = (\n")?;
        assert_eq!(m.depth(), 1, "compiling evaluates nothing");
        let error = m.eval().expect_err("the module does not parse");
        assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
        assert_eq!(m.depth(), 1, "the module stays");
        Ok(())
    })
    .expect("the turn should succeed");
}

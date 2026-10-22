//! A machine's stack operations: static arities, and twins that undo each
//! other on well-typed data (`api-stack-ops`).

use glam::{Error, ErrorKind, Key, Machine, Runtime, RuntimeConfig, ValueKind};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// Pops the top `n` values as integers, deepest first.
fn ints(m: &mut Machine<'_, '_>, n: usize) -> Result<Vec<i64>, Error> {
    let mut values = (0..n).map(|_| m.int_pop()).collect::<Result<Vec<_>, _>>()?;
    values.reverse();
    Ok(values)
}

/// Pushes the integers `values`, in order.
fn push(m: &mut Machine<'_, '_>, values: &[i64]) {
    for value in values {
        m.int_push(*value);
    }
}

/// Whether the top value is `{}`, popping it.
fn undefined(m: &mut Machine<'_, '_>) -> Result<bool, Error> {
    if m.kind()? == ValueKind::Dict && m.dict_unpack(Vec::<&str>::new()).is_ok() {
        return Ok(true);
    }
    m.discard(1)?;
    Ok(false)
}

#[test]
fn shuffles_copy_drop_and_reorder_by_pattern() {
    let mut rt = runtime();
    rt.run("m", |m| {
        push(m, &[1, 2, 3]);
        m.shuffle("abc-cabbca")?;
        assert_eq!(ints(m, 6)?, [3, 1, 2, 2, 3, 1]);

        push(m, &[1, 2]);
        m.dup(2)?;
        assert_eq!(ints(m, 4)?, [1, 2, 1, 2]);

        push(m, &[1, 2, 3]);
        m.flip(3)?;
        assert_eq!(ints(m, 3)?, [3, 2, 1]);

        push(m, &[1, 2]);
        for pattern in ["ab", "aa-a", "ab-c", "a1-a"] {
            assert!(m.shuffle(pattern).is_err(), "`{pattern}` is malformed");
        }
        assert!(m.shuffle("abc-a").is_err(), "two values cannot bind three");
        assert_eq!(m.depth(), 2, "a failed shuffle changes nothing");
        m.shuffle("ab-")?;
        assert_eq!(m.depth(), 0);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn lists_pack_and_unpack_as_twins() {
    let mut rt = runtime();
    rt.run("m", |m| {
        push(m, &[1, 2, 3]);
        m.list_pack(3)?;
        m.dup(1)?;
        assert!(m.list_unpack(2).is_err(), "a list of three is not of two");
        assert_eq!(m.depth(), 2, "the failed unpack changes nothing");
        m.list_unpack(3)?;
        assert_eq!(ints(m, 3)?, [1, 2, 3]);

        m.list_len()?;
        assert_eq!(m.int_pop()?, 3);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn lists_append_and_split_at_either_end_lazily() {
    let mut rt = runtime();
    rt.run("m", |m| {
        push(m, &[1, 2]);
        m.list_pack(2)?;
        push(m, &[3, 4, 5]);
        m.list_pack(3)?;
        m.list_append()?;
        assert_eq!(m.kind()?, ValueKind::Lazy, "appending evaluates nothing");
        m.dup(1)?;

        m.int_push(1);
        m.list_split()?;
        m.list_unpack(4)?;
        assert_eq!(ints(m, 4)?, [2, 3, 4, 5]);
        m.list_unpack(1)?;
        assert_eq!(ints(m, 1)?, [1]);

        m.int_push(2);
        m.list_split_end()?;
        m.list_unpack(2)?;
        assert_eq!(ints(m, 2)?, [4, 5]);
        m.list_unpack(3)?;
        assert_eq!(ints(m, 3)?, [1, 2, 3]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn dicts_pack_and_unpack_as_twins_that_drop_nothing() {
    let mut rt = runtime();
    rt.run("m", |m| {
        push(m, &[1, 2]);
        m.dict_pack(["a", "b"])?;
        m.dup(1)?;
        assert!(
            m.dict_unpack(["a"]).is_err(),
            "unpacking only `a` would drop `b`"
        );
        assert_eq!(m.depth(), 2, "the failed unpack changes nothing");
        m.dict_unpack(["b", "a", "c"])?;
        assert!(undefined(m)?, "an absent key unpacks as `{{}}`");
        assert_eq!(ints(m, 2)?, [2, 1]);

        // Keys are any keys, byte strings among them.
        let path = Key::binary_from_text("PATH");
        m.text_push("/bin");
        m.dict_pack([path.clone()])?;
        m.dict_unpack([path])?;
        assert_eq!(m.text_pop()?, "/bin");

        assert!(m.dict_pack(["x", "x"]).is_err(), "keys must differ");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn dicts_insert_and_extract_as_twins_lazily() {
    let mut rt = runtime();
    rt.run("m", |m| {
        // Keys given in Rust.
        push(m, &[2, 3]);
        m.int_push(1);
        m.dict_pack(["a"])?;
        m.dict_insert_keys(["b", "c"])?;
        assert_eq!(m.kind()?, ValueKind::Lazy, "inserting evaluates nothing");
        m.dup(1)?;
        m.dict_extract_keys(["b", "c"])?;
        m.dict_unpack(["a"])?;
        assert_eq!(ints(m, 3)?, [2, 3, 1]);

        // Keys on the stack.
        m.int_push(4);
        m.swap()?;
        m.atom_push("d");
        m.dict_insert()?;
        m.atom_push("d");
        m.dict_extract()?;
        m.dict_unpack(["a", "b", "c"])?;
        assert_eq!(ints(m, 4)?, [4, 1, 2, 3]);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn dict_paths_reach_through_nested_dicts() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.int_push(7);
        m.dict_pack(["c"])?;
        m.dict_pack(["b"])?;
        m.dup(1)?;
        m.dict_get_path(["b", "c"])?;
        assert_eq!(m.int_pop()?, 7);

        m.int_push(8);
        m.swap()?;
        m.dict_insert_path(["b", "d"])?;
        m.dict_extract_path(["b", "c"])?;
        m.swap()?;
        assert_eq!(m.int_pop()?, 7);
        m.dict_get_path(["b", "d"])?;
        assert_eq!(m.int_pop()?, 8);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_union_merges_lazily_and_a_conflict_fails_where_forced() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.int_push(1);
        m.dict_pack(["a"])?;
        m.int_push(2);
        m.dict_pack(["b"])?;
        m.dict_union()?;
        m.dict_unpack(["a", "b"])?;
        assert_eq!(ints(m, 2)?, [1, 2]);

        m.int_push(1);
        m.dict_pack(["a"])?;
        m.int_push(2);
        m.dict_pack(["a"])?;
        m.dict_union()?;
        assert_eq!(m.kind()?, ValueKind::Lazy, "the union is lazy");
        m.dict_unpack(["a"])?;
        let error = m.int_pop().expect_err("the values under `a` conflict");
        assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn tags_pack_and_unpack_only_as_themselves() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.int_push(5);
        m.tag_pack("some")?;
        m.dup(1)?;
        assert!(m.tag_unpack("none").is_err(), "the tag is `some`");
        m.discard(1)?;
        m.tag_unpack("some")?;
        assert_eq!(m.int_pop()?, 5);
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn local_state_reads_undefined_where_absent_and_moves_out() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.local_get("missing");
        assert!(undefined(m)?, "absent local state reads as `{{}}`");
        m.int_push(3);
        m.local_set("x")?;
        m.local_take("x");
        assert_eq!(m.int_pop()?, 3);
        m.local_get("x");
        assert!(undefined(m)?, "taking it left nothing");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn abstract_paths_push_and_pop_as_twins() {
    let mut rt = runtime();
    let parts = rt
        .run("m", |m| {
            m.abstract_path_push(["glam", "stack"]);
            m.abstract_path_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(parts, ["glam", "stack"]);
}

#[test]
fn a_popped_value_returns_to_this_turn_and_the_turns_within_it() {
    let mut rt = runtime();
    let (twice, below) = rt
        .run("m", |m| {
            m.int_push(20);
            m.int_push(1);
            m.list_pack(1)?;
            m.list_len()?;
            let lazy = m.value_pop()?;
            let twenty = m.value_pop()?;
            m.value_push(&lazy)?;
            assert_eq!(m.kind()?, ValueKind::Lazy, "pushing evaluates nothing");
            m.value_push(&twenty)?;
            m.value_push(&twenty)?;
            let twice = m.int_pop()? + m.int_pop()?;
            m.discard(1)?;
            let below = m.run("child", |c| {
                c.value_push(&twenty)?;
                c.int_pop()
            })?;
            Ok((twice, below))
        })
        .expect("the turn should succeed");
    assert_eq!((twice, below), (40, 20));
}

//! Volumes as first-class capabilities: a handle names a volume, whose
//! holder reads and writes it (`requests-travel-on-ports`).

use glam::{Error, ErrorKind, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

#[test]
fn a_volume_is_allocated_written_and_read_through_its_handle() {
    let mut rt = runtime();
    let read = rt
        .run("m", |m| {
            m.dict_pack(Vec::<&str>::new())?;
            m.volume_new()?;
            m.dup(1)?;
            m.int_push(7);
            m.volume_set(["count"])?;
            m.volume_get(["count"])?;
            m.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(read, 7);
}

#[test]
fn a_volume_removal_prunes_what_it_empties() {
    // `dict-presence-ops`: removing `a.b` prunes `a`, which the removal
    // left empty, and keeps `c`.
    let mut rt = runtime();
    let (pruned, kept) = rt
        .run("m", |m| {
            m.dict_pack(Vec::<&str>::new())?;
            m.volume_new()?;
            m.dup(1)?;
            m.int_push(1);
            m.volume_set(["a", "b"])?;
            m.dup(1)?;
            m.int_push(2);
            m.volume_set(["c"])?;
            m.dup(1)?;
            m.volume_del(["a", "b"])?;
            m.dup(1)?;
            let pruned = match m.volume_try_get(["a"]) {
                Ok(()) => m.discard(1).map(|()| false)?,
                // A failed read changes nothing, so the handle stays.
                Err(error) if error.kind() == ErrorKind::Backtrack => {
                    m.discard(1).map(|()| true)?
                }
                Err(error) => return Err(error),
            };
            m.volume_get(["c"])?;
            Ok((pruned, m.int_pop()?))
        })
        .expect("the turn should succeed");
    assert!(pruned, "`a` is undefined");
    assert_eq!(kept, 2);
}

#[test]
fn a_handle_passes_between_machines() {
    let mut rt = runtime();
    let read = rt
        .run("p", |p| {
            p.text_push("shared");
            p.dict_pack(["note"])?;
            p.volume_new()?;
            p.dup(1)?;
            p.give("writer", 1)?;
            p.run("writer", |w| {
                w.text_push("written");
                w.volume_set(["note"])
            })?;
            p.volume_get(["note"])?;
            p.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(read, "written", "the writer's handle names the same volume");
}

#[test]
fn a_failed_cut_allocates_nothing_and_a_committed_one_does() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let abandoned = m.cut(|m| -> Result<(), Error> {
            m.int_push(1);
            m.volume_new()?;
            m.dup(1)?;
            m.volume_get(Vec::<&str>::new())?;
            assert_eq!(m.int_pop()?, 1, "the cut reads what it allocated");
            Err(Error::new("abandoned"))
        });
        assert_eq!(
            abandoned.expect_err("the cut erred").kind(),
            ErrorKind::Failure
        );
        assert_eq!(m.depth(), 0, "the failed cut left no handle");

        m.cut(|m| {
            m.int_push(2);
            m.volume_new()
        })?;
        m.volume_get(Vec::<&str>::new())?;
        assert_eq!(m.int_pop()?, 2, "the committed volume stays");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn the_heap_is_a_volume_too() {
    let mut rt = runtime();
    let read = rt
        .run("m", |m| {
            m.int_push(3);
            m.heap_set(["answer"])?;
            m.heap_volume();
            m.volume_get(["answer"])?;
            m.int_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(read, 3);
}

#[test]
fn a_value_that_names_no_volume_is_an_error_that_pops_nothing() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.int_push(5);
        m.int_push(6);
        let error = m.volume_set(["x"]).expect_err("an integer is no handle");
        assert!(error.to_string().contains("not a handle"), "{error}");
        assert_eq!(m.depth(), 2, "it popped nothing");
        Ok(())
    })
    .expect("the turn should succeed");
}

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
            m.make_dict(Vec::<&str>::new())?;
            m.volume_new()?;
            m.dup()?;
            m.push_int(7);
            m.volume_set(["count"])?;
            m.volume_get(["count"])?;
            m.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(read, 7);
}

#[test]
fn a_handle_passes_between_machines() {
    let mut rt = runtime();
    let read = rt
        .run("p", |p| {
            p.push_text("shared");
            p.make_dict(["note"])?;
            p.volume_new()?;
            p.dup()?;
            p.give("writer", 1)?;
            p.run("writer", |w| {
                w.push_text("written");
                w.volume_set(["note"])
            })?;
            p.volume_get(["note"])?;
            p.pop_text()
        })
        .expect("the turn should succeed");
    assert_eq!(read, "written", "the writer's handle names the same volume");
}

#[test]
fn a_failed_cut_allocates_nothing_and_a_committed_one_does() {
    let mut rt = runtime();
    rt.run("m", |m| {
        let abandoned = m.cut(|m| -> Result<(), Error> {
            m.push_int(1);
            m.volume_new()?;
            m.dup()?;
            m.volume_get(Vec::<&str>::new())?;
            assert_eq!(m.pop_int()?, 1, "the cut reads what it allocated");
            Err(Error::new("abandoned"))
        });
        assert_eq!(
            abandoned.expect_err("the cut erred").kind(),
            ErrorKind::Failure
        );
        assert_eq!(m.depth(), 0, "the failed cut left no handle");

        m.cut(|m| {
            m.push_int(2);
            m.volume_new()
        })?;
        m.volume_get(Vec::<&str>::new())?;
        assert_eq!(m.pop_int()?, 2, "the committed volume stays");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn the_heap_is_a_volume_too() {
    let mut rt = runtime();
    let read = rt
        .run("m", |m| {
            m.push_int(3);
            m.heap_set(["answer"])?;
            m.heap_volume();
            m.volume_get(["answer"])?;
            m.pop_int()
        })
        .expect("the turn should succeed");
    assert_eq!(read, 3);
}

#[test]
fn a_value_that_names_no_volume_is_an_error_that_pops_nothing() {
    let mut rt = runtime();
    rt.run("m", |m| {
        m.push_int(5);
        m.push_int(6);
        let error = m.volume_set(["x"]).expect_err("an integer is no handle");
        assert!(error.to_string().contains("not a handle"), "{error}");
        assert_eq!(m.depth(), 2, "it popped nothing");
        Ok(())
    })
    .expect("the turn should succeed");
}

//! A machine's environment: the overlays that it and its ancestors stack,
//! composed from the root down (`machines-inherit-an-environment`).

use std::sync::mpsc;

use glam::{Error, ErrorKind, Machine, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

const SHARES: &str = "language g0
object root_share with
    greeting = \"hello\"
    name = \"root\"
object child_share with
    name := \"child\"
object own_share with
    own = \"mine\"
object extra_share with
    extra = \"more\"
object later_share with
    own := \"later\"
";

/// Overlays the object `share`, which the module above defines, on the
/// machine's environment.
fn bind(m: &mut Machine<'_, '_>, share: &str) -> Result<(), Error> {
    m.dict_pack(Vec::<&str>::new())?;
    m.script_object(["shares"], "g", SHARES)?;
    m.dict_get(share)?;
    m.env_overlay()
}

/// The text at `path` in the machine's environment, evaluated.
fn read(m: &mut Machine<'_, '_>, path: &str) -> Result<String, Error> {
    m.env_lookup(path.split('.'))?;
    m.text_pop()
}

/// Pushes the machine's whole environment.
fn environment(m: &mut Machine<'_, '_>) -> Result<(), Error> {
    m.env_lookup(Vec::<&str>::new())
}

#[test]
fn a_machine_s_environment_composes_its_ancestors_shares() {
    let mut rt = runtime();
    rt.run((), |root| bind(root, "root_share"))
        .expect("the root binds its share");
    rt.run("c", |c| bind(c, "child_share"))
        .expect("a child binds its share");
    let read_at = |rt: &mut Runtime, path: Vec<&str>| {
        rt.run(path, |m| Ok((read(m, "greeting")?, read(m, "name")?)))
            .expect("the environment reads")
    };
    assert_eq!(
        read_at(&mut rt, vec!["c", "g"]),
        ("hello".to_owned(), "child".to_owned()),
        "below the child, its share overrides the root's"
    );
    assert_eq!(
        read_at(&mut rt, vec!["d"]),
        ("hello".to_owned(), "root".to_owned()),
        "beside it, the root's alone"
    );
}

#[test]
fn an_unbound_environment_defines_nothing() {
    let mut rt = runtime();
    let (defined, read) = rt
        .run(["a", "b"], |m| {
            environment(m)?;
            let defined = m.dict_defines("greeting")?;
            m.dict_get("greeting")?;
            Ok((defined, m.eval().is_ok()))
        })
        .expect("the environment reads");
    assert!(!defined, "an unbound environment defines nothing");
    assert!(!read, "reading an undefined name is an error");
}

#[test]
fn bindings_are_transactional_and_clear_unbinds_a_subtree() {
    let mut rt = runtime();
    rt.run("c", |c| {
        let abandoned = c.cut(|c| -> Result<(), Error> {
            bind(c, "root_share")?;
            assert_eq!(read(c, "name")?, "root", "a cut sees its own binding");
            Err(Error::new("abandoned"))
        });
        assert_eq!(
            abandoned.expect_err("the cut ended in error").kind(),
            ErrorKind::Failure
        );
        environment(c)?;
        assert!(!c.dict_defines("name")?, "the failed cut bound nothing");
        c.discard(1)?;

        bind(c, "root_share")?;
        c.run("g", |g| bind(g, "child_share"))?;
        c.clear()?;
        c.run("g", |g| {
            environment(g)?;
            assert!(!g.dict_defines("name")?, "clear unbound the subtree");
            g.discard(1)
        })
    })
    .expect("the turn should succeed");
}

#[test]
fn a_cut_waiting_on_its_environment_wakes_when_an_ancestor_binds() {
    let mut rt = runtime();
    let (waiting, waited) = mpsc::channel();
    let name = rt
        .run("p", |p| {
            let fork = p.fork("c", move |c| {
                let mut attempts = 0;
                c.cut(|c| {
                    environment(c)?;
                    if !c.dict_defines("name")? {
                        attempts += 1;
                        if attempts == 1 {
                            waiting.send(()).expect("the parent waits for the attempt");
                        }
                        return Err(c.retry());
                    }
                    c.dict_get("name")?;
                    c.text_pop()
                })
                .map(|name| c.text_push(&name))
            })?;
            waited
                .recv()
                .expect("the fork reads before the parent binds");
            bind(p, "root_share")?;
            p.join(fork)?;
            p.take("c", 1)?;
            p.text_pop()
        })
        .expect("the turn should succeed");
    assert_eq!(name, "root", "the binding woke the cut, which read it");
}

#[test]
fn overlays_stack_and_a_clear_drops_them() {
    let mut rt = runtime();
    rt.run((), |root| bind(root, "root_share"))
        .expect("the root overlays its environment");
    let (stacked, overridden, kept) = rt
        .run("c", |c| {
            bind(c, "own_share")?;
            bind(c, "extra_share")?;
            let stacked = (read(c, "own")?, read(c, "extra")?, read(c, "greeting")?);
            bind(c, "later_share")?;
            let overridden = read(c, "own")?;
            c.env_clear()?;
            environment(c)?;
            assert!(!c.dict_defines("own")?, "its overlays are gone");
            c.discard(1)?;
            Ok((stacked, overridden, read(c, "greeting")?))
        })
        .expect("the turn should succeed");
    assert_eq!(
        stacked,
        ("mine".to_owned(), "more".to_owned(), "hello".to_owned()),
        "its overlays stack over the root's"
    );
    assert_eq!(overridden, "later", "a later overlay overrides with `:=`");
    assert_eq!(kept, "hello", "cleared, the environment is its ancestors'");
}

#[test]
fn a_machine_with_an_overlay_is_present() {
    let mut rt = runtime();
    rt.run(["p", "c"], |c| bind(c, "root_share"))
        .expect("a machine overlays its environment");
    let walked = |rt: &mut Runtime| {
        rt.machines("p")
            .map(|(path, _)| path.keys().len())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        walked(&mut rt),
        [2],
        "p/c holds only its overlay, and is present"
    );
    rt.run(["p", "c"], |c| c.env_clear())
        .expect("it clears its overlays");
    assert!(walked(&mut rt).is_empty(), "then nothing holds it");
}

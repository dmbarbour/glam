//! A machine's environment: the mixins that it and its ancestors set,
//! composed from the root down (`machines-inherit-an-environment`).

use std::sync::mpsc;

use glam::{Error, ErrorKind, Machine, Runtime, RuntimeConfig, ValueKind};

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
extended mixin = mixin with
    extra = \"more\"
";

/// Binds the object `share`, which the module above defines, as the
/// machine's share.
fn bind(m: &mut Machine<'_, '_>, share: &str) -> Result<(), Error> {
    m.make_dict(Vec::<&str>::new())?;
    m.script_object(["shares"], "g", SHARES)?;
    m.access(share)?;
    m.set_env_mixin()
}

/// The text at `path` in the machine's environment, evaluated.
fn read(m: &mut Machine<'_, '_>, path: &str) -> Result<String, Error> {
    m.env();
    m.access(path)?;
    m.pop_text()
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
fn an_unbound_environment_is_an_empty_object() {
    let mut rt = runtime();
    let kind = rt
        .run(["a", "b"], |m| {
            m.env();
            m.access("greeting")?;
            m.eval()?;
            m.kind()
        })
        .expect("the environment reads");
    assert_eq!(kind, ValueKind::Dict, "an undefined name reads `{{}}`");
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
        c.env();
        c.access("name")?;
        c.eval()?;
        assert_eq!(c.kind()?, ValueKind::Dict, "the failed cut bound nothing");

        bind(c, "root_share")?;
        c.run("g", |g| bind(g, "child_share"))?;
        c.clear()?;
        c.run("g", |g| {
            g.env();
            g.access("name")?;
            g.eval()?;
            assert_eq!(g.kind()?, ValueKind::Dict, "clear unbound the subtree");
            Ok(())
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
                    c.env();
                    c.access("name")?;
                    c.eval()?;
                    if c.kind()? == ValueKind::Dict {
                        attempts += 1;
                        if attempts == 1 {
                            waiting.send(()).expect("the parent waits for the attempt");
                        }
                        return Err(c.retry());
                    }
                    c.pop_text()
                })
                .map(|name| c.push_text(&name))
            })?;
            waited
                .recv()
                .expect("the fork reads before the parent binds");
            bind(p, "root_share")?;
            p.join(fork)?;
            p.take("c", 1)?;
            p.pop_text()
        })
        .expect("the turn should succeed");
    assert_eq!(name, "root", "the binding woke the cut, which read it");
}

#[test]
fn a_machine_extends_its_own_mixin_and_may_clear_it() {
    let mut rt = runtime();
    rt.run((), |root| bind(root, "root_share"))
        .expect("the root sets its mixin");
    let (extended, kept) = rt
        .run("c", |c| {
            c.get_env_mixin();
            c.access("greeting")?;
            c.eval()?;
            assert_eq!(c.kind()?, ValueKind::Dict, "it has no mixin of its own yet");
            c.discard(1)?;

            bind(c, "own_share")?;
            c.make_dict(Vec::<&str>::new())?;
            c.script_object(["shares"], "g", SHARES)?;
            c.access("extended")?;
            c.get_env_mixin();
            c.apply(1)?;
            c.set_env_mixin()?;
            let extended = (read(c, "own")?, read(c, "extra")?, read(c, "greeting")?);

            c.clear_env_mixin()?;
            c.env();
            c.access("own")?;
            c.eval()?;
            assert_eq!(c.kind()?, ValueKind::Dict, "its mixin is gone");
            c.discard(1)?;
            Ok((extended, read(c, "greeting")?))
        })
        .expect("the turn should succeed");
    assert_eq!(
        extended,
        ("mine".to_owned(), "more".to_owned(), "hello".to_owned()),
        "its own mixin, extended and set again, over the root's"
    );
    assert_eq!(kept, "hello", "cleared, the environment is its ancestors'");
}

#[test]
fn a_machine_with_a_mixin_is_present() {
    let mut rt = runtime();
    rt.run(["p", "c"], |c| bind(c, "root_share"))
        .expect("a machine sets its mixin");
    let walked = |rt: &mut Runtime| {
        rt.machines("p")
            .map(|(path, _)| path.keys().len())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        walked(&mut rt),
        [2],
        "p/c holds only its mixin, and is present"
    );
    rt.run(["p", "c"], |c| c.clear_env_mixin())
        .expect("it clears its mixin");
    assert!(walked(&mut rt).is_empty(), "then nothing holds it");
}

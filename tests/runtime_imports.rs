//! A forced import asks its host for its source's binary on the root's
//! `imports` port, and compiles the reply (`imports-are-source-requests`).

use glam::{
    Endpoint, Error, ErrorKind, Machine, ModuleInput, Runtime, RuntimeConfig, SourceIdentity,
};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// A top-level source named `main.g` in memory.
fn main_source(body: &str) -> ModuleInput {
    ModuleInput::source(
        SourceIdentity::new("main.g", "memory", "main.g"),
        body.to_owned(),
    )
}

/// A request as the server read it: the top-level source's name, the path,
/// and the kind.
#[derive(Debug, PartialEq)]
struct Request {
    source: Vec<u8>,
    path: String,
    kind: String,
}

/// Serves the request on top of the `sources` machine's stack from `files`,
/// breaking its promise where `files` has no such path.
fn answer(s: &mut Machine<'_, '_>, files: &[(&str, &str)]) -> Result<Request, Error> {
    s.set("received")?;
    s.get("received")?;
    s.access("request")?;
    s.dup()?;
    s.access("source")?;
    s.access("memory")?;
    let source = s.pop_bytes()?;
    s.dup()?;
    s.access("kind")?;
    let kind = s.pop_atom_name()?;
    s.access("path")?;
    let path = s.pop_text()?;
    s.get("received")?;
    s.access("reply")?;
    match files.iter().find(|(name, _)| *name == path) {
        Some((_, body)) => {
            s.push_bytes(body.as_bytes().to_vec());
            s.fulfill()?;
        }
        None => {
            s.push_text(format!("no such source: {path}"));
            s.break_promise()?;
        }
    }
    Ok(Request {
        source: source.to_vec(),
        path,
        kind,
    })
}

/// Serves every request the `sources` machine holds.
fn serve(root: &mut Machine<'_, '_>, files: &[(&str, &str)]) -> Result<Vec<Request>, Error> {
    root.run("sources", |s| {
        let count = s.available("imports");
        s.read("imports", count)?;
        (0..count).map(|_| answer(s, files)).collect()
    })
}

/// Wires the root's `imports` output, where requests arrive by default, to
/// the `sources` machine.
fn wire_sources(root: &mut Machine<'_, '_>) -> Result<(), Error> {
    root.wire(
        Endpoint::output("imports"),
        Endpoint::child("sources", "imports"),
    )
}

/// Evaluates the value on top of `main`'s stack to an integer, or returns
/// `None` where it waits on a reply, leaving it there.
fn try_int(root: &mut Machine<'_, '_>) -> Result<Option<i64>, Error> {
    root.run("main", |m| match m.pop_int() {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::Retry => Ok(None),
        Err(error) => Err(error),
    })
}

fn request(path: &str, kind: &str) -> Request {
    Request {
        source: b"main.g".to_vec(),
        path: path.to_owned(),
        kind: kind.to_owned(),
    }
}

#[test]
fn an_import_waits_for_its_source_and_compiles_the_reply() {
    let files = [
        (
            "pkg/lib.g",
            "language g0\nimport \"leaf.g\" as leaf\nvalue = leaf.value + 1\n",
        ),
        ("pkg/leaf.g", "language g0\nvalue = 41\n"),
    ];
    let mut rt = runtime();
    rt.run((), |root| {
        wire_sources(root)?;
        root.run("main", |m| {
            m.compile(
                ["main"],
                [main_source(
                    "language g0\nimport \"pkg/lib.g\" as lib\nresult = lib.value\n",
                )],
            )?;
            m.access("result")
        })?;
        assert_eq!(try_int(root)?, None, "the import waits for its source");
        assert_eq!(serve(root, &files)?, [request("pkg/lib.g", "module")]);
        assert_eq!(try_int(root)?, None, "the nested import waits for its own");
        assert_eq!(
            serve(root, &files)?,
            [request("pkg/leaf.g", "module")],
            "a nested import names the top-level source and its path from there"
        );
        assert_eq!(try_int(root)?, Some(42));
        assert!(serve(root, &files)?.is_empty(), "each import asked once");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_binary_import_is_the_reply() {
    let mut rt = runtime();
    let bytes = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", |m| {
                m.compile(
                    ["main"],
                    [main_source(
                        "language g0\nimport \"data.bin\" binary as data\nresult = data\n",
                    )],
                )?;
                m.access("result")?;
                assert_eq!(m.pop_bytes().map_err(|e| e.kind()), Err(ErrorKind::Retry));
                Ok(())
            })?;
            assert_eq!(
                serve(root, &[("data.bin", "\u{1}binary")])?,
                [request("data.bin", "binary")]
            );
            root.run("main", |m| m.pop_bytes())
        })
        .expect("the turn should succeed");
    assert_eq!(bytes, &b"\x01binary"[..]);
}

#[test]
fn a_broken_reply_fails_the_import() {
    let mut rt = runtime();
    let error = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", |m| {
                m.compile(
                    ["main"],
                    [main_source(
                        "language g0\nimport \"missing.g\" as missing\nresult = missing.value\n",
                    )],
                )?;
                m.access("result")
            })?;
            assert_eq!(try_int(root)?, None);
            assert_eq!(serve(root, &[])?, [request("missing.g", "module")]);
            let error = try_int(root).expect_err("the import fails");
            assert!(serve(root, &[])?.is_empty(), "a failed import asks no more");
            Ok(error)
        })
        .expect("the turn should succeed");
    assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
}

#[test]
fn an_unserved_request_waits_in_the_root_until_the_host_reads_it() {
    let mut rt = runtime();
    rt.run((), |root| {
        root.run("main", |m| {
            m.compile(
                ["main"],
                [main_source(
                    "language g0\nimport \"lib.g\" as lib\nresult = lib.value\n",
                )],
            )?;
            m.access("result")
        })?;
        assert_eq!(try_int(root)?, None);
        assert_eq!(
            root.available("imports"),
            1,
            "with the root's output unwired, the request waits in its input"
        );
        root.read("imports", 1)?;
        root.give("sources", 1)?;
        root.run("sources", |s| {
            answer(s, &[("lib.g", "language g0\nvalue = 7\n")])
        })?;
        assert_eq!(try_int(root)?, Some(7));
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn a_script_has_no_place_to_import_from() {
    let mut rt = runtime();
    let error = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", |m| {
                m.compile(
                    ["main"],
                    [ModuleInput::script(
                        "g",
                        "language g0\nimport \"lib.g\" as lib\nresult = lib.value\n",
                    )],
                )?;
                m.access("result")
            })?;
            let error = try_int(root).expect_err("the import fails");
            assert!(serve(root, &[])?.is_empty(), "nothing was asked");
            Ok(error)
        })
        .expect("the turn should succeed");
    assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
}

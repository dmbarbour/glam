//! Compiling is lazy, and a forced import asks its host for its source's
//! binary on the root's `imports` port (`imports-are-source-requests`,
//! `compile-yields-an-open-mixin`).

use glam::{Endpoint, Error, ErrorKind, Machine, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// Pushes the package the tests serve, `{memory: "pkg"}`.
fn package(m: &mut Machine<'_, '_>) -> Result<(), Error> {
    m.push_text("pkg");
    m.make_dict(["memory"])
}

/// Pushes the module that `main.g`, a source the host serves, makes of an
/// empty base, and then its `result`.
fn main_result(m: &mut Machine<'_, '_>) -> Result<(), Error> {
    m.make_dict(Vec::<&str>::new())?;
    package(m)?;
    m.source_record(["main"], ["main.g"])?;
    m.compile()?;
    m.instantiate()?;
    m.access("result")
}

/// A request as the server read it: its package's name and its path.
#[derive(Debug, PartialEq)]
struct Request {
    package: String,
    path: Vec<String>,
}

fn request(path: &[&str]) -> Request {
    Request {
        package: "pkg".to_owned(),
        path: path.iter().map(|name| (*name).to_owned()).collect(),
    }
}

/// Serves the request on top of the `sources` machine's stack from `files`,
/// by path, breaking its promise where `files` has no such source.
fn answer(s: &mut Machine<'_, '_>, files: &[(&str, &str)]) -> Result<Request, Error> {
    s.set("received")?;
    s.get("received")?;
    s.access("request")?;
    s.dup()?;
    s.access("package")?;
    s.access("memory")?;
    let package = s.pop_text()?;
    s.access("path")?;
    let count = s.unpack_list()?;
    let mut path = (0..count)
        .map(|_| s.pop_text())
        .collect::<Result<Vec<_>, _>>()?;
    path.reverse();
    s.get("received")?;
    s.access("reply")?;
    let name = path.join("/");
    match files.iter().find(|(file, _)| *file == name) {
        Some((_, body)) => {
            s.push_bytes(body.as_bytes().to_vec());
            s.fulfill()?;
        }
        None => {
            s.push_text(format!("no such source: {name}"));
            s.break_promise()?;
        }
    }
    Ok(Request { package, path })
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

#[test]
fn a_compile_asks_for_its_source_and_its_imports_when_forced() {
    let files = [
        (
            "main.g",
            "language g0\nimport \"pkg/lib.g\" as lib\nresult = lib.value\n",
        ),
        (
            "pkg/lib.g",
            "language g0\nimport \"leaf.g\" as leaf\nvalue = leaf.value + 1\n",
        ),
        ("pkg/leaf.g", "language g0\nvalue = 41\n"),
    ];
    let mut rt = runtime();
    rt.run((), |root| {
        wire_sources(root)?;
        root.run("main", main_result)?;
        assert!(
            serve(root, &files)?.is_empty(),
            "compiling evaluates nothing, so it asks for nothing"
        );
        assert_eq!(try_int(root)?, None, "the compile waits for its source");
        assert_eq!(serve(root, &files)?, [request(&["main.g"])]);
        assert_eq!(try_int(root)?, None, "the import waits for its own");
        assert_eq!(serve(root, &files)?, [request(&["pkg", "lib.g"])]);
        assert_eq!(try_int(root)?, None);
        assert_eq!(
            serve(root, &files)?,
            [request(&["pkg", "leaf.g"])],
            "a nested import's path continues from its importer's folder"
        );
        assert_eq!(try_int(root)?, Some(42));
        assert!(serve(root, &files)?.is_empty(), "each import asked once");
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn an_imported_source_is_the_reply() {
    let mut rt = runtime();
    let bytes = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", |m| {
                package(m)?;
                m.push_text("data.bin");
                m.make_list(1)?;
                m.make_dict(["package", "path"])?;
                m.import_source()?;
                assert_eq!(m.pop_bytes().map_err(|e| e.kind()), Err(ErrorKind::Retry));
                Ok(())
            })?;
            assert_eq!(
                serve(root, &[("data.bin", "\u{1}binary")])?,
                [request(&["data.bin"])]
            );
            root.run("main", |m| m.pop_bytes())
        })
        .expect("the turn should succeed");
    assert_eq!(bytes, &b"\x01binary"[..]);
}

#[test]
fn a_binary_import_is_its_reply() {
    let files = [
        (
            "main.g",
            "language g0\nimport \"data.bin\" binary as data\nresult = data\n",
        ),
        ("data.bin", "bytes"),
    ];
    let mut rt = runtime();
    let (bytes, asked) = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", main_result)?;
            let mut asked = Vec::new();
            loop {
                let value = root.run("main", |m| match m.pop_bytes() {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(error) if error.kind() == ErrorKind::Retry => Ok(None),
                    Err(error) => Err(error),
                })?;
                match value {
                    Some(bytes) => break Ok((bytes, asked)),
                    None => asked.extend(serve(root, &files)?),
                }
            }
        })
        .expect("the turn should succeed");
    assert_eq!(bytes, "bytes");
    assert_eq!(asked, [request(&["main.g"]), request(&["data.bin"])]);
}

#[test]
fn a_broken_reply_fails_the_import() {
    let files = [(
        "main.g",
        "language g0\nimport \"missing.g\" as missing\nresult = missing.value\n",
    )];
    let mut rt = runtime();
    let error = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", main_result)?;
            assert_eq!(try_int(root)?, None);
            serve(root, &files)?;
            assert_eq!(try_int(root)?, None);
            assert_eq!(serve(root, &files)?, [request(&["missing.g"])]);
            let error = try_int(root).expect_err("the import fails");
            assert!(
                serve(root, &files)?.is_empty(),
                "a failed import asks no more"
            );
            Ok(error)
        })
        .expect("the turn should succeed");
    assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
}

#[test]
fn an_unserved_request_waits_in_the_root_until_the_host_reads_it() {
    let mut rt = runtime();
    rt.run((), |root| {
        root.run("main", main_result)?;
        assert_eq!(try_int(root)?, None);
        assert_eq!(
            root.available("imports"),
            1,
            "with the root's output unwired, the request waits in its input"
        );
        root.read("imports", 1)?;
        root.give("sources", 1)?;
        root.run("sources", |s| {
            answer(s, &[("main.g", "language g0\nresult = 7\n")])
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
                m.make_dict(Vec::<&str>::new())?;
                m.script_object(
                    ["main"],
                    "g",
                    "language g0\nimport \"lib.g\" as lib\nresult = lib.value\n",
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

#[test]
fn composed_mixins_override_from_the_top() {
    let mut rt = runtime();
    let (x, y) = rt
        .run("main", |m| {
            m.make_dict(Vec::<&str>::new())?;
            m.script_record(["main"], "g", "language g0\nx = 1\ny = x * 10\n");
            m.compile()?;
            m.script_record(["main"], "g", "language g0\nx := 2\n");
            m.compile()?;
            m.compose(2)?;
            m.instantiate()?;
            m.dup()?;
            m.access("x")?;
            let x = m.pop_int()?;
            m.access("y")?;
            Ok((x, m.pop_int()?))
        })
        .expect("the turn should succeed");
    assert_eq!(
        (x, y),
        (2, 20),
        "the top mixin overrides, and the one below sees the override"
    );
}

#[test]
fn a_failed_compile_reports_at_the_root_when_forced() {
    let mut rt = runtime();
    rt.run((), |root| {
        root.run("main", |m| {
            m.make_dict(Vec::<&str>::new())?;
            m.script_object(["broken"], "g", "language g0\nx = (\n")
        })?;
        assert_eq!(
            root.available("diagnostics"),
            0,
            "compiling evaluates nothing, so reports nothing"
        );
        let error = root
            .run("main", |m| m.eval())
            .expect_err("the module does not parse");
        assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
        assert!(
            root.available("diagnostics") > 0,
            "the compile's diagnostics reach the root"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

#[test]
fn an_inline_import_mixes_into_its_importer() {
    let files = [
        (
            "main.g",
            "language g0\nimport \"lib.g\"\nresult = value + offset\noffset = 2\n",
        ),
        ("lib.g", "language g0\nvalue = offset * 20\n"),
    ];
    let mut rt = runtime();
    let result = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", main_result)?;
            loop {
                match try_int(root)? {
                    Some(result) => break Ok(result),
                    None => assert!(!serve(root, &files)?.is_empty(), "a wait asks"),
                }
            }
        })
        .expect("the turn should succeed");
    assert_eq!(result, 42, "the import reads its importer's final self");
}

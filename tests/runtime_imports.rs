//! Compiling is lazy, and a forced import asks its host for its source's
//! binary on the root's `imports` port (`imports-are-source-requests`,
//! `compile-yields-an-open-mixin`).

use std::sync::mpsc;

use glam::{Endpoint, Error, ErrorKind, Machine, Runtime, RuntimeConfig};

fn runtime() -> Runtime {
    Runtime::open(RuntimeConfig::new()).expect("a runtime should open")
}

/// Pushes the package the tests serve, `{memory: "pkg"}`.
fn package(m: &mut Machine<'_, '_>) -> Result<(), Error> {
    m.text_push("pkg");
    m.dict_pack(["memory"])
}

/// Pushes the module that `main.g`, a source the host serves, makes of an
/// empty base, and then its `result`.
fn main_result(m: &mut Machine<'_, '_>) -> Result<(), Error> {
    m.dict_pack(Vec::<&str>::new())?;
    package(m)?;
    m.source_record(["main"], ["main.g"])?;
    m.compile()?;
    m.instantiate()?;
    m.dict_get("result")
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
    s.local_set("received")?;
    s.local_get("received")?;
    s.dict_get("request")?;
    s.dup(1)?;
    s.dict_get("package")?;
    s.dict_get("memory")?;
    let package = s.text_pop()?;
    s.dict_get("path")?;
    s.dup(1)?;
    s.list_len()?;
    let count = usize::try_from(s.int_pop()?).expect("a path's length fits");
    s.list_unpack(count)?;
    let mut path = (0..count)
        .map(|_| s.text_pop())
        .collect::<Result<Vec<_>, _>>()?;
    path.reverse();
    s.local_get("received")?;
    s.dict_get("reply")?;
    let name = path.join("/");
    match files.iter().find(|(file, _)| *file == name) {
        Some((_, body)) => {
            s.bytes_push(body.as_bytes().to_vec());
            s.promise_fulfill()?;
        }
        None => {
            s.text_push(format!("no such source: {name}"));
            s.promise_break()?;
        }
    }
    Ok(Request { package, path })
}

/// Serves every request the `sources` machine holds.
fn serve(root: &mut Machine<'_, '_>, files: &[(&str, &str)]) -> Result<Vec<Request>, Error> {
    root.run("sources", |s| {
        let count = s.port_available("imports");
        s.port_read("imports", count)?;
        (0..count).map(|_| answer(s, files)).collect()
    })
}

/// Wires the root's `imports` output, where requests arrive by default, to
/// the `sources` machine.
fn wire_sources(root: &mut Machine<'_, '_>) -> Result<(), Error> {
    root.wire_add(
        Endpoint::output("imports"),
        Endpoint::child("sources", "imports"),
    )
}

/// Evaluates the value on top of `main`'s stack to an integer, or returns
/// `None` where it waits on a reply, leaving it there.
fn try_int(root: &mut Machine<'_, '_>) -> Result<Option<i64>, Error> {
    root.run("main", |m| match m.int_pop() {
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
                m.text_push("data.bin");
                m.list_pack(1)?;
                m.dict_pack(["package", "path"])?;
                m.source_import()?;
                assert_eq!(m.bytes_pop().map_err(|e| e.kind()), Err(ErrorKind::Retry));
                Ok(())
            })?;
            assert_eq!(
                serve(root, &[("data.bin", "\u{1}binary")])?,
                [request(&["data.bin"])]
            );
            root.run("main", |m| m.bytes_pop())
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
                let value = root.run("main", |m| match m.bytes_pop() {
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
            root.port_available("imports"),
            1,
            "with the root's output unwired, the request waits in its input"
        );
        root.port_read("imports", 1)?;
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
                m.dict_pack(Vec::<&str>::new())?;
                m.script_object(
                    ["main"],
                    "g",
                    "language g0\nimport \"lib.g\" as lib\nresult = lib.value\n",
                )?;
                m.dict_get("result")
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
            m.dict_pack(Vec::<&str>::new())?;
            m.script_record(["main"], "g", "language g0\nx = 1\ny = x * 10\n");
            m.compile()?;
            m.script_record(["main"], "g", "language g0\nx := 2\n");
            m.compile()?;
            m.compose(2)?;
            m.instantiate()?;
            m.dup(1)?;
            m.dict_get("x")?;
            let x = m.int_pop()?;
            m.dict_get("y")?;
            Ok((x, m.int_pop()?))
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
            m.dict_pack(Vec::<&str>::new())?;
            m.script_object(["broken"], "g", "language g0\nx = (\n")
        })?;
        assert_eq!(
            root.port_available("diagnostics"),
            0,
            "compiling evaluates nothing, so reports nothing"
        );
        let error = root
            .run("main", |m| m.eval())
            .expect_err("the module does not parse");
        assert_eq!(error.kind(), ErrorKind::Failure, "{error}");
        assert!(
            root.port_available("diagnostics") > 0,
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

const PAIR_MACRO: &str = "language g0
pair =
  .read.sep =>>
  .read.data >>= \\left ->
  .read.sep =>>
  .read.data >>= \\right ->
  .write.text \"[\" =>>
  .write.data left =>>
  .write.text \",\" =>>
  .write.data right =>>
  .write.text \"]\"
";

/// Serves `files` until `main`'s top value, `main.g`'s result, evaluates
/// to a list, returning its length and the requests served.
fn serve_until_list(
    root: &mut Machine<'_, '_>,
    files: &[(&str, &str)],
) -> Result<(usize, Vec<Request>), Error> {
    let mut asked = Vec::new();
    loop {
        let length = root.run("main", |m| {
            m.dup(1)?;
            m.list_len()?;
            match m.int_pop() {
                Ok(length) => Ok(Some(usize::try_from(length).expect("a length fits"))),
                Err(error) if error.kind() == ErrorKind::Retry => {
                    m.discard(1)?;
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        })?;
        match length {
            Some(length) => return Ok((length, asked)),
            None => {
                let served = serve(root, files)?;
                assert!(!served.is_empty(), "a compile that waits has asked");
                asked.extend(served);
            }
        }
    }
}

#[test]
fn a_macro_that_forces_an_import_while_compiling_waits_and_compiles_again() {
    let files = [
        (
            "main.g",
            "language g0\nimport \"macros.g\"\nresult = @pair 10 20\n",
        ),
        ("macros.g", PAIR_MACRO),
    ];
    let mut rt = runtime();
    let (length, asked) = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", main_result)?;
            serve_until_list(root, &files)
        })
        .expect("the turn should succeed");
    assert_eq!(length, 2);
    assert_eq!(
        asked,
        [request(&["main.g"]), request(&["macros.g"])],
        "the compile that waited asks for nothing again"
    );
}

#[test]
fn a_chain_of_compile_time_imports_waits_at_each() {
    let files = [
        (
            "main.g",
            "language g0\nimport \"a.g\"\nresult = @pair 3 4\n",
        ),
        ("a.g", "language g0\nimport \"b.g\"\nwrapped = @pair 1 2\n"),
        ("b.g", PAIR_MACRO),
    ];
    let mut rt = runtime();
    let (length, asked) = rt
        .run((), |root| {
            wire_sources(root)?;
            root.run("main", main_result)?;
            serve_until_list(root, &files)
        })
        .expect("the turn should succeed");
    assert_eq!(length, 2);
    assert_eq!(
        asked,
        [request(&["main.g"]), request(&["a.g"]), request(&["b.g"])]
    );
}

/// Runs `body` on the root while a fork of the `sources` machine serves
/// `files`, until the root writes to its `stop` input; returns what `body`
/// returns, with the requests the server answered.
fn serving<R>(
    root: &mut Machine<'_, '_>,
    files: &'static [(&'static str, &'static str)],
    body: impl FnOnce(&mut Machine<'_, '_>) -> Result<R, Error>,
) -> Result<(R, Vec<Request>), Error> {
    wire_sources(root)?;
    root.wire_add(Endpoint::output("stop"), Endpoint::child("sources", "stop"))?;
    let (answered, requests) = mpsc::channel();
    let server = root.fork("sources", move |s| {
        loop {
            let stopping = s.cut(|s| {
                if s.port_available("stop") > 0 {
                    return Ok(true);
                }
                s.port_read("imports", 1)?;
                Ok(false)
            })?;
            if stopping {
                return Ok(());
            }
            let request = answer(s, files)?;
            answered.send(request).expect("the test keeps its receiver");
        }
    })?;
    let result = body(root);
    root.unit_push();
    root.port_write("stop", 1)?;
    root.join(server)?;
    Ok((result?, requests.try_iter().collect()))
}

#[test]
fn an_import_that_background_work_forces_is_served_while_a_cut_waits() {
    static FILES: [(&str, &str); 2] = [
        (
            "main.g",
            "language g0\nimport 'std\nimport \"lib.g\" as lib\neffect = seq lib.value (.r ())\nresult = anno { refl:effect } \"ready\"\n",
        ),
        ("lib.g", "language g0\nvalue = 5\n"),
    ];
    let mut rt = Runtime::open(RuntimeConfig::new().worker_threads(2))
        .expect("a runtime with workers should open");
    let (text, requests) = rt
        .run((), |root| {
            serving(root, &FILES, |root| {
                root.run("main", |m| {
                    m.dict_pack(Vec::<&str>::new())?;
                    package(m)?;
                    m.source_record(["main"], ["main.g"])?;
                    m.compile()?;
                    m.instantiate()?;
                    m.dict_get("result")?;
                    m.cut(|m| m.text_pop())
                })
            })
        })
        .expect("the turn should succeed");
    assert_eq!(text, "ready");
    assert_eq!(requests, [request(&["main.g"]), request(&["lib.g"])]);
}

#[test]
fn a_compile_that_waited_reports_its_diagnostics_once() {
    let files = [
        (
            "main.g",
            "language g0\nimport \"macros.g\"\nnoise = let unused = 1 in 2\nresult = @pair 10 20\n",
        ),
        ("macros.g", PAIR_MACRO),
    ];
    let mut rt = runtime();
    rt.run((), |root| {
        wire_sources(root)?;
        root.run("main", main_result)?;
        let (length, asked) = serve_until_list(root, &files)?;
        assert_eq!((length, asked.len()), (2, 2), "the compile waited");
        assert_eq!(
            root.port_available("diagnostics"),
            1,
            "the attempt that waited dropped its warning; the one that finished reported it"
        );
        Ok(())
    })
    .expect("the turn should succeed");
}

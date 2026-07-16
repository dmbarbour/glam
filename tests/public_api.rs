use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use glam::{Assembler, Host, HostError, ModuleInput, Severity, Value};

#[test]
fn public_api_builds_a_script_module_and_extracts_binary_data() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["example"])
        .script("g", "language g0\nasm.result = \"Hello, library!\"\n")
        .build()
        .expect("script module should build");

    assert_eq!(module.diagnostics(), []);
    assert_eq!(
        assembler
            .binary_at(module.value(), "asm.result")
            .expect("asm.result should be binary"),
        b"Hello, library!".as_slice()
    );
    assert_eq!(
        assembler
            .read_diagnostics()
            .expect("default assembler should retain diagnostics")
            .dropped(),
        0
    );
}

#[test]
fn public_api_can_load_sources_and_binaries_from_a_custom_host() {
    let host = MemoryHost::new([
        (
            "main.g",
            b"language g0\nimport \"payload.bin\" binary as payload\nasm.result = payload\n"
                .as_slice(),
        ),
        ("payload.bin", b"virtual bytes".as_slice()),
    ]);
    let assembler = Assembler::default().with_host(host);
    let module = assembler
        .module(["virtual"])
        .inputs([ModuleInput::file("main.g")])
        .build()
        .expect("virtual module should build");

    assert_eq!(
        assembler
            .binary_at(module.value(), "asm.result")
            .expect("virtual binary import should evaluate"),
        b"virtual bytes".as_slice()
    );
}

#[test]
fn source_compiler_reports_invalid_utf8_with_assembler_provenance() {
    let assembler = Assembler::default().with_host(MemoryHost::new([(
        "invalid.g",
        b"language g0\nvalue = \xff\n".as_slice(),
    )]));

    let error = assembler
        .module(["invalid"])
        .file("invalid.g")
        .build()
        .expect_err("the built-in g compiler should reject invalid UTF-8");

    assert_eq!(error.diagnostics().len(), 1);
    let diagnostic = &error.diagnostics()[0];
    assert_eq!(diagnostic.source(), Some("invalid.g"));
    assert_eq!(diagnostic.line(), Some(1));
    assert_eq!(diagnostic.severity(), Severity::Error);
    assert!(diagnostic.message().contains("not valid UTF-8"));
}

#[test]
fn caller_selected_module_path_scopes_abstract_global_paths() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["client", "root"])
        .script("g", "language g0\nunique Marker\n")
        .build()
        .expect("module should build");

    assert_eq!(
        assembler
            .get(module.value(), "Marker")
            .expect("unique declaration should define Marker"),
        Value::abstract_global_path(["client", "root", "Marker"])
    );
}

#[test]
fn public_values_convert_numbers_without_exposing_big_number_types() {
    let integer = Value::integer(-42);
    assert_eq!(integer.as_i64(), Some(-42));
    assert_eq!(integer.as_rational_i64(), Some((-42, 1)));
    assert_eq!(integer.as_f64(), Some(-42.0));
    assert_eq!(integer.as_number_text().as_deref(), Some("-42"));

    let ratio = Value::number_from_text("-6/4").expect("exact rational should parse");
    assert_eq!(ratio.as_number_text().as_deref(), Some("-3/2"));
    assert_eq!(ratio.as_rational_i64(), Some((-3, 2)));
    assert_eq!(ratio.as_i64(), None);
    assert_eq!(ratio.as_f64(), Some(-1.5));
    assert_eq!(Value::rational(1, 0), None);

    assert_eq!(Value::number_from_f64(1.5), Value::rational(3, 2));
    assert_eq!(Value::number_from_f64(f64::NAN), None);
    assert_eq!(Value::number_from_f64(f64::INFINITY), None);
    assert!(Value::number_from_text("1/0").is_err());
}

#[test]
fn assembler_applies_and_evaluates_functions() {
    let assembler = Assembler::default();
    let module = assembler
        .module(["application"])
        .script("g", "language g0\nadd = \\x y -> x + y\n")
        .build()
        .expect("function module should build");
    let add = assembler
        .get(module.value(), "add")
        .expect("module should define add");
    let sum = assembler
        .apply(&add, [Value::integer(20), Value::integer(22)])
        .expect("application should be accepted lazily");

    assert_eq!(sum.kind(), glam::ValueKind::Lazy);
    assert_eq!(
        assembler
            .evaluate(&sum)
            .expect("application should evaluate")
            .as_i64(),
        Some(42)
    );
}

#[test]
fn assembler_extracts_ranges_from_compact_and_list_binary_data() {
    let assembler = Assembler::default();
    let compact = Value::binary(Bytes::from_static(b"abcdef"));
    assert_eq!(
        assembler
            .binary_slice(&compact, 1..5)
            .expect("compact binary should slice"),
        b"bcde".as_slice()
    );

    let listed = Value::list([
        Value::integer(b'a'.into()),
        Value::integer(b'b'.into()),
        Value::integer(b'c'.into()),
        Value::integer(b'd'.into()),
    ]);
    assert_eq!(
        assembler
            .binary_slice(&listed, 1..3)
            .expect("byte-valued list should slice"),
        b"bc".as_slice()
    );
    assert!(assembler.binary_slice(&listed, 3..5).is_err());
}

#[test]
fn checked_net_builder_constructs_an_identity_function() {
    let assembler = Assembler::default();
    let identity = assembler
        .net(|net| {
            let bind = net.bind();
            net.wire(bind.argument, bind.result)?;
            Ok(bind.application)
        })
        .expect("identity net should be closed");
    let result = assembler
        .apply(&identity, [Value::integer(42)])
        .and_then(|value| assembler.evaluate(&value))
        .expect("identity net should return its argument");

    assert_eq!(result.as_i64(), Some(42));
}

#[test]
fn checked_net_builder_exposes_data_through_copy_helpers() {
    let assembler = Assembler::default();
    let net = assembler
        .net(|net| {
            let data = net.data(Value::text("copied"));
            let copy = net.copy(1);
            net.wire(data, copy.input)?;
            Ok(copy.outputs[0])
        })
        .expect("one-output copy should normalize to a tunnel");

    assert_eq!(
        assembler
            .evaluate(&net)
            .expect("net should expose its data")
            .as_binary(),
        Some(b"copied".as_slice())
    );
}

#[test]
fn checked_net_builder_reports_wiring_and_finalization_errors() {
    let assembler = Assembler::default();
    let unwired = assembler
        .net(|net| {
            let bind = net.bind();
            Ok(bind.application)
        })
        .expect_err("unwired ports must reject the net");
    assert!(unwired.to_string().contains("is unwired"));

    let duplicate = assembler
        .net(|net| {
            let left = net.data(Value::integer(1));
            let right = net.data(Value::integer(2));
            let other = net.data(Value::integer(3));
            net.wire(left, right)?;
            net.wire(left, other)?;
            Ok(other)
        })
        .expect_err("a port cannot be wired twice");
    assert!(duplicate.to_string().contains("wired more than once"));
}

struct MemoryHost {
    files: HashMap<PathBuf, Bytes>,
}

impl MemoryHost {
    fn new<const N: usize>(files: [(&str, &[u8]); N]) -> Self {
        Self {
            files: files
                .into_iter()
                .map(|(path, bytes)| (PathBuf::from(path), Bytes::copy_from_slice(bytes)))
                .collect(),
        }
    }
}

impl Host for MemoryHost {
    fn read(&self, path: &Path) -> Result<Bytes, HostError> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| HostError::new(format!("missing virtual file `{}`", path.display())))
    }

    fn path_exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    fn environment_variable(&self, _name: &str) -> Option<OsString> {
        None
    }
}

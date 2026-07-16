use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use glam::{Assembler, Host, HostError, ModuleInput, Value};

#[test]
fn public_api_builds_a_script_module_and_extracts_binary_data() {
    let assembler = Assembler::default();
    let module = assembler
        .module()
        .env(Value::binary(Vec::new()))
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
    assert_eq!(assembler.diagnostics().dropped(), 0);
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
    let assembler = Assembler::with_host(host);
    let module = assembler
        .module()
        .env(Value::binary(Vec::new()))
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

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use glam::{Host, HostError, SystemHost};
use sha2::{Digest, Sha256};

type Sha256Digest = [u8; 32];

#[derive(Clone, Default)]
pub(crate) struct LocalFileHost {
    observed: Arc<Mutex<BTreeMap<PathBuf, Sha256Digest>>>,
}

impl LocalFileHost {
    fn absolute_path(path: &Path) -> Result<PathBuf, HostError> {
        std::path::absolute(path).map_err(|error| {
            HostError::new(format!(
                "could not make local path `{}` absolute: {error}",
                path.display()
            ))
        })
    }

    fn read_untracked(path: &Path) -> Result<Bytes, HostError> {
        SystemHost.read(path)
    }

    pub(crate) fn verify_unchanged(&self) -> Result<(), String> {
        let observed = self
            .observed
            .lock()
            .expect("local file observation mutex should not be poisoned")
            .clone();
        let mut changes = Vec::new();
        for (path, expected) in observed {
            match Self::read_untracked(&path) {
                Ok(bytes) if sha256(&bytes) == expected => {}
                Ok(_) => changes.push(format!("`{}` changed", path.display())),
                Err(error) => changes.push(format!(
                    "`{}` could not be re-read: {error}",
                    path.display()
                )),
            }
        }
        if changes.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "local files changed while the assembler was running:\n{}",
                changes.join("\n")
            ))
        }
    }

    pub(crate) fn write_manifest(&self, path: &Path) -> Result<(), String> {
        let output = Self::absolute_path(path).map_err(|error| error.to_string())?;
        let observed = self
            .observed
            .lock()
            .expect("local file observation mutex should not be poisoned");
        if observed.contains_key(&output) {
            return Err(format!(
                "manifest output `{}` is also an assembly input",
                output.display()
            ));
        }

        let mut manifest = String::from(
            "# glam local-file manifest v1\n# sha256<TAB>percent-encoded platform path bytes\n",
        );
        for (source, digest) in observed.iter() {
            manifest.push_str(&hex(digest));
            manifest.push('\t');
            manifest.push_str(&percent_encoded_path(source));
            manifest.push('\n');
        }
        fs::write(&output, manifest)
            .map_err(|error| format!("could not write manifest `{}`: {error}", output.display()))
    }
}

impl Host for LocalFileHost {
    fn read(&self, path: &Path) -> Result<Bytes, HostError> {
        let path = Self::absolute_path(path)?;
        let bytes = Self::read_untracked(&path)?;
        let digest = sha256(&bytes);
        let mut observed = self
            .observed
            .lock()
            .expect("local file observation mutex should not be poisoned");
        if let Some(previous) = observed.insert(path.clone(), digest)
            && previous != digest
        {
            observed.insert(path.clone(), previous);
            return Err(HostError::new(format!(
                "local file `{}` changed between reads",
                path.display()
            )));
        }
        Ok(bytes)
    }

    fn path_exists(&self, path: &Path) -> bool {
        SystemHost.path_exists(path)
    }
}

fn sha256(bytes: &[u8]) -> Sha256Digest {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    text
}

fn percent_encoded_path(path: &Path) -> String {
    let path = env::current_dir()
        .ok()
        .and_then(|working_directory| path.strip_prefix(working_directory).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(path);
    let mut encoded = String::new();
    for byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&hex(&[*byte]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_changes_after_a_read() {
        let directory =
            env::temp_dir().join(format!("glam-local-file-host-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let path = directory.join("input.g");
        fs::write(&path, "first").expect("test input should be written");
        let host = LocalFileHost::default();

        host.read(&path).expect("first read should succeed");
        fs::write(&path, "second").expect("test input should be changed");

        assert!(host.verify_unchanged().unwrap_err().contains("changed"));
        assert!(
            host.read(&path)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
    }
}

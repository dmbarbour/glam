//! Source acquisition, immutable loaded artifacts, and local consistency.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use sha2::{Digest, Sha256};

use crate::api::Value;

pub const CONTENT_DIGEST_ALGORITHM: &str = "sha256";

/// A SHA-256 digest of the exact bytes supplied by a source system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn algorithm(&self) -> &'static str {
        CONTENT_DIGEST_ALGORITHM
    }

    pub fn to_hex(self) -> String {
        hex(&self.0)
    }

    pub(crate) fn value(self) -> Value {
        Value::record([(
            self.algorithm(),
            Value::binary(Bytes::copy_from_slice(&self.0)),
        )])
    }
}

/// Assembler-owned source identity used for diagnostics and reflection.
///
/// `label` is a compact host-facing description. `value` is the extensible,
/// tagged Glam representation placed in diagnostic provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentity {
    label: Arc<str>,
    value: Value,
}

impl SourceIdentity {
    pub fn new(label: impl Into<Arc<str>>, value: Value) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }

    pub fn file(path: impl AsRef<Path>) -> Self {
        let label: Arc<str> = Arc::from(path.as_ref().display().to_string());
        Self::new(label.clone(), Value::record([("file", Value::text(label))]))
    }

    pub fn script(label: impl Into<Arc<str>>, bytes: Bytes) -> Self {
        Self::new(label, Value::record([("script", Value::binary(bytes))]))
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn value(&self) -> &Value {
        &self.value
    }
}

/// A validated child-relative source request originating in a front end.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativeSourcePath(Arc<str>);

impl RelativeSourcePath {
    pub fn new(request: impl AsRef<str>) -> Result<Self, SourceError> {
        let request = request.as_ref();
        validate_relative_source_path(request)?;
        Ok(Self(Arc::from(request)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Resolves imports relative to one already-loaded source artifact.
pub trait ImportResolver: Send + Sync {
    fn load_relative(&self, request: &RelativeSourcePath) -> Result<SourceArtifact, SourceError>;
}

impl<T: ImportResolver + ?Sized> ImportResolver for Arc<T> {
    fn load_relative(&self, request: &RelativeSourcePath) -> Result<SourceArtifact, SourceError> {
        (**self).load_relative(request)
    }
}

/// Immutable bytes and acquisition metadata returned by a source system.
#[derive(Clone)]
pub struct SourceArtifact {
    bytes: Bytes,
    identity: SourceIdentity,
    digest: ContentDigest,
    imports: Option<Arc<dyn ImportResolver>>,
}

impl SourceArtifact {
    /// Constructs an artifact and computes the digest from `bytes`.
    pub fn new(bytes: impl Into<Bytes>, identity: SourceIdentity) -> Self {
        let bytes = bytes.into();
        let digest = ContentDigest::of(&bytes);
        Self {
            bytes,
            identity,
            digest,
            imports: None,
        }
    }

    pub fn with_import_resolver(mut self, resolver: impl ImportResolver + 'static) -> Self {
        self.imports = Some(Arc::new(resolver));
        self
    }

    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    pub fn digest(&self) -> ContentDigest {
        self.digest
    }

    pub fn digest_algorithm(&self) -> &'static str {
        self.digest.algorithm()
    }

    pub fn load_relative(&self, request: &RelativeSourcePath) -> Result<Self, SourceError> {
        let resolver = self.imports.as_ref().ok_or_else(|| {
            SourceError::new(format!(
                "source `{}` does not support relative imports",
                self.identity.label()
            ))
        })?;
        resolver.load_relative(request)
    }
}

impl fmt::Debug for SourceArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceArtifact")
            .field("bytes", &self.bytes.len())
            .field("identity", &self.identity)
            .field("digest", &self.digest)
            .field("supports_imports", &self.imports.is_some())
            .finish()
    }
}

/// Trusted source discovery and acquisition for top-level local requests.
/// Relative requests continue through the resolver carried by the artifact.
pub trait SourceSystem: Send + Sync {
    fn load_top_level(&self, path: &Path) -> Result<SourceArtifact, SourceError>;
}

impl<T: SourceSystem + ?Sized> SourceSystem for Arc<T> {
    fn load_top_level(&self, path: &Path) -> Result<SourceArtifact, SourceError> {
        (**self).load_top_level(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    message: Arc<str>,
}

impl SourceError {
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SourceError {}

/// Compatibility byte host adapted into the artifact-oriented source API.
pub trait Host: Send + Sync {
    fn read(&self, path: &Path) -> Result<Bytes, SourceError>;
}

impl<T: Host + ?Sized> Host for Arc<T> {
    fn read(&self, path: &Path) -> Result<Bytes, SourceError> {
        (**self).read(path)
    }
}

pub type HostError = SourceError;

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemHost;

impl Host for SystemHost {
    fn read(&self, path: &Path) -> Result<Bytes, SourceError> {
        fs::read(path).map(Bytes::from).map_err(|error| {
            SourceError::new(format!("could not read `{}`: {error}", path.display()))
        })
    }
}

#[derive(Clone)]
pub struct HostSourceSystem {
    host: Arc<dyn Host>,
}

impl HostSourceSystem {
    pub fn new(host: impl Host + 'static) -> Self {
        Self {
            host: Arc::new(host),
        }
    }

    fn load_path(&self, path: &Path) -> Result<SourceArtifact, SourceError> {
        let bytes = self.host.read(path)?;
        let absolute = absolute_path(path)?;
        let base = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok(
            SourceArtifact::new(bytes, SourceIdentity::file(&absolute)).with_import_resolver(
                HostImportResolver {
                    system: self.clone(),
                    base,
                },
            ),
        )
    }
}

impl SourceSystem for HostSourceSystem {
    fn load_top_level(&self, path: &Path) -> Result<SourceArtifact, SourceError> {
        self.load_path(path)
    }
}

#[derive(Clone)]
struct HostImportResolver {
    system: HostSourceSystem,
    base: PathBuf,
}

impl ImportResolver for HostImportResolver {
    fn load_relative(&self, request: &RelativeSourcePath) -> Result<SourceArtifact, SourceError> {
        self.system.load_path(&self.base.join(request.as_str()))
    }
}

/// Local filesystem source system with per-instance consistency observations.
#[derive(Clone, Default)]
pub struct FileSourceSystem {
    observed: Arc<Mutex<BTreeMap<PathBuf, ContentDigest>>>,
}

impl FileSourceSystem {
    fn read_untracked(path: &Path) -> Result<Bytes, SourceError> {
        SystemHost.read(path)
    }

    fn load_path(&self, path: &Path) -> Result<SourceArtifact, SourceError> {
        let path = absolute_path(path)?;
        let bytes = Self::read_untracked(&path)?;
        let artifact = SourceArtifact::new(bytes, SourceIdentity::file(&path));
        let digest = artifact.digest();
        let mut observed = self
            .observed
            .lock()
            .expect("local source observation mutex should not be poisoned");
        if let Some(previous) = observed.insert(path.clone(), digest)
            && previous != digest
        {
            observed.insert(path.clone(), previous);
            return Err(SourceError::new(format!(
                "local file `{}` changed between reads",
                path.display()
            )));
        }
        drop(observed);
        let base = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok(artifact.with_import_resolver(FileImportResolver {
            system: self.clone(),
            base,
        }))
    }

    pub fn verify_unchanged(&self) -> Result<(), SourceError> {
        let observed = self
            .observed
            .lock()
            .expect("local source observation mutex should not be poisoned")
            .clone();
        let mut changes = Vec::new();
        for (path, expected) in observed {
            match Self::read_untracked(&path) {
                Ok(bytes) if ContentDigest::of(&bytes) == expected => {}
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
            Err(SourceError::new(format!(
                "local files changed while the assembler was running:\n{}",
                changes.join("\n")
            )))
        }
    }

    pub fn write_manifest(&self, path: &Path) -> Result<(), SourceError> {
        let output = absolute_path(path)?;
        let observed = self
            .observed
            .lock()
            .expect("local source observation mutex should not be poisoned");
        if observed.contains_key(&output) {
            return Err(SourceError::new(format!(
                "manifest output `{}` is also an assembly input",
                output.display()
            )));
        }

        let mut manifest = String::from(
            "# glam local-file manifest v2\n# percent-encoded platform path bytes<TAB>digest algorithm<TAB>hex digest bytes\n",
        );
        for (source, digest) in observed.iter() {
            manifest.push_str(&percent_encoded_path(source));
            manifest.push('\t');
            manifest.push_str(digest.algorithm());
            manifest.push('\t');
            manifest.push_str(&digest.to_hex());
            manifest.push('\n');
        }
        fs::write(&output, manifest).map_err(|error| {
            SourceError::new(format!(
                "could not write manifest `{}`: {error}",
                output.display()
            ))
        })
    }
}

impl SourceSystem for FileSourceSystem {
    fn load_top_level(&self, path: &Path) -> Result<SourceArtifact, SourceError> {
        self.load_path(path)
    }
}

#[derive(Clone)]
struct FileImportResolver {
    system: FileSourceSystem,
    base: PathBuf,
}

impl ImportResolver for FileImportResolver {
    fn load_relative(&self, request: &RelativeSourcePath) -> Result<SourceArtifact, SourceError> {
        self.system.load_path(&self.base.join(request.as_str()))
    }
}

fn validate_relative_source_path(request: &str) -> Result<(), SourceError> {
    let invalid = |reason: &str| {
        Err(SourceError::new(format!(
            "local source request `{request}` {reason}; only child-relative `/`-separated paths are permitted"
        )))
    };

    if request.is_empty() {
        return invalid("is empty");
    }
    if request.starts_with('/') || request.starts_with('\\') {
        return invalid("must not be absolute");
    }
    if request.as_bytes().get(1) == Some(&b':') && request.as_bytes()[0].is_ascii_alphabetic() {
        return invalid("must not use an absolute drive path");
    }
    if request.contains('\\') {
        return invalid("must use `/` rather than platform-specific separators");
    }
    for component in request.split('/') {
        if component.is_empty() {
            return invalid("contains an empty path component");
        }
        if component == ".." {
            return invalid("must not traverse to a parent folder");
        }
        if component == "." || component.starts_with('.') {
            return invalid("must not use current-folder or dot-prefixed components");
        }
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, SourceError> {
    std::path::absolute(path).map_err(|error| {
        SourceError::new(format!(
            "could not make local path `{}` absolute: {error}",
            path.display()
        ))
    })
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
    fn artifact_digest_matches_exact_bytes() {
        let artifact = SourceArtifact::new(
            Bytes::from_static(b"source bytes"),
            SourceIdentity::new("memory", Value::record([("memory", Value::text("source"))])),
        );

        assert_eq!(
            artifact.digest().to_hex(),
            "4d4823794cbed3c4ee0bbc684c8f66e1dfd5afa6f078d494ce254ec5a4671753"
        );
        assert_eq!(artifact.digest_algorithm(), "sha256");
    }

    #[test]
    fn relative_paths_enforce_the_front_end_boundary() {
        assert!(RelativeSourcePath::new("lib/child.g").is_ok());
        for invalid in ["", "/root.g", "../parent.g", ".hidden", "a//b", "a\\b"] {
            assert!(RelativeSourcePath::new(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn file_system_detects_changes_after_a_read() {
        let directory =
            env::temp_dir().join(format!("glam-file-source-change-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let path = directory.join("input.g");
        fs::write(&path, "first").expect("test input should be written");
        let sources = FileSourceSystem::default();

        sources
            .load_top_level(&path)
            .expect("first read should succeed");
        fs::write(&path, "second").expect("test input should be changed");

        assert!(
            sources
                .verify_unchanged()
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        assert!(
            sources
                .load_top_level(&path)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
    }

    #[test]
    fn manifest_uses_the_digest_of_consumed_bytes() {
        let directory =
            env::temp_dir().join(format!("glam-file-source-manifest-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let input = directory.join("input.g");
        let manifest = directory.join("manifest.txt");
        fs::write(&input, "consumed").expect("test input should be written");
        let sources = FileSourceSystem::default();

        sources.load_top_level(&input).expect("source should load");
        fs::write(&input, "later edit").expect("source should be edited");
        sources
            .write_manifest(&manifest)
            .expect("manifest should be written from retained observations");

        let manifest = fs::read_to_string(manifest).expect("manifest should be readable");
        let consumed_digest = ContentDigest::of(b"consumed").to_hex();
        assert_eq!(
            manifest.lines().collect::<Vec<_>>(),
            [
                "# glam local-file manifest v2",
                "# percent-encoded platform path bytes<TAB>digest algorithm<TAB>hex digest bytes",
                &format!(
                    "{}\t{}\t{consumed_digest}",
                    percent_encoded_path(&input),
                    CONTENT_DIGEST_ALGORITHM
                ),
            ]
        );
        assert!(!manifest.contains(&ContentDigest::of(b"later edit").to_hex()));
    }
}

//! Source acquisition, immutable loaded artifacts, and local consistency.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use sha2::{Digest, Sha256};

use crate::api::Value;

pub const CONTENT_DIGEST_ALGORITHM: &str = "sha256";
const LOCAL_MANIFEST_HEADER: &str = "# glam local-file manifest v2";
const LOCAL_MANIFEST_COLUMNS: &str =
    "# percent-encoded platform path bytes<TAB>digest algorithm<TAB>hex digest bytes";

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

/// A local file that no longer agrees with a retained manifest entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestMismatch {
    path: PathBuf,
    expected: ContentDigest,
    observation: ManifestObservation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManifestObservation {
    Digest(ContentDigest),
    Unreadable(Arc<str>),
}

impl ManifestMismatch {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn expected_digest(&self) -> ContentDigest {
        self.expected
    }

    pub fn observed_digest(&self) -> Option<ContentDigest> {
        match self.observation {
            ManifestObservation::Digest(digest) => Some(digest),
            ManifestObservation::Unreadable(_) => None,
        }
    }

    pub fn read_error(&self) -> Option<&str> {
        match &self.observation {
            ManifestObservation::Digest(_) => None,
            ManifestObservation::Unreadable(error) => Some(error),
        }
    }
}

impl fmt::Display for ManifestMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "changed: `{}`", self.path.display())?;
        match &self.observation {
            ManifestObservation::Digest(actual) => write!(
                formatter,
                " (expected {}:{}, found {}:{})",
                self.expected.algorithm(),
                self.expected.to_hex(),
                actual.algorithm(),
                actual.to_hex()
            ),
            ManifestObservation::Unreadable(error) => {
                write!(formatter, " (could not read: {error})")
            }
        }
    }
}

/// Checks every local file recorded by a versioned Glam manifest.
///
/// Relative entries are resolved by the filesystem relative to the process's
/// current working directory, matching the directory used when writing them.
pub fn check_local_manifest(path: &Path) -> Result<Vec<ManifestMismatch>, SourceError> {
    let manifest = fs::read_to_string(path).map_err(|error| {
        SourceError::new(format!(
            "could not read manifest `{}`: {error}",
            path.display()
        ))
    })?;
    let mut lines = manifest.lines();
    match lines.next() {
        Some(LOCAL_MANIFEST_HEADER) => {}
        Some(header) => {
            return Err(SourceError::new(format!(
                "unsupported manifest header `{header}` in `{}`",
                path.display()
            )));
        }
        None => {
            return Err(SourceError::new(format!(
                "manifest `{}` is empty",
                path.display()
            )));
        }
    }

    let mut seen = BTreeSet::new();
    let mut mismatches = Vec::new();
    for (index, line) in lines.enumerate() {
        let line_number = index + 2;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let encoded_path = fields.next().unwrap_or_default();
        let algorithm = fields.next();
        let digest = fields.next();
        if encoded_path.is_empty()
            || algorithm.is_none()
            || digest.is_none()
            || fields.next().is_some()
        {
            return Err(manifest_line_error(
                path,
                line_number,
                "expected path, digest algorithm, and digest bytes separated by tabs",
            ));
        }
        let algorithm = algorithm.expect("checked above");
        if algorithm != CONTENT_DIGEST_ALGORITHM {
            return Err(manifest_line_error(
                path,
                line_number,
                &format!("unsupported digest algorithm `{algorithm}`"),
            ));
        }
        let expected = parse_digest_hex(digest.expect("checked above")).ok_or_else(|| {
            manifest_line_error(
                path,
                line_number,
                "SHA-256 digest must contain exactly 64 hexadecimal digits",
            )
        })?;
        let source = decode_manifest_path(encoded_path).map_err(|message| {
            manifest_line_error(path, line_number, &format!("invalid path: {message}"))
        })?;
        if !seen.insert(source.clone()) {
            return Err(manifest_line_error(
                path,
                line_number,
                &format!("duplicate path `{}`", source.display()),
            ));
        }

        let observation = match fs::read(&source) {
            Ok(bytes) => ManifestObservation::Digest(ContentDigest::of(&bytes)),
            Err(error) => ManifestObservation::Unreadable(Arc::from(error.to_string())),
        };
        if !matches!(observation, ManifestObservation::Digest(actual) if actual == expected) {
            mismatches.push(ManifestMismatch {
                path: source,
                expected,
                observation,
            });
        }
    }
    Ok(mismatches)
}

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

        let mut manifest = format!("{LOCAL_MANIFEST_HEADER}\n{LOCAL_MANIFEST_COLUMNS}\n");
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

fn manifest_line_error(manifest: &Path, line: usize, message: &str) -> SourceError {
    SourceError::new(format!(
        "invalid manifest `{}` at line {line}: {message}",
        manifest.display()
    ))
}

fn parse_digest_hex(text: &str) -> Option<ContentDigest> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut digest = [0; 32];
    for (byte, pair) in digest.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        *byte = (high << 4) | low;
    }
    Some(ContentDigest(digest))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_manifest_path(encoded: &str) -> Result<PathBuf, &'static str> {
    if !encoded.is_ascii() {
        return Err("path representation must be ASCII");
    }
    let encoded = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%' {
            let Some(pair) = encoded.get(index + 1..index + 3) else {
                return Err("incomplete percent escape");
            };
            let Some(high) = hex_digit(pair[0]) else {
                return Err("invalid percent escape");
            };
            let Some(low) = hex_digit(pair[1]) else {
                return Err("invalid percent escape");
            };
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            let byte = encoded[index];
            if !(byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':')) {
                return Err("unescaped character in path representation");
            }
            decoded.push(byte);
            index += 1;
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(decoded)))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(decoded)
            .map(PathBuf::from)
            .map_err(|_| "decoded path is not valid UTF-8 on this platform")
    }
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
        let working_directory = env::current_dir().expect("test should have a working directory");
        let directory = working_directory
            .join("target")
            .join(format!("glam-file-source-manifest-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let input = directory.join("input file.g");
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

        let mismatches = check_local_manifest(&directory.join("manifest.txt"))
            .expect("generated manifest should be accepted");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(
            mismatches[0].path(),
            input
                .strip_prefix(working_directory)
                .expect("manifest input should be relative to the working directory")
        );
        assert_eq!(
            mismatches[0].expected_digest(),
            ContentDigest::of(b"consumed")
        );
        assert_eq!(
            mismatches[0].observed_digest(),
            Some(ContentDigest::of(b"later edit"))
        );
        assert_eq!(mismatches[0].read_error(), None);
    }

    #[test]
    fn manifest_check_rejects_unknown_digest_algorithms() {
        let directory =
            env::temp_dir().join(format!("glam-manifest-algorithm-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("test directory should be created");
        let manifest = directory.join("manifest.txt");
        fs::write(
            &manifest,
            format!("{LOCAL_MANIFEST_HEADER}\nfile.g\tmd5\t{}\n", "0".repeat(64)),
        )
        .expect("test manifest should be written");

        let error = check_local_manifest(&manifest).expect_err("unknown algorithm should fail");
        assert!(
            error
                .to_string()
                .contains("unsupported digest algorithm `md5`")
        );
    }
}

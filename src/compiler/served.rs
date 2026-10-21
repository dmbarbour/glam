//! Sources a host serves (`imports-are-source-requests`,
//! `compile-yields-an-open-mixin`): their keys, the records a compile reads,
//! and the compiler the runtime hands to the compile builtin.

use std::sync::Arc;

use bytes::Bytes;

use crate::core::{
    Dict, EvaluationFailure, HostCallRecord, Key, LazyValue, List, OpaquePayloadFamily,
    OpaquePayloadRecord, RuntimeValueAccess, Value, keys,
};
use crate::runtime::RuntimeValueRoot;
use crate::source::RelativeSourcePath;

/// A source's key, `{package, path}`: an opaque package and a path of keys
/// from it. The assembler composes paths, and never learns how packages are
/// sourced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceKey {
    package: Key,
    path: Arc<[Key]>,
}

impl SourceKey {
    /// The source that `request` names from this one's folder: imports are
    /// child-relative, so the path drops this source's name and descends.
    pub(crate) fn child(&self, request: &RelativeSourcePath) -> Self {
        let folder = &self.path[..self.path.len().saturating_sub(1)];
        let path = folder
            .iter()
            .cloned()
            .chain(request.as_str().split('/').map(Key::binary_from_text))
            .collect::<Vec<_>>();
        Self {
            package: self.package.clone(),
            path: Arc::from(path),
        }
    }

    /// The key as data, for a source's identity, which reads it back as a
    /// value.
    pub(crate) fn key(&self) -> Key {
        Key::Dict(Arc::from([
            (keys::PACKAGE.clone(), self.package.clone()),
            (keys::PATH.clone(), Key::List(self.path.clone())),
        ]))
    }

    pub(crate) fn value_in(&self, access: &RuntimeValueAccess<'_>) -> Value {
        Value::Dict(
            Dict::new_sync()
                .insert(keys::PACKAGE.clone(), self.package.to_value_in(access))
                .insert(
                    keys::PATH.clone(),
                    Value::List(List::from_values(
                        self.path
                            .iter()
                            .map(|key| key.to_value_in(access))
                            .collect(),
                    )),
                ),
        )
    }

    /// A label for people: the path joined by `/`, under a folder package's
    /// own path.
    pub(crate) fn label(&self) -> String {
        let path = self
            .path
            .iter()
            .map(|key| match key {
                Key::Binary(text) => String::from_utf8_lossy(text).into_owned(),
                key => format!("{key:?}"),
            })
            .collect::<Vec<_>>()
            .join("/");
        match entry(&self.package, &keys::FOLDER) {
            Some(Key::Binary(folder)) => format!("{}/{path}", String::from_utf8_lossy(folder)),
            _ => path,
        }
    }

    /// The text of the path's last key after its last `.`, which names the
    /// language of the source.
    pub(crate) fn extension(&self) -> Option<&str> {
        match self.path.last()? {
            Key::Binary(name) => extension(std::str::from_utf8(name).ok()?),
            _ => None,
        }
    }

    fn from_key(key: &Key) -> Result<Self, String> {
        let package = entry(key, &keys::PACKAGE)
            .ok_or("a source's key names its package, as `{package, path}`")?;
        let Some(Key::List(path)) = entry(key, &keys::PATH) else {
            return Err(
                "a source's key names its path, a list of keys, as `{package, path}`".into(),
            );
        };
        if path.is_empty() {
            return Err("a source's path is not empty".into());
        }
        Ok(Self {
            package: package.clone(),
            path: path.clone(),
        })
    }
}

/// The text of `name` after its last `.`, where there is one.
pub(crate) fn extension(name: &str) -> Option<&str> {
    let name = name.rsplit('/').next()?;
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.is_empty()).then_some(extension)
}

/// What a client supplies to a compile and a compiler never sees: where the
/// source sits, for its relative imports, absent for a script, and the
/// namespace that seeds its global paths.
#[derive(Clone, Debug)]
pub(crate) struct SourceScope {
    pub(crate) location: Option<SourceKey>,
    pub(crate) namespace: Arc<[String]>,
}

/// A compile's record, `{scope, lang, source}`, as data.
#[derive(Clone, Debug)]
pub(crate) struct SourceRecord {
    pub(crate) scope: SourceScope,
    pub(crate) lang: String,
    pub(crate) source: Bytes,
}

impl SourceRecord {
    /// Reads a record that evaluation converted to a key.
    pub(crate) fn from_key(key: &Key) -> Result<Self, String> {
        let shape = "a compile reads a record `{scope, lang, source}`";
        let scope = entry(key, &keys::SCOPE).ok_or(shape)?;
        let location = entry(scope, &keys::LOCATION)
            .map(SourceKey::from_key)
            .transpose()?;
        let namespace = match entry(scope, &keys::NAMESPACE) {
            None => Vec::new(),
            Some(Key::List(names)) => names
                .iter()
                .map(|name| match name {
                    Key::Binary(text) => String::from_utf8(text.to_vec())
                        .map_err(|_| "a namespace's names are UTF-8 texts".to_owned()),
                    _ => Err("a namespace is a list of texts".to_owned()),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("a namespace is a list of texts".into()),
        };
        let Some(Key::Binary(lang)) = entry(key, &keys::LANG) else {
            return Err(format!("{shape}, whose `lang` is a text"));
        };
        let lang = String::from_utf8(lang.to_vec())
            .map_err(|_| "a compile's `lang` is a UTF-8 text".to_owned())?;
        let Some(Key::Binary(source)) = entry(key, &keys::SOURCE) else {
            return Err(format!("{shape}, whose `source` is a binary"));
        };
        Ok(Self {
            scope: SourceScope {
                location,
                namespace: Arc::from(namespace),
            },
            lang,
            source: source.clone(),
        })
    }
}

/// The value of `key`'s entry `name`, where `key` is a dict.
fn entry<'a>(key: &'a Key, name: &Key) -> Option<&'a Key> {
    let Key::Dict(entries) = key else {
        return None;
    };
    entries
        .iter()
        .find(|(entry, _)| entry == name)
        .map(|(_, value)| value)
}

/// The record `{scope, lang, source}` for a compile, its location and
/// source as given.
pub(crate) fn source_record_in(
    _access: &RuntimeValueAccess<'_>,
    location: Option<Value>,
    namespace: &[String],
    lang: &str,
    source: Value,
) -> Value {
    let mut scope = Dict::new_sync().insert(
        keys::NAMESPACE.clone(),
        Value::List(List::from_values(
            namespace
                .iter()
                .map(|name| Value::binary_from_text(name))
                .collect(),
        )),
    );
    if let Some(location) = location {
        scope = scope.insert(keys::LOCATION.clone(), location);
    }
    Value::Dict(
        Dict::new_sync()
            .insert(keys::SCOPE.clone(), Value::Dict(scope))
            .insert(keys::LANG.clone(), Value::binary_from_text(lang))
            .insert(keys::SOURCE.clone(), source),
    )
}

/// Compiles a record's source over a base and a self, returning the
/// module's definitions.
pub(crate) type SourceCompile = dyn Fn(
        SourceRecord,
        RuntimeValueRoot,
        RuntimeValueRoot,
    ) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>>
    + Send
    + Sync;

/// The runtime's compiler, which the compile builtin takes as its first
/// argument, an opaque value. One serves a client's compile or one import
/// site, whose provenance it carries.
pub(crate) struct SourceCompiler(pub(crate) Arc<SourceCompile>);

// SAFETY: the payload is a host closure over the assembler's deferred route,
// a compilation trace, and a weak compilation execution. It holds no Glam
// value, runtime root or managed pointer: the base and self it compiles over
// arrive as roots per call, and the runtime's external-owner registry
// retires it.
unsafe impl OpaquePayloadFamily for SourceCompiler {
    const PAYLOAD_RECORD: OpaquePayloadRecord =
        OpaquePayloadRecord::external("source compiler", "src/compiler/served.rs");
}

/// Writes a request for the source a key names into the root's `imports`
/// input, returning its reply's promise.
pub(crate) type SourceRequester =
    Arc<dyn Fn(RuntimeValueRoot) -> Result<RuntimeValueRoot, Arc<EvaluationFailure>> + Send + Sync>;

/// A lazy binary: the source that `key` names, asked for when forced.
pub(crate) fn import_source_in(
    access: &RuntimeValueAccess<'_>,
    requester: SourceRequester,
    key: Value,
) -> Value {
    Value::Lazy(LazyValue::external_host_call_in(
        access,
        HostCallRecord::external_with_semantic_values(
            "source import",
            "src/compiler/served.rs",
            "the runtime's request route plus the source's key",
        ),
        [key],
        move |captures| {
            let mut roots = captures.into_roots().into_vec().into_iter();
            let (Some(key), None) = (roots.next(), roots.next()) else {
                panic!("a source import receives exactly its key");
            };
            requester(key)
        },
    ))
}

/// Where a compile's imports come from: its importer's location, a compiler
/// for each import site, and the route for requests.
#[derive(Clone)]
pub(crate) struct ServedImports {
    pub(crate) location: Option<SourceKey>,
    pub(crate) compilers: ImportCompilers,
    pub(crate) requester: SourceRequester,
}

/// One import site's provenance: the request as written, and the namespace
/// it extends.
pub(crate) struct ImportSite {
    pub(crate) request: Arc<str>,
    pub(crate) extends: Arc<[String]>,
}

/// Makes the compiler for one import site.
pub(crate) type ImportCompilers = Arc<dyn Fn(ImportSite) -> SourceCompiler + Send + Sync>;

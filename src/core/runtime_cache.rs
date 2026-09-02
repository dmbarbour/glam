//! Reviewed admission for type-erased runtime value-cache attachments.
//!
//! `TypeId` keeps optional compiler layers independent from `core`, but it is
//! not itself an ownership proof. Every cached family therefore supplies one
//! stable record and enumerates all runtime roots retained by its completed
//! payload before that payload can cross the type-erased boundary.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::runtime::{EvaluationRuntimeId, RuntimeValueRoot};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeCacheRootPolicy {
    /// The family retains no Glam value or runtime capability.
    ValueFree,
    /// Every retained Glam value is reported as a `RuntimeValueRoot`.
    SameRuntimeRoots,
}

/// The reviewed containment policy for one type-erased cache family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeCacheFamilyRecord {
    family: &'static str,
    source: &'static str,
    roots: RuntimeCacheRootPolicy,
}

impl RuntimeCacheFamilyRecord {
    #[allow(
        dead_code,
        reason = "the initial production cache families retain roots; value-free admission is exercised by the boundary fixture"
    )]
    pub(crate) const fn value_free(family: &'static str, source: &'static str) -> Self {
        Self::reviewed(family, source, RuntimeCacheRootPolicy::ValueFree)
    }

    pub(crate) const fn same_runtime_roots(family: &'static str, source: &'static str) -> Self {
        Self::reviewed(family, source, RuntimeCacheRootPolicy::SameRuntimeRoots)
    }

    const fn reviewed(
        family: &'static str,
        source: &'static str,
        roots: RuntimeCacheRootPolicy,
    ) -> Self {
        assert!(!family.is_empty(), "runtime cache family must be recorded");
        assert!(
            !source.is_empty(),
            "runtime cache family source must be recorded"
        );
        Self {
            family,
            source,
            roots,
        }
    }

    #[cfg(test)]
    pub(crate) const fn fields(self) -> (&'static str, &'static str, RuntimeCacheRootPolicy) {
        (self.family, self.source, self.roots)
    }
}

/// Admits one reviewed family to the runtime's type-erased value cache.
///
/// # Safety
///
/// `visit_runtime_roots` must report every `RuntimeValueRoot` retained by the
/// completed value, including roots behind synchronized containers. A
/// `ValueFree` family must retain no Glam value, value-domain service, managed
/// pointer, or active runtime capability. Any later mutation of a rooted
/// family must preserve the same-runtime condition through an equally strict
/// family-owned gateway.
pub(crate) unsafe trait RuntimeCacheFamily: Any + Send + Sync {
    const CACHE_RECORD: RuntimeCacheFamilyRecord;

    fn visit_runtime_roots(&self, visit: &mut dyn FnMut(&RuntimeValueRoot));
}

/// One admitted value plus the private type erasure used by both cache tiers.
pub(super) struct RuntimeCacheEntry {
    type_id: TypeId,
    record: RuntimeCacheFamilyRecord,
    value: Box<dyn Any + Send + Sync>,
}

impl RuntimeCacheEntry {
    pub(super) fn admit<T>(runtime: EvaluationRuntimeId, value: Arc<T>) -> Self
    where
        T: RuntimeCacheFamily,
    {
        let record = T::CACHE_RECORD;
        let mut reported_roots = 0usize;
        value.visit_runtime_roots(&mut |root| {
            reported_roots += 1;
            assert_eq!(
                root.runtime_id(),
                runtime,
                "runtime cache family `{}` from {} retained a root from another runtime",
                record.family,
                record.source,
            );
        });
        assert!(
            record.roots != RuntimeCacheRootPolicy::ValueFree || reported_roots == 0,
            "value-free runtime cache family `{}` reported a runtime root",
            record.family,
        );

        Self {
            type_id: TypeId::of::<T>(),
            record,
            value: Box::new(value),
        }
    }

    pub(super) fn get<T>(&self) -> Arc<T>
    where
        T: RuntimeCacheFamily,
    {
        assert_eq!(
            self.type_id,
            TypeId::of::<T>(),
            "one runtime cache TypeId must name one concrete family"
        );
        assert_eq!(
            self.record,
            T::CACHE_RECORD,
            "one runtime cache family must retain one stable admission record"
        );
        self.value
            .downcast_ref::<Arc<T>>()
            .expect("an admitted runtime cache entry must retain its recorded family")
            .clone()
    }
}

pub(super) type RuntimeCacheMap = HashMap<TypeId, Arc<RuntimeCacheEntry>>;
pub(super) type SharedRuntimeCacheMap = Arc<Mutex<RuntimeCacheMap>>;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use syn::visit::{self, Visit};
    use syn::{ItemImpl, Type};

    #[derive(Default)]
    struct FamilyVisitor {
        names: BTreeSet<String>,
    }

    impl<'ast> Visit<'ast> for FamilyVisitor {
        fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
            let is_cache_family = item
                .trait_
                .as_ref()
                .and_then(|(_, path, _)| path.segments.last())
                .is_some_and(|segment| segment.ident == "RuntimeCacheFamily");
            if is_cache_family {
                let Type::Path(path) = item.self_ty.as_ref() else {
                    panic!("runtime cache admission must name a concrete path type");
                };
                let family = path
                    .path
                    .segments
                    .last()
                    .expect("runtime cache family path must not be empty")
                    .ident
                    .to_string();
                assert!(
                    self.names.insert(family.clone()),
                    "runtime cache family `{family}` is admitted more than once"
                );
            }
            visit::visit_item_impl(self, item);
        }
    }

    fn rust_sources(path: &Path, output: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(path).expect("source directory should be readable") {
            let path = entry.expect("source entry should be readable").path();
            if path.is_dir() {
                rust_sources(&path, output);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                output.push(path);
            }
        }
    }

    #[test]
    fn runtime_cache_family_source_inventory_is_complete() {
        let mut sources = Vec::new();
        rust_sources(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut sources,
        );
        sources.sort();

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut actual = BTreeSet::new();
        for source in sources {
            let syntax = syn::parse_file(
                &fs::read_to_string(&source).expect("Rust source should be readable"),
            )
            .unwrap_or_else(|error| panic!("{} did not parse: {error}", source.display()));
            let mut visitor = FamilyVisitor::default();
            visitor.visit_file(&syntax);
            let relative = source
                .strip_prefix(manifest)
                .expect("a discovered source should belong to this package")
                .to_str()
                .expect("repository source paths should be UTF-8")
                .replace('\\', "/");
            actual.extend(
                visitor
                    .names
                    .into_iter()
                    .map(|family| format!("{relative}::{family}")),
            );
        }

        let expected = BTreeSet::from([
            "src/core.rs::CachedProbe".to_owned(),
            "src/core.rs::RootedCachedProbe".to_owned(),
            "src/g_syntax/compiler_values.rs::GCompilerValues".to_owned(),
            "src/g_syntax/diagnostic_formatter.rs::CachedDiagnosticFormatter".to_owned(),
        ]);
        assert_eq!(
            actual, expected,
            "every type-erased runtime cache family requires an ownership review and source-inventory update"
        );
    }
}

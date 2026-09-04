//! Phase I5A source-backed inventory for recursive semantic identities.
//!
//! The inventory deliberately examines stored production declarations rather
//! than expression-local uses. A declaration enters the candidate set when it
//! defines one of the three recursive identity families or directly stores a
//! family-specific handle. General `Value` and `RuntimeValueRoot` ownership is
//! already latched by `durable_owner_inventory`; I5A classifies the places
//! where the current representation exposes the recursive identity directly.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Attribute, Fields, GenericParam, Generics, Item};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetDisposition {
    ExactManagedEdge,
    DurableRoot,
    BoundedAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
enum CycleSource {
    Lazy,
    Promise,
    CoreNet,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd, Ord)]
struct IdentitySignals {
    lazy: usize,
    promise: usize,
    core_net: usize,
}

#[derive(Clone, Copy, Debug)]
struct IdentityOwnerEntry {
    declaration: &'static str,
    signals: IdentitySignals,
    target: TargetDisposition,
    source: Option<CycleSource>,
    reason: &'static str,
}

macro_rules! owner {
    ($declaration:literal, [$lazy:literal, $promise:literal, $core_net:literal], $target:ident, $source:expr, $reason:literal) => {
        IdentityOwnerEntry {
            declaration: $declaration,
            signals: IdentitySignals::new($lazy, $promise, $core_net),
            target: TargetDisposition::$target,
            source: $source,
            reason: $reason,
        }
    };
}

impl IdentitySignals {
    const fn new(lazy: usize, promise: usize, core_net: usize) -> Self {
        Self {
            lazy,
            promise,
            core_net,
        }
    }

    fn is_empty(self) -> bool {
        self == Self::default()
    }
}

#[derive(Default)]
struct IdentityVisitor {
    signals: IdentitySignals,
}

impl<'ast> Visit<'ast> for IdentityVisitor {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(identifier) = path.segments.last().map(|segment| &segment.ident) {
            match identifier.to_string().as_str() {
                "LazyValue" | "LazyCell" => self.signals.lazy += 1,
                "PromisedValue" | "PromiseCell" => self.signals.promise += 1,
                "CoreRuntimeNet" => self.signals.core_net += 1,
                _ => {}
            }
        }
        visit::visit_path(self, path);
    }
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string().contains("test"))
    })
}

fn item_is_test_only(item: &Item) -> bool {
    let attributes = match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        _ => return false,
    };
    is_test_only(attributes)
}

fn visit_generics(visitor: &mut IdentityVisitor, generics: &Generics) {
    for parameter in &generics.params {
        match parameter {
            GenericParam::Type(parameter) => {
                for bound in &parameter.bounds {
                    visitor.visit_type_param_bound(bound);
                }
                if let Some(default) = &parameter.default {
                    visitor.visit_type(default);
                }
            }
            GenericParam::Const(parameter) => visitor.visit_type(&parameter.ty),
            GenericParam::Lifetime(_) => {}
        }
    }
    if let Some(where_clause) = &generics.where_clause {
        visitor.visit_where_clause(where_clause);
    }
}

fn visit_fields(visitor: &mut IdentityVisitor, fields: &Fields) {
    for field in fields {
        if !is_test_only(&field.attrs) {
            visitor.visit_type(&field.ty);
        }
    }
}

fn defined_identity(name: &str) -> IdentitySignals {
    match name {
        "LazyValue" | "LazyCell" => IdentitySignals::new(1, 0, 0),
        "PromisedValue" | "PromiseCell" => IdentitySignals::new(0, 1, 0),
        "CoreRuntimeNet" => IdentitySignals::new(0, 0, 1),
        _ => IdentitySignals::default(),
    }
}

fn add_signals(target: &mut IdentitySignals, additional: IdentitySignals) {
    target.lazy += additional.lazy;
    target.promise += additional.promise;
    target.core_net += additional.core_net;
}

fn collect_items(
    relative: &Path,
    prefix: &str,
    items: &[Item],
    declarations: &mut BTreeMap<String, IdentitySignals>,
) {
    for item in items {
        if item_is_test_only(item) {
            continue;
        }

        let (name, signals) = match item {
            Item::Struct(item) => {
                let mut visitor = IdentityVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                visit_fields(&mut visitor, &item.fields);
                add_signals(
                    &mut visitor.signals,
                    defined_identity(&item.ident.to_string()),
                );
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Enum(item) => {
                let mut visitor = IdentityVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                for variant in &item.variants {
                    if !is_test_only(&variant.attrs) {
                        visit_fields(&mut visitor, &variant.fields);
                    }
                }
                add_signals(
                    &mut visitor.signals,
                    defined_identity(&item.ident.to_string()),
                );
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Union(item) => {
                let mut visitor = IdentityVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                for field in &item.fields.named {
                    visitor.visit_type(&field.ty);
                }
                add_signals(
                    &mut visitor.signals,
                    defined_identity(&item.ident.to_string()),
                );
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Type(item) => {
                let mut visitor = IdentityVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                visitor.visit_type(&item.ty);
                add_signals(
                    &mut visitor.signals,
                    defined_identity(&item.ident.to_string()),
                );
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Static(item) => {
                let mut visitor = IdentityVisitor::default();
                visitor.visit_type(&item.ty);
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Const(item) => {
                let mut visitor = IdentityVisitor::default();
                visitor.visit_type(&item.ty);
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Mod(item) => {
                if let Some((_, items)) = &item.content {
                    let nested_prefix = if prefix.is_empty() {
                        item.ident.to_string()
                    } else {
                        format!("{prefix}::{}", item.ident)
                    };
                    collect_items(relative, &nested_prefix, items, declarations);
                }
                (None, IdentitySignals::default())
            }
            _ => (None, IdentitySignals::default()),
        };

        if let Some(name) = name
            && !signals.is_empty()
        {
            let item_name = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}::{name}")
            };
            let key = format!("{}::{item_name}", relative.display());
            assert!(
                declarations.insert(key.clone(), signals).is_none(),
                "duplicate recursive-identity declaration {key}"
            );
        }
    }
}

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the source tree should be readable") {
        let path = entry.expect("a source entry should be readable").path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

fn is_production_source(relative: &Path) -> bool {
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return false;
    }
    if relative
        .file_name()
        .is_some_and(|name| name == "tests.rs" || name == "test_support.rs")
    {
        return false;
    }
    relative != Path::new("src/core/managed/recursive_identity_inventory.rs")
}

fn source_inventory() -> BTreeMap<String, IdentitySignals> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    sources.sort();

    let mut declarations = BTreeMap::new();
    for source_path in sources {
        let relative = source_path
            .strip_prefix(manifest)
            .expect("a discovered source should belong to this package");
        if !is_production_source(relative) {
            continue;
        }
        let source =
            fs::read_to_string(&source_path).expect("an inventoried Rust source should be UTF-8");
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} should parse as Rust: {error}", relative.display()));
        collect_items(relative, "", &syntax.items, &mut declarations);
    }
    declarations
}

// Captured from the deliberately failing first I5A scan, then assigned a
// semantic disposition from the I5F-003 M/R/A/C review. New declarations fail
// closed instead of inheriting a role from a path-name heuristic.
const DIRECT_IDENTITY_INVENTORY: &[IdentityOwnerEntry] = &[
    owner!(
        "src/api/value.rs::PromiseResolver",
        [0, 1, 0],
        DurableRoot,
        None,
        "public affine completion capability survives evaluator access"
    ),
    owner!(
        "src/core.rs::FunctionCode",
        [0, 0, 1],
        ExactManagedEdge,
        None,
        "function code semantically refers to its runtime net"
    ),
    owner!(
        "src/core.rs::LazyCell",
        [1, 0, 0],
        ExactManagedEdge,
        Some(CycleSource::Lazy),
        "authoritative mutable lazy source and terminal cache"
    ),
    owner!(
        "src/core.rs::LazyValue",
        [2, 0, 0],
        ExactManagedEdge,
        None,
        "semantic lazy identity handle"
    ),
    owner!(
        "src/core.rs::ListThunk",
        [1, 1, 0],
        ExactManagedEdge,
        None,
        "persistent-list semantic edge to lazy or promised content"
    ),
    owner!(
        "src/core.rs::NetValue",
        [0, 0, 1],
        ExactManagedEdge,
        None,
        "semantic net value edge"
    ),
    owner!(
        "src/core.rs::PromiseCell",
        [0, 1, 0],
        ExactManagedEdge,
        Some(CycleSource::Promise),
        "authoritative mutable promise assignment"
    ),
    owner!(
        "src/core.rs::PromisedValue",
        [0, 2, 0],
        ExactManagedEdge,
        None,
        "semantic promise identity handle"
    ),
    owner!(
        "src/core.rs::Value",
        [1, 1, 0],
        ExactManagedEdge,
        None,
        "recursive semantic value variants"
    ),
    owner!(
        "src/core/evaluation_halt.rs::EvaluationHaltKind",
        [0, 1, 0],
        ExactManagedEdge,
        None,
        "retryable halt refers to the promise being observed"
    ),
    owner!(
        "src/core/evaluation_halt.rs::EvaluationHaltPayload",
        [0, 1, 0],
        ExactManagedEdge,
        None,
        "borrowed halt payload exposes the same semantic promise edge"
    ),
    owner!(
        "src/core_net.rs::CoreRuntimeNet",
        [0, 0, 1],
        ExactManagedEdge,
        Some(CycleSource::CoreNet),
        "authoritative mutable interaction-net topology and payload identity"
    ),
    owner!(
        "src/core_net.rs::CoreRuntimeNetAccess",
        [0, 0, 1],
        BoundedAccess,
        None,
        "access view is branded by one matching runtime value-access scope"
    ),
    owner!(
        "src/core_net.rs::CoreRuntimeNetPayload",
        [0, 0, 1],
        BoundedAccess,
        None,
        "synchronous payload visitor projection"
    ),
    owner!(
        "src/eval/net.rs::CoreCallClaim",
        [0, 0, 1],
        BoundedAccess,
        None,
        "claim cannot escape its evaluator and net-access scopes"
    ),
    owner!(
        "src/eval/net.rs::CoreCallable",
        [0, 0, 1],
        BoundedAccess,
        None,
        "callable classification is consumed within one semantic net step"
    ),
    owner!(
        "src/eval/net.rs::CoreOperatorClaim",
        [0, 0, 1],
        BoundedAccess,
        None,
        "operator claim cannot escape its evaluator and net-access scopes"
    ),
    owner!(
        "src/eval/net.rs::NetBatchOutcome",
        [0, 0, 1],
        BoundedAccess,
        None,
        "batch outcome is consumed before the evaluator quantum exits"
    ),
    owner!(
        "src/eval/net.rs::NetDriverWork",
        [0, 0, 4],
        BoundedAccess,
        None,
        "reconstructible normalization worklist is local to one drive"
    ),
    owner!(
        "src/eval/net.rs::NormalizationRequest",
        [0, 0, 1],
        BoundedAccess,
        None,
        "normalization request is retried from rooted enclosing evaluator state"
    ),
    owner!(
        "src/eval/value.rs::LazyTaskMachine",
        [1, 0, 0],
        DurableRoot,
        None,
        "parked producer machine must publish the lazy terminal cache"
    ),
    owner!(
        "src/eval/value.rs::PromiseFollower",
        [0, 1, 0],
        DurableRoot,
        None,
        "parked follower actively observes eventual assignment"
    ),
    owner!(
        "src/evaluation/coordinator.rs::TaskOwnedPromiseObligation",
        [0, 1, 0],
        DurableRoot,
        None,
        "producer obligation keeps each unresolved task promise assignable"
    ),
    owner!(
        "src/evaluation/coordinator.rs::WorkDependency",
        [0, 1, 0],
        DurableRoot,
        None,
        "parked dependency actively observes and subscribes to assignment"
    ),
    owner!(
        "src/evaluation/coordinator/deferred.rs::DeferredLazyCycleMember",
        [1, 0, 0],
        DurableRoot,
        None,
        "deferred cycle completion must publish to every member"
    ),
    owner!(
        "src/evaluation/coordinator/deferred.rs::DeferredProducer",
        [1, 1, 0],
        DurableRoot,
        None,
        "coordinator-held deferred producer survives a mutator scope"
    ),
    owner!(
        "src/evaluation/coordinator/task.rs::LocalPromiseObligation",
        [0, 1, 0],
        DurableRoot,
        None,
        "direct-runner producer obligation keeps each unresolved promise assignable"
    ),
    owner!(
        "src/reflection/machine.rs::ActiveFix",
        [0, 1, 0],
        DurableRoot,
        None,
        "parked reflection fixpoint machine retains its result promise"
    ),
    owner!(
        "src/reflection/machine.rs::Continuation",
        [0, 1, 0],
        DurableRoot,
        None,
        "continuation may retain a fixpoint promise across effect steps"
    ),
];

#[test]
fn recursive_identity_source_inventory_is_complete() {
    let actual = source_inventory();
    let expected = DIRECT_IDENTITY_INVENTORY
        .iter()
        .map(|entry| (entry.declaration.to_owned(), entry.signals))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual, expected,
        "a direct recursive-identity declaration requires an I5 ownership review"
    );
}

#[test]
fn compatibility_graph_cycle_sources_are_classified() {
    let mut sources = BTreeMap::new();
    for entry in DIRECT_IDENTITY_INVENTORY {
        assert!(
            !entry.reason.is_empty(),
            "{} has no semantic disposition reason",
            entry.declaration
        );
        if let Some(source) = entry.source {
            assert_eq!(
                entry.target,
                TargetDisposition::ExactManagedEdge,
                "cycle source {} must become an exact managed identity",
                entry.declaration
            );
            assert!(
                sources.insert(source, entry.declaration).is_none(),
                "one recursive family must have one authoritative cycle source"
            );
        }
    }

    assert_eq!(
        sources,
        BTreeMap::from([
            (CycleSource::Lazy, "src/core.rs::LazyCell"),
            (CycleSource::Promise, "src/core.rs::PromiseCell"),
            (CycleSource::CoreNet, "src/core_net.rs::CoreRuntimeNet"),
        ]),
        "removing the three authoritative cells must leave only classified compatibility paths"
    );

    let counts = DIRECT_IDENTITY_INVENTORY
        .iter()
        .fold([0usize; 3], |mut counts, entry| {
            let index = match entry.target {
                TargetDisposition::ExactManagedEdge => 0,
                TargetDisposition::DurableRoot => 1,
                TargetDisposition::BoundedAccess => 2,
            };
            counts[index] += 1;
            counts
        });
    assert_eq!(
        counts,
        [11, 10, 8],
        "every direct identity occurrence remains assigned to the reviewed M/R/A split"
    );
}

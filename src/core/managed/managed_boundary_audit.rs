//! Negative rules for the managed graph's boundaries, and runtime tests of the
//! same boundaries.
//!
//! The collector finalizes managed allocations and traces only the edges each
//! family reports. Three shapes would break that contract and still compile,
//! so the rules below reject them:
//!
//! - a managed-graph declaration that holds value-domain authority or a
//!   registered root, which would keep its own runtime or allocation alive;
//! - a managed-graph declaration that implements or holds an active `Drop`,
//!   which collector finalization would then run;
//! - an opaque payload that reaches `OpaqueValue` without the unsafe
//!   `OpaquePayloadFamily` admission.
//!
//! The rules read production declarations with `syn` and report each finding
//! by module path and item name. Production code is the module tree reachable
//! from `src/lib.rs` through `mod` declarations that are not test-only. The
//! rules hold no counts or fingerprints. An exception names one item and says
//! why it is safe, and an exception that no longer matches anything fails.
//!
//! The rules look one declaration deep: they inspect the stored field types of
//! each managed-graph declaration, not the types those fields contain.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, Field, Item, Meta, Token, Type};

use crate::core::{
    Builtin, ClosedCompatibilityValue, Dict, EvaluationFailure, EvaluationHalt, FunctionValue,
    HostCallRecord, LazySource, LazyValue, List, NetValue, OpaquePayloadFamily,
    OpaquePayloadRecord, OpaqueValue, PromisedValue, Value, set_test_promise,
};
use crate::core_net::CoreSpecialization;
use crate::evaluation::EvaluatorStepContext;
use crate::interaction_net::NetBuilder;
use crate::runtime::RuntimeValueRoot;

// ---------------------------------------------------------------------------
// Production source index
// ---------------------------------------------------------------------------

/// One production source file, with its module path (`""` at the crate root).
pub(super) struct ProductionSource {
    pub(super) module: String,
    pub(super) path: PathBuf,
    pub(super) text: String,
}

/// A production item, keyed by module path and name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ItemKey {
    module: String,
    name: String,
}

impl fmt::Display for ItemKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.module.is_empty() {
            write!(formatter, "crate::{}", self.name)
        } else {
            write!(formatter, "{}::{}", self.module, self.name)
        }
    }
}

/// Facts the rules need, extracted in one parse of the production tree.
#[derive(Default)]
struct ProductionIndex {
    sources: Vec<ProductionSource>,
    /// Every name in the stored field types of each struct, enum, or union.
    declarations: BTreeMap<ItemKey, BTreeSet<String>>,
    /// Types with a production `Drop` implementation, by simple name, with
    /// the module of each implementation.
    drop_types: BTreeMap<String, BTreeSet<String>>,
    /// Types with a production `ManagedFamily` admission, by simple name.
    managed_families: BTreeSet<String>,
}

fn production_index() -> &'static ProductionIndex {
    static INDEX: OnceLock<ProductionIndex> = OnceLock::new();
    INDEX.get_or_init(build_production_index)
}

/// Production source files in path order. Shared by every source rule so the
/// tree is discovered and read once per test process.
pub(super) fn production_sources() -> &'static [ProductionSource] {
    &production_index().sources
}

/// Walks the module tree from `src/lib.rs`, skipping test-only modules and
/// items.
fn build_production_index() -> ProductionIndex {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut index = ProductionIndex::default();
    let mut files = vec![(String::new(), PathBuf::from("src/lib.rs"))];
    while let Some((module, path)) = files.pop() {
        let text = fs::read_to_string(manifest.join(&path))
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
        let syntax = syn::parse_file(&text)
            .unwrap_or_else(|error| panic!("{} should parse: {error}", path.display()));
        let file_directory = path
            .parent()
            .expect("a source file has a directory")
            .to_path_buf();
        let mut scopes = vec![(
            module.clone(),
            child_module_directory(&path),
            true,
            syntax.items.as_slice(),
        )];
        while let Some((scope, directory, file_root, items)) = scopes.pop() {
            for item in items {
                if is_test_only(item_attributes(item)) {
                    continue;
                }
                match item {
                    Item::Mod(declared) => {
                        let name = declared.ident.to_string();
                        let child = join_module(&scope, &name);
                        match &declared.content {
                            Some((_, nested)) => {
                                scopes.push((child, directory.join(&name), false, nested));
                            }
                            None => {
                                let base = if file_root {
                                    &file_directory
                                } else {
                                    &directory
                                };
                                let file =
                                    module_file(manifest, base, &directory, &name, &declared.attrs);
                                files.push((child, file));
                            }
                        }
                    }
                    Item::Struct(declared) => {
                        index.declare(&scope, &declared.ident, declared.fields.iter());
                    }
                    Item::Enum(declared) => {
                        let fields = declared
                            .variants
                            .iter()
                            .filter(|variant| !is_test_only(&variant.attrs))
                            .flat_map(|variant| variant.fields.iter());
                        index.declare(&scope, &declared.ident, fields);
                    }
                    Item::Union(declared) => {
                        index.declare(&scope, &declared.ident, declared.fields.named.iter());
                    }
                    Item::Impl(implementation) => {
                        let Some((_, implemented, _)) = &implementation.trait_ else {
                            continue;
                        };
                        let Some(target) = type_name(&implementation.self_ty) else {
                            continue;
                        };
                        match implemented.segments.last().map(|segment| &segment.ident) {
                            Some(name) if name == "Drop" => {
                                index
                                    .drop_types
                                    .entry(target)
                                    .or_default()
                                    .insert(scope.clone());
                            }
                            Some(name) if name == "ManagedFamily" => {
                                index.managed_families.insert(target);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        index.sources.push(ProductionSource { module, path, text });
    }
    index
        .sources
        .sort_by(|left, right| left.path.cmp(&right.path));
    index
}

impl ProductionIndex {
    fn declare<'field>(
        &mut self,
        module: &str,
        name: &syn::Ident,
        fields: impl Iterator<Item = &'field Field>,
    ) {
        let mut names = NameCollector::default();
        for field in fields.filter(|field| !is_test_only(&field.attrs)) {
            names.visit_type(&field.ty);
        }
        // Mutually exclusive `cfg` variants of one declaration share a key.
        self.declarations
            .entry(ItemKey {
                module: module.to_owned(),
                name: name.to_string(),
            })
            .or_default()
            .extend(names.names);
    }

    fn source(&self, module: &str) -> &ProductionSource {
        self.sources
            .iter()
            .find(|source| source.module == module)
            .unwrap_or_else(|| panic!("production module {module} should exist"))
    }
}

#[derive(Default)]
struct NameCollector {
    names: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for NameCollector {
    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        self.names.insert(segment.ident.to_string());
        visit::visit_path_segment(self, segment);
    }
}

fn join_module(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}::{child}")
    }
}

/// The directory holding the files of a source file's child modules.
fn child_module_directory(path: &Path) -> PathBuf {
    let directory = path.parent().expect("a source file has a directory");
    match path.file_name().and_then(|name| name.to_str()) {
        Some("lib.rs" | "main.rs" | "mod.rs") => directory.to_path_buf(),
        _ => directory.join(path.file_stem().expect("a source file has a file stem")),
    }
}

fn module_file(
    manifest: &Path,
    path_base: &Path,
    directory: &Path,
    name: &str,
    attributes: &[Attribute],
) -> PathBuf {
    for attribute in attributes {
        if attribute.path().is_ident("path")
            && let Meta::NameValue(value) = &attribute.meta
            && let syn::Expr::Lit(literal) = &value.value
            && let syn::Lit::Str(relative) = &literal.lit
        {
            return path_base.join(relative.value());
        }
    }
    let flat = directory.join(format!("{name}.rs"));
    if manifest.join(&flat).is_file() {
        return flat;
    }
    let nested = directory.join(name).join("mod.rs");
    assert!(
        manifest.join(&nested).is_file(),
        "module {name} should have a file in {}",
        directory.display()
    );
    nested
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// Whether the attributes confine an item to test builds: `#[test]`,
/// `#[cfg(test)]`, or a `cfg(all(..))` that requires `test`.
pub(super) fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute.parse_args::<Meta>().is_ok_and(cfg_requires_test))
    })
}

fn cfg_requires_test(predicate: Meta) -> bool {
    let mut pending = vec![predicate];
    while let Some(predicate) = pending.pop() {
        match predicate {
            Meta::Path(path) if path.is_ident("test") => return true,
            Meta::List(list) if list.path.is_ident("all") => {
                if let Ok(members) =
                    list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                {
                    pending.extend(members);
                }
            }
            _ => {}
        }
    }
    false
}

fn type_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        Type::Group(group) => type_name(&group.elem),
        Type::Paren(paren) => type_name(&paren.elem),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Managed-graph declarations
// ---------------------------------------------------------------------------

/// Types stored inline in managed allocations that are not themselves managed
/// families. Each must exist at this module and name.
const MANAGED_PAYLOAD_CARRIERS: &[(&str, &str)] = &[
    ("core", "Value"),
    ("core", "LazySource"),
    ("core", "OpaqueValue"),
    ("core", "HostCallProducer"),
    ("core", "ReflectionComputation"),
];

/// Every production `ManagedFamily` declaration, plus the payload carriers.
fn managed_graph_declarations(index: &ProductionIndex) -> Vec<(&ItemKey, &BTreeSet<String>)> {
    let mut declarations = index
        .declarations
        .iter()
        .filter(|(key, _)| index.managed_families.contains(&key.name))
        .collect::<Vec<_>>();
    for (module, name) in MANAGED_PAYLOAD_CARRIERS {
        let key = index
            .declarations
            .get_key_value(&ItemKey {
                module: (*module).to_owned(),
                name: (*name).to_owned(),
            })
            .unwrap_or_else(|| {
                panic!("managed payload carrier {module}::{name} is not a production declaration")
            });
        if !declarations.iter().any(|(existing, _)| *existing == key.0) {
            declarations.push(key);
        }
    }
    assert!(
        declarations
            .iter()
            .any(|(key, _)| key.module == "core::managed::recursive_cells"),
        "managed-family discovery found none of the recursive identity cells"
    );
    declarations
}

/// Rule: no managed-graph declaration stores value-domain authority or a
/// registered root.
///
/// The collector owns managed allocations and the value domain owns the
/// collector. A managed allocation that strongly held the domain, its factory,
/// an access region, or a registered root would keep itself alive and block
/// domain teardown. Managed payloads reach other managed values only through
/// traced `Gc` edges.
#[test]
fn managed_graph_holds_no_value_domain_authority_or_registered_root() {
    const FORBIDDEN: &[&str] = &[
        // Value-domain authority.
        "RuntimeValueDomain",
        "CoreValueFactory",
        "RuntimeValueAccess",
        "RuntimeValueObserver",
        "EvaluationRuntime",
        "RuntimeSharedResources",
        // Registered roots.
        "Root",
        "RuntimeValueRoot",
        "ManagedLazyRoot",
        "ManagedPromiseRoot",
        "ManagedCoreNetRoot",
    ];
    let index = production_index();
    let violations = managed_graph_declarations(index)
        .into_iter()
        .flat_map(|(key, names)| {
            FORBIDDEN
                .iter()
                .filter(|forbidden| names.contains(**forbidden))
                .map(move |forbidden| format!("{key} stores {forbidden}"))
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "managed-graph declarations hold value-domain authority or a registered root: {violations:#?}"
    );
}

/// Production `Drop` implementations that collector finalization may run,
/// with the reason each is passive.
const PASSIVE_MANAGED_DROPS: &[(&str, &str)] = &[(
    "RuntimeNetCell",
    "closes and wakes only its edge-free disturbance companion",
)];

/// Rule: collector finalization never runs an active destructor.
///
/// Dropping a managed allocation happens inside collection, where taking
/// locks, publishing, or waking evaluation work is not allowed. So no
/// managed-graph declaration implements `Drop` or stores a type with a
/// production `Drop`, apart from the listed passive drops. Active lifecycle
/// owners stay outside the managed graph behind the external-owner registry.
#[test]
fn managed_graph_holds_no_active_drop() {
    let index = production_index();
    let passive = PASSIVE_MANAGED_DROPS
        .iter()
        .map(|(name, _)| *name)
        .collect::<BTreeSet<_>>();
    let mut used_exceptions = BTreeSet::new();
    let mut violations = Vec::new();
    for (key, names) in managed_graph_declarations(index) {
        for name in std::iter::once(&key.name).chain(names) {
            let Some(modules) = index.drop_types.get(name) else {
                continue;
            };
            if passive.contains(name.as_str()) {
                used_exceptions.insert(name.as_str());
            } else {
                violations.push(format!(
                    "{key} reaches Drop for {name} (implemented in {modules:?})"
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "managed-graph declarations reach an active Drop: {violations:#?}"
    );
    let stale = passive.difference(&used_exceptions).collect::<Vec<_>>();
    assert!(
        stale.is_empty(),
        "passive-drop exceptions no longer match a managed-graph declaration: {stale:?}"
    );
}

/// Rule: an opaque payload enters `OpaqueValue` only through the unsafe
/// `OpaquePayloadFamily` admission.
///
/// `OpaqueValue` erases its payload's type, so the collector cannot see edges
/// inside it. The admission trait stays `unsafe`, so every family states why
/// its payload holds no managed edge. `OpaqueValue`'s fields stay private, and
/// each of its constructors bounds its payload by the admission trait.
#[test]
fn opaque_payloads_enter_only_through_unsafe_admission() {
    let index = production_index();

    let managed = syn::parse_file(&index.source("core::managed").text)
        .expect("the managed module should parse");
    let admission = managed
        .items
        .iter()
        .find_map(|item| match item {
            Item::Trait(item) if item.ident == "OpaquePayloadFamily" => Some(item),
            _ => None,
        })
        .expect("core::managed::OpaquePayloadFamily should exist");
    assert!(
        admission.unsafety.is_some(),
        "core::managed::OpaquePayloadFamily must remain an unsafe trait"
    );

    let core = syn::parse_file(&index.source("core").text).expect("the core module should parse");
    let opaque = core
        .items
        .iter()
        .find_map(|item| match item {
            Item::Struct(item) if item.ident == "OpaqueValue" => Some(item),
            _ => None,
        })
        .expect("core::OpaqueValue should exist");
    for field in &opaque.fields {
        assert!(
            matches!(field.vis, syn::Visibility::Inherited),
            "core::OpaqueValue fields must stay private"
        );
    }

    let mut constructors = Vec::new();
    for item in &core.items {
        let Item::Impl(implementation) = item else {
            continue;
        };
        if implementation.trait_.is_some()
            || type_name(&implementation.self_ty).as_deref() != Some("OpaqueValue")
        {
            continue;
        }
        for member in &implementation.items {
            let syn::ImplItem::Fn(function) = member else {
                continue;
            };
            let syn::ReturnType::Type(_, output) = &function.sig.output else {
                continue;
            };
            if !matches!(type_name(output).as_deref(), Some("Self" | "OpaqueValue")) {
                continue;
            }
            constructors.push(function.sig.ident.to_string());
            let mut bounds = NameCollector::default();
            bounds.visit_generics(&function.sig.generics);
            assert!(
                bounds.names.contains("OpaquePayloadFamily"),
                "core::OpaqueValue::{} constructs an opaque value without OpaquePayloadFamily admission",
                function.sig.ident
            );
        }
    }
    assert!(
        !constructors.is_empty(),
        "core::OpaqueValue should have an admitted constructor"
    );
}

// ---------------------------------------------------------------------------
// Runtime tests of the same boundaries
// ---------------------------------------------------------------------------

pub(super) fn compatibility_variant_name(value: &Value) -> &'static str {
    match value {
        Value::Atom(_) => "atom",
        Value::Number(_) => "number",
        Value::Binary(_) => "binary",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Builtin(_) => "builtin",
        Value::PartialBuiltin(_) => "partial builtin",
        Value::Function(_) => "function",
        Value::Net(_) => "net",
        Value::Lazy(_) => "lazy",
        Value::Promised(_) => "promised",
        Value::Metadata(_) => "metadata",
        Value::Opaque(_) => "opaque",
    }
}

struct ExternalDropProbe(Arc<AtomicUsize>);

impl Drop for ExternalDropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: this test payload contains no Glam value or managed pointer. It is
// intentionally external so the closure test can distinguish managed
// finalization from the later safe registry drain.
unsafe impl OpaquePayloadFamily for ExternalDropProbe {
    const PAYLOAD_RECORD: OpaquePayloadRecord = OpaquePayloadRecord::external(
        "passive-closure opaque probe",
        "src/core/managed/managed_boundary_audit.rs",
    );
}

fn closed_net(values: &crate::core::CoreValueFactory) -> crate::core_net::CoreRuntimeNet {
    let mut builder = NetBuilder::<CoreSpecialization>::new();
    let exposed = builder.data(Value::Number(0.into()));
    values.instantiate_core_net(&builder.finish(exposed))
}

/// One value of every `Value` variant. The host-call and opaque variants hold
/// external drop probes that count into `active_drops`.
pub(super) fn closed_compatibility_variants(
    values: &crate::core::CoreValueFactory,
    active_drops: &Arc<AtomicUsize>,
) -> Vec<Value> {
    let runtime = closed_net(values);
    let function = FunctionValue::new(NetValue::new(runtime.duplicate_for_test(values)), 1);
    let host_probe = ExternalDropProbe(Arc::clone(active_drops));
    let opaque_probe = Arc::new(ExternalDropProbe(Arc::clone(active_drops)));

    vec![
        values.with_runtime_value_access(|access| access.unit()),
        Value::Number(1.into()),
        Value::Binary(Bytes::from_static(b"closed")),
        Value::List(List::from_values(vec![Value::Number(2.into())])),
        Value::Dict(Dict::new_sync().insert(
            crate::core::Key::binary_from_text("field"),
            Value::Number(3.into()),
        )),
        Value::Builtin(Builtin::Append),
        Value::builtin_call(values, Builtin::Append, vec![Value::Number(4.into())]),
        Value::Function(function),
        Value::Net(NetValue::new(runtime)),
        Value::external_host_call(
            values,
            "passive closure host probe",
            HostCallRecord::external_without_semantic_values(
                "passive closure host probe",
                "src/core/managed/managed_boundary_audit.rs",
                "one external drop probe",
            ),
            [],
            move |_| {
                let _ = &host_probe;
                unreachable!("passive-closure collection must not invoke a host callback")
            },
        ),
        Value::Promised(PromisedValue::new(values, "passive closure promise")),
        values.with_runtime_value_access(|access| access.initial_metadata()),
        Value::Opaque(OpaqueValue::new(values, opaque_probe)),
    ]
}

#[test]
fn every_real_value_variant_has_passive_managed_destruction() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let managed_drops = Arc::new(AtomicUsize::new(0));
    let active_drops = Arc::new(AtomicUsize::new(0));
    let variants = closed_compatibility_variants(&values, &active_drops);
    let baseline = values
        .collect_managed_for_test()
        .expect("canonical roots should collect before the compatibility fixture");

    let roots = values.with_managed_values(|scope| {
        let allocator = scope
            .allocator::<ClosedCompatibilityValue>()
            .expect("the closed compatibility wrapper should fit a managed run");
        variants
            .into_iter()
            .map(|value| {
                scope.root(allocator.alloc(ClosedCompatibilityValue::new(value, &managed_drops)))
            })
            .collect::<Vec<_>>()
    });

    let live = values
        .collect_managed_for_test()
        .expect("rooted closed compatibility values should survive collection");
    assert_eq!(live.marked_slots(), baseline.marked_slots() + roots.len());
    assert_eq!(managed_drops.load(Ordering::Relaxed), 0);
    assert_eq!(active_drops.load(Ordering::Relaxed), 0);
    values.with_managed_values(|scope| {
        assert_eq!(
            roots
                .iter()
                .map(|root| compatibility_variant_name(scope.get(root).value()))
                .collect::<Vec<_>>(),
            [
                "atom",
                "number",
                "binary",
                "list",
                "dict",
                "builtin",
                "partial builtin",
                "function",
                "net",
                "lazy",
                "promised",
                "metadata",
                "opaque",
            ]
        );
    });

    drop(roots);
    let dead = values
        .collect_managed_for_test()
        .expect("unrooted closed compatibility values should be reclaimed");
    assert_eq!(dead.finalized_slots(), 13);
    assert_eq!(managed_drops.load(Ordering::Relaxed), 13);
    assert_eq!(
        active_drops.load(Ordering::Relaxed),
        0,
        "managed finalization must not retire external callback or opaque owners"
    );

    assert_eq!(values.drain_external_owners_for_test(), 2);
    assert_eq!(active_drops.load(Ordering::Relaxed), 2);
}

#[test]
fn arbitrary_host_callback_root_backedge_is_conservative_external_ownership() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the host-backedge fixture should start collectible");
    let captured_root = Arc::new(Mutex::new(None::<RuntimeValueRoot>));
    let callback_capture = Arc::clone(&captured_root);
    let value = Value::external_host_call(
        &values,
        "external-root backedge",
        HostCallRecord::external_without_semantic_values(
            "external-root backedge",
            "src/core/managed/managed_boundary_audit.rs",
            "one explicitly removable same-runtime root",
        ),
        [],
        move |_| {
            let _ = &callback_capture;
            Err(Arc::new(EvaluationFailure::message(
                "the containment fixture must not invoke its callback",
            )))
        },
    );
    *captured_root
        .lock()
        .expect("the host capture should not be poisoned") =
        Some(RuntimeValueRoot::new(&values, value));

    let retained = values
        .collect_managed_for_test()
        .expect("the explicit external root should retain its lazy");
    assert_eq!(retained.root_entries(), baseline.root_entries() + 1);
    assert_eq!(
        retained.marked_slots(),
        baseline.marked_slots() + 2,
        "the external root owns one value shell which reaches the managed lazy"
    );
    assert_eq!(values.external_owner_count_for_test(), 1);

    drop(
        captured_root
            .lock()
            .expect("the host capture should not be poisoned")
            .take(),
    );
    let reclaimed = values
        .collect_managed_for_test()
        .expect("removing the explicit external root should make the lazy collectible");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(
        reclaimed.finalized_slots(),
        2,
        "the host source cycle contains only its managed lazy and callback payload"
    );
    assert_eq!(
        values.drain_external_owners_for_test(),
        1,
        "the collection makes the conservative host owner drainable; it does not eagerly run host cleanup"
    );
    assert_eq!(values.external_owner_count_for_test(), 0);
}

#[test]
fn production_reflection_result_edges_do_not_need_an_external_root() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the reflection-edge fixture should start collectible");
    {
        let promise = PromisedValue::new(&values, "reflection effect backedge");
        let reflected = values.with_runtime_value_access(|access| {
            Value::reflection_task_result_in(
                &access,
                Value::Promised(promise.duplicate_in(&access)),
            )
        });
        assert!(
            set_test_promise(&values, &promise, reflected).is_ok(),
            "the reflection backedge promise should start unassigned"
        );
    }
    let reclaimed = values
        .collect_managed_for_test()
        .expect("direct reflection edges should be traced without registered roots");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.marked_slots(), baseline.marked_slots());
    assert_eq!(
        reclaimed.finalized_slots(),
        3,
        "the result cycle includes its managed completion promise"
    );
    assert_eq!(values.drain_external_owners_for_test(), 0);
    assert_eq!(values.external_owner_count_for_test(), 0);
}

#[test]
fn production_reflection_gate_target_backedge_reclaims_without_an_external_root() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the reflection-target fixture should start collectible");
    {
        let promise = PromisedValue::new(&values, "reflection target backedge");
        let reflected = values.with_runtime_value_access(|access| {
            Value::Lazy(LazyValue::from_reflection_gate_in(
                &access,
                access.unit(),
                Value::Promised(promise.duplicate_in(&access)),
            ))
        });
        assert!(
            set_test_promise(&values, &promise, reflected).is_ok(),
            "the reflection target promise should start unassigned"
        );
    }

    let reclaimed = values
        .collect_managed_for_test()
        .expect("the direct reflection target edge should close its managed cycle");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.marked_slots(), baseline.marked_slots());
    assert_eq!(
        reclaimed.finalized_slots(),
        3,
        "the gate cycle includes its managed completion promise"
    );
    assert_eq!(values.drain_external_owners_for_test(), 0);
    assert_eq!(values.external_owner_count_for_test(), 0);
}

#[test]
fn host_call_constructor_rejects_captures_that_contradict_its_record() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let mismatch = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        LazyValue::external_host_call(
            &values,
            "misclassified host-call capture",
            HostCallRecord::external_without_semantic_values(
                "misclassified host-call capture",
                "src/core/managed/managed_boundary_audit.rs",
                "incorrectly claims no semantic captures",
            ),
            [Value::Number(1.into())],
            |_| Err(Arc::new(EvaluationFailure::message("not invoked"))),
        )
    }));
    assert!(
        mismatch.is_err(),
        "a host-call constructor must reject capture state which contradicts its record"
    );
}

fn return_second_capture(
    context: &EvaluatorStepContext<'_>,
    captures: &[Value],
) -> Result<Value, EvaluationHalt> {
    let [_, result] = captures else {
        unreachable!("the semantic-computation fixture has two captures")
    };
    Ok(context.with_value_access(|access| access.values().duplicate_value(result)))
}

#[test]
fn semantic_computation_captures_are_explicit() {
    let values = crate::core::test_value_factory();
    let lazy = LazyValue::semantic_computation(
        &values,
        "explicit capture fixture",
        [Value::Number(1.into()), Value::Number(2.into())],
        return_second_capture,
    );
    let Some(LazySource::SemanticComputation(computation)) = lazy.source_snapshot(&values) else {
        panic!("semantic computation should retain its explicit source")
    };

    assert_eq!(computation.captures.len(), 2);
    assert!(
        values.same_representation_for_test(&computation.captures[1], &Value::Number(2.into())),
        "the function pointer receives the exact ordered capture array"
    );
}

#[test]
fn external_host_call_requires_a_source_backed_record() {
    let values = crate::core::test_value_factory();
    let lazy = LazyValue::external_host_call(
        &values,
        "external call fixture",
        HostCallRecord::external_without_semantic_values(
            "external call fixture",
            "src/core/managed/managed_boundary_audit.rs",
            "no value captures",
        ),
        [],
        |_| Err(Arc::new(EvaluationFailure::message("not invoked"))),
    );
    let Some(LazySource::HostCall(producer)) = lazy.source_snapshot(&values) else {
        panic!("external host call should retain its classified source")
    };

    assert_eq!(
        producer.record().fields(),
        (
            "external call fixture",
            "src/core/managed/managed_boundary_audit.rs",
            "no value captures",
        )
    );
}

#[test]
fn external_closure_bundle_retains_only_declared_roots() {
    let values = crate::core::test_value_factory();
    let observed = Arc::new(Mutex::new(None));
    let callback_observed = Arc::clone(&observed);
    let lazy = LazyValue::external_host_call(
        &values,
        "explicit host root bundle",
        HostCallRecord::external_with_semantic_values(
            "explicit host root bundle",
            "src/core/managed/managed_boundary_audit.rs",
            "one explicit numeric value",
        ),
        [Value::Number(42.into())],
        move |captures| {
            let runtime = captures.runtime_id();
            let mut roots = captures.into_roots().into_vec();
            *callback_observed
                .lock()
                .expect("host-call observation mutex should not be poisoned") =
                Some((runtime, roots.len()));
            Ok(roots
                .pop()
                .expect("the declared host-call capture should be present"))
        },
    );
    let Some(LazySource::HostCall(producer)) = lazy.source_snapshot(&values) else {
        panic!("external host call should retain its classified source")
    };

    let result = match producer.invoke(&values) {
        Ok(result) => result,
        Err(_) => panic!("the explicit root-bundle callback should succeed"),
    };
    assert!(
        values.same_representation_for_test(
            &result.clone_core_for_test(),
            &Value::Number(42.into()),
        ),
        "the callback receives the declared semantic value as a temporary runtime root"
    );
    assert_eq!(
        *observed
            .lock()
            .expect("host-call observation mutex should not be poisoned"),
        Some((values.runtime_id(), 1))
    );
}

#[test]
fn managed_deferred_state_cycle_reclaims() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let baseline = values
        .collect_managed_for_test()
        .expect("the deferred-state fixture should start collectible");
    {
        let promise = PromisedValue::new(&values, "host-call capture backedge");
        let lazy = LazyValue::external_host_call(
            &values,
            "traceable host-call capture",
            HostCallRecord::external_with_semantic_values(
                "traceable host-call capture",
                "src/core/managed/managed_boundary_audit.rs",
                "one explicit promised value",
            ),
            [Value::Promised(promise.duplicate_for_test(&values))],
            |_| Err(Arc::new(EvaluationFailure::message("not invoked"))),
        );
        assert!(
            set_test_promise(&values, &promise, Value::Lazy(lazy)).is_ok(),
            "the cycle promise should start unassigned"
        );
    }

    let reclaimed = values
        .collect_managed_for_test()
        .expect("the explicit deferred-state cycle should collect");
    assert_eq!(reclaimed.root_entries(), baseline.root_entries());
    assert_eq!(reclaimed.marked_slots(), baseline.marked_slots());
    assert!(
        reclaimed.finalized_slots() >= 2,
        "the host-call lazy and promise cycle should both be reclaimed"
    );
    assert_eq!(values.drain_external_owners_for_test(), 1);
}

struct HostCaptureDrop(Arc<AtomicUsize>);

impl Drop for HostCaptureDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn host_call_capture_retires_only_during_external_registry_drain() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let drops = Arc::new(AtomicUsize::new(0));
    let capture = HostCaptureDrop(Arc::clone(&drops));
    {
        let _lazy = LazyValue::external_host_call(
            &values,
            "external owner fixture",
            HostCallRecord::external_without_semantic_values(
                "external owner fixture",
                "src/core/managed/managed_boundary_audit.rs",
                "passive drop observer",
            ),
            [],
            move |_| {
                let _ = &capture;
                Err(Arc::new(EvaluationFailure::message("not invoked")))
            },
        );
        assert_eq!(values.external_owner_count_for_test(), 1);
    }
    assert_eq!(
        drops.load(Ordering::Relaxed),
        0,
        "ending the lazy edge's scope must not destroy its host capture"
    );
    values
        .collect_managed_for_test()
        .expect("the unrooted managed lazy should retire its external-owner handle");
    assert_eq!(values.drain_external_owners_for_test(), 1);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(values.external_owner_count_for_test(), 0);
}

struct OpaqueDropSignal(Arc<AtomicUsize>);

impl Drop for OpaqueDropSignal {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: this fixture contains no Glam value or managed pointer. Its drop
// observer models an arbitrary opaque destructor owned by the external
// registry rather than the managed-reachable token.
unsafe impl OpaquePayloadFamily for OpaqueDropSignal {
    const PAYLOAD_RECORD: OpaquePayloadRecord = OpaquePayloadRecord::external(
        "opaque external-owner fixture",
        "src/core/managed/managed_boundary_audit.rs",
    );
}

#[test]
fn opaque_payload_requires_matching_runtime_and_retires_during_registry_drain() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let other_values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let drops = Arc::new(AtomicUsize::new(0));
    let payload = Arc::new(OpaqueDropSignal(Arc::clone(&drops)));
    let retained = Arc::downgrade(&payload);
    let opaque = OpaqueValue::new(&values, payload);

    assert!(opaque.downcast::<OpaqueDropSignal>(&other_values).is_none());
    assert!(opaque.downcast::<OpaqueDropSignal>(&values).is_some());
    drop(opaque);
    assert!(retained.upgrade().is_some());
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    assert_eq!(values.drain_external_owners_for_test(), 1);
    assert!(retained.upgrade().is_none());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn opaque_external_capabilities_retain_only_reviewed_routes() {
    crate::api::assert_effect_token_family_shape();
    crate::reflection::assert_task_handle_family_shape();
}

#[test]
fn opaque_downcast_requires_matching_runtime_and_preserves_owner_identity() {
    let values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let other_values = crate::core::CoreValueFactory::new(
        crate::runtime::allocate_evaluation_runtime_id(),
        crate::runtime::RuntimeIds::new(),
    );
    let payload = Arc::new(OpaqueDropSignal(Arc::new(AtomicUsize::new(0))));
    let opaque = OpaqueValue::new(&values, Arc::clone(&payload));
    let same_owner = opaque.clone();
    let distinct_owner = OpaqueValue::new(&values, Arc::clone(&payload));

    assert_eq!(opaque, same_owner);
    assert_ne!(opaque, distinct_owner);
    let extracted = opaque
        .downcast::<OpaqueDropSignal>(&values)
        .expect("matching runtime and family should open the opaque owner");
    assert!(Arc::ptr_eq(&extracted, &payload));
    assert!(opaque.downcast::<u64>(&values).is_none());
    assert!(opaque.downcast::<OpaqueDropSignal>(&other_values).is_none());
}

#[test]
fn managed_drop_during_domain_teardown_is_passive() {
    let drops = Arc::new(AtomicUsize::new(0));
    let domain = {
        let values = crate::core::CoreValueFactory::new(
            crate::runtime::allocate_evaluation_runtime_id(),
            crate::runtime::RuntimeIds::new(),
        );
        let domain = Arc::downgrade(values.value_domain());
        let opaque = OpaqueValue::new(&values, Arc::new(OpaqueDropSignal(Arc::clone(&drops))));
        let root = values.construct_runtime_value_root(|_| Value::Opaque(opaque));

        drop(root);
        let collected = values
            .collect_managed_for_test()
            .expect("the unrooted opaque shell should collect before domain teardown");
        assert_eq!(collected.finalized_slots(), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert_eq!(values.external_owner_count_for_test(), 1);
        domain
    };

    assert!(domain.upgrade().is_none());
    assert_eq!(
        drops.load(Ordering::Relaxed),
        1,
        "terminal domain teardown must retire the already-detached passive opaque handle once"
    );
}

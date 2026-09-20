//! W6G.3a source-backed inventory of the durable WHNF checkpoint boundary.
//!
//! The aggregate-cell transition deliberately preserves source-entry
//! ownership, keeps ordinary access-free construction cheap, and moves only
//! access-qualified structured construction directly into managed state. This
//! census makes that migration surface exact before the representation moves.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{Attribute, ExprCall, ExprMethodCall, ImplItemFn, ItemFn};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CheckpointApi {
    FromRoot,
    FromLazySource,
    FromApplicationCheckpoint,
    FromStaticAccessCheckpoint,
    FromPromiseRoot,
    SourceRoot,
    InstallSourceResult,
    ApplicationFramePending,
    RuntimeId,
    WithSourceOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundaryRole {
    SourceEntry,
    DemandSeed,
    AccessQualifiedStructuredDemand,
    ScalarObserver,
    SeedModifier,
}

fn boundary_role(api: CheckpointApi) -> BoundaryRole {
    match api {
        CheckpointApi::FromLazySource
        | CheckpointApi::SourceRoot
        | CheckpointApi::InstallSourceResult => BoundaryRole::SourceEntry,
        CheckpointApi::FromRoot | CheckpointApi::FromPromiseRoot => BoundaryRole::DemandSeed,
        CheckpointApi::FromApplicationCheckpoint | CheckpointApi::FromStaticAccessCheckpoint => {
            BoundaryRole::AccessQualifiedStructuredDemand
        }
        CheckpointApi::RuntimeId | CheckpointApi::ApplicationFramePending => {
            BoundaryRole::ScalarObserver
        }
        CheckpointApi::WithSourceOwner => BoundaryRole::SeedModifier,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Occurrence {
    declaration: String,
    ordinal: usize,
    api: CheckpointApi,
}

impl Occurrence {
    fn record(&self) -> String {
        format!(
            "{}#{}|{:?}|{:?}",
            self.declaration,
            self.ordinal,
            self.api,
            boundary_role(self.api),
        )
    }
}

struct InventoryVisitor<'path> {
    path: &'path Path,
    module: Vec<String>,
    impl_name: Option<String>,
    function: Option<String>,
    ordinals: BTreeMap<(String, CheckpointApi), usize>,
    occurrences: Vec<Occurrence>,
}

impl<'path> InventoryVisitor<'path> {
    fn new(path: &'path Path) -> Self {
        Self {
            path,
            module: Vec::new(),
            impl_name: None,
            function: None,
            ordinals: BTreeMap::new(),
            occurrences: Vec::new(),
        }
    }

    fn declaration(&self) -> String {
        let mut parts = vec![self.path.display().to_string()];
        parts.extend(self.module.iter().cloned());
        if let Some(name) = &self.impl_name {
            parts.push(name.clone());
        }
        parts.push(
            self.function
                .clone()
                .unwrap_or_else(|| "<module>".to_owned()),
        );
        parts.join("::")
    }

    fn record(&mut self, api: CheckpointApi) {
        let declaration = self.declaration();
        let ordinal = self.ordinals.entry((declaration.clone(), api)).or_default();
        *ordinal += 1;
        self.occurrences.push(Occurrence {
            declaration,
            ordinal: *ordinal,
            api,
        });
    }

    fn visit_function(&mut self, name: String, attributes: &[Attribute], body: &syn::Block) {
        if is_test_only(attributes) {
            return;
        }
        let prior = self.function.replace(name);
        self.visit_block(body);
        self.function = prior;
    }
}

impl<'ast> Visit<'ast> for InventoryVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if is_test_only(&node.attrs) {
            return;
        }
        let Some((_, items)) = &node.content else {
            return;
        };
        self.module.push(node.ident.to_string());
        for item in items {
            self.visit_item(item);
        }
        self.module.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if is_test_only(&node.attrs) {
            return;
        }
        let prior = self.impl_name.replace(type_name(&node.self_ty));
        visit::visit_item_impl(self, node);
        self.impl_name = prior;
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.visit_function(node.sig.ident.to_string(), &node.attrs, &node.block);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.visit_function(node.sig.ident.to_string(), &node.attrs, &node.block);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(path) = node.func.as_ref() {
            let full = path_string(&path.path);
            let whnf_constructor = full.contains("WhnfComputation")
                || (full.starts_with("Self::")
                    && self.impl_name.as_deref() == Some("WhnfComputation"));
            if whnf_constructor
                && let Some(api) =
                    constructor_api(path.path.segments.last().map(|part| part.ident.to_string()))
            {
                self.record(api);
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        if let Some(api) = method_api(&method, &self.declaration()) {
            self.record(api);
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // `matches!` hides its guard expression from syn's expression visitor.
        // Source-entry probing currently uses exactly one such guard, so keep
        // that boundary visible to the same exact inventory.
        for _ in 0..node.tokens.to_string().matches("source_root").count() {
            self.record(CheckpointApi::SourceRoot);
        }
        visit::visit_macro(self, node);
    }
}

fn constructor_api(name: Option<String>) -> Option<CheckpointApi> {
    match name.as_deref() {
        Some("from_root") => Some(CheckpointApi::FromRoot),
        Some("from_lazy_source") => Some(CheckpointApi::FromLazySource),
        Some("from_application_checkpoint_in") => Some(CheckpointApi::FromApplicationCheckpoint),
        Some("from_static_access_checkpoint_in") => Some(CheckpointApi::FromStaticAccessCheckpoint),
        Some("from_promise_root") => Some(CheckpointApi::FromPromiseRoot),
        _ => None,
    }
}

fn method_api(name: &str, declaration: &str) -> Option<CheckpointApi> {
    match name {
        "source_root" => Some(CheckpointApi::SourceRoot),
        "install_source_result" => Some(CheckpointApi::InstallSourceResult),
        "application_frame_pending" => Some(CheckpointApi::ApplicationFramePending),
        "with_source_owner" => Some(CheckpointApi::WithSourceOwner),
        "runtime_id" if declaration.ends_with("ClientDemandOperation::runtime_id") => {
            Some(CheckpointApi::RuntimeId)
        }
        _ => None,
    }
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute
                    .meta
                    .to_token_stream()
                    .to_string()
                    .contains("test"))
    })
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map_or_else(|| "impl".to_owned(), |part| part.ident.to_string()),
        _ => "impl".to_owned(),
    }
}

fn path_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|part| part.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the source tree should be readable") {
        let path = entry.expect("a source entry should be readable").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "tests") {
                continue;
            }
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path.file_name().is_some_and(|name| name != "tests.rs")
            && path
                .file_name()
                .is_some_and(|name| name != "test_support.rs")
            && path
                .file_name()
                .is_some_and(|name| name != "whnf_checkpoint_inventory.rs")
        {
            sources.push(path);
        }
    }
}

fn collect_occurrences(manifest: &Path) -> Vec<Occurrence> {
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    sources.sort();

    let mut occurrences = Vec::new();
    for path in sources {
        let source = fs::read_to_string(&path).expect("checkpoint source should be readable");
        if !source.contains("WhnfComputation") {
            continue;
        }
        let relative = path
            .strip_prefix(manifest)
            .expect("checkpoint source must belong to the package");
        let syntax = syn::parse_file(&source).unwrap_or_else(|error| {
            panic!(
                "{} must parse for checkpoint census: {error}",
                relative.display()
            )
        });
        let mut visitor = InventoryVisitor::new(relative);
        visitor.visit_file(&syntax);
        occurrences.extend(visitor.occurrences);
    }
    occurrences.sort_by_key(Occurrence::record);
    occurrences
}

fn occurrence_fingerprint(occurrences: &[Occurrence]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    occurrences
        .iter()
        .fold(FNV_OFFSET, |mut fingerprint, occurrence| {
            for byte in occurrence.record().bytes().chain([0xff]) {
                fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
            }
            fingerprint
        })
}

fn api_counts(occurrences: &[Occurrence]) -> BTreeMap<CheckpointApi, usize> {
    occurrences
        .iter()
        .fold(BTreeMap::new(), |mut counts, occurrence| {
            *counts.entry(occurrence.api).or_default() += 1;
            counts
        })
}

const EXPECTED_API_COUNTS: &[(CheckpointApi, usize)] = &[
    // W6G.1f.3g.1a removes the two rooted list-back child projections;
    // W6G.1f.3g.2b removes assertion, provenance, and conditional projections.
    (CheckpointApi::FromRoot, 76),
    (CheckpointApi::FromLazySource, 1),
    (CheckpointApi::FromApplicationCheckpoint, 4),
    (CheckpointApi::FromStaticAccessCheckpoint, 1),
    (CheckpointApi::FromPromiseRoot, 2),
    (CheckpointApi::SourceRoot, 2),
    (CheckpointApi::InstallSourceResult, 1),
    (CheckpointApi::ApplicationFramePending, 1),
    (CheckpointApi::RuntimeId, 1),
    // W6G.1f.3g.2a-.2b give every regional scalar/direct demand the same exact
    // lazy owner as the outer managed builtin checkpoint transition.
    (CheckpointApi::WithSourceOwner, 5),
];
const EXPECTED_OCCURRENCES: usize = 94;
const EXPECTED_FINGERPRINT: u64 = 17_073_741_932_416_319_276;

#[test]
fn durable_whnf_checkpoint_boundary_is_exact() {
    let occurrences = collect_occurrences(Path::new(env!("CARGO_MANIFEST_DIR")));
    assert_eq!(
        api_counts(&occurrences),
        EXPECTED_API_COUNTS.iter().copied().collect(),
        "W6G.3a checkpoint API counts drifted; occurrences: {occurrences:#?}"
    );
    assert_eq!(
        (occurrences.len(), occurrence_fingerprint(&occurrences)),
        (EXPECTED_OCCURRENCES, EXPECTED_FINGERPRINT),
        "W6G.3a checkpoint boundary drifted; records: {:#?}",
        occurrences
            .iter()
            .map(Occurrence::record)
            .collect::<Vec<_>>()
    );
}

#[test]
fn checkpoint_boundary_roles_are_compile_exhaustive() {
    let roles = EXPECTED_API_COUNTS
        .iter()
        .map(|(api, _)| boundary_role(*api))
        .collect::<Vec<_>>();
    assert!(roles.contains(&BoundaryRole::SourceEntry));
    assert!(roles.contains(&BoundaryRole::DemandSeed));
    assert!(roles.contains(&BoundaryRole::AccessQualifiedStructuredDemand));
    assert!(roles.contains(&BoundaryRole::ScalarObserver));
    assert!(roles.contains(&BoundaryRole::SeedModifier));
}

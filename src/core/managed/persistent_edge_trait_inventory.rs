//! P0A syntax-backed inventory for persistent managed-edge trait migration.
//!
//! This inventory is intentionally conservative. It records stored typed
//! edges, typed carrier surfaces, operations which currently consume a copied
//! edge, traits on representations which can reach an edge, and every use of
//! the collector-private erased identity. Later compiler-enforced trait
//! removal is the definitive move-after-use check; this latch prevents the
//! known surface from growing silently before that cutover.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{Attribute, Fields, Type};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum SourceScope {
    Production,
    Test,
}

impl SourceScope {
    const fn label(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum EdgeSurface {
    Typed,
    Erased,
}

impl EdgeSurface {
    const fn label(self) -> &'static str {
        match self {
            Self::Typed => "Gc",
            Self::Erased => "ErasedGc",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum OccurrenceKind {
    StoredField,
    TypedCarrier,
    TraitDependency,
    FreshAllocation,
    TraceVisit,
    EraseIdentity,
    PointerIdentity,
    RootCreation,
    RootProjection,
    MutationInput,
    CopySensitiveExpression,
    ErasedIdentity,
}

impl OccurrenceKind {
    const fn label(self) -> &'static str {
        match self {
            Self::StoredField => "stored-field",
            Self::TypedCarrier => "typed-carrier",
            Self::TraitDependency => "trait-dependency",
            Self::FreshAllocation => "fresh-allocation",
            Self::TraceVisit => "trace-visit",
            Self::EraseIdentity => "erase-identity",
            Self::PointerIdentity => "pointer-identity",
            Self::RootCreation => "root-creation",
            Self::RootProjection => "root-projection",
            Self::MutationInput => "mutation-input",
            Self::CopySensitiveExpression => "copy-sensitive-expression",
            Self::ErasedIdentity => "erased-identity",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum EdgeDisposition {
    FreshUnpublishedEdge,
    TracedPersistentEdge,
    RegisteredRootProjection,
    MutatorLocalWorkingDuplicate,
    AccessQualifiedObservation,
    MutationInput,
    CollectorPrivateErasedIdentity,
    Defect,
}

impl EdgeDisposition {
    const fn label(self) -> &'static str {
        match self {
            Self::FreshUnpublishedEdge => "fresh-unpublished-edge",
            Self::TracedPersistentEdge => "traced-persistent-edge",
            Self::RegisteredRootProjection => "registered-root-projection",
            Self::MutatorLocalWorkingDuplicate => "mutator-local-working-duplicate",
            Self::AccessQualifiedObservation => "access-qualified-observation",
            Self::MutationInput => "mutation-input",
            Self::CollectorPrivateErasedIdentity => "collector-private-erased-identity",
            Self::Defect => "defect",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct EdgeOccurrence {
    declaration: String,
    shape: String,
    scope: SourceScope,
    surface: EdgeSurface,
    kind: OccurrenceKind,
    disposition: EdgeDisposition,
}

impl EdgeOccurrence {
    fn record(&self) -> String {
        format!(
            "{}|shape={}|scope={}|surface={}|kind={}|disposition={}",
            self.declaration,
            self.shape,
            self.scope.label(),
            self.surface.label(),
            self.kind.label(),
            self.disposition.label(),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct EdgeSignals {
    typed: usize,
    erased: usize,
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute
                    .meta
                    .require_list()
                    .is_ok_and(|list| list.tokens.to_string().contains("test")))
    })
}

fn source_scope(relative: &Path) -> SourceScope {
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests")
        || relative.file_name().is_some_and(|name| {
            name == "tests.rs"
                || name == "test_support.rs"
                || name.to_string_lossy().ends_with("_inventory.rs")
        })
    {
        SourceScope::Test
    } else {
        SourceScope::Production
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

fn normalized(tokens: impl ToTokens) -> String {
    tokens.to_token_stream().to_string()
}

fn type_signals(ty: &Type) -> EdgeSignals {
    struct SignalVisitor {
        signals: EdgeSignals,
        inside_phantom: usize,
    }

    impl<'ast> Visit<'ast> for SignalVisitor {
        fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
            let last = path.path.segments.last().map(|segment| &segment.ident);
            if last.is_some_and(|identifier| identifier == "PhantomData") {
                self.inside_phantom += 1;
                visit::visit_type_path(self, path);
                self.inside_phantom -= 1;
                return;
            }
            if self.inside_phantom == 0 {
                match last.map(ToString::to_string).as_deref() {
                    Some("Gc") => self.signals.typed += 1,
                    Some("ErasedGc") => self.signals.erased += 1,
                    _ => {}
                }
            }
            visit::visit_type_path(self, path);
        }
    }

    let mut visitor = SignalVisitor {
        signals: EdgeSignals::default(),
        inside_phantom: 0,
    };
    visitor.visit_type(ty);
    visitor.signals
}

fn type_dependencies(ty: &Type, dependencies: &mut BTreeSet<String>) {
    struct DependencyVisitor<'dependencies> {
        dependencies: &'dependencies mut BTreeSet<String>,
        inside_phantom: usize,
    }

    impl<'ast> Visit<'ast> for DependencyVisitor<'_> {
        fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
            let Some(last) = path.path.segments.last() else {
                return;
            };
            if last.ident == "PhantomData" {
                self.inside_phantom += 1;
                visit::visit_type_path(self, path);
                self.inside_phantom -= 1;
                return;
            }
            if self.inside_phantom == 0 {
                self.dependencies.insert(last.ident.to_string());
            }
            visit::visit_type_path(self, path);
        }
    }

    DependencyVisitor {
        dependencies,
        inside_phantom: 0,
    }
    .visit_type(ty);
}

fn field_types(fields: &Fields) -> impl Iterator<Item = &Type> {
    fields.iter().map(|field| &field.ty)
}

#[derive(Clone, Debug)]
struct TypeDefinition {
    declaration: String,
    source: String,
    domain: &'static str,
    name: String,
    scope: SourceScope,
    direct_gc: bool,
    dependencies: BTreeSet<String>,
    derived_traits: BTreeSet<String>,
}

#[derive(Clone, Debug)]
struct TraitImplementation {
    declaration: String,
    source: String,
    domain: &'static str,
    target: String,
    scope: SourceScope,
    implemented_trait: String,
}

fn forbidden_derive_traits(attributes: &[Attribute]) -> BTreeSet<String> {
    let mut traits = BTreeSet::new();
    for attribute in attributes {
        if !attribute.path().is_ident("derive") {
            continue;
        }
        attribute
            .parse_nested_meta(|meta| {
                if let Some(name) = meta
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                    && matches!(
                        name.as_str(),
                        "Copy" | "Clone" | "PartialEq" | "Eq" | "Debug"
                    )
                {
                    traits.insert(name);
                }
                Ok(())
            })
            .expect("a parsed Rust derive should have valid nested metadata");
    }
    traits
}

fn type_target_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

struct DefinitionCollector<'source> {
    relative: &'source Path,
    domain: &'static str,
    modules: Vec<String>,
    test_depth: usize,
    base_scope: SourceScope,
    definitions: Vec<TypeDefinition>,
    implementations: Vec<TraitImplementation>,
}

impl DefinitionCollector<'_> {
    fn declaration(&self, name: &str) -> String {
        let mut declaration = self.relative.display().to_string();
        for module in &self.modules {
            declaration.push_str("::");
            declaration.push_str(module);
        }
        declaration.push_str("::");
        declaration.push_str(name);
        declaration
    }

    fn scope(&self) -> SourceScope {
        if self.base_scope == SourceScope::Test || self.test_depth != 0 {
            SourceScope::Test
        } else {
            SourceScope::Production
        }
    }

    fn add_definition<'type_ref>(
        &mut self,
        name: String,
        attributes: &[Attribute],
        types: impl IntoIterator<Item = &'type_ref Type>,
    ) {
        let mut direct_gc = name == "Gc";
        let mut dependencies = BTreeSet::new();
        for ty in types {
            direct_gc |= type_signals(ty).typed != 0;
            type_dependencies(ty, &mut dependencies);
        }
        self.definitions.push(TypeDefinition {
            declaration: self.declaration(&name),
            source: self.relative.display().to_string(),
            domain: self.domain,
            name,
            scope: if is_test_only(attributes) {
                SourceScope::Test
            } else {
                self.scope()
            },
            direct_gc,
            dependencies,
            derived_traits: forbidden_derive_traits(attributes),
        });
    }
}

impl<'ast> Visit<'ast> for DefinitionCollector<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let test = is_test_only(&item.attrs);
        self.test_depth += usize::from(test);
        self.modules.push(item.ident.to_string());
        if let Some((_, items)) = &item.content {
            for item in items {
                self.visit_item(item);
            }
        }
        self.modules.pop();
        self.test_depth -= usize::from(test);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.add_definition(
            item.ident.to_string(),
            &item.attrs,
            field_types(&item.fields),
        );
        visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        let types = item
            .variants
            .iter()
            .flat_map(|variant| field_types(&variant.fields));
        self.add_definition(item.ident.to_string(), &item.attrs, types);
        visit::visit_item_enum(self, item);
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        self.add_definition(
            item.ident.to_string(),
            &item.attrs,
            item.fields.named.iter().map(|field| &field.ty),
        );
        visit::visit_item_union(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        self.add_definition(
            item.ident.to_string(),
            &item.attrs,
            std::iter::once(item.ty.as_ref()),
        );
        visit::visit_item_type(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if let Some((_, path, _)) = &item.trait_
            && let (Some(implemented_trait), Some(target)) = (
                path.segments
                    .last()
                    .map(|segment| segment.ident.to_string()),
                type_target_name(&item.self_ty),
            )
            && matches!(
                implemented_trait.as_str(),
                "Copy" | "Clone" | "PartialEq" | "Eq" | "Debug"
            )
        {
            self.implementations.push(TraitImplementation {
                declaration: self.declaration(&format!("impl {implemented_trait} for {target}")),
                source: self.relative.display().to_string(),
                domain: self.domain,
                target,
                scope: if is_test_only(&item.attrs) {
                    SourceScope::Test
                } else {
                    self.scope()
                },
                implemented_trait,
            });
        }
        visit::visit_item_impl(self, item);
    }
}

/// Finds traits on the direct managed facades and on same-source semantic
/// carriers built from them.
///
/// Same-source propagation is deliberate: resolving an unqualified Rust type
/// name across modules without the compiler would confuse unrelated names
/// such as the public and core `Value` types. Cross-module operation and owner
/// dependencies remain covered by the parent raw-value inventories; the
/// explicit terminal names below are the reviewed managed-edge imports.
fn managed_type_declarations(definitions: &[TypeDefinition]) -> BTreeSet<String> {
    let direct_edge_names = BTreeSet::from([
        "CoreRuntimeNet",
        "ManagedCoreNetEdge",
        "ManagedLazyEdge",
        "ManagedPromiseEdge",
    ]);
    let mut managed = BTreeSet::new();
    for definition in definitions {
        if definition.direct_gc
            || definition
                .dependencies
                .iter()
                .any(|dependency| direct_edge_names.contains(dependency.as_str()))
        {
            managed.insert(definition.declaration.clone());
        }
    }

    loop {
        let managed_names_by_source = definitions.iter().fold(
            BTreeMap::<(&str, &'static str), BTreeSet<&str>>::new(),
            |mut names, definition| {
                if managed.contains(&definition.declaration) {
                    names
                        .entry((&definition.source, definition.domain))
                        .or_default()
                        .insert(&definition.name);
                }
                names
            },
        );
        let mut additions = Vec::new();
        for definition in definitions {
            if managed.contains(&definition.declaration) {
                continue;
            }
            if definition.dependencies.iter().any(|dependency| {
                managed_names_by_source
                    .get(&(definition.source.as_str(), definition.domain))
                    .is_some_and(|names| names.contains(dependency.as_str()))
            }) {
                additions.push(definition.declaration.clone());
            }
        }
        if additions.is_empty() {
            return managed;
        }
        managed.extend(additions);
    }
}

struct OccurrenceVisitor<'source> {
    relative: &'source Path,
    domain: &'static str,
    base_scope: SourceScope,
    test_depth: usize,
    modules: Vec<String>,
    items: Vec<String>,
    occurrences: Vec<EdgeOccurrence>,
}

impl OccurrenceVisitor<'_> {
    fn scope(&self) -> SourceScope {
        if self.base_scope == SourceScope::Test || self.test_depth != 0 {
            SourceScope::Test
        } else {
            SourceScope::Production
        }
    }

    fn declaration(&self, suffix: &str) -> String {
        let mut declaration = self.relative.display().to_string();
        for component in self.modules.iter().chain(&self.items) {
            declaration.push_str("::");
            declaration.push_str(component);
        }
        if !suffix.is_empty() {
            declaration.push_str("::");
            declaration.push_str(suffix);
        }
        declaration
    }

    fn record(
        &mut self,
        surface: EdgeSurface,
        kind: OccurrenceKind,
        disposition: EdgeDisposition,
        suffix: &str,
        shape: impl ToTokens,
    ) {
        self.occurrences.push(EdgeOccurrence {
            declaration: self.declaration(suffix),
            shape: normalized(shape),
            scope: self.scope(),
            surface,
            kind,
            disposition,
        });
    }

    fn record_type(&mut self, ty: &Type, kind: OccurrenceKind, suffix: &str) {
        let signals = type_signals(ty);
        for _ in 0..signals.typed {
            let disposition = match kind {
                OccurrenceKind::StoredField => EdgeDisposition::TracedPersistentEdge,
                OccurrenceKind::FreshAllocation => EdgeDisposition::FreshUnpublishedEdge,
                OccurrenceKind::RootCreation | OccurrenceKind::RootProjection => {
                    EdgeDisposition::RegisteredRootProjection
                }
                OccurrenceKind::MutationInput => EdgeDisposition::MutationInput,
                _ => EdgeDisposition::MutatorLocalWorkingDuplicate,
            };
            self.record(EdgeSurface::Typed, kind, disposition, suffix, ty);
        }
    }

    fn with_item(&mut self, name: String, test: bool, visit: impl FnOnce(&mut Self)) {
        self.test_depth += usize::from(test);
        self.items.push(name);
        visit(self);
        self.items.pop();
        self.test_depth -= usize::from(test);
    }

    fn edge_relevant_file(&self) -> bool {
        self.domain == "glam-gc"
            || self.relative.starts_with("src/core")
            || self.relative == Path::new("src/core_net.rs")
            || self.relative.starts_with("src/eval")
            || self.relative.starts_with("src/evaluation")
    }

    fn typed_pointer_identity(&self) -> bool {
        self.relative == Path::new("crates/glam-gc/src/pointer.rs")
            || self.relative == Path::new("src/core_net.rs")
            || self.relative == Path::new("src/eval/net.rs")
            || (self.relative == Path::new("src/core/managed/recursive_cells.rs")
                && self.items.last().is_some_and(|item| item == "eq"))
    }

    fn record_repeated_paths<'expression>(
        &mut self,
        expressions: impl IntoIterator<Item = &'expression syn::Expr>,
        suffix: &str,
    ) {
        let mut paths = BTreeMap::<String, usize>::new();
        for expression in expressions {
            if let syn::Expr::Path(path) = expression {
                *paths.entry(normalized(path)).or_default() += 1;
            }
        }
        for (path, count) in paths {
            if count > 1 && path != "None" && self.edge_relevant_file() {
                let expression = syn::parse_str::<syn::Expr>(&path)
                    .expect("an emitted expression path should parse");
                self.record(
                    EdgeSurface::Typed,
                    OccurrenceKind::CopySensitiveExpression,
                    EdgeDisposition::Defect,
                    suffix,
                    expression,
                );
            }
        }
    }
}

fn domain_for(relative: &Path) -> &'static str {
    if relative.starts_with("crates/glam-gc/") {
        "glam-gc"
    } else {
        "glam"
    }
}

fn is_access_qualified_diagnostic_trait(declaration: &str, implemented_trait: &str) -> bool {
    implemented_trait == "Debug"
        && matches!(
            declaration,
            "src/core.rs::impl Debug for DiagnosticValueDebug"
                | "src/core.rs::impl Debug for DiagnosticValueSliceDebug"
                | "src/core.rs::impl Debug for DiagnosticListDebug"
                | "src/core.rs::impl Debug for DiagnosticDictDebug"
                | "src/core.rs::impl Debug for DiagnosticListThunkDebug"
        )
}

fn collect_occurrences(manifest: &Path) -> Vec<EdgeOccurrence> {
    let mut sources = Vec::new();
    for directory in [
        manifest.join("src"),
        manifest.join("tests"),
        manifest.join("crates/glam-gc/src"),
    ] {
        if directory.exists() {
            collect_rust_sources(&directory, &mut sources);
        }
    }
    sources.sort();

    let mut parsed = Vec::new();
    let mut definitions = Vec::new();
    let mut implementations = Vec::new();
    for source_path in sources {
        let relative = source_path
            .strip_prefix(manifest)
            .expect("a discovered source should belong to the workspace");
        if relative.ends_with("persistent_edge_trait_inventory.rs") {
            continue;
        }
        let source = fs::read_to_string(&source_path)
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", relative.display()));
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} should parse as Rust: {error}", relative.display()));
        let domain = domain_for(relative);
        let mut collector = DefinitionCollector {
            relative,
            domain,
            modules: Vec::new(),
            test_depth: 0,
            base_scope: source_scope(relative),
            definitions: Vec::new(),
            implementations: Vec::new(),
        };
        collector.visit_file(&syntax);
        definitions.extend(collector.definitions);
        implementations.extend(collector.implementations);
        parsed.push((relative.to_path_buf(), domain, syntax));
    }

    let managed = managed_type_declarations(&definitions);
    let mut occurrences = Vec::new();
    for definition in &definitions {
        if definition.name == "ErasedGc" {
            occurrences.push(EdgeOccurrence {
                declaration: definition.declaration.clone(),
                shape: "definition".to_owned(),
                scope: definition.scope,
                surface: EdgeSurface::Erased,
                kind: OccurrenceKind::ErasedIdentity,
                disposition: EdgeDisposition::CollectorPrivateErasedIdentity,
            });
            for derived in &definition.derived_traits {
                occurrences.push(EdgeOccurrence {
                    declaration: definition.declaration.clone(),
                    shape: derived.clone(),
                    scope: definition.scope,
                    surface: EdgeSurface::Erased,
                    kind: OccurrenceKind::TraitDependency,
                    disposition: EdgeDisposition::CollectorPrivateErasedIdentity,
                });
            }
        }
        if !managed.contains(&definition.declaration) {
            continue;
        }
        for derived in &definition.derived_traits {
            occurrences.push(EdgeOccurrence {
                declaration: definition.declaration.clone(),
                shape: derived.clone(),
                scope: definition.scope,
                surface: EdgeSurface::Typed,
                kind: OccurrenceKind::TraitDependency,
                disposition: EdgeDisposition::Defect,
            });
        }
    }
    for implementation in implementations {
        if definitions.iter().any(|definition| {
            definition.domain == implementation.domain
                && definition.source == implementation.source
                && definition.name == implementation.target
                && managed.contains(&definition.declaration)
        }) {
            let disposition = if is_access_qualified_diagnostic_trait(
                &implementation.declaration,
                &implementation.implemented_trait,
            ) {
                EdgeDisposition::AccessQualifiedObservation
            } else {
                EdgeDisposition::Defect
            };
            occurrences.push(EdgeOccurrence {
                declaration: implementation.declaration,
                shape: implementation.implemented_trait,
                scope: implementation.scope,
                surface: EdgeSurface::Typed,
                kind: OccurrenceKind::TraitDependency,
                disposition,
            });
        }
    }

    for (relative, domain, syntax) in parsed {
        let mut visitor = OccurrenceVisitor {
            relative: &relative,
            domain,
            base_scope: source_scope(&relative),
            test_depth: 0,
            modules: Vec::new(),
            items: Vec::new(),
            occurrences: Vec::new(),
        };
        visitor.visit_file(&syntax);
        occurrences.extend(visitor.occurrences);
    }
    occurrences.sort();
    occurrences
}

fn occurrence_fingerprint(occurrences: &[EdgeOccurrence]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut fingerprint = FNV_OFFSET;
    for occurrence in occurrences {
        for byte in occurrence.record().bytes().chain([0xff]) {
            fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
    fingerprint
}

fn occurrence_summary(
    occurrences: &[EdgeOccurrence],
) -> BTreeMap<(SourceScope, EdgeSurface, OccurrenceKind, EdgeDisposition), usize> {
    occurrences
        .iter()
        .fold(BTreeMap::new(), |mut summary, occurrence| {
            *summary
                .entry((
                    occurrence.scope,
                    occurrence.surface,
                    occurrence.kind,
                    occurrence.disposition,
                ))
                .or_default() += 1;
            summary
        })
}

fn current_inventory() -> &'static [EdgeOccurrence] {
    static INVENTORY: OnceLock<Vec<EdgeOccurrence>> = OnceLock::new();
    INVENTORY
        .get_or_init(|| collect_occurrences(Path::new(env!("CARGO_MANIFEST_DIR"))))
        .as_slice()
}

#[test]
fn persistent_edge_trait_occurrence_inventory_is_complete() {
    let actual = current_inventory();

    if std::env::var_os("GLAM_DUMP_PERSISTENT_EDGE_TRAIT_INVENTORY").is_some() {
        for occurrence in actual {
            eprintln!("{}", occurrence.record());
        }
    }

    assert_eq!(
        actual.len(),
        723,
        "persistent-edge occurrence count drifted: {:#?}",
        occurrence_summary(actual)
    );
    assert_eq!(
        occurrence_fingerprint(actual),
        10_565_937_531_246_708_560,
        "persistent-edge occurrence fingerprint drifted: {:#?}",
        occurrence_summary(actual)
    );
}

#[test]
fn persistent_edge_inventory_classifications_are_closed() {
    let actual = current_inventory();
    assert!(
        actual.iter().all(|occurrence| match occurrence.surface {
            EdgeSurface::Erased => {
                occurrence.disposition == EdgeDisposition::CollectorPrivateErasedIdentity
            }
            EdgeSurface::Typed => {
                occurrence.disposition != EdgeDisposition::CollectorPrivateErasedIdentity
                    || occurrence.kind == OccurrenceKind::EraseIdentity
            }
        }),
        "ordinary typed edges and collector-private erased identities must remain distinct"
    );
    assert!(
        actual
            .iter()
            .any(|occurrence| occurrence.scope == SourceScope::Production),
        "production occurrences must remain represented"
    );
    assert!(
        actual
            .iter()
            .any(|occurrence| occurrence.scope == SourceScope::Test),
        "test-only occurrences must remain represented"
    );
    let partitions = actual.iter().fold(
        BTreeMap::<(SourceScope, EdgeSurface), usize>::new(),
        |mut partitions, occurrence| {
            *partitions
                .entry((occurrence.scope, occurrence.surface))
                .or_default() += 1;
            partitions
        },
    );
    assert_eq!(
        partitions,
        BTreeMap::from([
            ((SourceScope::Production, EdgeSurface::Typed), 156),
            ((SourceScope::Production, EdgeSurface::Erased), 36),
            ((SourceScope::Test, EdgeSurface::Typed), 517),
            ((SourceScope::Test, EdgeSurface::Erased), 14),
        ]),
        "production/test and typed/erased inventory partitions drifted"
    );
    assert!(
        actual.iter().all(|occurrence| {
            occurrence.surface != EdgeSurface::Erased
                || occurrence.declaration.starts_with("crates/glam-gc/src/")
        }),
        "ErasedGc escaped the collector-private source boundary"
    );
    assert!(actual.iter().any(|occurrence| {
        occurrence.declaration
            == "crates/glam-gc/src/pointer.rs::tests::pointer_identity_is_all_gc_equality_observes::same_allocation_in"
            && occurrence.kind == OccurrenceKind::PointerIdentity
            && occurrence.disposition == EdgeDisposition::MutatorLocalWorkingDuplicate
    }), "macro-contained edge operations must remain visible to the inventory");
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum RemainingDefectOwner {
    P4CollectorTraitCutover,
    P4ManagedFacadeCutoverAfterD2b,
    D2bCoreCarrierMigration,
}

fn remaining_defect_owner(occurrence: &EdgeOccurrence) -> Option<RemainingDefectOwner> {
    if occurrence.disposition != EdgeDisposition::Defect {
        return None;
    }
    match occurrence.declaration.as_str() {
        declaration if declaration.starts_with("crates/glam-gc/src/pointer.rs::") => {
            Some(RemainingDefectOwner::P4CollectorTraitCutover)
        }
        declaration if declaration.starts_with("src/core/managed/recursive_cells.rs::") => {
            Some(RemainingDefectOwner::P4ManagedFacadeCutoverAfterD2b)
        }
        declaration
            if declaration.starts_with("src/core.rs::")
                || declaration.starts_with("src/core_net.rs::") =>
        {
            Some(RemainingDefectOwner::D2bCoreCarrierMigration)
        }
        _ => None,
    }
}

#[test]
fn remaining_persistent_edge_defects_have_exact_cutover_owners() {
    let defects = current_inventory()
        .iter()
        .filter(|occurrence| occurrence.disposition == EdgeDisposition::Defect)
        .collect::<Vec<_>>();
    assert_eq!(defects.len(), 73);
    assert!(defects.iter().all(|occurrence| matches!(
        occurrence.kind,
        OccurrenceKind::TraitDependency | OccurrenceKind::PointerIdentity
    )));

    let owners = defects
        .iter()
        .fold(BTreeMap::new(), |mut owners, occurrence| {
            let owner = remaining_defect_owner(occurrence).unwrap_or_else(|| {
                panic!(
                    "direct or unassigned persistent-edge defect reopened P2A-P2C: {}",
                    occurrence.record()
                )
            });
            *owners.entry(owner).or_default() += 1;
            owners
        });
    assert_eq!(
        owners,
        BTreeMap::from([
            (RemainingDefectOwner::P4CollectorTraitCutover, 5),
            (RemainingDefectOwner::P4ManagedFacadeCutoverAfterD2b, 13),
            (RemainingDefectOwner::D2bCoreCarrierMigration, 55),
        ])
    );
}

#[test]
fn access_qualified_diagnostic_traits_have_an_exact_closed_set() {
    let observations = current_inventory()
        .iter()
        .filter(|occurrence| occurrence.disposition == EdgeDisposition::AccessQualifiedObservation)
        .map(|occurrence| (occurrence.declaration.as_str(), occurrence.shape.as_str()))
        .collect::<BTreeSet<_>>();

    assert_eq!(
        observations,
        BTreeSet::from([
            ("src/core.rs::impl Debug for DiagnosticValueDebug", "Debug",),
            (
                "src/core.rs::impl Debug for DiagnosticValueSliceDebug",
                "Debug",
            ),
            ("src/core.rs::impl Debug for DiagnosticListDebug", "Debug",),
            ("src/core.rs::impl Debug for DiagnosticDictDebug", "Debug",),
            (
                "src/core.rs::impl Debug for DiagnosticListThunkDebug",
                "Debug",
            ),
        ]),
        "only the access-borrowing D.2b.1d adapters may format a managed value carrier"
    );
}

#[test]
fn collector_p2a_direct_trait_dependencies_are_closed() {
    let collector_defects = current_inventory()
        .iter()
        .filter(|occurrence| {
            occurrence.declaration.starts_with("crates/glam-gc/src/")
                && occurrence.disposition == EdgeDisposition::Defect
        })
        .map(|occurrence| occurrence.declaration.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        collector_defects,
        BTreeSet::from([
            "crates/glam-gc/src/pointer.rs::impl Clone for Gc",
            "crates/glam-gc/src/pointer.rs::impl Copy for Gc",
            "crates/glam-gc/src/pointer.rs::impl Debug for Gc",
            "crates/glam-gc/src/pointer.rs::impl Eq for Gc",
            "crates/glam-gc/src/pointer.rs::impl PartialEq for Gc",
        ]),
        "P2A permits only the five explicitly transitional Gc trait implementations in the collector crate"
    );
}

#[test]
fn glam_p2b_managed_identity_trait_dependencies_are_closed() {
    let is_direct_family = |declaration: &str| {
        matches!(
            declaration,
            "src/core.rs::LazyValue"
                | "src/core.rs::PromisedValue"
                | "src/core.rs::impl Debug for LazyValue"
                | "src/core.rs::impl Debug for PromisedValue"
                | "src/core.rs::impl Eq for LazyValue"
                | "src/core.rs::impl Eq for PromisedValue"
                | "src/core.rs::impl PartialEq for LazyValue"
                | "src/core.rs::impl PartialEq for PromisedValue"
                | "src/core_net.rs::CoreRuntimeNet"
                | "src/core_net.rs::impl Debug for CoreRuntimeNet"
                | "src/core_net.rs::impl Eq for CoreRuntimeNet"
                | "src/core_net.rs::impl PartialEq for CoreRuntimeNet"
                | "src/core/managed/recursive_cells.rs::ManagedLazyEdge"
                | "src/core/managed/recursive_cells.rs::ManagedPromiseEdge"
                | "src/core/managed/recursive_cells.rs::ManagedCoreNetEdge"
                | "src/core/managed/recursive_cells.rs::impl Debug for ManagedLazyEdge"
                | "src/core/managed/recursive_cells.rs::impl Debug for ManagedPromiseEdge"
                | "src/core/managed/recursive_cells.rs::impl Debug for ManagedCoreNetEdge"
                | "src/core/managed/recursive_cells.rs::impl Eq for ManagedLazyEdge"
                | "src/core/managed/recursive_cells.rs::impl Eq for ManagedPromiseEdge"
                | "src/core/managed/recursive_cells.rs::impl Eq for ManagedCoreNetEdge"
                | "src/core/managed/recursive_cells.rs::impl PartialEq for ManagedLazyEdge"
                | "src/core/managed/recursive_cells.rs::impl PartialEq for ManagedPromiseEdge"
                | "src/core/managed/recursive_cells.rs::impl PartialEq for ManagedCoreNetEdge"
        )
    };
    let direct_managed_traits = current_inventory()
        .iter()
        .filter(|occurrence| {
            occurrence.kind == OccurrenceKind::TraitDependency
                && occurrence.disposition == EdgeDisposition::Defect
                && is_direct_family(&occurrence.declaration)
        })
        .map(|occurrence| (occurrence.declaration.clone(), occurrence.shape.clone()))
        .collect::<BTreeSet<_>>();

    let mut expected = BTreeSet::new();
    for edge in [
        "ManagedLazyEdge",
        "ManagedPromiseEdge",
        "ManagedCoreNetEdge",
    ] {
        expected.insert((
            format!("src/core/managed/recursive_cells.rs::{edge}"),
            "Clone".to_owned(),
        ));
        for implemented in ["Eq", "PartialEq"] {
            expected.insert((
                format!("src/core/managed/recursive_cells.rs::impl {implemented} for {edge}"),
                implemented.to_owned(),
            ));
        }
    }
    expected.insert((
        "src/core/managed/recursive_cells.rs::impl Debug for ManagedCoreNetEdge".to_owned(),
        "Debug".to_owned(),
    ));
    for (source, carrier) in [
        ("src/core.rs", "LazyValue"),
        ("src/core.rs", "PromisedValue"),
        ("src/core_net.rs", "CoreRuntimeNet"),
    ] {
        expected.insert((format!("{source}::{carrier}"), "Clone".to_owned()));
        for implemented in ["Debug", "Eq", "PartialEq"] {
            expected.insert((
                format!("{source}::impl {implemented} for {carrier}"),
                implemented.to_owned(),
            ));
        }
    }

    assert_eq!(
        direct_managed_traits, expected,
        "P2B permits only parent-carrier trait interlocks on the three managed identity families"
    );
}

#[test]
fn persistent_edge_type_scanner_distinguishes_phantom_and_erased_identity() {
    let typed: Type = syn::parse_str("Vec<Option<Gc<Node>>>").unwrap();
    assert_eq!(
        type_signals(&typed),
        EdgeSignals {
            typed: 1,
            erased: 0,
        }
    );

    let erased: Type = syn::parse_str("(ErasedGc, Vec<ErasedGc>)").unwrap();
    assert_eq!(
        type_signals(&erased),
        EdgeSignals {
            typed: 0,
            erased: 2,
        }
    );

    let phantom: Type = syn::parse_str("PhantomData<fn() -> Gc<Node>>").unwrap();
    assert_eq!(type_signals(&phantom), EdgeSignals::default());
}

#[test]
fn persistent_edge_inventory_records_every_selected_disposition() {
    let actual = current_inventory();
    let dispositions = actual
        .iter()
        .map(|occurrence| occurrence.disposition)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        dispositions,
        BTreeSet::from([
            EdgeDisposition::FreshUnpublishedEdge,
            EdgeDisposition::TracedPersistentEdge,
            EdgeDisposition::RegisteredRootProjection,
            EdgeDisposition::MutatorLocalWorkingDuplicate,
            EdgeDisposition::AccessQualifiedObservation,
            EdgeDisposition::MutationInput,
            EdgeDisposition::CollectorPrivateErasedIdentity,
            EdgeDisposition::Defect,
        ])
    );
}

impl<'ast> Visit<'ast> for OccurrenceVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let test = is_test_only(&item.attrs);
        self.test_depth += usize::from(test);
        self.modules.push(item.ident.to_string());
        if let Some((_, items)) = &item.content {
            for item in items {
                self.visit_item(item);
            }
        }
        self.modules.pop();
        self.test_depth -= usize::from(test);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let shape = normalized(item);
        for _ in 0..shape.matches("ErasedGc").count() {
            self.record(
                EdgeSurface::Erased,
                OccurrenceKind::ErasedIdentity,
                EdgeDisposition::CollectorPrivateErasedIdentity,
                "erased-import",
                item,
            );
        }
        visit::visit_item_use(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.with_item(item.ident.to_string(), is_test_only(&item.attrs), |this| {
            for (index, field) in item.fields.iter().enumerate() {
                let name = field
                    .ident
                    .as_ref()
                    .map_or_else(|| index.to_string(), ToString::to_string);
                this.record_type(&field.ty, OccurrenceKind::StoredField, &name);
            }
            visit::visit_item_struct(this, item);
        });
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.with_item(item.ident.to_string(), is_test_only(&item.attrs), |this| {
            for variant in &item.variants {
                for (index, field) in variant.fields.iter().enumerate() {
                    let name = field
                        .ident
                        .as_ref()
                        .map_or_else(|| index.to_string(), ToString::to_string);
                    this.record_type(
                        &field.ty,
                        OccurrenceKind::StoredField,
                        &format!("{}::{name}", variant.ident),
                    );
                }
            }
            visit::visit_item_enum(this, item);
        });
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        self.with_item(item.ident.to_string(), is_test_only(&item.attrs), |this| {
            for field in &item.fields.named {
                this.record_type(
                    &field.ty,
                    OccurrenceKind::StoredField,
                    &field
                        .ident
                        .as_ref()
                        .expect("a union field must be named")
                        .to_string(),
                );
            }
            visit::visit_item_union(this, item);
        });
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.with_item(
            item.sig.ident.to_string(),
            is_test_only(&item.attrs),
            |this| {
                this.record_signature(&item.sig);
                visit::visit_item_fn(this, item);
            },
        );
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.with_item(
            item.sig.ident.to_string(),
            is_test_only(&item.attrs),
            |this| {
                this.record_signature(&item.sig);
                visit::visit_impl_item_fn(this, item);
            },
        );
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let syn::Pat::Type(pattern) = &local.pat {
            self.record_type(&pattern.ty, OccurrenceKind::TypedCarrier, "local");
        }
        visit::visit_local(self, local);
    }

    fn visit_type_path(&mut self, path: &'ast syn::TypePath) {
        if path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "ErasedGc")
        {
            self.record(
                EdgeSurface::Erased,
                OccurrenceKind::ErasedIdentity,
                EdgeDisposition::CollectorPrivateErasedIdentity,
                "erased-type",
                path,
            );
        }
        visit::visit_type_path(self, path);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if self.edge_relevant_file() {
            let method = call.method.to_string();
            let classified = match method.as_str() {
                "alloc" | "alloc_with_before_initialize" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::FreshAllocation,
                    EdgeDisposition::FreshUnpublishedEdge,
                )),
                "visit" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::TraceVisit,
                    EdgeDisposition::TracedPersistentEdge,
                )),
                "erase" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::EraseIdentity,
                    EdgeDisposition::CollectorPrivateErasedIdentity,
                )),
                "ptr_eq" if self.typed_pointer_identity() => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::PointerIdentity,
                    EdgeDisposition::Defect,
                )),
                "duplicate_in" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::TypedCarrier,
                    EdgeDisposition::MutatorLocalWorkingDuplicate,
                )),
                "same_allocation_in" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::PointerIdentity,
                    EdgeDisposition::MutatorLocalWorkingDuplicate,
                )),
                "root" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::RootCreation,
                    EdgeDisposition::RegisteredRootProjection,
                )),
                "as_gc" | "project_root" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::RootProjection,
                    EdgeDisposition::RegisteredRootProjection,
                )),
                "with_edge_transition"
                | "with_edge_state_transition"
                | "with_edge_replacement"
                | "replace_edge" => Some((
                    EdgeSurface::Typed,
                    OccurrenceKind::MutationInput,
                    EdgeDisposition::MutationInput,
                )),
                _ => None,
            };
            if let Some((surface, kind, disposition)) = classified {
                self.record(surface, kind, disposition, &method, call);
            }
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_tuple(&mut self, tuple: &'ast syn::ExprTuple) {
        self.record_repeated_paths(&tuple.elems, "repeated-tuple-path");
        visit::visit_expr_tuple(self, tuple);
    }

    fn visit_expr_array(&mut self, array: &'ast syn::ExprArray) {
        self.record_repeated_paths(&array.elems, "repeated-array-path");
        visit::visit_expr_array(self, array);
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        let segments = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if segments.iter().any(|segment| segment == "ErasedGc") {
            self.record(
                EdgeSurface::Erased,
                OccurrenceKind::ErasedIdentity,
                EdgeDisposition::CollectorPrivateErasedIdentity,
                "erased-expression",
                path,
            );
        } else if segments.iter().any(|segment| segment == "Gc")
            && segments.last().is_some_and(|segment| segment == "from_raw")
        {
            self.record(
                EdgeSurface::Typed,
                OccurrenceKind::FreshAllocation,
                EdgeDisposition::FreshUnpublishedEdge,
                "from_raw",
                path,
            );
        }
        visit::visit_expr_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if self.edge_relevant_file() {
            let shape = normalized(mac);
            for (method, kind, disposition) in [
                (
                    "alloc",
                    OccurrenceKind::FreshAllocation,
                    EdgeDisposition::FreshUnpublishedEdge,
                ),
                (
                    "alloc_with_before_initialize",
                    OccurrenceKind::FreshAllocation,
                    EdgeDisposition::FreshUnpublishedEdge,
                ),
                (
                    "visit",
                    OccurrenceKind::TraceVisit,
                    EdgeDisposition::TracedPersistentEdge,
                ),
                (
                    "erase",
                    OccurrenceKind::EraseIdentity,
                    EdgeDisposition::CollectorPrivateErasedIdentity,
                ),
                (
                    "root",
                    OccurrenceKind::RootCreation,
                    EdgeDisposition::RegisteredRootProjection,
                ),
                (
                    "as_gc",
                    OccurrenceKind::RootProjection,
                    EdgeDisposition::RegisteredRootProjection,
                ),
                (
                    "project_root",
                    OccurrenceKind::RootProjection,
                    EdgeDisposition::RegisteredRootProjection,
                ),
                (
                    "duplicate_in",
                    OccurrenceKind::TypedCarrier,
                    EdgeDisposition::MutatorLocalWorkingDuplicate,
                ),
                (
                    "same_allocation_in",
                    OccurrenceKind::PointerIdentity,
                    EdgeDisposition::MutatorLocalWorkingDuplicate,
                ),
                (
                    "with_edge_transition",
                    OccurrenceKind::MutationInput,
                    EdgeDisposition::MutationInput,
                ),
                (
                    "with_edge_state_transition",
                    OccurrenceKind::MutationInput,
                    EdgeDisposition::MutationInput,
                ),
                (
                    "with_edge_replacement",
                    OccurrenceKind::MutationInput,
                    EdgeDisposition::MutationInput,
                ),
                (
                    "replace_edge",
                    OccurrenceKind::MutationInput,
                    EdgeDisposition::MutationInput,
                ),
            ] {
                for _ in 0..shape.matches(&format!(". {method} (")).count() {
                    self.record(EdgeSurface::Typed, kind, disposition, method, mac);
                }
            }
            if self.typed_pointer_identity() {
                for _ in 0..shape.matches(". ptr_eq (").count() {
                    self.record(
                        EdgeSurface::Typed,
                        OccurrenceKind::PointerIdentity,
                        EdgeDisposition::Defect,
                        "ptr_eq",
                        mac,
                    );
                }
            }
            for _ in 0..shape.matches("ErasedGc").count() {
                self.record(
                    EdgeSurface::Erased,
                    OccurrenceKind::ErasedIdentity,
                    EdgeDisposition::CollectorPrivateErasedIdentity,
                    "erased-macro-token",
                    mac,
                );
            }
            for _ in 0..shape.matches("Gc :: from_raw").count() {
                self.record(
                    EdgeSurface::Typed,
                    OccurrenceKind::FreshAllocation,
                    EdgeDisposition::FreshUnpublishedEdge,
                    "from_raw",
                    mac,
                );
            }
        }
        visit::visit_macro(self, mac);
    }
}

impl OccurrenceVisitor<'_> {
    fn record_signature(&mut self, signature: &syn::Signature) {
        let name = signature.ident.to_string();
        let mutation = name.contains("edge_transition") || name.contains("replace_edge");
        let root_creation = matches!(name.as_str(), "root" | "candidate" | "register_root");
        for input in &signature.inputs {
            if let syn::FnArg::Typed(input) = input {
                self.record_type(
                    &input.ty,
                    if mutation {
                        OccurrenceKind::MutationInput
                    } else if root_creation {
                        OccurrenceKind::RootCreation
                    } else {
                        OccurrenceKind::TypedCarrier
                    },
                    "input",
                );
            }
        }
        if let syn::ReturnType::Type(_, output) = &signature.output {
            self.record_type(
                output,
                if matches!(name.as_str(), "as_gc" | "project_root") {
                    OccurrenceKind::RootProjection
                } else if matches!(name.as_str(), "alloc" | "alloc_with_before_initialize") {
                    OccurrenceKind::FreshAllocation
                } else {
                    OccurrenceKind::TypedCarrier
                },
                "output",
            );
        }
    }
}

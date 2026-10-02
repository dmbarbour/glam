//! I3F inventory of every Glam-side managed-heap admission.
//!
//! The core value domain owns the only direct `Heap::with_mutator` calls.
//! Evaluation, compiler, API, and test code enter through one of its two
//! higher-ranked gateways, so mutator authority cannot outlive the callback.
//! Exact per-owner counts make a new admission site an explicit review event;
//! the concurrent collector plan reuses this inventory when these bounded
//! regions become participant epochs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprMethodCall, FnArg, GenericArgument, ImplItemFn, ItemFn, ItemImpl, ItemMod,
    PathArguments, Signature, Type,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GatewayCounts {
    access: usize,
    construction: usize,
}

impl GatewayCounts {
    const fn new(access: usize, construction: usize) -> Self {
        Self {
            access,
            construction,
        }
    }

    fn in_source(source: &str) -> Self {
        Self {
            access: source.matches(".with_runtime_value_access(").count(),
            construction: source.matches(".with_managed_values(").count(),
        }
    }

    fn is_empty(self) -> bool {
        self == Self::new(0, 0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AdmissionSurface {
    RuntimeAccess,
    Construction,
    DirectMutator,
}

impl AdmissionSurface {
    const fn label(self) -> &'static str {
        match self {
            Self::RuntimeAccess => "runtime-access",
            Self::Construction => "construction",
            Self::DirectMutator => "direct-mutator",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AccessCarrier {
    None,
    Runtime,
    Evaluation,
}

impl AccessCarrier {
    const fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Runtime => "runtime",
            Self::Evaluation => "evaluation",
        }
    }

    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Evaluation, _) | (_, Self::Evaluation) => Self::Evaluation,
            (Self::Runtime, _) | (_, Self::Runtime) => Self::Runtime,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdmissionOwner {
    D2bCoreCarriers,
    D2cEvaluator,
    D2dOrchestration,
    D2eFrontend,
    D2fReflection,
    D2gPublicCompilerDiagnostics,
    D2eTestFixtures,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdmissionDisposition {
    OuterAdmission,
    CanonicalGateway,
    DeliberateRecursiveException,
    PendingRegionalReuse,
    PendingRootedTransport,
    TestOnlyCompatibility,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdmissionReview {
    owner: AdmissionOwner,
    disposition: AdmissionDisposition,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AdmissionOccurrence {
    declaration: String,
    surface: AdmissionSurface,
    ordinal: usize,
    test_only: bool,
    nesting: usize,
    carrier: AccessCarrier,
}

impl AdmissionOccurrence {
    fn record(&self) -> String {
        format!(
            "{}#{}|surface={}|scope={}|nested={}|carrier={}",
            self.declaration,
            self.ordinal,
            self.surface.label(),
            if self.test_only { "test" } else { "production" },
            self.nesting,
            self.carrier.label(),
        )
    }

    fn review(&self) -> AdmissionReview {
        if self.test_only {
            return AdmissionReview {
                owner: AdmissionOwner::D2eTestFixtures,
                disposition: AdmissionDisposition::TestOnlyCompatibility,
            };
        }
        let owner = if self.declaration.starts_with("src/core.rs::")
            || self.declaration.starts_with("src/core_net.rs::")
            || self.declaration.starts_with("src/core/")
            || self.declaration.starts_with("src/runtime.rs::")
        {
            AdmissionOwner::D2bCoreCarriers
        } else if self.declaration.starts_with("src/eval/") {
            AdmissionOwner::D2cEvaluator
        } else if self.declaration.starts_with("src/evaluation/") {
            AdmissionOwner::D2dOrchestration
        } else if self.declaration.starts_with("src/g_syntax") {
            AdmissionOwner::D2eFrontend
        } else if self.declaration.starts_with("src/reflection/") {
            AdmissionOwner::D2fReflection
        } else if self.declaration.starts_with("src/api/")
            || self.declaration.starts_with("src/compiler.rs::")
            || self.declaration.starts_with("src/diagnostic.rs::")
            || self.declaration.starts_with("src/source.rs::")
        {
            AdmissionOwner::D2gPublicCompilerDiagnostics
        } else {
            panic!(
                "{} has no GCI11R-002D.2 mutator-introduction owner",
                self.declaration
            );
        };

        let disposition = if self.surface == AdmissionSurface::DirectMutator {
            match self.declaration.as_str() {
                "src/core/managed.rs::impl CoreValueFactory::with_managed_values"
                | "src/core/managed.rs::impl CoreValueFactory::with_runtime_value_access" => {
                    AdmissionDisposition::CanonicalGateway
                }
                _ => panic!(
                    "{} is an unreviewed direct mutator admission",
                    self.declaration
                ),
            }
        } else if self.nesting != 0 || self.carrier != AccessCarrier::None {
            AdmissionDisposition::PendingRegionalReuse
        } else {
            match self.declaration.as_str() {
                "src/api/value.rs::impl Values::with_access"
                | "src/core.rs::impl CoreValueFactory::try_construct_runtime_value_root"
                | "src/evaluation/access.rs::impl EvaluationPollContext::with_value_access" => {
                    AdmissionDisposition::CanonicalGateway
                }
                "src/api/value.rs::impl Value::clone_core_in_own_domain"
                | "src/api/value.rs::impl Values::clone_runtime_root"
                | "src/compiler.rs::impl CompileContext::clone_root"
                | "src/core.rs::impl CoreValueFactory::clone_cached_root"
                | "src/g_syntax.rs::impl Diagnostic::into_emission"
                | "src/g_syntax/compiler_values.rs::project_value"
                | "src/g_syntax/diagnostic_formatter.rs::value"
                | "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::definitions"
                | "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::finish" => {
                    AdmissionDisposition::PendingRootedTransport
                }
                _ => AdmissionDisposition::OuterAdmission,
            }
        };
        AdmissionReview { owner, disposition }
    }
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute
                    .meta
                    .require_list()
                    .is_ok_and(|list| list.tokens.to_string() == "test"))
    })
}

fn type_access_carrier(value_type: &Type) -> AccessCarrier {
    match value_type {
        Type::Path(path) => {
            path.path
                .segments
                .iter()
                .fold(AccessCarrier::None, |carrier, segment| {
                    let direct = match segment.ident.to_string().as_str() {
                        "EvaluationValueAccess" => AccessCarrier::Evaluation,
                        "RuntimeValueAccess" => AccessCarrier::Runtime,
                        _ => AccessCarrier::None,
                    };
                    let arguments =
                        match &segment.arguments {
                            PathArguments::AngleBracketed(arguments) => arguments.args.iter().fold(
                                AccessCarrier::None,
                                |carrier, argument| match argument {
                                    GenericArgument::Type(value_type) => {
                                        carrier.combine(type_access_carrier(value_type))
                                    }
                                    _ => carrier,
                                },
                            ),
                            _ => AccessCarrier::None,
                        };
                    carrier.combine(direct).combine(arguments)
                })
        }
        Type::Reference(reference) => type_access_carrier(&reference.elem),
        Type::Group(group) => type_access_carrier(&group.elem),
        Type::Paren(paren) => type_access_carrier(&paren.elem),
        Type::Tuple(tuple) => tuple
            .elems
            .iter()
            .fold(AccessCarrier::None, |carrier, item| {
                carrier.combine(type_access_carrier(item))
            }),
        // An `impl FnOnce(RuntimeValueAccess)` parameter is an introduction
        // callback, not authority already supplied by the caller. Do not walk
        // arbitrary trait bounds here.
        _ => AccessCarrier::None,
    }
}

fn signature_carrier(signature: &Signature) -> AccessCarrier {
    signature
        .inputs
        .iter()
        .fold(AccessCarrier::None, |carrier, input| match input {
            FnArg::Receiver(_) => carrier,
            FnArg::Typed(input) => carrier.combine(type_access_carrier(&input.ty)),
        })
}

struct AdmissionVisitor {
    path: String,
    scopes: Vec<String>,
    test_scopes: Vec<bool>,
    carrier_scopes: Vec<AccessCarrier>,
    nesting: usize,
    ordinals: BTreeMap<(String, AdmissionSurface), usize>,
    occurrences: Vec<AdmissionOccurrence>,
}

impl AdmissionVisitor {
    fn new(path: String, test_only: bool) -> Self {
        Self {
            path,
            scopes: Vec::new(),
            test_scopes: vec![test_only],
            carrier_scopes: vec![AccessCarrier::None],
            nesting: 0,
            ordinals: BTreeMap::new(),
            occurrences: Vec::new(),
        }
    }

    fn in_test_scope(&self) -> bool {
        self.test_scopes.last().copied().unwrap_or(false)
    }

    fn carrier(&self) -> AccessCarrier {
        self.carrier_scopes
            .last()
            .copied()
            .unwrap_or(AccessCarrier::None)
    }

    fn with_scope(
        &mut self,
        scope: String,
        attributes: &[Attribute],
        carrier: AccessCarrier,
        visit: impl FnOnce(&mut Self),
    ) {
        self.scopes.push(scope);
        self.test_scopes
            .push(self.in_test_scope() || is_test_only(attributes));
        self.carrier_scopes.push(self.carrier().combine(carrier));
        visit(self);
        self.carrier_scopes.pop();
        self.test_scopes.pop();
        self.scopes.pop();
    }

    fn declaration(&self) -> String {
        if self.scopes.is_empty() {
            self.path.clone()
        } else {
            format!("{}::{}", self.path, self.scopes.join("::"))
        }
    }

    fn record(&mut self, surface: AdmissionSurface) {
        let declaration = self.declaration();
        let ordinal = self
            .ordinals
            .entry((declaration.clone(), surface))
            .or_default();
        *ordinal += 1;
        self.occurrences.push(AdmissionOccurrence {
            declaration,
            surface,
            ordinal: *ordinal,
            test_only: self.in_test_scope(),
            nesting: self.nesting,
            carrier: self.carrier(),
        });
    }
}

impl<'ast> Visit<'ast> for AdmissionVisitor {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        self.with_scope(
            node.ident.to_string(),
            &node.attrs,
            AccessCarrier::None,
            |visitor| visit::visit_item_mod(visitor, node),
        );
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        self.with_scope(
            format!("impl {}", node.self_ty.to_token_stream()),
            &node.attrs,
            type_access_carrier(node.self_ty.as_ref()),
            |visitor| visit::visit_item_impl(visitor, node),
        );
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.with_scope(
            node.sig.ident.to_string(),
            &node.attrs,
            signature_carrier(&node.sig),
            |visitor| visit::visit_item_fn(visitor, node),
        );
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.with_scope(
            node.sig.ident.to_string(),
            &node.attrs,
            signature_carrier(&node.sig),
            |visitor| visit::visit_impl_item_fn(visitor, node),
        );
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let surface = match node.method.to_string().as_str() {
            "with_runtime_value_access" => Some(AdmissionSurface::RuntimeAccess),
            "with_managed_values" => Some(AdmissionSurface::Construction),
            "with_mutator" => Some(AdmissionSurface::DirectMutator),
            _ => None,
        };
        if let Some(surface) = surface {
            self.record(surface);
            // The receiver and ordinary arguments are evaluated before the
            // gateway opens its mutator. Only a passed closure's body runs
            // inside the introduced region and therefore contributes lexical
            // nesting for another gateway call.
            self.visit_expr(&node.receiver);
            for argument in &node.args {
                if matches!(argument, syn::Expr::Closure(_)) {
                    self.nesting += 1;
                    self.visit_expr(argument);
                    self.nesting -= 1;
                } else {
                    self.visit_expr(argument);
                }
            }
        } else {
            visit::visit_expr_method_call(self, node);
        }
    }
}

fn is_test_source(relative: &Path) -> bool {
    relative
        .components()
        .any(|component| component.as_os_str() == "tests")
        || relative.file_name().is_some_and(|name| {
            name == "tests.rs"
                || name == "test_support.rs"
                || name.to_string_lossy().ends_with("_inventory.rs")
        })
}

fn collect_admission_occurrences(manifest: &Path) -> Vec<AdmissionOccurrence> {
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    let inventory_path = Path::new("src/evaluation/access_inventory.rs");
    let mut occurrences = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a source path should belong to this package");
        if relative == inventory_path || relative.starts_with("src/bin") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("Rust source should be readable");
        let syntax = syn::parse_file(&source).expect("an inventoried source should parse");
        let mut visitor = AdmissionVisitor::new(
            relative
                .to_str()
                .expect("repository paths should be UTF-8")
                .replace('\\', "/"),
            is_test_source(relative),
        );
        visitor.visit_file(&syntax);
        occurrences.extend(visitor.occurrences);
    }
    occurrences.sort();
    occurrences
}

fn admission_occurrence_fingerprint(occurrences: &[AdmissionOccurrence]) -> u64 {
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

#[test]
fn all_managed_entries_have_bounded_mutator_regions() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let inventory_path = Path::new("src/evaluation/access_inventory.rs");
    let mut actual_gateways = BTreeMap::new();
    let mut direct_entries = BTreeMap::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a source path should belong to this package");
        if relative == inventory_path {
            continue;
        }
        let source = fs::read_to_string(&path).expect("Rust source should be readable");
        let gateways = GatewayCounts::in_source(&source);
        if !gateways.is_empty() {
            actual_gateways.insert(relative.to_path_buf(), gateways);
        }
        let direct = source.matches(".with_mutator(").count();
        if direct != 0 {
            direct_entries.insert(relative.to_path_buf(), direct);
        }
    }

    let gateway_totals =
        actual_gateways
            .values()
            .fold(GatewayCounts::new(0, 0), |totals, counts| {
                GatewayCounts::new(
                    totals.access + counts.access,
                    totals.construction + counts.construction,
                )
            });
    assert_eq!(
        gateway_totals,
        GatewayCounts::new(533, 17),
        "managed gateway occurrence totals drifted",
    );
    assert_eq!(
        direct_entries,
        [(PathBuf::from("src/core/managed.rs"), 2)]
            .into_iter()
            .collect(),
        "raw mutator admission must remain private to the core value domain"
    );

    let owner = fs::read_to_string(manifest.join("src/core/managed.rs"))
        .expect("the managed value-domain owner should be readable");
    assert_eq!(
        owner.matches("impl for<'scope> FnOnce(").count(),
        2,
        "both managed gateways must preserve a higher-ranked callback scope"
    );
}

#[test]
fn every_mutator_introduction_has_an_exact_disposition() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let occurrences = collect_admission_occurrences(manifest);
    let actual = occurrences
        .iter()
        .map(AdmissionOccurrence::record)
        .collect::<Vec<_>>();
    let reviews = occurrences
        .iter()
        .map(AdmissionOccurrence::review)
        .collect::<Vec<_>>();
    if std::env::var_os("GLAM_DUMP_MUTATOR_INTRODUCTION_INVENTORY").is_some() {
        for (occurrence, review) in occurrences.iter().zip(&reviews) {
            eprintln!(
                "{}|owner={:?}|disposition={:?}",
                occurrence.record(),
                review.owner,
                review.disposition
            );
        }
    }
    assert_eq!(
        actual.len(),
        537,
        "managed mutator-introduction count drifted"
    );
    // D.2h.4c.2 adds two test-only bounded accesses: one constructs
    // structured ownership-fixture failures, and one inspects the structured
    // killed-work diagnostic without relying on Rust Display policy.
    // D.2h.4c.2b.2 adds six more bounded fixture accesses: five retain exact
    // promise/lazy identities across terminal transitions, while one creates
    // a host-call lazy and its public value root in the same access region.
    // D.2h.4c.2b.3 then replaces three self-opening reflection helpers with
    // same-region rooted builders, retiring three separate test admissions.
    // I12A adds three test-only construction regions for recoverable trace,
    // recoverable finalizer, and pressure-threshold maintenance fixtures.
    assert_eq!(
        admission_occurrence_fingerprint(&occurrences),
        926_874_688_153_584_830,
        "managed mutator-introduction source fingerprint drifted"
    );

    assert_eq!(
        occurrences
            .iter()
            .zip(&reviews)
            .filter(|(occurrence, review)| {
                review.disposition == AdmissionDisposition::CanonicalGateway
                    && occurrence.surface == AdmissionSurface::DirectMutator
            })
            .count(),
        2,
        "only the two CoreValueFactory gateways may directly enter the collector"
    );
    assert_eq!(
        reviews
            .iter()
            .filter(|review| {
                review.disposition == AdmissionDisposition::DeliberateRecursiveException
            })
            .count(),
        0,
        "production currently requires no recursive mutator-introduction exception"
    );
    assert!(
        occurrences
            .iter()
            .zip(&reviews)
            .all(|(occurrence, review)| {
                (occurrence.test_only
                    && review.owner == AdmissionOwner::D2eTestFixtures
                    && review.disposition == AdmissionDisposition::TestOnlyCompatibility)
                    || (!occurrence.test_only
                        && review.owner != AdmissionOwner::D2eTestFixtures
                        && review.disposition != AdmissionDisposition::TestOnlyCompatibility)
            }),
        "production and test-only mutator introductions must remain distinct"
    );
    assert_eq!(
        occurrences
            .iter()
            .filter(|occurrence| !occurrence.test_only && occurrence.nesting != 0)
            .count(),
        0,
        "production currently contains no lexically nested mutator introduction"
    );
    assert_eq!(
        occurrences
            .iter()
            .filter(|occurrence| {
                !occurrence.test_only && occurrence.carrier != AccessCarrier::None
            })
            .count(),
        0,
        "no production API which directly accepts access may reopen its value domain"
    );
    let production_disposition_count = |disposition| {
        occurrences
            .iter()
            .zip(&reviews)
            .filter(|(occurrence, review)| {
                !occurrence.test_only && review.disposition == disposition
            })
            .count()
    };
    assert_eq!(
        production_disposition_count(AdmissionDisposition::CanonicalGateway),
        5
    );
    assert_eq!(
        production_disposition_count(AdmissionDisposition::PendingRootedTransport),
        0
    );
    assert_eq!(
        production_disposition_count(AdmissionDisposition::PendingRegionalReuse),
        0
    );
    assert_eq!(
        production_disposition_count(AdmissionDisposition::OuterAdmission),
        54
    );
}

#[test]
fn mutator_inventory_distinguishes_input_authority_from_introduction_callbacks() {
    let syntax = syn::parse_file(
        r#"
        fn runtime_input(access: &RuntimeValueAccess<'_>) {
            values.with_runtime_value_access(|_| ());
        }
        fn evaluation_input(access: Option<&EvaluationValueAccess<'_>>) {
            values.with_runtime_value_access(|_| ());
        }
        fn introduction_callback(
            operation: impl for<'scope> FnOnce(RuntimeValueAccess<'scope>),
        ) {
            values.with_runtime_value_access(|_| ());
        }
        fn nested() {
            values.with_runtime_value_access(|_| {
                values.with_runtime_value_access(|_| ());
            });
        }
        "#,
    )
    .expect("the mutator inventory fixture should parse");
    let mut visitor = AdmissionVisitor::new("fixture.rs".to_owned(), false);
    visitor.visit_file(&syntax);
    visitor.occurrences.sort();

    let actual = visitor
        .occurrences
        .iter()
        .map(AdmissionOccurrence::record)
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            "fixture.rs::evaluation_input#1|surface=runtime-access|scope=production|nested=0|carrier=evaluation",
            "fixture.rs::introduction_callback#1|surface=runtime-access|scope=production|nested=0|carrier=none",
            "fixture.rs::nested#1|surface=runtime-access|scope=production|nested=0|carrier=none",
            "fixture.rs::nested#2|surface=runtime-access|scope=production|nested=1|carrier=none",
            "fixture.rs::runtime_input#1|surface=runtime-access|scope=production|nested=0|carrier=runtime",
        ]
    );
}

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
                | "src/evaluation/access.rs::impl EvaluationPollContext::with_value_access"
                | "src/evaluation/access.rs::impl EvaluatorStepContext < '_ >::with_value_access" => {
                    AdmissionDisposition::CanonicalGateway
                }
                "src/api/value.rs::impl Value::clone_core_in_own_domain"
                | "src/api/value.rs::impl Values::clone_runtime_root"
                | "src/compiler.rs::impl CompileContext::clone_root"
                | "src/core.rs::impl CoreValueFactory::clone_cached_root"
                | "src/evaluation/session.rs::impl EvalContext::clone_root"
                | "src/g_syntax.rs::impl Diagnostic::into_emission"
                | "src/g_syntax/compiler_values.rs::project_value"
                | "src/g_syntax/diagnostic_formatter.rs::value"
                | "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::definitions"
                | "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::finish"
                | "src/reflection/machine.rs::impl Branch < S >::new" => {
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

const EXPECTED_ADMISSION_OCCURRENCES: &[&str] = &[
    "src/api/assembly.rs::impl Assembler::seal_module#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/api/tests.rs::runtime_value_domain_has_no_scheduler_or_profile_backedge#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/api/tests.rs::runtime_value_domain_has_no_scheduler_or_profile_backedge#2|surface=construction|scope=test|nested=0|carrier=none",
    "src/api/value.rs::impl PromiseResolver::drop#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/api/value.rs::impl PromiseResolver::fail#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/api/value.rs::impl PromiseResolver::fail_with#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/api/value.rs::impl PromiseResolver::resolve#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/api/value.rs::impl Value::clone_core_in_own_domain#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/api/value.rs::impl Values::clone_runtime_root#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/api/value.rs::impl Values::with_access#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/compiler.rs::impl CompileContext::clone_root#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/compiler.rs::impl CompileContext::import_binary#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/compiler.rs::impl CompileContext::import_module#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::cache_test_lazy#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl CoreValueFactory::cached#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/core.rs::impl CoreValueFactory::clone_cached_root#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/core.rs::impl CoreValueFactory::root_core_net#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl CoreValueFactory::try_construct_runtime_value_root#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/core.rs::impl CoreValues::new#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/core.rs::impl HostCallRootBundle::from_captures#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::cached#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::computed_fixpoint#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::error#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::external_host_call#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::from_access#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::from_application#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::from_net_computation#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::from_net_construction#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::from_reflection_gate#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::id#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::root#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::semantic_computation#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::semantic_thunk#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl LazyValue::source_snapshot#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::assignment#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::exact_subscription_count#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::fixpoint#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::fixpoint#2|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::id#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::root#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::task#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl PromisedValue::with_cell#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl Value::builtin_call#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::impl Value::reflection_task_result#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::publish_test_promise#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::access_qualified_diagnostic_debug_is_recursive_hidden_and_non_demanding#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::access_qualified_key_conversion_is_non_demanding_and_round_trips_strict_data#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::access_qualified_representation_comparison_is_structural_and_non_demanding#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::access_qualified_value_duplication_preserves_managed_identity_without_rooting#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::canonical_cache_releases_with_the_last_value_domain_owner#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::factory_scoped_allocation_uses_current_mutator#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::factory_scoped_allocation_uses_current_mutator#2|surface=construction|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::managed_family_requested_layout_is_accepted#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::managed_family_requested_layout_is_accepted#2|surface=construction|scope=test|nested=0|carrier=none",
    "src/core.rs::tests::scoped_factory_does_not_retain_allocator_or_scheduler#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed.rs::impl CoreValueFactory::rooted_error_lazy_for_test#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed.rs::impl CoreValueFactory::with_managed_values#1|surface=direct-mutator|scope=production|nested=0|carrier=none",
    "src/core/managed.rs::impl CoreValueFactory::with_runtime_value_access#1|surface=direct-mutator|scope=production|nested=0|carrier=none",
    "src/core/managed.rs::tests::allocate_fixture#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed.rs::tests::managed_drop_has_no_runtime_or_heap_capability#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed.rs::tests::runtime_value_access_rejects_an_owner_from_another_heap_before_mutation#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed.rs::tests::runtime_value_access_rejects_an_owner_from_another_heap_before_mutation#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed.rs::tests::runtime_value_access_routes_borrowed_edge_sets_to_the_collector_gateway#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/active_owner_inventory.rs::every_real_value_variant_has_passive_managed_destruction#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed/active_owner_inventory.rs::every_real_value_variant_has_passive_managed_destruction#2|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed/active_owner_inventory.rs::production_reflection_gate_target_backedge_reclaims_without_an_external_root#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/active_owner_inventory.rs::production_reflection_result_edges_do_not_need_an_external_root#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/payload_edges/managed.rs::tests::nested_compatibility_owners_report_the_synthetic_managed_leaf#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed/payload_edges/managed.rs::tests::recursive_identity_stops_do_not_enter_raw_cells_or_nets#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed/payload_edges/persistent.rs::tests::persistent_adapter_cycle_reclaims_in_isolated_heap#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed/payload_edges/runtime_net.rs::tests::generic_runtime_net_payload_cycle_marks_exactly#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/core/managed/payload_edges/runtime_net.rs::tests::managed_core_net_trace_does_not_reduce_materialize_or_force#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::assert_lazy_promise_cycle_through_source#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::assert_single_promise_cycle_through#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::assert_single_promise_failure_cycle_through#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_core_net_gateway_preserves_cell_mutation_publication#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_core_net_gateway_preserves_cell_mutation_publication#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_core_net_gateway_preserves_cell_mutation_publication#3|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_lazy_gateway_preserves_terminal_publication_protocol#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_lazy_gateway_preserves_terminal_publication_protocol#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_lazy_gateway_preserves_terminal_publication_protocol#3|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_promise_gateway_preserves_one_terminal_winner#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_promise_gateway_preserves_one_terminal_winner#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::bounded_promise_gateway_preserves_one_terminal_winner#3|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::core_net_reduction_enters_the_same_managed_transition_gateway#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::core_net_reduction_enters_the_same_managed_transition_gateway#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::lazy_failure_transition_reports_failure_edges_and_releases_source#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::lazy_success_transition_reports_source_removal_and_terminal_addition_without_forcing#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::losing_promise_publisher_reports_its_proposed_addition_without_changing_the_winner#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::losing_promise_publisher_reports_its_proposed_addition_without_changing_the_winner#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::losing_promise_publisher_reports_its_proposed_addition_without_changing_the_winner#3|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::losing_promise_publisher_reports_its_proposed_addition_without_changing_the_winner#4|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_core_net_source_self_cycle_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_core_net_stuck_reason_self_cycle_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_lazy_core_net_pair_cycle_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_lazy_net_promise_ring_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_lazy_source_self_cycle_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_operator_payload_cycle_survives_ready_and_claimed_work_then_reclaims::assert_state#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_promise_assignment_self_cycle_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_promise_core_net_pair_cycle_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_promise_cycle_through_function_stage_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::managed_promise_cycle_through_remote_cursor_source_is_traced_and_reclaimed#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::promise_publication_callbacks_observe_assignment_before_wake_detachment#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::recursive_cell_family_contracts_and_registered_root_lifecycles#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::recursive_cell_family_contracts_and_registered_root_lifecycles#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::recursive_cell_family_contracts_and_registered_root_lifecycles#3|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::semantic_edges_and_durable_roots_share_one_managed_identity#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::semantic_edges_and_durable_roots_share_one_managed_identity#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/recursive_cells.rs::tests::semantic_edges_and_durable_roots_share_one_managed_identity#3|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/value_node.rs::tests::managed_value_node_dispatches_every_real_variant_with_exact_recursive_edges#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/value_node.rs::tests::managed_value_node_dispatches_every_real_variant_with_exact_recursive_edges#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/value_node.rs::tests::managed_value_node_family_contract_and_lifecycle#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/value_node.rs::tests::managed_value_node_family_contract_and_lifecycle#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/value_node.rs::tests::prepare#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/value_node.rs::tests::prepared_root_projection_nests_inside_one_runtime_access_region#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core/managed/value_node.rs::tests::prepared_root_projection_nests_inside_one_runtime_access_region#2|surface=runtime-access|scope=test|nested=1|carrier=none",
    "src/core/managed/value_node.rs::tests::project#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::impl CorePreparedCopySource::into_inner_for_factory#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::impl CoreRuntimeNet::duplicate_for_test#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::impl CoreRuntimeNet::with_test_access#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::impl CoreValueFactory::construct_core_runtime_net_for_test#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::impl CoreValueFactory::instantiate_core_net#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::core_contention_does_not_retain_the_semantic_net#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::core_cursor_step_rejects_a_live_claim#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::core_net_access_rejects_a_foreign_runtime#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::core_net_matching_access_reads_its_managed_cell#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::identity_only_net_work_outlives_scoped_access#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::identity_only_net_work_outlives_scoped_access#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::scoped_normalization_batch_closes_and_publishes_once#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::scoped_normalization_batch_closes_on_unwind#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::scoped_normalization_batch_wakes_forced_concurrent_followers#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::scoped_normalization_batch_wakes_forced_concurrent_followers#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/core_net.rs::tests::scoped_normalization_batch_wakes_forced_concurrent_followers#3|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/diagnostic.rs::apply_updates#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/eval/builtins/provenance.rs::apply#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/eval/net.rs::driver_tests::contending_evaluator_hands_off_then_resumes_after_batch_publication#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/net.rs::driver_tests::contending_evaluator_hands_off_then_resumes_after_batch_publication#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/net.rs::driver_tests::cursor_dependency_work_orders_child_before_parent_retry#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/net.rs::driver_tests::cursor_driver_releases_each_runtime_before_crossing_to_the_next#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/operator.rs::constant_effect#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/tests.rs::abandoned_reflection_activation_permit_discards_reserved_work_before_owner_drain#1|surface=construction|scope=test|nested=0|carrier=none",
    "src/eval/tests.rs::compiled_function_values_reuse_one_shared_interaction_net#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/tests.rs::curried_function_partial_application_retains_a_shared_stage#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/tests.rs::deferred_computation_caches_one_structured_failure#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/eval/tests.rs::immediate_diagnostic_shell_operations_share_one_root_neutral_access_region#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/access.rs::impl EvaluationPollContext::with_value_access#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/access.rs::impl EvaluatorStepContext < '_ >::with_value_access#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/access.rs::tests::different_heap_authority_is_rejected#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/access.rs::tests::runtime_tls_caches_remain_heap_qualified#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/access.rs::tests::runtime_tls_caches_remain_heap_qualified#2|surface=runtime-access|scope=test|nested=1|carrier=none",
    "src/evaluation/coordinator.rs::impl TaskOwnedPromiseObligation::publish_failure_guarded#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/coordinator/task.rs::impl LocalPromiseOwner::fail_all#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/executor.rs::tests::worker_termination_releases_inactive_collector_caches#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/pump.rs::poison_lazy_cycle#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/session.rs::impl EvalContext::clone_root#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/session.rs::impl EvalContext::lazy_task#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/session.rs::impl EvalContext::promise_task#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/evaluation/tests.rs::assigned_task_promise_is_removed_before_later_task_terminalization#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/tests.rs::impl AssignPromiseAfterRelease::poll#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/tests.rs::impl AssignPromiseThenYield::poll#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/tests.rs::impl CacheLazyFailure::poll#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/tests.rs::promise_follow_reprojects_its_rooted_assignment_across_polls#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/tests.rs::promise_follow_reprojects_its_rooted_assignment_across_polls#2|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/evaluation/tests.rs::synchronous_client_demand_waits_for_worker_owned_runtime_progress#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/g_syntax.rs::impl Diagnostic::into_emission#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/g_syntax/compiler_values.rs::project_value#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/g_syntax/diagnostic_formatter.rs::value#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::definitions#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::finish#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::lower_declaration#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/g_syntax/net_lowering.rs::impl ResolvedNetLowerer < 'access , 'scope >::lower_code#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/g_syntax/net_lowering.rs::impl ResolvedNetLowerer < 'access , 'scope >::lower_template#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/g_syntax/net_lowering.rs::lower_resolved_expr#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/reflection/lifecycle.rs::combine_composed_result#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/reflection/machine.rs::impl Branch < S >::new#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/reflection/machine.rs::impl EffectTask < S >::deliver_step#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/reflection/machine.rs::impl EffectTask < S >::start_fixpoint#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/reflection/protocol.rs::root_inventory_tests::public_context_roots_a_bounded_evaluation_failure#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/reflection/protocol.rs::root_inventory_tests::structured_api_error_preserves_its_runtime_root#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/runtime.rs::impl RuntimeFailureRoot::new#1|surface=runtime-access|scope=production|nested=0|carrier=none",
    "src/runtime.rs::impl RuntimeValueRoot::clone_core_for_test#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/runtime.rs::impl RuntimeValueRoot::new#1|surface=runtime-access|scope=test|nested=0|carrier=none",
    "src/runtime.rs::tests::runtime_failure_root_alone_retains_and_releases_its_managed_values#1|surface=runtime-access|scope=test|nested=0|carrier=none",
];

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

    let expected_gateways = [
        // D.2b.2 publishes imported bytes and the final module promise through
        // one explicit bounded value region.
        ("src/api/assembly.rs", GatewayCounts::new(1, 0)),
        ("src/api/tests.rs", GatewayCounts::new(0, 2)),
        // D.2b.2 removes authority-free promise publication and public-root
        // reprojection. Resolver success, structured failure, textual failure,
        // and drop each use a local region; public projection explicitly
        // upgrades its weak observer.
        ("src/api/value.rs", GatewayCounts::new(7, 0)),
        ("src/compiler.rs", GatewayCounts::new(3, 0)),
        // I4.0's owner-local destruction fixtures exercise the admitted
        // construction gateway; production allocation still enters through
        // the same higher-ranked scope. GCI5R-002B's scoped gateway and
        // foreign-owner fixtures add three matching-domain access regions.
        // I8A.2's rooted-lazy transition fixture uses the same explicit test
        // gateway instead of exposing recursive-cell construction to sibling
        // modules.
        ("src/core/managed.rs", GatewayCounts::new(4, 2)),
        // I4F.2b's test-only passive-closure matrix allocates each real
        // compatibility value variant through the same bounded gateway.
        // GCI5R-005A/E add production effect- and target-backedge reflection
        // cycles plus matching-runtime source inspection.
        (
            "src/core/managed/active_owner_inventory.rs",
            GatewayCounts::new(2, 2),
        ),
        (
            "src/core/managed/payload_edges/persistent.rs",
            GatewayCounts::new(0, 1),
        ),
        // I5B's synthetic managed-leaf fixtures construct closed graphs to
        // verify transitive compatibility traversal and identity stops.
        (
            "src/core/managed/payload_edges/managed.rs",
            GatewayCounts::new(0, 2),
        ),
        (
            "src/core/managed/payload_edges/runtime_net.rs",
            // I8C retired two bounded compatibility-projection fixtures. The
            // remaining access constructs the closed generic visitor fixture.
            GatewayCounts::new(1, 1),
        ),
        // I5D's recursive-cell cutover and I5F's closed self- and cross-family
        // cycle fixtures use matching-domain access for construction,
        // observation, publication, mutation-gateway installation, and root
        // projection. GCI5R-002 added deterministic production-writer probes
        // for lazy, promise, and synchronized net transitions through that
        // same bounded access surface. I8A.2 retires the two-access arbitrary
        // whole-net replacement fixture with the whole-net mutation bridge.
        // I8B adds two isolated managed-net cycle fixtures, each with one
        // construction/publication region.
        (
            "src/core/managed/recursive_cells.rs",
            // D.2b.2 removes two authority-free prepared-root round trips;
            // their callers now reuse an already-open region.
            GatewayCounts::new(37, 0),
        ),
        // I4F.2c keeps the production-shaped node and prepared root private
        // while their local lifecycle, provenance, and nested-access fixtures
        // exercise construction and observation.
        ("src/core/managed/value_node.rs", GatewayCounts::new(8, 0)),
        // I5D routes lazy and promise cell construction and access through the
        // same bounded domain gateway as reflection-value projection.
        // GCI5R-001B adds one synchronous construction entry which publishes
        // its returned graph before that same bounded access ends. GCI5R-003E
        // adds the explicit producer-installation gateway plus test-only
        // promise/lazy publication helpers; semantic facades no longer reopen
        // managed access to mutate themselves. GCI5R-003F removes their weak
        // observers; seven test-only inspection helpers now require an
        // explicit matching factory and enter through this counted gateway.
        // GCI5R-005B removes the former external reflection-root projection.
        // I10A adds one bounded access region which turns a HostCall's
        // declared semantic captures into its one-shot external root bundle.
        // Closed runtime-cache candidates keep one outer access region until
        // their declared runtime roots have been installed. D.2b.1a adds one
        // focused region proving raw shell duplication preserves managed
        // identity without registering a root. D.2b.1b adds one sibling
        // region proving key conversion rejects a deferred list segment
        // without evaluating it. D.2b.1c adds one comparison region covering
        // structural containers and exact managed identity. D.2b.1d adds one
        // non-demanding recursive diagnostic-rendering region.
        // D.2b.2 constructs the canonical runtime roots in one additional
        // shared region.
        ("src/core.rs", GatewayCounts::new(37, 5)),
        // I5D scopes every managed core-net construction, root handoff, and
        // source-frontier traversal through matching value-domain authority.
        // GCI5R-008's test-only prepared-source bridge reopens the matching
        // runtime solely to project a root-owned source for generic fixtures.
        // P2B moves exact net identity observation behind the same bounded
        // access authority as topology inspection. P2C adds one explicit
        // test-only duplicate gateway and roots a net before a worker handoff.
        ("src/core_net.rs", GatewayCounts::new(16, 0)),
        ("src/diagnostic.rs", GatewayCounts::new(1, 0)),
        // D.2b.2 gives provenance-generated halt context an explicit bounded
        // value region.
        ("src/eval/builtins/provenance.rs", GatewayCounts::new(1, 0)),
        // P2B compares registered net roots under the same explicit access
        // authority used by production normalization batches. P2C adds
        // explicit test-only duplicate/root handoffs for cursor-driver and
        // concurrent normalization fixtures.
        ("src/eval/net.rs", GatewayCounts::new(6, 0)),
        ("src/eval/operator.rs", GatewayCounts::new(1, 0)),
        // Reflection evaluator fixtures construct their managed wrapper under
        // one bounded access region. P2B's two shared-function-stage checks
        // compare managed-net identity under matching access.
        // D.2b.2's structured deferred-failure fixture constructs its halt
        // payload in one explicit access region.
        ("src/eval/tests.rs", GatewayCounts::new(4, 1)),
        ("src/evaluation/access.rs", GatewayCounts::new(5, 0)),
        // D.2b.2 terminal promise assignment is access-qualified before its
        // detached completion wake is delivered.
        (
            "src/evaluation/coordinator/task.rs",
            GatewayCounts::new(1, 0),
        ),
        // Promise terminalization projects a managed assignment through the
        // producer root while the coordinator mutation remains admitted.
        ("src/evaluation/coordinator.rs", GatewayCounts::new(1, 0)),
        ("src/evaluation/executor.rs", GatewayCounts::new(1, 0)),
        // GCI5R-003E publishes a strict lazy-cycle failure through all of the
        // already-retained producer roots in one bounded batch.
        ("src/evaluation/pump.rs", GatewayCounts::new(1, 0)),
        // GCI5R-003D roots lazy and promise producers before coordinator
        // admission instead of letting either semantic façade reopen access.
        ("src/evaluation/session.rs", GatewayCounts::new(3, 0)),
        // Production-shaped task fixtures retain lazy/promise roots and use
        // explicit matching-domain access rather than facade mutation.
        // D.2b.2's production-shaped promise publishers install assignments
        // under matching access before detached wakes.
        ("src/evaluation/tests.rs", GatewayCounts::new(7, 0)),
        // GCI11R-002C returns the client-demand result root directly, removing
        // the projection/re-root access gap from closed compiler evaluation.
        ("src/g_syntax/compiler_values.rs", GatewayCounts::new(1, 0)),
        (
            "src/g_syntax/diagnostic_formatter.rs",
            GatewayCounts::new(1, 0),
        ),
        // D.2b.2 roots diagnostic emission through the compiler domain's
        // explicit local region.
        ("src/g_syntax.rs", GatewayCounts::new(1, 0)),
        ("src/g_syntax/module_lowering.rs", GatewayCounts::new(3, 0)),
        ("src/g_syntax/net_lowering.rs", GatewayCounts::new(3, 0)),
        // GCI5R-003D roots a freshly constructed reflection fixpoint before
        // publishing it into branch/coordinator state.
        // D.2b.2 adds access-qualified branch-root construction and reflection
        // fixpoint publication without carrying access across machine polls.
        // D.2c.1a projects a composed child failure through one short
        // diagnostic region after child settlement has completed.
        ("src/reflection/lifecycle.rs", GatewayCounts::new(1, 0)),
        ("src/reflection/machine.rs", GatewayCounts::new(3, 0)),
        // Structured halt fixtures and production conversion now construct
        // their raw payloads only within explicit regions.
        ("src/reflection/protocol.rs", GatewayCounts::new(2, 0)),
        // I6C's isolated failure-root lifecycle fixture constructs its managed
        // promise in one explicit region before publishing the durable root.
        // D.2b.2 replaces authority-free failure-root and test projection
        // helpers with explicit bounded regions.
        ("src/runtime.rs", GatewayCounts::new(4, 0)),
    ]
    .into_iter()
    .map(|(path, counts)| (PathBuf::from(path), counts))
    .collect::<BTreeMap<_, _>>();

    assert_eq!(
        actual_gateways.keys().collect::<Vec<_>>(),
        expected_gateways.keys().collect::<Vec<_>>(),
        "managed gateway owners drifted"
    );
    for (path, expected) in &expected_gateways {
        assert_eq!(
            actual_gateways.get(path),
            Some(expected),
            "managed gateway count drifted for {}",
            path.display()
        );
    }
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
    let expected = EXPECTED_ADMISSION_OCCURRENCES
        .iter()
        .map(|record| (*record).to_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        actual, expected,
        "managed mutator-introduction ledger drifted"
    );

    let reviews = occurrences
        .iter()
        .map(AdmissionOccurrence::review)
        .collect::<Vec<_>>();
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
        6
    );
    assert_eq!(
        production_disposition_count(AdmissionDisposition::PendingRootedTransport),
        11
    );
    assert_eq!(
        production_disposition_count(AdmissionDisposition::PendingRegionalReuse),
        0
    );
    assert_eq!(
        production_disposition_count(AdmissionDisposition::OuterAdmission),
        22
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

//! W0B source-backed census of recursive WHNF work and suspension boundaries.
//!
//! This inventory complements the D.2c raw-`Value` inventory: it records the
//! control-flow operation at each occurrence, plus the reviewed resumption
//! role which must survive the WHNF trampoline transition. It intentionally
//! scans orchestration adapters only where they translate evaluator halts or
//! host resumptions; it is not a second inventory of the entire scheduler.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprCall, ExprForLoop, ExprLoop, ExprMethodCall, ExprWhile, ImplItemFn, ItemFn,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Signal {
    EvalValue,
    EvalLazy,
    EvalPromise,
    ApplyValue,
    ApplyValues,
    ProduceLazySource,
    ReflectionEvaluate,
    RetryableWait,
    UnassignedPromise,
    DependencyTranslation,
    CoordinatorBoundary,
    ReflectionBoundary,
    HostBoundary,
    NetBoundary,
    StructuralRecursion,
    UserSizedLoop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OuterOwner {
    PureEvaluator,
    LazyTask,
    ClientDemand,
    Spark,
    ReflectionMachine,
    NetWorklist,
    NetConstruction,
    CoordinatorAdapter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResultDisposition {
    ReturnWhnf,
    PublishLazyCache,
    PublishClientDemand,
    DiscardSparkResult,
    ParseReflectionRequest,
    ContinueReflectionPhase,
    ContinueNetWork,
    TranslateDependency,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StableOwner {
    InputValue,
    LazyIdentity,
    PromiseIdentity,
    ClientDemandRecord,
    ReflectionTaskRecord,
    NetMachine,
    CoordinatorRecord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DependencyKind {
    None,
    LazyWait,
    PromiseAssignment,
    GenericWait,
    ReflectionTask,
    Host,
    Net,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum WorkShape {
    TailDemand,
    DemandThenInspect,
    OrderedOperands,
    CollectionWalk,
    Application,
    KeyConversion,
    AccessPath,
    DiagnosticContext,
    OrchestrationHandoff,
}

/// Compile-exhaustive latch for W0C's selected shared work-stack vocabulary.
/// Production payloads arrive in W1; changing this set first requires updating
/// the census-backed representation decision in the plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectedWorkVariant {
    Delegate,
    DemandThenInspect,
    OrderedOperands,
    CollectionWalk,
    Application,
    KeyConversion,
    AccessPath,
    DiagnosticContext,
    OrchestrationHandoff,
}

fn selected_variant_shape(variant: SelectedWorkVariant) -> Option<WorkShape> {
    match variant {
        SelectedWorkVariant::Delegate => None,
        SelectedWorkVariant::DemandThenInspect => Some(WorkShape::DemandThenInspect),
        SelectedWorkVariant::OrderedOperands => Some(WorkShape::OrderedOperands),
        SelectedWorkVariant::CollectionWalk => Some(WorkShape::CollectionWalk),
        SelectedWorkVariant::Application => Some(WorkShape::Application),
        SelectedWorkVariant::KeyConversion => Some(WorkShape::KeyConversion),
        SelectedWorkVariant::AccessPath => Some(WorkShape::AccessPath),
        SelectedWorkVariant::DiagnosticContext => Some(WorkShape::DiagnosticContext),
        SelectedWorkVariant::OrchestrationHandoff => Some(WorkShape::OrchestrationHandoff),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContextBehavior {
    Preserve,
    AttachEvaluatorContext,
    TranslateToTaskHalt,
    TranslateToDependency,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Classification {
    outer: OuterOwner,
    disposition: ResultDisposition,
    stable_owner: StableOwner,
    dependency: DependencyKind,
    remaining: WorkShape,
    context: ContextBehavior,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Occurrence {
    declaration: String,
    ordinal: usize,
    signal: Signal,
    classification: Classification,
}

impl Occurrence {
    fn record(&self) -> String {
        format!(
            "{}#{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
            self.declaration,
            self.ordinal,
            self.signal,
            self.classification.outer,
            self.classification.disposition,
            self.classification.stable_owner,
            self.classification.dependency,
            self.classification.remaining,
            self.classification.context,
        )
    }
}

struct CensusVisitor<'path> {
    path: &'path Path,
    module: Vec<String>,
    impl_name: Option<String>,
    function: Option<String>,
    ordinals: BTreeMap<String, usize>,
    occurrences: Vec<Occurrence>,
}

impl<'path> CensusVisitor<'path> {
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

    fn record(&mut self, signal: Signal) {
        let declaration = self.declaration();
        let ordinal = self.ordinals.entry(declaration.clone()).or_default();
        *ordinal += 1;
        self.occurrences.push(Occurrence {
            classification: classify(self.path, &declaration, signal),
            declaration,
            ordinal: *ordinal,
            signal,
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

impl<'ast> Visit<'ast> for CensusVisitor<'_> {
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
            let name = path.path.segments.last().map(|part| part.ident.to_string());
            if let Some(signal) = name
                .as_deref()
                .and_then(|name| function_signal(&full, name))
            {
                self.record(signal);
            }
            if name.as_deref() == self.function.as_deref() {
                self.record(Signal::StructuralRecursion);
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if let Some(signal) = method_signal(self.path, &node.method.to_string()) {
            self.record(signal);
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast ExprForLoop) {
        self.record(Signal::UserSizedLoop);
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        self.record(Signal::UserSizedLoop);
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast ExprLoop) {
        // W1B proves that the target WHNF driver consumes one explicit budget
        // unit before every transition. It cannot scale with user data inside
        // one quantum, so it is not one of this migration census's
        // user-sized-loop sources.
        if self.declaration() != "src/eval/whnf.rs::drive_regional" {
            self.record(Signal::UserSizedLoop);
        }
        visit::visit_expr_loop(self, node);
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

fn function_signal(full: &str, name: &str) -> Option<Signal> {
    match name {
        "eval_value_in" => Some(Signal::EvalValue),
        "eval_lazy_in" => Some(Signal::EvalLazy),
        "eval_promised_in" => Some(Signal::EvalPromise),
        "apply_value_in" => Some(Signal::ApplyValue),
        "apply_values_in" => Some(Signal::ApplyValues),
        "produce_lazy_source_in" => Some(Signal::ProduceLazySource),
        "evaluate_in" => Some(Signal::ReflectionEvaluate),
        "task_eval_error" | "client_demand_halt_poll" | "client_demand_halt" => {
            Some(Signal::DependencyTranslation)
        }
        "blocked" if full.contains("EvaluationHalt") => Some(Signal::RetryableWait),
        "unassigned_root" if full.contains("EvaluationHalt") => Some(Signal::UnassignedPromise),
        _ => None,
    }
}

fn method_signal(path: &Path, name: &str) -> Option<Signal> {
    match name {
        "poll_wait"
        | "pump_wait"
        | "wait_for_claimed_task"
        | "wait_for_observed_dependency_progress"
        | "retry_after_no_progress"
        | "lazy_task"
        | "promise_task"
        | "promise_root_task" => Some(Signal::CoordinatorBoundary),
        "defer_reflection_activation"
        | "poll_reflection_task"
        | "reserve_reflection_task"
        | "activate_reflection_task" => Some(Signal::ReflectionBoundary),
        "wait_for_disturbance" => Some(Signal::NetBoundary),
        "handle_request" | "snapshot" | "commit" if path.starts_with("src/reflection") => {
            Some(Signal::HostBoundary)
        }
        _ => None,
    }
}

fn classify(path: &Path, declaration: &str, signal: Signal) -> Classification {
    let path_text = path.to_string_lossy();
    let declaration_lower = declaration.to_ascii_lowercase();
    let is_reflection = path_text.starts_with("src/reflection/");
    let is_net = path_text == "src/eval/net.rs";
    let is_net_construction = path_text.contains("builtins/net/construction.rs");
    let is_value = path_text == "src/eval/value.rs";
    let is_client = declaration.contains("ClientDemand") || declaration.contains("client_demand");
    let is_spark = declaration.contains("spark");

    let outer = if is_reflection {
        OuterOwner::ReflectionMachine
    } else if is_net_construction {
        OuterOwner::NetConstruction
    } else if is_net {
        OuterOwner::NetWorklist
    } else if is_client {
        OuterOwner::ClientDemand
    } else if is_spark {
        OuterOwner::Spark
    } else if is_value && declaration.contains("LazyTaskMachine") {
        OuterOwner::LazyTask
    } else if path_text.starts_with("src/evaluation/") {
        OuterOwner::CoordinatorAdapter
    } else {
        OuterOwner::PureEvaluator
    };

    let disposition = match outer {
        OuterOwner::LazyTask => ResultDisposition::PublishLazyCache,
        OuterOwner::ClientDemand => ResultDisposition::PublishClientDemand,
        OuterOwner::Spark => ResultDisposition::DiscardSparkResult,
        OuterOwner::ReflectionMachine if declaration.contains("effect_request") => {
            ResultDisposition::ParseReflectionRequest
        }
        OuterOwner::ReflectionMachine => ResultDisposition::ContinueReflectionPhase,
        OuterOwner::NetWorklist | OuterOwner::NetConstruction => ResultDisposition::ContinueNetWork,
        OuterOwner::CoordinatorAdapter => ResultDisposition::TranslateDependency,
        OuterOwner::PureEvaluator => ResultDisposition::ReturnWhnf,
    };

    let stable_owner = match outer {
        OuterOwner::LazyTask => StableOwner::LazyIdentity,
        OuterOwner::ClientDemand => StableOwner::ClientDemandRecord,
        OuterOwner::ReflectionMachine => StableOwner::ReflectionTaskRecord,
        OuterOwner::NetWorklist | OuterOwner::NetConstruction => StableOwner::NetMachine,
        OuterOwner::CoordinatorAdapter | OuterOwner::Spark => StableOwner::CoordinatorRecord,
        OuterOwner::PureEvaluator => match signal {
            Signal::EvalPromise | Signal::UnassignedPromise => StableOwner::PromiseIdentity,
            Signal::EvalLazy | Signal::ProduceLazySource => StableOwner::LazyIdentity,
            _ => StableOwner::InputValue,
        },
    };

    let dependency = match signal {
        Signal::EvalLazy | Signal::ProduceLazySource => DependencyKind::LazyWait,
        Signal::EvalPromise | Signal::UnassignedPromise => DependencyKind::PromiseAssignment,
        Signal::RetryableWait | Signal::DependencyTranslation | Signal::CoordinatorBoundary => {
            DependencyKind::GenericWait
        }
        Signal::ReflectionBoundary => DependencyKind::ReflectionTask,
        Signal::HostBoundary => DependencyKind::Host,
        Signal::NetBoundary => DependencyKind::Net,
        _ => DependencyKind::None,
    };

    let remaining = if matches!(
        signal,
        Signal::CoordinatorBoundary
            | Signal::ReflectionBoundary
            | Signal::HostBoundary
            | Signal::NetBoundary
            | Signal::DependencyTranslation
    ) {
        WorkShape::OrchestrationHandoff
    } else if path_text.contains("application.rs") {
        WorkShape::Application
    } else if path_text.contains("access") || declaration.contains("path") {
        WorkShape::AccessPath
    } else if path_text.contains("pattern.rs") || declaration.contains("key") {
        WorkShape::KeyConversion
    } else if path_text.contains("list")
        || path_text.contains("dict")
        || declaration.contains("list")
        || declaration.contains("dict")
    {
        WorkShape::CollectionWalk
    } else if path_text.contains("comparison")
        || path_text.contains("numeric")
        || declaration_lower.contains("numeric")
        || declaration.contains("arguments")
        || declaration.contains("operands")
    {
        WorkShape::OrderedOperands
    } else if declaration.contains("context") || declaration.contains("failure") {
        WorkShape::DiagnosticContext
    } else if matches!(signal, Signal::EvalLazy | Signal::EvalPromise)
        && (declaration.ends_with("::eval_value_in")
            || declaration.contains("eval_lazy")
            || declaration.contains("eval_promised")
            || declaration.contains("follow"))
    {
        WorkShape::TailDemand
    } else {
        WorkShape::DemandThenInspect
    };

    let context = match signal {
        Signal::DependencyTranslation => ContextBehavior::TranslateToDependency,
        _ if is_reflection => ContextBehavior::TranslateToTaskHalt,
        _ if declaration.contains("context") || declaration.contains("failure") => {
            ContextBehavior::AttachEvaluatorContext
        }
        _ => ContextBehavior::Preserve,
    };

    Classification {
        outer,
        disposition,
        stable_owner,
        dependency,
        remaining,
        context,
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

fn is_in_scope(relative: &Path) -> bool {
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return false;
    }
    if relative.starts_with("src/eval") {
        return !matches!(
            relative.to_str(),
            Some(
                "src/eval/access_inventory.rs"
                    | "src/eval/whnf_inventory.rs"
                    | "src/eval/test_support.rs"
                    | "src/eval/tests.rs"
            )
        );
    }
    matches!(
        relative.to_str(),
        Some(
            "src/reflection/machine.rs"
                | "src/reflection/protocol.rs"
                | "src/reflection/requests.rs"
                | "src/evaluation/session.rs"
                | "src/evaluation/pump.rs"
                | "src/evaluation/whnf.rs"
        )
    )
}

fn collect_occurrences(manifest: &Path) -> Vec<Occurrence> {
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src/eval"), &mut sources);
    sources.extend([
        manifest.join("src/reflection/machine.rs"),
        manifest.join("src/reflection/protocol.rs"),
        manifest.join("src/reflection/requests.rs"),
        manifest.join("src/evaluation/session.rs"),
        manifest.join("src/evaluation/pump.rs"),
        manifest.join("src/evaluation/whnf.rs"),
    ]);
    sources.sort();
    sources.dedup();

    let mut occurrences = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("census source must belong to the package");
        if !is_in_scope(relative) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("census source should be readable");
        let syntax = syn::parse_file(&source).unwrap_or_else(|error| {
            panic!("{} must parse for census: {error}", relative.display())
        });
        let mut visitor = CensusVisitor::new(relative);
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

fn signal_counts(occurrences: &[Occurrence]) -> BTreeMap<Signal, usize> {
    occurrences
        .iter()
        .fold(BTreeMap::new(), |mut counts, occurrence| {
            *counts.entry(occurrence.signal).or_default() += 1;
            counts
        })
}

fn shape_counts(occurrences: &[Occurrence]) -> BTreeMap<WorkShape, usize> {
    occurrences
        .iter()
        .fold(BTreeMap::new(), |mut counts, occurrence| {
            *counts
                .entry(occurrence.classification.remaining)
                .or_default() += 1;
            counts
        })
}

fn validate_classifications(occurrences: &[Occurrence]) -> Result<(), String> {
    for occurrence in occurrences {
        let path = occurrence
            .declaration
            .split("::")
            .next()
            .expect("a census declaration begins with its path");
        let expected = classify(Path::new(path), &occurrence.declaration, occurrence.signal);
        if occurrence.classification != expected {
            return Err(format!(
                "{}#{} was classified as {:?}, expected {:?}",
                occurrence.declaration,
                occurrence.ordinal,
                occurrence.classification.remaining,
                expected.remaining,
            ));
        }
    }
    Ok(())
}

// Filled from W0B's deliberately failing initial AST scan. The per-signal
// summary explains count drift; the full record fingerprint detects moves or
// classification substitutions which leave those counts unchanged.
// W6B.0 removed two copied-budget adapter occurrences while preserving their
// resumable owners; nested WHNF now borrows the outer poll budget directly.
// NC2.0 removes NC1's borrowed net/regional projection walks. The remaining
// bounded state walks are durable root conversion and the canonical managed
// edge visitor, not recursive Rust evaluation.
// W6B.4b.1 replaces synchronous effect-API lookup and application with the
// existing resumable static-access and application lazy owners.
// W6C.4 replaces recursive numeric operand demand with one durable builtin
// owner which polls each operand through the ordinary WHNF machine.
// W6C.3 replaces recursive comparison, tuple-tag, and semantic-undefined
// demand with explicit durable machine stacks and bounded collection walks.
// W6C.6 gives seq and best-effort sparks one shared resumable demand owner;
// scheduler admission is a rooted post-access poll disposition.
// W6D.1-W6D.2 replace synchronous dictionary dispatch, key-path demand, and
// recursive merge demand with one durable dictionary owner. Its remaining
// recursive calls transform already-observed persistent dictionary/key data.
const EXPECTED_OCCURRENCES: usize = 221;
const EXPECTED_FINGERPRINT: u64 = 952_608_516_740_461_389;
const EXPECTED_SIGNAL_COUNTS: &[(Signal, usize)] = &[
    (Signal::EvalValue, 18),
    (Signal::EvalLazy, 2),
    (Signal::EvalPromise, 1),
    (Signal::ApplyValue, 10),
    (Signal::ApplyValues, 2),
    (Signal::ProduceLazySource, 1),
    (Signal::RetryableWait, 11),
    (Signal::UnassignedPromise, 2),
    (Signal::DependencyTranslation, 2),
    (Signal::CoordinatorBoundary, 21),
    (Signal::ReflectionBoundary, 7),
    (Signal::HostBoundary, 21),
    (Signal::NetBoundary, 1),
    (Signal::StructuralRecursion, 73),
    (Signal::UserSizedLoop, 49),
];
const EXPECTED_SHAPE_COUNTS: &[(WorkShape, usize)] = &[
    (WorkShape::TailDemand, 2),
    (WorkShape::DemandThenInspect, 126),
    (WorkShape::OrderedOperands, 6),
    (WorkShape::CollectionWalk, 16),
    (WorkShape::Application, 11),
    (WorkShape::KeyConversion, 2),
    (WorkShape::AccessPath, 5),
    (WorkShape::DiagnosticContext, 1),
    (WorkShape::OrchestrationHandoff, 52),
];

#[test]
fn whnf_suspension_and_recursion_census_is_exact() {
    let occurrences = collect_occurrences(Path::new(env!("CARGO_MANIFEST_DIR")));
    validate_classifications(&occurrences).expect("live census classifications must be canonical");
    assert_eq!(
        signal_counts(&occurrences),
        EXPECTED_SIGNAL_COUNTS.iter().copied().collect(),
        "W0B per-family counts drifted"
    );
    assert_eq!(
        shape_counts(&occurrences),
        EXPECTED_SHAPE_COUNTS.iter().copied().collect(),
        "W0B resumption-shape counts drifted"
    );
    assert_eq!(
        (occurrences.len(), occurrence_fingerprint(&occurrences)),
        (EXPECTED_OCCURRENCES, EXPECTED_FINGERPRINT),
        "W0B census drifted; reviewed shapes: {:#?}",
        shape_counts(&occurrences),
    );
}

#[test]
fn reflection_has_no_unowned_recursive_whnf_demand() {
    let occurrences = collect_occurrences(Path::new(env!("CARGO_MANIFEST_DIR")));
    let unowned = occurrences
        .iter()
        .filter(|occurrence| occurrence.declaration.starts_with("src/reflection/"))
        .filter(|occurrence| {
            matches!(
                occurrence.signal,
                Signal::EvalValue
                    | Signal::EvalLazy
                    | Signal::EvalPromise
                    | Signal::ReflectionEvaluate
            )
        })
        .map(Occurrence::record)
        .collect::<Vec<_>>();
    assert!(
        unowned.is_empty(),
        "reflection must own WHNF demand in resumable computations: {unowned:#?}"
    );
}

#[test]
fn whnf_census_rejects_tail_and_nested_misclassification() {
    let occurrences = collect_occurrences(Path::new(env!("CARGO_MANIFEST_DIR")));
    for target_shape in [WorkShape::TailDemand, WorkShape::DemandThenInspect] {
        let mut misclassified = occurrences.clone();
        let target = misclassified
            .iter_mut()
            .find(|occurrence| occurrence.classification.remaining == target_shape)
            .unwrap_or_else(|| panic!("census must contain a {target_shape:?} witness"));
        target.classification.remaining = if target_shape == WorkShape::TailDemand {
            WorkShape::DemandThenInspect
        } else {
            WorkShape::TailDemand
        };
        assert!(
            validate_classifications(&misclassified).is_err(),
            "the exact census must reject a {target_shape:?} classification substitution"
        );
    }
}

#[test]
fn synchronous_whnf_demand_uses_the_runtime_owned_client_submachine() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let session = fs::read_to_string(manifest.join("src/evaluation/session.rs"))
        .expect("the evaluation session source should be readable");
    let client = fs::read_to_string(manifest.join("src/evaluation/coordinator/client_demand.rs"))
        .expect("the client-demand source should be readable");
    let pump = fs::read_to_string(manifest.join("src/evaluation/pump.rs"))
        .expect("the evaluation pump source should be readable");

    assert!(session.contains("let handle = self\n            .demand_whnf(value)"));
    assert!(client.contains(
        "pub(crate) struct ClientDemandOperation(pub(in crate::evaluation) WhnfComputation);"
    ));
    assert!(pump.contains(
        "super::whnf::poll_computation(&mut self.0, poll_context, context, step_budget)"
    ));
    assert!(
        !pump.contains("crate::eval::eval_value_in"),
        "the runtime-owned client operation must not restart recursive evaluation"
    );
}

#[test]
fn selected_whnf_work_vocabulary_is_compile_exhaustive() {
    let variants = [
        SelectedWorkVariant::Delegate,
        SelectedWorkVariant::DemandThenInspect,
        SelectedWorkVariant::OrderedOperands,
        SelectedWorkVariant::CollectionWalk,
        SelectedWorkVariant::Application,
        SelectedWorkVariant::KeyConversion,
        SelectedWorkVariant::AccessPath,
        SelectedWorkVariant::DiagnosticContext,
        SelectedWorkVariant::OrchestrationHandoff,
    ];
    assert_eq!(selected_variant_shape(variants[0]), None);
    assert_eq!(variants.len(), 9);
}

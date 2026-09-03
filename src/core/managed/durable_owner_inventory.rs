//! Phase I4F.1a inventory of values which can outlive one mutator region.
//!
//! This inventory has two complementary layers. `OWNER_INVENTORY` records the
//! reviewed semantic ownership decision, while `DECLARATION_BASELINE` latches
//! every production Rust declaration whose stored type mentions a value/root,
//! failure graph, synchronized net, managed pointer, type erasure, or callback.
//! The syntax-backed baseline deliberately ignores function-local values: I3's
//! bounded access inventories own those scopes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Attribute, Fields, GenericParam, Generics, Item};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CurrentStorage {
    BareValue,
    CompatibilityRoot,
    PublicRoot,
    SynchronizedNet,
    TypeErased,
    CallbackCapture,
    BoundedLocal,
    EdgeFree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetDisposition {
    RootSurface,
    ExactManagedEdge,
    BoundedLocal,
    EdgeFree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationState {
    Closed,
    Open(&'static str),
}

#[derive(Clone, Copy, Debug)]
struct DurableFixtureContract {
    collection_checkpoint: &'static str,
    constructor: &'static str,
    publication: &'static str,
    observation: &'static str,
    retirement: &'static str,
}

#[derive(Clone, Copy, Debug)]
enum OwnerVerification {
    Durable(DurableFixtureContract),
    ExactManaged { proof: &'static str },
    Bounded { scope_proof: &'static str },
    EdgeFree { proof: &'static str },
}

#[derive(Clone, Copy, Debug)]
struct OwnerEntry {
    source: &'static str,
    owner: &'static str,
    fields: &'static str,
    lifetime: &'static str,
    publication: &'static str,
    retirement: &'static str,
    current: CurrentStorage,
    target: TargetDisposition,
    migration: MigrationState,
    verification: OwnerVerification,
}

macro_rules! open_durable {
    ($source:literal, $owner:literal, $fields:literal, $lifetime:literal, $publication:literal, $retirement:literal, $current:ident, $target:ident, $migration:literal, $fixture:literal) => {
        OwnerEntry {
            source: $source,
            owner: $owner,
            fields: $fields,
            lifetime: $lifetime,
            publication: $publication,
            retirement: $retirement,
            current: CurrentStorage::$current,
            target: TargetDisposition::$target,
            migration: MigrationState::Open($migration),
            verification: OwnerVerification::Durable(DurableFixtureContract {
                collection_checkpoint: $fixture,
                constructor: concat!($owner, " construction"),
                publication: $publication,
                observation: concat!($owner, " retained-value observation"),
                retirement: $retirement,
            }),
        }
    };
}

macro_rules! closed_durable {
    ($source:literal, $owner:literal, $fields:literal, $lifetime:literal, $publication:literal, $retirement:literal, $current:ident, $target:ident, $fixture:literal) => {
        OwnerEntry {
            source: $source,
            owner: $owner,
            fields: $fields,
            lifetime: $lifetime,
            publication: $publication,
            retirement: $retirement,
            current: CurrentStorage::$current,
            target: TargetDisposition::$target,
            migration: MigrationState::Closed,
            verification: OwnerVerification::Durable(DurableFixtureContract {
                collection_checkpoint: $fixture,
                constructor: concat!($owner, " construction"),
                publication: $publication,
                observation: concat!($owner, " retained-value observation"),
                retirement: $retirement,
            }),
        }
    };
}

macro_rules! bounded {
    ($source:literal, $owner:literal, $fields:literal, $lifetime:literal, $proof:literal) => {
        OwnerEntry {
            source: $source,
            owner: $owner,
            fields: $fields,
            lifetime: $lifetime,
            publication: "does not publish",
            retirement: "scope exit",
            current: CurrentStorage::BoundedLocal,
            target: TargetDisposition::BoundedLocal,
            migration: MigrationState::Closed,
            verification: OwnerVerification::Bounded {
                scope_proof: $proof,
            },
        }
    };
}

macro_rules! exact_managed {
    ($source:literal, $owner:literal, $fields:literal, $lifetime:literal, $current:ident, $proof:literal) => {
        OwnerEntry {
            source: $source,
            owner: $owner,
            fields: $fields,
            lifetime: $lifetime,
            publication: "published only through an inventoried outer root",
            retirement: "owning managed graph retirement",
            current: CurrentStorage::$current,
            target: TargetDisposition::ExactManagedEdge,
            migration: MigrationState::Closed,
            verification: OwnerVerification::ExactManaged { proof: $proof },
        }
    };
}

macro_rules! edge_free {
    ($source:literal, $owner:literal, $fields:literal, $lifetime:literal, $current:ident, $proof:literal) => {
        OwnerEntry {
            source: $source,
            owner: $owner,
            fields: $fields,
            lifetime: $lifetime,
            publication: "edge-free publication",
            retirement: "ordinary Rust drop",
            current: CurrentStorage::$current,
            target: TargetDisposition::EdgeFree,
            migration: MigrationState::Closed,
            verification: OwnerVerification::EdgeFree { proof: $proof },
        }
    };
}

// I4F.1a records open work; the named owner checkpoint closes each row. The
// collection checkpoint names the I4F.2d slice which will exercise the real
// owner after RuntimeValueRoot becomes a registered collector root.
const OWNER_INVENTORY: &[OwnerEntry] = &[
    closed_durable!(
        "src/api/value.rs; src/runtime.rs",
        "public Value / EvaluatedValue / RuntimeValueRoot / RuntimeFailureRoot facade",
        "opaque public handles and the compatibility value/failure roots",
        "public or runtime-owned value lifetime",
        "matching-runtime construction or evaluation",
        "last public/runtime root drop",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/core.rs",
        "CoreValues",
        "unit, object_reflection_guard, tuple, info, warn, error, initial_metadata",
        "runtime value-domain cache",
        "complete canonical bundle publication",
        "last RuntimeValueDomain owner",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/core.rs; src/core/runtime_cache.rs",
        "RuntimeValueCache.extensions / CoreValueFactory.local_extensions",
        "TypeId -> admitted RuntimeCacheEntry -> Arc<T>",
        "runtime or compilation-local extension cache",
        "same-runtime family validation then complete extension bundle insertion",
        "cache/domain or scoped factory drop",
        TypeErased,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/g_syntax/compiler_values.rs",
        "GCompilerValues / BuiltinModule / BuildingEffectValues",
        "cached RuntimeValueRoot fields and effect maps",
        "runtime compiler extension cache",
        "complete compiler bundle or effect insertion",
        "runtime value-domain cache drop",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/g_syntax/diagnostic_formatter.rs",
        "CachedDiagnosticFormatter",
        "tuple RuntimeValueRoot",
        "runtime compiler extension cache",
        "complete formatter insertion",
        "runtime value-domain cache drop",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/evaluation/coordinator.rs; src/evaluation/coordinator/reflection.rs; src/evaluation/coordinator/task.rs",
        "task, wait, exit, terminal, and failure-ledger records",
        "RuntimeValueRoot outcomes plus RuntimeFailureRoot failures",
        "parked or terminal coordinator state",
        "guarded task/wait terminal publication",
        "acknowledgement, settlement, cancellation, or session retirement",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/evaluation/coordinator/client_demand.rs",
        "ClientDemandOperation / ClientDemandResultCell / ClientDemandWork",
        "RuntimeValueRoot operations/results plus RuntimeFailureRoot failures",
        "cross-poll client demand",
        "demand registration and terminal publication",
        "handle abandonment or terminal retirement",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/evaluation/coordinator/spark.rs",
        "SparkDemand",
        "value: RuntimeValueRoot",
        "queued or claimed spark",
        "spark registration",
        "spark completion or abandonment",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/evaluation/session.rs",
        "EvaluationSession and pending activation/effect state",
        "RuntimeValueRoot effects plus RuntimeFailureRoot reports and unfinished state",
        "session and parked reflection/effect lifecycle",
        "session demand or activation reservation",
        "session close, cancellation, or terminal settlement",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/api/runtime/readiness.rs; src/evaluation/coordinator/settlement.rs",
        "quiescence, deadlock, and unfinished-task snapshots",
        "RuntimeFailureRoot settlement/block payloads plus rooted public diagnostics and dispositions",
        "host-visible readiness report",
        "stable readiness snapshot",
        "report drop",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.1"
    ),
    closed_durable!(
        "src/reflection/store.rs",
        "State / Set / Rewrite / StoreSnapshot / StoreJournal / query and transaction records",
        "persistent public Value roots in maps, snapshots, edits, and query results",
        "persistent reflection store or transaction",
        "snapshot/query/transaction creation and commit",
        "snapshot, query, journal, transaction, or store retirement",
        PublicRoot,
        RootSurface,
        "I4F.2d.2"
    ),
    closed_durable!(
        "src/reflection/lifecycle.rs",
        "EffectRun lifecycle state",
        "public effect/result/context roots and rooted TaskHalt failures",
        "active reflection effect run",
        "effect reservation/activation",
        "completion, cancellation, or abandonment",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.2"
    ),
    closed_durable!(
        "src/reflection/protocol.rs",
        "reflection protocol requests, results, snapshots, transactions, and failures",
        "public Value results plus specialization-owned Snapshot / Journal root contracts and rooted TaskHalt failures",
        "cross-phase reflection protocol state",
        "decoded request/result publication",
        "protocol completion or transaction retirement",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.2"
    ),
    closed_durable!(
        "src/reflection/search.rs",
        "isolated search host, branch, block, and result state",
        "public Value environment/results, specialization-owned snapshot/journal roots, rooted TaskHalt failures, and the separately inventoried nested effect machine",
        "pollable isolated search and returned result collection",
        "isolated-search construction and branch publication",
        "restart, cancellation, or search/result retirement",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.2"
    ),
    open_durable!(
        "src/reflection/machine.rs",
        "EffectTask frames, requests, continuations, fixpoints, branches, and task blocks",
        "raw Value/default RuntimeValueRoot fields plus Arc<EvaluationFailure>",
        "parked or worker-transferred effect machine",
        "frame push, request decode, branch/fix capture, or task park",
        "frame consumption, terminal publication, cancellation, or abandonment",
        BareValue,
        RootSurface,
        "I4F.1d.3",
        "I4F.2d.2"
    ),
    open_durable!(
        "src/reflection/requests.rs",
        "ReflectionJournal / QueryRead / decoded standard requests",
        "raw Value request/result and Arc<EvaluationFailure> fields",
        "transaction journal, query, or parked request",
        "request decoding or journal update",
        "commit, response delivery, or request retirement",
        BareValue,
        RootSurface,
        "I4F.1d.3",
        "I4F.2d.2"
    ),
    open_durable!(
        "src/api/diagnostics.rs",
        "Diagnostic / DiagnosticEvent / bus, ingress, and subscription state",
        "public Value emission/origin/context and callback captures",
        "host-visible diagnostic and queued/subscribed transport",
        "diagnostic construction or bus publication",
        "delivery, subscription retirement, or bus drop",
        PublicRoot,
        RootSurface,
        "I4F.1e",
        "I4F.2d.3"
    ),
    open_durable!(
        "src/api/runtime/events.rs",
        "RuntimeInputRecord / RuntimeOutputIntent / RuntimeDeliveryRecord and snapshots",
        "RuntimeValueRoot payloads plus converter/decoder callbacks",
        "admitted input, committed output, or running delivery",
        "event admission, output commit, or delivery claim",
        "consumption, delivery terminalization, or runtime event-state drop",
        CallbackCapture,
        RootSurface,
        "I4F.1e",
        "I4F.2d.3"
    ),
    open_durable!(
        "src/api/assembly.rs",
        "AssemblerReflectionHost / CompilationExecution / CompileSetup / BuiltModule / Assembler",
        "RuntimeValueRoot definitions/environment and diagnostic/failure state",
        "assembly, module, compiler, or reasoning-session lifecycle",
        "assembly setup, import handoff, module publication, or diagnostic attachment",
        "assembly/session/module/diagnostic retirement",
        CompatibilityRoot,
        RootSurface,
        "I4F.1f.1",
        "I4F.2d.4"
    ),
    open_durable!(
        "src/bin/glam/command_line",
        "CLI command-line search and token state",
        "public Value evidence, journals, successful branches, and token runs",
        "one configured command-line search",
        "search branch, journal, or token-run construction",
        "branch retirement or command-line completion",
        PublicRoot,
        RootSurface,
        "I4F.1f.3",
        "I4F.2d.4"
    ),
    open_durable!(
        "src/compiler.rs",
        "CompileContext / ModuleLoadArgs and loader/emitter callbacks",
        "RuntimeValueRoot definitions/origin plus callback captures and failures",
        "compilation or deferred import lifecycle",
        "compile setup, import demand, or final-definition publication",
        "compilation drain, loader completion, or context drop",
        CallbackCapture,
        RootSurface,
        "I4F.1f.1",
        "I4F.2d.4"
    ),
    exact_managed!(
        "src/core.rs; src/core/evaluation_halt.rs",
        "recursive core value and failure payloads",
        "Value variants, lazy/fix/reflection payloads, failure emissions, and contexts",
        "reachable only beneath an inventoried root surface after the production switch",
        BareValue,
        "I4B-I4E compile-exhaustive compatibility visitors establish every logical edge"
    ),
    closed_durable!(
        "src/g_syntax/module_lowering.rs",
        "ModuleLowerer",
        "definitions and module_reflection RuntimeValueRoot",
        "declaration-to-declaration lowering",
        "lowerer construction or definition replacement",
        "lowering completion",
        CompatibilityRoot,
        RootSurface,
        "I4F.2d.4"
    ),
    open_durable!(
        "src/g_syntax/macro_expansion/runner.rs",
        "MacroRun and suspended expansion state",
        "public Value macro inputs/results/failures",
        "macro client-demand and rewrite lifecycle",
        "macro invocation or suspended demand",
        "expansion completion/failure",
        PublicRoot,
        RootSurface,
        "I4F.1f.2",
        "I4F.2d.4"
    ),
    open_durable!(
        "src/g_syntax/parser/logical.rs",
        "macro invocation/work/journal logical source state",
        "embedded public Value data",
        "staged macro rewrite",
        "lexical macro discovery or output insertion",
        "logical parse/rewrite completion",
        PublicRoot,
        RootSurface,
        "I4F.1f.2",
        "I4F.2d.4"
    ),
    open_durable!(
        "src/bin/glam/configuration/mod.rs",
        "PreparedAssembly / LoadedConfiguration",
        "public Value configuration roots and evaluated observations",
        "batch configuration and assembly execution",
        "configuration load or prepared assembly creation",
        "batch completion or configuration drop",
        PublicRoot,
        RootSurface,
        "I4F.1f.3",
        "I4F.2d.4"
    ),
    open_durable!(
        "src/bin/glam/configuration/logger/supervisor.rs",
        "LogHost / LoggerSupervisor / LoggerInstallation / SettledReportSelection",
        "logger task, diagnostic, report, and callback-owned public values",
        "logger supervision and settlement",
        "logger installation or report selection",
        "logger completion, fallback, or supervisor drop",
        PublicRoot,
        RootSurface,
        "I4F.1f.3",
        "I4F.2d.4"
    ),
    open_durable!(
        "src/core.rs; src/core_net.rs; src/eval/net.rs",
        "FunctionCode / FunctionValue / NetValue / CoreOperator / parked net work",
        "CoreRuntimeNet identities and direct Value/operator payloads",
        "shared function/net value or parked normalization/cursor work",
        "net/function construction or driver park",
        "value/work retirement or net owner drop",
        SynchronizedNet,
        RootSurface,
        "I4F.1g",
        "I4F.2d.4"
    ),
    edge_free!(
        "src/core.rs; src/eval/builtins/net/construction.rs; src/reflection/requests.rs",
        "admitted opaque token families",
        "EffectToken, ConstructionPort, TaskHandleCell, CompilationOrigin",
        "external capability/token lifetime",
        TypeErased,
        "I4B containment inventory proves no Value, RuntimeValueRoot, or Gc field"
    ),
    bounded!(
        "src/evaluation/access.rs; src/evaluation/pump.rs",
        "EvaluationValueAccess / EvaluatorStepContext / claimed poll locals",
        "borrowed core values and claimed machine values",
        "one callback-free I3 evaluator or poll scope",
        "I3A-I3C lifetime-bound non-Send access and root-before-publication tests"
    ),
    bounded!(
        "src/eval/builtins; src/eval/value.rs",
        "callback-free evaluator temporary representations",
        "parsed builtin operands, pattern helpers, and current lazy/promise observations",
        "one callback-free evaluator quantum",
        "I3B-I3D scoped evaluator inventories and no-mutator-across-wait fixtures"
    ),
    bounded!(
        "src/g_syntax and src/compiler.rs",
        "compiler, lowering, parser, and macro operation locals",
        "raw semantic values projected from roots",
        "one callback-free compiler/macro operation",
        "I3E.2 compiler_root_and_projection_inventory_is_complete"
    ),
    edge_free!(
        "src/core/managed.rs; src/core/managed/payload_edges.rs",
        "certified managed-family and compatibility edge adapters",
        "Gc fields admitted by ManagedFamily and borrowed compatibility edge callbacks",
        "managed representation or synchronous visitor call",
        EdgeFree,
        "I4.0 ManagedFamily admission and I4A-I4E exact visitor suites"
    ),
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd, Ord)]
struct DeclarationSignals {
    value: usize,
    runtime_root: usize,
    evaluated_value: usize,
    evaluation_failure: usize,
    runtime_net: usize,
    gc: usize,
    any: usize,
    callback: usize,
}

impl DeclarationSignals {
    const fn new(counts: [usize; 8]) -> Self {
        Self {
            value: counts[0],
            runtime_root: counts[1],
            evaluated_value: counts[2],
            evaluation_failure: counts[3],
            runtime_net: counts[4],
            gc: counts[5],
            any: counts[6],
            callback: counts[7],
        }
    }

    fn is_empty(self) -> bool {
        self == Self::default()
    }
}

#[derive(Default)]
struct SignalVisitor {
    signals: DeclarationSignals,
}

impl<'ast> Visit<'ast> for SignalVisitor {
    fn visit_path(&mut self, node: &'ast syn::Path) {
        if let Some(identifier) = node.segments.last().map(|segment| &segment.ident) {
            match identifier.to_string().as_str() {
                "Value" => self.signals.value += 1,
                "RuntimeValueRoot" => self.signals.runtime_root += 1,
                "EvaluatedValue" => self.signals.evaluated_value += 1,
                "EvaluationFailure" => self.signals.evaluation_failure += 1,
                "CoreRuntimeNet" => self.signals.runtime_net += 1,
                "Gc" => self.signals.gc += 1,
                "Any" => self.signals.any += 1,
                "Fn" | "FnOnce" | "FnMut" => self.signals.callback += 1,
                _ => {}
            }
        }
        visit::visit_path(self, node);
    }
}

fn visit_generics(visitor: &mut SignalVisitor, generics: &Generics) {
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

fn visit_fields(visitor: &mut SignalVisitor, fields: &Fields) {
    for field in fields {
        if !is_test_only(&field.attrs) {
            visitor.visit_type(&field.ty);
        }
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

fn collect_items(
    relative: &Path,
    prefix: &str,
    items: &[Item],
    declarations: &mut BTreeMap<String, DeclarationSignals>,
) {
    for item in items {
        if item_is_test_only(item) {
            continue;
        }

        let (name, signals) = match item {
            Item::Struct(item) => {
                let mut visitor = SignalVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                visit_fields(&mut visitor, &item.fields);
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Enum(item) => {
                let mut visitor = SignalVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                for variant in &item.variants {
                    if !is_test_only(&variant.attrs) {
                        visit_fields(&mut visitor, &variant.fields);
                    }
                }
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Union(item) => {
                let mut visitor = SignalVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                for field in &item.fields.named {
                    visitor.visit_type(&field.ty);
                }
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Type(item) => {
                let mut visitor = SignalVisitor::default();
                visit_generics(&mut visitor, &item.generics);
                visitor.visit_type(&item.ty);
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Static(item) => {
                let mut visitor = SignalVisitor::default();
                visitor.visit_type(&item.ty);
                (Some(item.ident.to_string()), visitor.signals)
            }
            Item::Const(item) => {
                let mut visitor = SignalVisitor::default();
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
                (None, DeclarationSignals::default())
            }
            _ => (None, DeclarationSignals::default()),
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
                "duplicate inventoried declaration {key}"
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
    !matches!(
        relative.to_str(),
        Some(
            "src/api/value/prototype.rs"
                | "src/core/managed/value_shell.rs"
                | "src/core/managed/containment_inventory.rs"
                | "src/core/managed/durable_owner_inventory.rs"
                | "src/api/value/access_inventory.rs"
                | "src/evaluation/access_inventory.rs"
                | "src/eval/access_inventory.rs"
                | "src/g_syntax/access_inventory.rs"
        )
    )
}

// Filled from the deliberately failing first source scan in I4F.1a. The
// aggregate makes category drift legible, while the deterministic fingerprint
// detects a declaration being exchanged for another with the same counts.
// `owner_for_declaration` is the reviewed semantic assignment for every entry.
const DECLARATION_BASELINE_COUNT: usize = 129;
const DECLARATION_BASELINE_SIGNALS: DeclarationSignals =
    DeclarationSignals::new([115, 56, 1, 11, 15, 0, 2, 9]);
const DECLARATION_BASELINE_FINGERPRINT: u64 = 1_149_144_414_569_917_185;

fn declaration_signal_totals(
    declarations: &BTreeMap<String, DeclarationSignals>,
) -> DeclarationSignals {
    declarations
        .values()
        .copied()
        .fold(DeclarationSignals::default(), |mut totals, signals| {
            totals.value += signals.value;
            totals.runtime_root += signals.runtime_root;
            totals.evaluated_value += signals.evaluated_value;
            totals.evaluation_failure += signals.evaluation_failure;
            totals.runtime_net += signals.runtime_net;
            totals.gc += signals.gc;
            totals.any += signals.any;
            totals.callback += signals.callback;
            totals
        })
}

fn declaration_fingerprint(declarations: &BTreeMap<String, DeclarationSignals>) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut fingerprint = FNV_OFFSET;
    let mut add = |bytes: &[u8]| {
        for byte in bytes {
            fingerprint ^= u64::from(*byte);
            fingerprint = fingerprint.wrapping_mul(FNV_PRIME);
        }
    };
    for (name, signals) in declarations {
        add(name.as_bytes());
        add(&[0xff]);
        for count in [
            signals.value,
            signals.runtime_root,
            signals.evaluated_value,
            signals.evaluation_failure,
            signals.runtime_net,
            signals.gc,
            signals.any,
            signals.callback,
        ] {
            add(&(count as u64).to_le_bytes());
        }
    }
    fingerprint
}

fn owner_for_declaration(declaration: &str) -> Option<&'static str> {
    let owner = if declaration.starts_with("src/api/assembly.rs::") {
        "AssemblerReflectionHost / CompilationExecution / CompileSetup / BuiltModule / Assembler"
    } else if declaration.starts_with("src/api/diagnostics.rs::") {
        "Diagnostic / DiagnosticEvent / bus, ingress, and subscription state"
    } else if declaration.starts_with("src/api/runtime/events.rs::") {
        "RuntimeInputRecord / RuntimeOutputIntent / RuntimeDeliveryRecord and snapshots"
    } else if declaration.starts_with("src/api/runtime/readiness.rs::")
        || declaration.starts_with("src/evaluation/coordinator/settlement.rs::")
    {
        "quiescence, deadlock, and unfinished-task snapshots"
    } else if declaration.starts_with("src/api/value.rs::")
        || matches!(
            declaration,
            "src/runtime.rs::RuntimeValueRoot"
                | "src/runtime.rs::RuntimeFailureRoot"
                | "src/runtime.rs::RuntimeFailureRootInner"
        )
    {
        "public Value / EvaluatedValue / RuntimeValueRoot / RuntimeFailureRoot facade"
    } else if declaration.starts_with("src/bin/glam/command_line/") {
        "CLI command-line search and token state"
    } else if declaration.starts_with("src/bin/glam/configuration/logger/")
        || declaration.starts_with("src/bin/glam/rendering.rs::")
    {
        "LogHost / LoggerSupervisor / LoggerInstallation / SettledReportSelection"
    } else if declaration.starts_with("src/bin/glam/configuration/") {
        "PreparedAssembly / LoadedConfiguration"
    } else if declaration.starts_with("src/compiler.rs::") {
        "CompileContext / ModuleLoadArgs and loader/emitter callbacks"
    } else if declaration == "src/core.rs::CoreValues" {
        "CoreValues"
    } else if declaration == "src/core.rs::RuntimeValueCache"
        || declaration == "src/core/runtime_cache.rs::RuntimeCacheEntry"
    {
        "RuntimeValueCache.extensions / CoreValueFactory.local_extensions"
    } else if declaration == "src/core.rs::OpaqueValue" {
        "admitted opaque token families"
    } else if declaration.starts_with("src/core/evaluation_halt.rs::")
        || declaration.starts_with("src/core.rs::")
            && !matches!(
                declaration,
                "src/core.rs::FunctionCode" | "src/core.rs::NetValue"
            )
    {
        "recursive core value and failure payloads"
    } else if declaration.starts_with("src/core_net.rs::")
        || declaration.starts_with("src/eval/net.rs::")
        || matches!(
            declaration,
            "src/core.rs::FunctionCode" | "src/core.rs::NetValue"
        )
    {
        "FunctionCode / FunctionValue / NetValue / CoreOperator / parked net work"
    } else if declaration.starts_with("src/eval/builtins/")
        || declaration.starts_with("src/eval/value.rs::")
    {
        "callback-free evaluator temporary representations"
    } else if declaration.starts_with("src/evaluation/coordinator/client_demand.rs::") {
        "ClientDemandOperation / ClientDemandResultCell / ClientDemandWork"
    } else if declaration.starts_with("src/evaluation/coordinator/spark.rs::") {
        "SparkDemand"
    } else if declaration.starts_with("src/evaluation/coordinator/task.rs::")
        || declaration.starts_with("src/evaluation/coordinator.rs::")
    {
        "task, wait, exit, terminal, and failure-ledger records"
    } else if declaration.starts_with("src/evaluation/session.rs::") {
        "EvaluationSession and pending activation/effect state"
    } else if declaration.starts_with("src/g_syntax/compiler_values.rs::") {
        "GCompilerValues / BuiltinModule / BuildingEffectValues"
    } else if declaration.starts_with("src/g_syntax/diagnostic_formatter.rs::") {
        "CachedDiagnosticFormatter"
    } else if declaration == "src/g_syntax/module_lowering.rs::ModuleLowerer" {
        "ModuleLowerer"
    } else if declaration.starts_with("src/g_syntax/")
        || declaration.starts_with("src/g_syntax.rs::")
    {
        "macro invocation/work/journal logical source state"
    } else if declaration.starts_with("src/reflection/lifecycle.rs::") {
        "EffectRun lifecycle state"
    } else if declaration.starts_with("src/reflection/machine.rs::") {
        "EffectTask frames, requests, continuations, fixpoints, branches, and task blocks"
    } else if declaration.starts_with("src/reflection/protocol.rs::") {
        "reflection protocol requests, results, snapshots, transactions, and failures"
    } else if declaration.starts_with("src/reflection/search.rs::") {
        "isolated search host, branch, block, and result state"
    } else if declaration.starts_with("src/reflection/requests.rs::") {
        "ReflectionJournal / QueryRead / decoded standard requests"
    } else if declaration.starts_with("src/reflection/store.rs::") {
        "State / Set / Rewrite / StoreSnapshot / StoreJournal / query and transaction records"
    } else {
        return None;
    };
    Some(owner)
}

#[test]
fn durable_value_owner_inventory_is_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    sources.sort();

    let mut actual = BTreeMap::new();
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
        collect_items(relative, "", &syntax.items, &mut actual);
    }

    let actual_baseline = (
        actual.len(),
        declaration_signal_totals(&actual),
        declaration_fingerprint(&actual),
    );
    let expected_baseline = (
        DECLARATION_BASELINE_COUNT,
        DECLARATION_BASELINE_SIGNALS,
        DECLARATION_BASELINE_FINGERPRINT,
    );
    assert_eq!(
        actual_baseline, expected_baseline,
        "durable declaration source drift requires an ownership review:\n{actual:#?}"
    );

    let mut owners = BTreeMap::new();
    for entry in OWNER_INVENTORY {
        for (label, value) in [
            ("source", entry.source),
            ("owner", entry.owner),
            ("fields", entry.fields),
            ("lifetime", entry.lifetime),
            ("publication", entry.publication),
            ("retirement", entry.retirement),
        ] {
            assert!(!value.is_empty(), "{} has no {label}", entry.owner);
        }
        assert!(
            owners.insert(entry.owner, entry).is_none(),
            "{} appears twice in the durable-owner inventory",
            entry.owner
        );

        match entry.verification {
            OwnerVerification::Durable(fixture) => {
                assert_eq!(entry.target, TargetDisposition::RootSurface);
                for (label, value) in [
                    ("collection checkpoint", fixture.collection_checkpoint),
                    ("constructor", fixture.constructor),
                    ("publication", fixture.publication),
                    ("observation", fixture.observation),
                    ("retirement", fixture.retirement),
                ] {
                    assert!(!value.is_empty(), "{} has no {label}", entry.owner);
                }
                if let MigrationState::Open(checkpoint) = entry.migration {
                    assert!(
                        checkpoint.starts_with("I4F.1"),
                        "{} has invalid open checkpoint {checkpoint}",
                        entry.owner
                    );
                }
            }
            OwnerVerification::ExactManaged { proof } => {
                assert_eq!(entry.target, TargetDisposition::ExactManagedEdge);
                assert_eq!(entry.migration, MigrationState::Closed);
                assert!(!proof.is_empty(), "{} has no exact-edge proof", entry.owner);
            }
            OwnerVerification::Bounded { scope_proof } => {
                assert_eq!(entry.current, CurrentStorage::BoundedLocal);
                assert_eq!(entry.target, TargetDisposition::BoundedLocal);
                assert_eq!(entry.migration, MigrationState::Closed);
                assert!(
                    !scope_proof.is_empty(),
                    "{} has no scope proof",
                    entry.owner
                );
            }
            OwnerVerification::EdgeFree { proof } => {
                assert_eq!(entry.target, TargetDisposition::EdgeFree);
                assert_eq!(entry.migration, MigrationState::Closed);
                assert!(!proof.is_empty(), "{} has no edge-free proof", entry.owner);
            }
        }
    }

    let unassigned = actual
        .keys()
        .filter(|declaration| owner_for_declaration(declaration).is_none())
        .collect::<Vec<_>>();
    assert!(
        unassigned.is_empty(),
        "durable declarations have no semantic owner assignment: {unassigned:#?}"
    );
    for (declaration, signals) in &actual {
        let owner = owner_for_declaration(declaration).expect("the assignment was checked above");
        let entry = owners
            .get(owner)
            .unwrap_or_else(|| panic!("{declaration} names unknown owner row {owner}"));
        if signals.any != 0 {
            assert_eq!(
                entry.current,
                CurrentStorage::TypeErased,
                "type-erased declaration {declaration} is not assigned to a reviewed type-erased family"
            );
        }
        if signals.gc != 0 {
            assert_eq!(
                entry.target,
                TargetDisposition::ExactManagedEdge,
                "managed declaration {declaration} is not assigned to an exact managed-edge family"
            );
        }
    }

    assert!(
        OWNER_INVENTORY
            .iter()
            .any(|entry| matches!(entry.migration, MigrationState::Open(_))),
        "I4F.1a should retain explicitly assigned open conversion rows"
    );
}

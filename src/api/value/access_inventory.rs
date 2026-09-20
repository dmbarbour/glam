//! Production managed-root publication and forbidden-escape inventories.
//!
//! Registered root creation is a legitimate publication boundary and remains
//! source-counted by owner, including publication through an already-admitted
//! `RuntimeValueAccess`. Authority-free bare-core conversions are not a
//! migration allowance: the second latch rejects them anywhere in production.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{Attribute, ExprCall, ExprMethodCall, ImplItemFn, ItemFn, ItemImpl, ItemMod};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootPublicationCounts {
    compatibility_root_new: usize,
    scoped_factory_root: usize,
    access_root: usize,
}

impl RootPublicationCounts {
    const fn new(
        compatibility_root_new: usize,
        scoped_factory_root: usize,
        access_root: usize,
    ) -> Self {
        Self {
            compatibility_root_new,
            scoped_factory_root,
            access_root,
        }
    }

    fn in_source(source: &str) -> Self {
        Self::new(
            source.matches("RuntimeValueRoot::new(").count(),
            source.matches(".construct_runtime_value_root(").count()
                + source.matches(".try_construct_runtime_value_root(").count(),
            source.matches(".root_runtime_value(").count(),
        )
    }
}

struct InventoryEntry {
    path: &'static str,
    counts: RootPublicationCounts,
    role: &'static str,
    migration: &'static str,
}

macro_rules! entry {
    ($path:literal, $root_new:literal, $scoped_root:literal, $access_root:literal, $role:literal, $migration:literal) => {
        InventoryEntry {
            path: $path,
            counts: RootPublicationCounts::new($root_new, $scoped_root, $access_root),
            role: $role,
            migration: $migration,
        }
    };
}

const INVENTORY: &[InventoryEntry] = &[
    entry!(
        "src/api/assembly.rs",
        0,
        1,
        0,
        "assembly setup, rooted compiler handoff, import results, modules, and reflection environment",
        "I3E.2 bounded compiler regions; I4F.1 durable roots; GCI11R-002D.1b rooted module completion"
    ),
    entry!(
        "src/api/value.rs",
        0,
        0,
        1,
        "constructors, composite validation, observers, extraction, and net data",
        "I3B.1 scoped construction/extraction; I4F.2 public facade switch; GCI5R-001B same-region root publication"
    ),
    entry!(
        "src/compiler.rs",
        4,
        5,
        0,
        "rooted source context, origins, definition promises, and import results; I10A import inputs are declared HostCall captures",
        "I3E.2 bounded compiler regions; I4F.1 durable roots; I10A explicit deferred-capture handoff"
    ),
    entry!(
        "src/core.rs",
        3,
        1,
        5,
        "post-domain canonical-root initialization, test mutation-root publication, I10A one-shot HostCall capture bundles, and transient reflection handoff roots",
        "I4F.2d.0 canonical initialization; I4F.2a.1c fixture closure; GCI5R-001B regional construction entry; GCI5R-003E root-owned mutation; GCI5R-005B direct reflection semantic edges; I10A deferred callback containment; W6G.1f.3b autonomous reflection handoff"
    ),
    entry!(
        "src/core/managed/active_owner_inventory.rs",
        1,
        0,
        0,
        "test-only external callback root-backedge containment proof",
        "I5F.4 external-owner closure audit; I10A deferred callback containment"
    ),
    entry!(
        "src/core/managed/containment_inventory.rs",
        0,
        1,
        0,
        "source-backed managed-containment verification fixture",
        "I11B managed containment closure; GCI11R-002D.2b.2 scoped root publication inventory"
    ),
    entry!(
        "src/core/managed/recursive_cells.rs",
        0,
        4,
        0,
        "recursive-cell ownership and publication lifecycle fixtures",
        "I5 managed recursive identities; GCI11R-002D.2b.2 access-qualified promise publication"
    ),
    entry!(
        "src/eval/access_machine.rs",
        0,
        1,
        0,
        "shared key/list conversion retains one temporary managed regional checkpoint while computed access itself is an edge-owned lazy checkpoint",
        "W6G.1f.3d.1 regional converter bridge; W6G.1f.3d.2-.3 access checkpoint cutover"
    ),
    entry!(
        "src/eval/annotation_machine.rs",
        0,
        0,
        12,
        "durable annotation payload, collection, metadata, and result publication",
        "W6E.1-W6E.4 resumable annotation ownership"
    ),
    entry!(
        "src/eval/builtin_machine.rs",
        0,
        0,
        2,
        "durable interaction-net construction and net-arity result publication",
        "W6F.5 resumable net builtin ownership"
    ),
    entry!(
        "src/eval/comparison_machine.rs",
        0,
        0,
        10,
        "durable comparison operands, recursive list/dictionary members, and tuple payloads",
        "W6C.3 resumable recursive comparison ownership"
    ),
    entry!(
        "src/eval/dict_machine.rs",
        0,
        0,
        4,
        "durable dictionary results published before their producing access region closes",
        "W6D.1-W6D.2 resumable dictionary ownership"
    ),
    entry!(
        "src/eval/effect_machine.rs",
        0,
        0,
        6,
        "durable effect-call and map operands, deferred application, continuation, and fixpoint result publication",
        "W6E.5-W6E.6 resumable effect dispatch, map, and fixpoint ownership"
    ),
    entry!(
        "src/eval/list_machine.rs",
        0,
        2,
        4,
        "forced-collection fixtures plus shared managed front/back list reconstruction",
        "W6G.1f.3e.1 regional list-front bridge / W6G.1f.3g.1 regional list-back bridge"
    ),
    entry!(
        "src/eval/list_observation_machine.rs",
        0,
        0,
        4,
        "durable list-observation result publication",
        "W6D.3 resumable list observation"
    ),
    entry!(
        "src/eval/list_transform_machine.rs",
        0,
        0,
        3,
        "durable structural list-transformation result publication",
        "W6D.4 non-forcing structural transforms and resumable text extraction"
    ),
    entry!(
        "src/eval/object_builtin_machine.rs",
        0,
        0,
        7,
        "durable object specification, diagnostic normalization, local-name, instance, definition-adapter, and plain-dictionary conversion result publication",
        "W6F.2/W6F.4a-W6F.4c resumable object builtin ownership"
    ),
    entry!(
        "src/eval/object_composition_machine.rs",
        0,
        0,
        8,
        "durable ordinary extension, composed-definition application, and recursive override results",
        "W6F.3 resumable object composition ownership"
    ),
    entry!(
        "src/eval/pattern_machine.rs",
        0,
        0,
        2,
        "durable compiler-pattern result publication",
        "W6D.5 resumable pattern observations"
    ),
    entry!(
        "src/eval/strategy_machine.rs",
        0,
        0,
        1,
        "hidden metadata retained across resumable seq and spark demand",
        "W6C.6 shared durable strategy-demand owner"
    ),
    entry!(
        "src/eval/tagged_machine.rs",
        0,
        0,
        3,
        "tagged payload and recursive semantic-undefined traversal",
        "W6C.3 shared resumable tagged-payload owner"
    ),
    entry!(
        "src/eval/builtins/net/construction.rs",
        0,
        0,
        2,
        "interaction-net diagnostic context and completed replay publication",
        "W6F.6-W6F.7 rooted net-construction lifecycle and context"
    ),
    entry!(
        "src/eval/value.rs",
        0,
        0,
        9,
        "computed-access, object-fixpoint, and list-effect terminal publication plus resumable builtin, immediate builtin result, and net-construction source arguments published before leaving their access regions",
        "W3B.2b, W6C.1b, W6F.6, and W6G.1f.3d.2-.3/W6G.1f.3f source or terminal handoff"
    ),
    entry!(
        "src/eval/whnf.rs",
        0,
        1,
        1,
        "terminal WHNF result rooting and promise-follower focus construction",
        "W2B.2 promise-follower ownership; W6G.3f retired root-per-field checkpoint publication in favor of one managed state root"
    ),
    entry!(
        "src/evaluation/access.rs",
        0,
        2,
        0,
        "poll/evaluator-step completion rooting and scoped projection",
        "I3A.4/I3C.2 outcome typing and projection; I4F.2 managed root switch"
    ),
    entry!(
        "src/evaluation/coordinator/spark.rs",
        0,
        1,
        0,
        "durable spark demand",
        "I3A.4/I3C.2 poll outcomes; I4F.1 coordinator roots"
    ),
    entry!(
        "src/evaluation/coordinator/task.rs",
        0,
        1,
        0,
        "promise terminal projection into one durable wait result",
        "GCI11R-002D.2b.2 scoped task-promise terminal publication"
    ),
    entry!(
        "src/evaluation/pump.rs",
        0,
        0,
        1,
        "centralized client/spark evaluation and same-region exceptional lazy-cycle publication",
        "I3A.3/I3B.1b/I3B.2/I3C.2 scoped polling; I4F.1 outcomes; GCI11R-002D.2b.2e.3 nested-construction repair"
    ),
    entry!(
        "src/evaluation/session.rs",
        1,
        3,
        0,
        "session demand, effect entry, and patient completion; reflection completion activation now roots through explicit access publication",
        "I3A.3/I3B.2/I3C.1-I3D.1 scoped polling and activation; I4F.1 outcomes"
    ),
    entry!(
        "src/evaluation/whnf.rs",
        0,
        3,
        0,
        "post-region WHNF lazy admission and focused dependency tests",
        "W2A.2 regional-shell polling followed by callback-capable admission"
    ),
    entry!(
        "src/g_syntax.rs",
        0,
        1,
        0,
        "rooted lowered definitions and compiler diagnostics across publication",
        "I3E.2 bounded compiler regions; I4F.1 durable roots"
    ),
    entry!(
        "src/g_syntax/compiler_values.rs",
        0,
        3,
        0,
        "owned closed-evaluation results and complete runtime-cached compiler helper bundles",
        "I3E.2 rooted cache publication; I4F.1 durable roots; GCI11R-002C direct client-demand result ownership"
    ),
    entry!(
        "src/g_syntax/macro_expansion/effects.rs",
        0,
        1,
        0,
        "macro effect completion fixture publication",
        "phase-9 macro effect ownership; GCI11R-002D.2b.2 scoped root publication inventory"
    ),
    entry!(
        "src/g_syntax/module_lowering.rs",
        0,
        1,
        1,
        "declaration-to-declaration definitions and directly owned reflection annotator",
        "I3E.2 bounded lowering regions; I4F.1 durable roots; GCI11R-002C owned closed-result handoff; W2R-001D.1 post-resolution publication"
    ),
    entry!(
        "src/reflection/machine.rs",
        0,
        9,
        2,
        "rooted reflection machine and decoded-request handoff plus bounded evaluator, parser, and store access",
        "I3D.2/I3D.4 interpreter phases; I4F.1d.3 complete machine roots and bounded raw values; I4F.2a compatibility-access retirement"
    ),
    entry!(
        "src/reflection/protocol.rs",
        0,
        1,
        0,
        "reflection protocol structured-failure fixtures",
        "GCI11R-002D.2b.2 scoped halt payload construction"
    ),
    entry!(
        "src/reflection/store.rs",
        0,
        4,
        0,
        "reflection store publication and inspection boundaries",
        "I3D.4 bounded reflection-store access; GCI11R-002D.2b.2 scoped root publication inventory"
    ),
    entry!(
        "src/runtime.rs",
        0,
        0,
        2,
        "shallow direct-value rooting for one runtime failure root",
        "I4F.1c.1 failure-root boundary; I6C failure-shell and owner audit"
    ),
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RootPublicationSurface {
    CompatibilityNew,
    ScopedFactory,
    AccessPublication,
}

impl RootPublicationSurface {
    const fn label(self) -> &'static str {
        match self {
            Self::CompatibilityNew => "compatibility-new",
            Self::ScopedFactory => "scoped-factory",
            Self::AccessPublication => "access-publication",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RootPublicationOccurrence {
    declaration: String,
    surface: RootPublicationSurface,
    ordinal: usize,
    test_only: bool,
    access_nesting: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootPublicationOwner {
    D2bCoreCarriers,
    D2cEvaluator,
    D2dOrchestration,
    D2eFrontend,
    D2fReflection,
    D2gPublicCompilerDiagnostics,
    D2eTestFixtures,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootPublicationDisposition {
    OuterConstructionBoundary,
    RegionalPublication,
    RootedTransportMigration,
    CanonicalConstructor,
    TemporaryTestFixture,
    Defect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RootPublicationReview {
    owner: RootPublicationOwner,
    disposition: RootPublicationDisposition,
}

impl RootPublicationOccurrence {
    fn record(&self) -> String {
        format!(
            "{}#{}|surface={}|scope={}",
            self.declaration,
            self.ordinal,
            self.surface.label(),
            if self.test_only { "test" } else { "production" },
        )
    }

    fn review(&self) -> RootPublicationReview {
        if self.test_only {
            return RootPublicationReview {
                owner: RootPublicationOwner::D2eTestFixtures,
                disposition: RootPublicationDisposition::TemporaryTestFixture,
            };
        }

        let owner = if self.declaration.starts_with("src/core.rs::")
            || self.declaration.starts_with("src/core_net.rs::")
            || self.declaration.starts_with("src/runtime.rs::")
        {
            RootPublicationOwner::D2bCoreCarriers
        } else if self.declaration.starts_with("src/eval/") {
            RootPublicationOwner::D2cEvaluator
        } else if self.declaration.starts_with("src/evaluation/") {
            RootPublicationOwner::D2dOrchestration
        } else if self.declaration.starts_with("src/g_syntax") {
            RootPublicationOwner::D2eFrontend
        } else if self.declaration.starts_with("src/reflection/") {
            RootPublicationOwner::D2fReflection
        } else if self.declaration.starts_with("src/api/")
            || self.declaration.starts_with("src/compiler.rs::")
            || self.declaration.starts_with("src/diagnostic.rs::")
            || self.declaration.starts_with("src/source.rs::")
        {
            RootPublicationOwner::D2gPublicCompilerDiagnostics
        } else {
            panic!(
                "{} has no GCI11R-002D.2 root-publication owner",
                self.declaration
            );
        };

        let disposition = if self.declaration == "src/reflection/machine.rs::impl Branch < S >::new"
            && self.ordinal == 1
        {
            // The effect arrives from reflection activation as a registered
            // root today, but the launcher interface projects it to a raw
            // value before Branch registers a replacement. D.2f owns the
            // rooted transport cutover. The second occurrence is the freshly
            // constructed initial state and remains a regional publication.
            RootPublicationDisposition::RootedTransportMigration
        } else {
            match self.declaration.as_str() {
                "src/core.rs::impl CoreValueFactory::construct_runtime_value_root"
                | "src/core.rs::impl CoreValueFactory::try_construct_runtime_value_root" => {
                    RootPublicationDisposition::CanonicalConstructor
                }
                "src/evaluation/coordinator/spark.rs::impl EvaluationWorkCoordinator::submit_spark"
                | "src/evaluation/coordinator/task.rs::promise_assignment_terminal"
                | "src/evaluation/session.rs::impl EvalContext::evaluate_compatibility_whnf"
                | "src/evaluation/session.rs::impl EvalContext::reserve_reflection_activation"
                | "src/evaluation/session.rs::impl EvalContext::reserve_reflection_task"
                | "src/reflection/machine.rs::impl Branch < S >::set_state"
                | "src/reflection/machine.rs::impl Branch < S >::root_value"
                | "src/reflection/machine.rs::impl ContextualValueEffectTask < S >::new" => {
                    RootPublicationDisposition::RootedTransportMigration
                }
                "src/api/assembly.rs::impl Assembler::load_local_binary"
                | "src/g_syntax/compiler_values.rs::evaluate_closed"
                | "src/g_syntax/compiler_values.rs::run_pure_match_resolved"
                | "src/g_syntax/macro_expansion/effects.rs::hidden_effect"
                | "src/reflection/machine.rs::alternative_returns_root"
                | "src/reflection/machine.rs::effect_api"
                | "src/reflection/machine.rs::volume_effects"
                | "src/reflection/protocol.rs::impl EffectRequestSpec < R >::effect" => {
                    RootPublicationDisposition::OuterConstructionBoundary
                }
                "src/api/value.rs::impl ScopedValues < '_ >::wrap"
                | "src/compiler.rs::impl CompileContext::new"
                | "src/compiler.rs::impl CompileContext::with_compilation_trace"
                | "src/core.rs::impl CoreValues::new"
                | "src/core.rs::impl HostCallRootBundle::from_captures"
                | "src/core.rs::impl ReflectionComputation::handoff_roots_in"
                | "src/eval/annotation_machine.rs::annotation_error_root"
                | "src/eval/annotation_machine.rs::finish_metadata_update"
                | "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::begin_recognized"
                | "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll"
                | "src/eval/annotation_machine.rs::recognize_annotation"
                | "src/eval/annotation_machine.rs::root_builtin"
                | "src/eval/builtin_machine.rs::impl NetBuiltinMachine::poll"
                | "src/eval/builtins/net/construction.rs::net_construction_context"
                | "src/eval/builtins/net/construction.rs::replay"
                | "src/eval/comparison_machine.rs::classify_equality"
                | "src/eval/comparison_machine.rs::classify_ordering"
                | "src/eval/comparison_machine.rs::demand_tuple_payload"
                | "src/eval/comparison_machine.rs::impl ComparisonBuiltinMachine::finish"
                | "src/eval/comparison_machine.rs::impl DictEqualityFrame::new"
                | "src/eval/dict_machine.rs::finish_merge_duplicate"
                | "src/eval/dict_machine.rs::finish_union"
                | "src/eval/dict_machine.rs::finish_update"
                | "src/eval/dict_machine.rs::impl DictBuiltinMachine::poll"
                | "src/eval/effect_machine.rs::impl EffectBuiltinMachine::poll"
                | "src/eval/effect_machine.rs::root_application"
                | "src/eval/effect_machine.rs::root_effect_call"
                | "src/eval/effect_machine.rs::root_effect_map"
                | "src/eval/effect_machine.rs::root_effect_map_continuation"
                | "src/eval/effect_machine.rs::root_effect_map_sequence"
                | "src/eval/list_machine.rs::impl ManagedListBackRoot::poll_in"
                | "src/eval/list_machine.rs::impl ManagedListFrontRoot::poll_in"
                | "src/eval/list_observation_machine.rs::rooted_binary_split"
                | "src/eval/list_observation_machine.rs::rooted_list_from_items"
                | "src/eval/list_observation_machine.rs::rooted_split_from_items_and_root"
                | "src/eval/list_observation_machine.rs::rooted_split_from_root_and_items"
                | "src/eval/list_transform_machine.rs::finish_text_lines"
                | "src/eval/list_transform_machine.rs::impl ListConcatMachine::poll"
                | "src/eval/list_transform_machine.rs::impl ListMapMachine::poll"
                | "src/eval/object_builtin_machine.rs::optional_spec_member"
                | "src/eval/object_builtin_machine.rs::finish_dict_defs"
                | "src/eval/object_builtin_machine.rs::root_local_name"
                | "src/eval/object_builtin_machine.rs::root_instance_from_plain_dict"
                | "src/eval/object_builtin_machine.rs::root_object_from_dict"
                | "src/eval/object_builtin_machine.rs::root_object_instance"
                | "src/eval/object_builtin_machine.rs::spec_name"
                | "src/eval/object_composition_machine.rs::finish_object_extension"
                | "src/eval/object_composition_machine.rs::impl ObjectCompositionMachine::poll"
                | "src/eval/object_composition_machine.rs::insert_override_value"
                | "src/eval/object_composition_machine.rs::next_override_step"
                | "src/eval/object_composition_machine.rs::root_application"
                | "src/eval/object_composition_machine.rs::root_plain_extension_in"
                | "src/eval/pattern_machine.rs::pattern_effect_in"
                | "src/eval/pattern_machine.rs::impl PatternDictTakeMachine::poll"
                | "src/eval/strategy_machine.rs::impl StrategyDemandMachine::poll"
                | "src/eval/tagged_machine.rs::impl SemanticUndefinedMachine::poll"
                | "src/eval/tagged_machine.rs::impl TaggedPayloadMachine::new"
                | "src/eval/value.rs::impl LazyTaskMachine::poll"
                | "src/eval/value.rs::impl LazyTaskMachine::poll_access_checkpoint"
                | "src/eval/value.rs::impl LazyTaskMachine::poll_object_fixpoint_checkpoint"
                | "src/eval/value.rs::impl LazyTaskMachine::poll_list_effect_checkpoint"
                | "src/eval/value/tests/w4.rs::host_call_follows_a_lazy_result_without_reinvocation"
                | "src/eval/whnf.rs::impl WhnfComputation::from_promise_root"
                | "src/eval/whnf.rs::regional_status_poll"
                | "src/evaluation/access.rs::impl EvaluatorStepContext < '_ >::root_value"
                | "src/evaluation/pump.rs::poison_lazy_cycle"
                | "src/evaluation/session.rs::impl EvalContext::compose_builtin"
                | "src/g_syntax.rs::impl Diagnostic::with_emission"
                | "src/g_syntax/compiler_values.rs::root_value"
                | "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::lower_declaration"
                | "src/reflection/machine.rs::impl Branch < S >::new"
                | "src/reflection/machine.rs::impl EffectTask < S >::capture_continuation"
                | "src/reflection/machine.rs::impl EffectTask < S >::interpret_decoded_drive"
                | "src/reflection/machine.rs::impl EffectTask < S >::store_path_step"
                | "src/reflection/machine.rs::lazy_value_path_root"
                | "src/reflection/store.rs::apply_edit"
                | "src/reflection/store.rs::apply_value_at_path"
                | "src/reflection/store.rs::impl StoreJournal::peek_query_with_observation"
                | "src/reflection/store.rs::impl StoreSnapshot::poll_query"
                | "src/runtime.rs::impl RuntimeFailureRoot::root_direct_values" => {
                    RootPublicationDisposition::RegionalPublication
                }
                _ => panic!(
                    "{} has no exact GCI11R-002D.2 root-publication disposition",
                    self.declaration
                ),
            }
        };
        RootPublicationReview { owner, disposition }
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

struct RootPublicationVisitor {
    path: String,
    scopes: Vec<String>,
    test_scopes: Vec<bool>,
    ordinals: BTreeMap<(String, RootPublicationSurface), usize>,
    occurrences: Vec<RootPublicationOccurrence>,
    access_nesting: usize,
}

impl RootPublicationVisitor {
    fn new(path: String, test_only: bool) -> Self {
        Self {
            path,
            scopes: Vec::new(),
            test_scopes: vec![test_only],
            ordinals: BTreeMap::new(),
            occurrences: Vec::new(),
            access_nesting: 0,
        }
    }

    fn in_test_scope(&self) -> bool {
        self.test_scopes.last().copied().unwrap_or(false)
    }

    fn with_scope(
        &mut self,
        scope: String,
        attributes: &[Attribute],
        visit: impl FnOnce(&mut Self),
    ) {
        self.scopes.push(scope);
        self.test_scopes
            .push(self.in_test_scope() || is_test_only(attributes));
        visit(self);
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

    fn record(&mut self, surface: RootPublicationSurface) {
        let declaration = self.declaration();
        let ordinal = self
            .ordinals
            .entry((declaration.clone(), surface))
            .or_default();
        *ordinal += 1;
        self.occurrences.push(RootPublicationOccurrence {
            declaration,
            surface,
            ordinal: *ordinal,
            test_only: self.in_test_scope(),
            access_nesting: self.access_nesting,
        });
    }
}

impl<'ast> Visit<'ast> for RootPublicationVisitor {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        self.with_scope(node.ident.to_string(), &node.attrs, |visitor| {
            visit::visit_item_mod(visitor, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let scope = format!("impl {}", node.self_ty.to_token_stream());
        self.with_scope(scope, &node.attrs, |visitor| {
            visit::visit_item_impl(visitor, node);
        });
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.with_scope(node.sig.ident.to_string(), &node.attrs, |visitor| {
            visit::visit_item_fn(visitor, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.with_scope(node.sig.ident.to_string(), &node.attrs, |visitor| {
            visit::visit_impl_item_fn(visitor, node);
        });
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(function) = node.func.as_ref() {
            let segments = function
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            if segments.ends_with(&["RuntimeValueRoot".to_owned(), "new".to_owned()]) {
                self.record(RootPublicationSurface::CompatibilityNew);
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        match method.as_str() {
            "construct_runtime_value_root" | "try_construct_runtime_value_root" => {
                self.record(RootPublicationSurface::ScopedFactory);
            }
            "root_runtime_value" => {
                self.record(RootPublicationSurface::AccessPublication);
            }
            _ => {}
        }
        if method == "with_runtime_value_access" {
            self.visit_expr(&node.receiver);
            for argument in &node.args {
                if matches!(argument, syn::Expr::Closure(_)) {
                    self.access_nesting += 1;
                    self.visit_expr(argument);
                    self.access_nesting -= 1;
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

fn collect_root_publication_occurrences(manifest: &Path) -> Vec<RootPublicationOccurrence> {
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    let inventory_path = Path::new("src/api/value/access_inventory.rs");
    let mut occurrences = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a discovered source should belong to this package");
        if relative == inventory_path || relative.starts_with("src/bin") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("an inventoried source should be UTF-8");
        let syntax = syn::parse_file(&source).expect("an inventoried source should parse");
        let mut visitor = RootPublicationVisitor::new(
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

const EXPECTED_ROOT_PUBLICATION_OCCURRENCES: &[&str] = &[
    "src/api/assembly.rs::impl Assembler::load_local_binary#1|surface=scoped-factory|scope=production",
    "src/api/value.rs::impl ScopedValues < '_ >::wrap#1|surface=access-publication|scope=production",
    "src/compiler.rs::impl CompileContext::new#1|surface=scoped-factory|scope=production",
    "src/compiler.rs::impl CompileContext::new#2|surface=scoped-factory|scope=production",
    "src/compiler.rs::impl CompileContext::new#3|surface=scoped-factory|scope=production",
    "src/compiler.rs::impl CompileContext::with_compilation_trace#1|surface=scoped-factory|scope=production",
    "src/compiler.rs::impl CompileContext::with_prior_defs#1|surface=scoped-factory|scope=test",
    "src/compiler.rs::tests::binary_import_forwards_hidden_source_provenance#1|surface=compatibility-new|scope=test",
    "src/compiler.rs::tests::module_import_qualifies_only_the_relative_child_namespace#1|surface=compatibility-new|scope=test",
    "src/compiler.rs::tests::module_load_arguments_retain_definition_roots_until_handoff_retires#1|surface=compatibility-new|scope=test",
    "src/compiler.rs::tests::module_load_arguments_retain_definition_roots_until_handoff_retires#2|surface=compatibility-new|scope=test",
    "src/core.rs::impl CoreValueFactory::construct_runtime_value_root#1|surface=scoped-factory|scope=production",
    "src/core.rs::impl CoreValueFactory::try_construct_runtime_value_root#1|surface=access-publication|scope=production",
    "src/core.rs::impl CoreValues::new#1|surface=access-publication|scope=production",
    "src/core.rs::impl HostCallRootBundle::from_captures#1|surface=access-publication|scope=production",
    "src/core.rs::impl ReflectionComputation::handoff_roots_in#1|surface=access-publication|scope=production",
    "src/core.rs::impl ReflectionComputation::handoff_roots_in#2|surface=access-publication|scope=production",
    "src/core.rs::tests::losing_complete_cache_candidate_retires_after_the_atomic_winner_race#1|surface=compatibility-new|scope=test",
    "src/core.rs::tests::runtime_cache_rejects_a_root_from_another_runtime_before_publication#1|surface=compatibility-new|scope=test",
    "src/core.rs::tests::runtime_cache_retires_an_admitted_owner_with_the_value_domain#1|surface=compatibility-new|scope=test",
    "src/core/managed/active_owner_inventory.rs::arbitrary_host_callback_root_backedge_is_conservative_external_ownership#1|surface=compatibility-new|scope=test",
    "src/core/managed/containment_inventory.rs::managed_drop_during_domain_teardown_is_passive#1|surface=scoped-factory|scope=test",
    "src/core/managed/recursive_cells.rs::tests::early_regional_return_leaves_partial_managed_graph_collectible#1|surface=scoped-factory|scope=test",
    "src/core/managed/recursive_cells.rs::tests::failed_lazy_gateway_is_terminal_before_traced_handoff#1|surface=scoped-factory|scope=test",
    "src/core/managed/recursive_cells.rs::tests::fresh_managed_facades_survive_until_first_publication#1|surface=scoped-factory|scope=test",
    "src/core/managed/recursive_cells.rs::tests::regional_value_publication_retains_only_the_returned_managed_graph#1|surface=scoped-factory|scope=test",
    "src/eval/access_machine.rs::tests::shared_key_converter_uses_one_managed_root_and_traces_nested_regional_state#1|surface=scoped-factory|scope=test",
    "src/eval/annotation_machine.rs::annotation_error_root#1|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::finish_metadata_update#1|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::begin_recognized#1|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll#2|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll#3|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll#4|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll#5|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll#6|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::impl AnnotationBuiltinMachine::poll#7|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::recognize_annotation#1|surface=access-publication|scope=production",
    "src/eval/annotation_machine.rs::root_builtin#1|surface=access-publication|scope=production",
    "src/eval/builtin_machine.rs::impl NetBuiltinMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/builtin_machine.rs::impl NetBuiltinMachine::poll#2|surface=access-publication|scope=production",
    "src/eval/builtins/net/construction.rs::net_construction_context#1|surface=access-publication|scope=production",
    "src/eval/builtins/net/construction.rs::replay#1|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::classify_equality#1|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::classify_equality#2|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::classify_ordering#1|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::classify_ordering#2|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::demand_tuple_payload#1|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::impl ComparisonBuiltinMachine::finish#1|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::impl DictEqualityFrame::new#1|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::impl DictEqualityFrame::new#2|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::impl DictEqualityFrame::new#3|surface=access-publication|scope=production",
    "src/eval/comparison_machine.rs::impl DictEqualityFrame::new#4|surface=access-publication|scope=production",
    "src/eval/dict_machine.rs::finish_merge_duplicate#1|surface=access-publication|scope=production",
    "src/eval/dict_machine.rs::finish_union#1|surface=access-publication|scope=production",
    "src/eval/dict_machine.rs::finish_update#1|surface=access-publication|scope=production",
    "src/eval/dict_machine.rs::impl DictBuiltinMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/effect_machine.rs::impl EffectBuiltinMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/effect_machine.rs::root_application#1|surface=access-publication|scope=production",
    "src/eval/effect_machine.rs::root_effect_call#1|surface=access-publication|scope=production",
    "src/eval/effect_machine.rs::root_effect_map#1|surface=access-publication|scope=production",
    "src/eval/effect_machine.rs::root_effect_map_continuation#1|surface=access-publication|scope=production",
    "src/eval/effect_machine.rs::root_effect_map_sequence#1|surface=access-publication|scope=production",
    "src/eval/list_machine.rs::impl ManagedListBackRoot::poll_in#1|surface=access-publication|scope=production",
    "src/eval/list_machine.rs::impl ManagedListBackRoot::poll_in#2|surface=access-publication|scope=production",
    "src/eval/list_machine.rs::impl ManagedListFrontRoot::poll_in#1|surface=access-publication|scope=production",
    "src/eval/list_machine.rs::impl ManagedListFrontRoot::poll_in#2|surface=access-publication|scope=production",
    "src/eval/list_machine.rs::tests::back_projection_uses_one_managed_root_and_survives_deferred_collection#1|surface=scoped-factory|scope=test",
    "src/eval/list_machine.rs::tests::front_projection_uses_one_managed_root_and_survives_deferred_collection#1|surface=scoped-factory|scope=test",
    "src/eval/list_observation_machine.rs::rooted_binary_split#1|surface=access-publication|scope=production",
    "src/eval/list_observation_machine.rs::rooted_list_from_items#1|surface=access-publication|scope=production",
    "src/eval/list_observation_machine.rs::rooted_split_from_items_and_root#1|surface=access-publication|scope=production",
    "src/eval/list_observation_machine.rs::rooted_split_from_root_and_items#1|surface=access-publication|scope=production",
    "src/eval/list_transform_machine.rs::finish_text_lines#1|surface=access-publication|scope=production",
    "src/eval/list_transform_machine.rs::impl ListConcatMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/list_transform_machine.rs::impl ListMapMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/net/tests/nc5.rs::callable_checkpoint_admits_each_lazy_source_family_once#1|surface=compatibility-new|scope=test",
    "src/eval/object_builtin_machine.rs::finish_dict_defs#1|surface=access-publication|scope=production",
    "src/eval/object_builtin_machine.rs::optional_spec_member#1|surface=access-publication|scope=production",
    "src/eval/object_builtin_machine.rs::root_instance_from_plain_dict#1|surface=access-publication|scope=production",
    "src/eval/object_builtin_machine.rs::root_local_name#1|surface=access-publication|scope=production",
    "src/eval/object_builtin_machine.rs::root_object_from_dict#1|surface=access-publication|scope=production",
    "src/eval/object_builtin_machine.rs::root_object_instance#1|surface=access-publication|scope=production",
    "src/eval/object_builtin_machine.rs::spec_name#1|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::finish_object_extension#1|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::impl ObjectCompositionMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::insert_override_value#1|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::next_override_step#1|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::next_override_step#2|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::next_override_step#3|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::root_application#1|surface=access-publication|scope=production",
    "src/eval/object_composition_machine.rs::root_plain_extension_in#1|surface=access-publication|scope=production",
    "src/eval/pattern_machine.rs::impl PatternDictTakeMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/pattern_machine.rs::pattern_effect_in#1|surface=access-publication|scope=production",
    "src/eval/strategy_machine.rs::impl StrategyDemandMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/tagged_machine.rs::impl SemanticUndefinedMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/tagged_machine.rs::impl TaggedPayloadMachine::new#1|surface=access-publication|scope=production",
    "src/eval/tagged_machine.rs::impl TaggedPayloadMachine::new#2|surface=access-publication|scope=production",
    "src/eval/tests.rs::concurrent_host_calls_share_one_rooted_producer_without_parking#1|surface=compatibility-new|scope=test",
    "src/eval/tests.rs::dropped_reflection_completion_activation_permit_terminalizes_managed_promise#1|surface=access-publication|scope=test",
    "src/eval/tests.rs::host_call_rejects_a_foreign_runtime_root#1|surface=compatibility-new|scope=test",
    "src/eval/tests.rs::unobserved_reflection_failure_remains_reportable_until_promise_propagation#1|surface=access-publication|scope=test",
    "src/eval/tests.rs::wrapper_application_budget_probe_yields_without_publishing_a_cache#1|surface=compatibility-new|scope=test",
    "src/eval/tests.rs::wrapper_returning_function_then_accepts_remaining_application#1|surface=compatibility-new|scope=test",
    "src/eval/value.rs::impl LazyTaskMachine::poll#1|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll#2|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll#3|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll_access_checkpoint#1|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll_access_checkpoint#2|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll_list_effect_checkpoint#1|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll_list_effect_checkpoint#2|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll_object_fixpoint_checkpoint#1|surface=access-publication|scope=production",
    "src/eval/value.rs::impl LazyTaskMachine::poll_object_fixpoint_checkpoint#2|surface=access-publication|scope=production",
    "src/eval/value/tests/w4.rs::completed_host_call_checkpoint_survives_route_loss_and_collection#1|surface=compatibility-new|scope=test",
    "src/eval/value/tests/w4.rs::host_call_follows_a_lazy_result_without_reinvocation#1|surface=access-publication|scope=test",
    "src/eval/value/tests/w4.rs::host_call_yields_on_both_sides_and_consumes_its_result_once#1|surface=compatibility-new|scope=test",
    "src/eval/value/tests/w4.rs::list_effect_fix_checkpoint_constructs_and_assigns_one_promise#1|surface=compatibility-new|scope=test",
    "src/eval/value/tests/w4.rs::list_effect_fix_checkpoint_constructs_and_assigns_one_promise#2|surface=compatibility-new|scope=test",
    "src/eval/value/tests/w4.rs::list_effect_run_checkpoint_does_not_replay_effect_or_handler_demand#1|surface=compatibility-new|scope=test",
    "src/eval/value/tests/w4.rs::list_effect_run_checkpoint_does_not_replay_effect_or_handler_demand#2|surface=compatibility-new|scope=test",
    "src/eval/value/tests/w4.rs::object_checkpoint_does_not_replay_mixin_stages_after_route_loss#1|surface=compatibility-new|scope=test",
    "src/eval/value/tests/w4.rs::object_checkpoint_does_not_replay_mixin_stages_after_route_loss#2|surface=compatibility-new|scope=test",
    "src/eval/whnf.rs::impl WhnfComputation::from_promise_root#1|surface=scoped-factory|scope=production",
    "src/eval/whnf.rs::regional_status_poll#1|surface=access-publication|scope=production",
    "src/eval/whnf/tests/w1c.rs::collection_between_polls_preserves_only_the_installed_checkpoint#1|surface=scoped-factory|scope=test",
    "src/eval/whnf/tests/w1c.rs::root#1|surface=scoped-factory|scope=test",
    "src/eval/whnf/tests/w2a.rs::lazy_computation#1|surface=scoped-factory|scope=test",
    "src/eval/whnf/tests/w2b.rs::promise_computation#1|surface=scoped-factory|scope=test",
    "src/eval/whnf/tests/w6g3a.rs::root#1|surface=scoped-factory|scope=test",
    "src/eval/whnf/tests/w6g3e.rs::managed_checkpoint_resumes_on_another_worker_after_collection#1|surface=scoped-factory|scope=test",
    "src/eval/whnf/tests/w6g3e.rs::one_poll_aggregates_every_focus_and_frame_edit_into_one_edge_transition#1|surface=scoped-factory|scope=test",
    "src/evaluation/access.rs::impl EvaluationPollContext::root_value#1|surface=scoped-factory|scope=test",
    "src/evaluation/access.rs::impl EvaluatorStepContext < '_ >::root_value#1|surface=scoped-factory|scope=production",
    "src/evaluation/coordinator/spark.rs::impl EvaluationWorkCoordinator::submit_spark#1|surface=scoped-factory|scope=test",
    "src/evaluation/coordinator/task.rs::promise_assignment_terminal#1|surface=scoped-factory|scope=production",
    "src/evaluation/coordinator/tests.rs::a_task_reblocked_on_another_wait_ignores_its_prior_terminal_source#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::a_task_reblocked_on_another_wait_ignores_its_prior_terminal_source#2|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::deferred_insertion_is_immediately_dormant_and_promotable#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::exact_and_broad_task_wakes_share_one_block_epoch#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::exact_wait_completion_requeues_only_its_cross_session_task#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::exact_wait_completion_requeues_only_its_cross_session_task#2|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::permanent_exit_wait_retains_only_its_summary_and_obligations#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::reflection_promise_terminal_mapper_covers_every_terminal_disposition#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::reflection_promise_terminal_mapper_covers_every_terminal_disposition#2|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::retired_deferred_machine_does_not_delay_same_session_client_admission#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::retired_task_makes_a_late_exact_wait_wake_harmless#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::terminal_publication_releases_same_session_client_admission_before_retirement#1|surface=compatibility-new|scope=test",
    "src/evaluation/coordinator/tests.rs::worker_and_runtime_pump_selectors_reject_foreground_client_demand#1|surface=compatibility-new|scope=test",
    "src/evaluation/pump.rs::poison_lazy_cycle#1|surface=access-publication|scope=production",
    "src/evaluation/session.rs::impl EvalContext::complete_wait_with_value#1|surface=compatibility-new|scope=test",
    "src/evaluation/session.rs::impl EvalContext::compose_builtin#1|surface=scoped-factory|scope=production",
    "src/evaluation/session.rs::impl EvalContext::evaluate_compatibility_whnf#1|surface=scoped-factory|scope=production",
    "src/evaluation/session.rs::impl EvalContext::reserve_reflection_task#1|surface=scoped-factory|scope=production",
    "src/evaluation/tests.rs::abandoning_one_client_demand_preserves_another_exact_consumer#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::all_poll_routes_use_scheduler_context#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::blocked_client_checkpoint_survives_collection_until_promise_assignment#1|surface=access-publication|scope=test",
    "src/evaluation/tests.rs::client_demand_can_follow_a_lazy_producer_owned_by_another_session#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_completes_whnf_into_its_result_cell#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_exactly_restarts_after_promise_assignment#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_operation_and_result_roots_follow_owner_lifecycle#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_operation_and_result_roots_follow_owner_lifecycle#2|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_owner_close_and_forced_kill_answer_once#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_owner_close_and_forced_kill_answer_once#2|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_result_cell_releases_after_terminal_handle_drop#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_retirement_publishes_after_runtime_unlock#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_retirement_publishes_after_runtime_unlock#2|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_retirement_publishes_after_runtime_unlock#3|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_demand_retirement_publishes_after_runtime_unlock#4|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_failure_root_survives_work_and_owner_session_retirement#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::client_lazy_root#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::exit_readiness_snapshot_root_survives_after_settlement_report_drop#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::exit_wait_does_not_publish_task_status_or_failure#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::forced_deadlock_settlement_preserves_exits_and_kills_other_participants#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::foreground_client_demand_closes_the_retirement_publication_handoff#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::generic_client_demand_resumes_composed_access_and_binary_annotation#1|surface=access-publication|scope=test",
    "src/evaluation/tests.rs::lazy_task_follow_retains_a_fresh_deferred_result_across_polls#1|surface=scoped-factory|scope=test",
    "src/evaluation/tests.rs::pending_reflection_activation_roots_retire_with_their_reservations#1|surface=access-publication|scope=test",
    "src/evaluation/tests.rs::promise_follow_reprojects_its_rooted_assignment_across_polls#1|surface=scoped-factory|scope=test",
    "src/evaluation/tests.rs::readiness_reports_terminalizing_work_as_busy_without_mutating_it#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::ready_settlement_publishes_exited_once_and_retains_exit_errors#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::retained_client_handle_waits_across_external_disturbance_without_a_lost_wake#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::retained_client_handle_waits_across_external_disturbance_without_a_lost_wake#2|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::root_promise_value#1|surface=access-publication|scope=test",
    "src/evaluation/tests.rs::rooted_promise_value#1|surface=access-publication|scope=test",
    "src/evaluation/tests.rs::rooted_semantic_lazy_value#1|surface=access-publication|scope=test",
    "src/evaluation/tests.rs::runtime_deadlock_retains_typed_task_and_client_dependencies#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::runtime_readiness_retains_exit_dispositions_without_settling_tasks#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::settled_report_root_survives_after_exit_snapshot_and_task_retire#1|surface=compatibility-new|scope=test",
    "src/evaluation/tests.rs::terminal_wait_dispositions_retain_only_their_documented_runtime_roots#1|surface=compatibility-new|scope=test",
    "src/evaluation/whnf.rs::tests::task_owned_promise_self_observation_fails_outside_regional_access#1|surface=scoped-factory|scope=test",
    "src/evaluation/whnf.rs::tests::unassigned_resolver_promise_becomes_a_direct_dependency#1|surface=scoped-factory|scope=test",
    "src/evaluation/whnf.rs::tests::uncached_lazy_admission_occurs_after_regional_access_closes#1|surface=scoped-factory|scope=test",
    "src/g_syntax.rs::impl Diagnostic::with_emission#1|surface=scoped-factory|scope=production",
    "src/g_syntax/compiler_values.rs::evaluate_closed#1|surface=scoped-factory|scope=production",
    "src/g_syntax/compiler_values.rs::root_value#1|surface=scoped-factory|scope=production",
    "src/g_syntax/compiler_values.rs::run_pure_match_resolved#1|surface=scoped-factory|scope=production",
    "src/g_syntax/macro_expansion/effects.rs::hidden_effect#1|surface=scoped-factory|scope=production",
    "src/g_syntax/macro_expansion/tests.rs::environment_root#1|surface=compatibility-new|scope=test",
    "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::lower_declaration#1|surface=scoped-factory|scope=production",
    "src/g_syntax/module_lowering.rs::impl ModuleLowerer < 'context >::lower_declaration#1|surface=access-publication|scope=production",
    "src/g_syntax/tests.rs::reflection_test_module#1|surface=scoped-factory|scope=test",
    "src/reflection/machine.rs::alternative_returns_root#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::effect_api#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::impl Branch < S >::new#1|surface=access-publication|scope=production",
    "src/reflection/machine.rs::impl Branch < S >::new#2|surface=access-publication|scope=production",
    "src/reflection/machine.rs::impl Branch < S >::root_value#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::impl Branch < S >::set_state#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::impl ContextualValueEffectTask < S >::new#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::impl EffectTask < S >::capture_continuation#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::impl EffectTask < S >::store_path_step#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::lazy_value_path_root#1|surface=scoped-factory|scope=production",
    "src/reflection/machine.rs::volume_effects#1|surface=scoped-factory|scope=production",
    "src/reflection/machine/tests.rs::captured_control_installation_waits_before_publishing_its_resume_layer#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::captured_control_installation_waits_before_publishing_its_resume_layer#2|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::captured_control_installation_waits_before_publishing_its_resume_layer#3|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::captured_control_payloads_retain_roots_until_retirement#1|surface=compatibility-new|scope=test",
    "src/reflection/machine/tests.rs::delivery_selects_reset_or_delimiter_only_after_stack_decoding#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::delivery_selects_reset_or_delimiter_only_after_stack_decoding#2|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::delivery_selects_reset_or_delimiter_only_after_stack_decoding#3|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::dropping_a_blocked_reset_stack_decoder_releases_its_owner_without_consuming_the_promise#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::execution_work_and_cut_payloads_retain_roots_until_retirement#1|surface=compatibility-new|scope=test",
    "src/reflection/machine/tests.rs::fixpoint_frames_retain_the_shared_function_root_until_retirement#1|surface=compatibility-new|scope=test",
    "src/reflection/machine/tests.rs::fixpoint_restart_retains_its_selection_while_the_entry_stack_is_blocked#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::fixpoint_restart_retains_its_selection_while_the_entry_stack_is_blocked#2|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::initial_fixpoint_waits_for_the_reset_stack_before_allocating_control#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::malformed_restore_stack_fails_before_popping_the_delimiter#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::malformed_restore_stack_fails_before_popping_the_delimiter#2|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::malformed_restore_stack_fails_before_popping_the_delimiter#3|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::request_decode_resumes_the_exact_lazy_list_chunk_without_replay#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::request_decode_resumes_the_exact_lazy_payload_without_replay#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_checks_every_fixed_frame_arity#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_preserves_strict_frames_and_serialized_root#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_propagates_a_deferred_failure#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_rejects_each_invalid_frame_field#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_rejects_non_list_and_wrong_arity#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_resumes_a_promised_numeric_field#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_resumes_each_lazy_field_once_and_leaves_continuation_lazy#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::reset_stack_decoder_resumes_each_lazy_structural_layer_once#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::restore_delimiter_waits_for_its_saved_stack_before_replacing_control#1|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::restore_delimiter_waits_for_its_saved_stack_before_replacing_control#2|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::restore_delimiter_waits_for_its_saved_stack_before_replacing_control#3|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::restore_delimiter_waits_for_its_saved_stack_before_replacing_control#4|surface=scoped-factory|scope=test",
    "src/reflection/machine/tests.rs::resume_request_decodes_lazy_ids_once_in_source_order#1|surface=scoped-factory|scope=test",
    "src/reflection/protocol.rs::impl EffectRequestSpec < R >::effect#1|surface=scoped-factory|scope=production",
    "src/reflection/store.rs::apply_edit#1|surface=scoped-factory|scope=production",
    "src/reflection/store.rs::apply_value_at_path#1|surface=scoped-factory|scope=production",
    "src/reflection/store.rs::impl StoreJournal::peek_query_with_observation#1|surface=scoped-factory|scope=production",
    "src/reflection/store.rs::impl StoreSnapshot::poll_query#1|surface=scoped-factory|scope=production",
    "src/reflection/store/tests.rs::query_state_is_transactional_and_retired_after_the_last_handle#1|surface=scoped-factory|scope=test",
    "src/runtime.rs::impl RuntimeFailureRoot::root_direct_values#1|surface=access-publication|scope=production",
    "src/runtime.rs::impl RuntimeValueRoot::new#1|surface=access-publication|scope=test",
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

fn is_inventoried_source(relative: &Path) -> bool {
    if relative.starts_with("src/bin") {
        return false;
    }
    if relative
        .components()
        .any(|component| component.as_os_str() == "tests")
    {
        return false;
    }
    if relative
        .file_name()
        .is_some_and(|name| name == "tests.rs" || name == "access_inventory.rs")
    {
        return false;
    }
    true
}

#[test]
fn registered_runtime_root_publication_inventory_is_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(manifest)
                .expect("a discovered source should belong to this package");
            if !is_inventoried_source(relative) {
                return None;
            }
            let source = fs::read_to_string(&path).expect("an inventoried source should be UTF-8");
            let counts = RootPublicationCounts::in_source(&source);
            (counts != RootPublicationCounts::new(0, 0, 0)).then(|| {
                (
                    relative
                        .to_str()
                        .expect("repository source paths should be UTF-8")
                        .replace('\\', "/"),
                    counts,
                )
            })
        })
        .collect::<BTreeMap<_, _>>();

    let mut expected = BTreeMap::new();
    for entry in INVENTORY {
        assert!(!entry.role.is_empty(), "{} needs a role", entry.path);
        assert!(
            !entry.migration.is_empty(),
            "{} needs a migration checkpoint",
            entry.path
        );
        assert!(
            expected
                .insert(entry.path.to_owned(), entry.counts)
                .is_none(),
            "{} appears twice in the root-publication inventory",
            entry.path
        );
    }

    assert_eq!(actual, expected);
}

#[test]
fn every_runtime_root_publication_has_an_exact_disposition() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let occurrences = collect_root_publication_occurrences(manifest);
    let actual = occurrences
        .iter()
        .map(RootPublicationOccurrence::record)
        .collect::<Vec<_>>();
    let expected = EXPECTED_ROOT_PUBLICATION_OCCURRENCES
        .iter()
        .map(|record| (*record).to_owned())
        .collect::<Vec<_>>();

    if actual.len() != expected.len() {
        let actual_set = actual.iter().collect::<BTreeSet<_>>();
        let expected_set = expected.iter().collect::<BTreeSet<_>>();
        let missing = expected_set
            .difference(&actual_set)
            .copied()
            .collect::<Vec<_>>();
        let unexpected = actual_set
            .difference(&expected_set)
            .copied()
            .collect::<Vec<_>>();
        panic!(
            "runtime-root publication ledger length drifted: missing {missing:?}; unexpected {unexpected:?}"
        );
    }
    if let Some((index, (actual, expected))) = actual
        .iter()
        .zip(&expected)
        .enumerate()
        .find(|(_, (actual, expected))| actual != expected)
    {
        panic!(
            "runtime-root publication ledger drifted at index {index}:\n  actual: {actual}\nexpected: {expected}"
        );
    }

    let reviews = occurrences
        .iter()
        .map(RootPublicationOccurrence::review)
        .collect::<Vec<_>>();
    assert_eq!(
        reviews
            .iter()
            .filter(|review| review.disposition == RootPublicationDisposition::Defect)
            .count(),
        0,
        "the immediate nested lazy-cycle root construction defect must stay repaired"
    );
    assert_eq!(
        reviews
            .iter()
            .filter(|review| review.disposition == RootPublicationDisposition::CanonicalConstructor)
            .count(),
        2,
        "only the two CoreValueFactory implementation calls are canonical constructors"
    );
    assert!(
        occurrences
            .iter()
            .zip(&reviews)
            .all(|(occurrence, review)| {
                (occurrence.test_only
                    && review.owner == RootPublicationOwner::D2eTestFixtures
                    && review.disposition == RootPublicationDisposition::TemporaryTestFixture)
                    || (!occurrence.test_only
                        && review.owner != RootPublicationOwner::D2eTestFixtures
                        && review.disposition != RootPublicationDisposition::TemporaryTestFixture)
            }),
        "production and test-only root-publication dispositions must remain distinct"
    );
}

#[test]
fn runtime_root_construction_does_not_reenter_an_active_access_region() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let nested = collect_root_publication_occurrences(manifest)
        .into_iter()
        .filter(|occurrence| {
            occurrence.access_nesting != 0
                && occurrence.surface == RootPublicationSurface::ScopedFactory
        })
        .map(|occurrence| occurrence.record())
        .collect::<Vec<_>>();
    assert!(
        nested.is_empty(),
        "factory root construction reentered active runtime access: {nested:?}"
    );
}

#[test]
fn public_whnf_orchestration_reuses_registered_input_and_completion_roots() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let evaluator = fs::read_to_string(manifest.join("src/api/evaluator.rs"))
        .expect("the public evaluator source should be readable");
    let assembly = fs::read_to_string(manifest.join("src/api/assembly.rs"))
        .expect("the assembly source should be readable");

    assert!(evaluator.contains(".evaluate_root_whnf(value.0.clone())"));
    assert!(evaluator.contains("Value::from_runtime_root(value)"));
    assert!(!evaluator.contains("values.clone_core(value)?"));
    assert!(assembly.contains(".evaluate_root_whnf(definitions.clone())"));
}

#[test]
fn public_value_switch_inventory_has_no_compatibility_escape() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);

    let forbidden = [
        ".as_core()",
        ".into_core()",
        "Value::from_core(",
        "Value::from_runtime(",
        "Self::from_runtime(",
        "RuntimeValueRoot::from_runtime(",
    ];
    let mut escapes = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a discovered source should belong to this package");
        if !is_inventoried_source(relative) {
            continue;
        }
        let source = fs::read_to_string(&path).expect("production source should be UTF-8");
        for forbidden in forbidden {
            if source.contains(forbidden) {
                escapes.push((relative.to_path_buf(), forbidden));
            }
        }
    }

    assert!(
        escapes.is_empty(),
        "the managed public-value switch regained authority-free core escapes: {escapes:?}"
    );
}

fn braced_item_after<'a>(source: &'a str, marker: &str) -> &'a str {
    let start = source
        .find(marker)
        .expect("source item marker should exist");
    let open = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("source item should have a body");
    let mut depth = 0_usize;
    for (offset, byte) in source[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("source item body should be balanced");
}

#[test]
fn public_value_facade_exposes_no_core_or_provenance_observer() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(manifest.join("src/api/value.rs"))
        .expect("the public value facade should be readable");
    let value_impl = braced_item_after(&source, "impl Value {");

    for forbidden in [
        "pub fn as_core",
        "pub fn into_core",
        "pub(crate) fn into_core",
        "pub fn runtime_id",
        "pub fn is_undefined",
        "pub fn as_binary",
        "pub fn as_i64",
        "pub fn kind",
        "pub fn as_number_text",
    ] {
        assert!(
            !value_impl.contains(forbidden),
            "public Value facade regained forbidden surface `{forbidden}`"
        );
    }
}

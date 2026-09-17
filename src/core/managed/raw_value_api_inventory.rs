//! GCI11R-002D.1a syntax-backed inventory of APIs transporting raw core values.
//!
//! `core::Value` is not lifetime branded, so Rust's type system does not yet
//! prevent it from escaping a managed-access region. This inventory makes
//! every production signature which directly or through a type alias carries
//! that representation an explicit review event. Storage declarations remain
//! owned by `durable_owner_inventory`; this module audits operations on them.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::visit::{self, Visit};
use syn::{Attribute, ImplItem, Item, ReturnType, Signature, TraitItem, Type, UseTree};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ApiKind {
    Function,
    TypeAlias,
    DerivedTrait,
}

impl ApiKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::TypeAlias => "type-alias",
            Self::DerivedTrait => "derived-trait",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ApiDisposition {
    RegionalRepresentation,
    RegionalAccess,
    CollectorPrimitive,
    Violation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum RemediationOwner {
    D2bCoreCompatibility,
    D2cEvaluator,
    D2dOrchestration,
    D2eFrontend,
    D2fReflection,
    D2gPublicCompilerDiagnostics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ReplacementShape {
    CoreStructuralOperation,
    ManagedCellAccess,
    RuntimeRootProjection,
    EvaluatorQuantum,
    RootedOrchestration,
    FrontendRegion,
    ReflectionRegionOrRoot,
    PublicDurableBoundary,
    CompilerDiagnosticRegion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum D2cFamily {
    ValueDemand,
    ApplicationAndSequence,
    OperatorAndNet,
    DispatchScalarAndStrategy,
    CollectionsAndPatterns,
    AnnotationsAndEffects,
    Objects,
    NetBuiltins,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum D2cCurrentContext {
    EvaluatorStep,
    DurableEval,
    ContextFree,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum D2cExecutionShape {
    RegionalOperation,
    SuspendableCoordinator,
    ImmediateDataHelper,
}

/// Exact implementation checkpoint for each live D.2c declaration after the
/// resumable-WHNF W5 boundary. The per-checkpoint fingerprints below prevent a
/// broad source-family rule from silently absorbing later declarations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum D2cCheckpoint {
    W6A0aLazyOwnerHandoff,
    W6A0bNumericProjection,
    W6A0cKeyTagUndefined,
    W6A0dLazyListProjection,
    W8ValueCompatibility,
    W6A1ApplicationLeaves,
    W6A2ApplicationWork,
    W6A3SequenceLeaves,
    W6A4SequenceWork,
    W6B1OperatorDescriptors,
    W6B2OperatorExecution,
    W6B3NetClaimProjection,
    W6B4NetApplication,
    W6C1DispatchAndArity,
    W6C2AssertionAndConditional,
    W6C3Comparison,
    W6C4Numeric,
    W6C5Provenance,
    W6D1DictBasic,
    W6D2DictMerge,
    W6D3ListObservation,
    W6D4ListTransformAndDispatch,
    W6D5aPatternDictAndPath,
    W6D5bPatternList,
    W6D5cPatternEffectAndDispatch,
    W6E1AnnotationRecognition,
    W6E2AnnotationCollections,
    W6E3MetadataPure,
    W6E4AnnotationReflection,
    W6E5EffectDispatchAndFixpoint,
    W6E6EffectMap,
    W6E7ListEffectApi,
    W6E8ListEffectControl,
    W6E9ListEffectSource,
    W6F1ObjectLeaves,
    W6F2ObjectSpecification,
    W6F3ObjectComposition,
    W6F4ObjectInstantiation,
    W6F5NetDispatch,
    W6F6NetConstructionLifecycle,
    W6F7NetConstructionValues,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RemediationAssignment {
    owner: RemediationOwner,
    replacement: ReplacementShape,
}

impl ApiDisposition {
    const fn label(self) -> &'static str {
        match self {
            Self::RegionalRepresentation => "regional-representation",
            Self::RegionalAccess => "regional-access",
            Self::CollectorPrimitive => "collector-primitive",
            Self::Violation => "violation",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
struct TypeSignals {
    raw_values: usize,
    value_accesses: usize,
}

impl TypeSignals {
    fn merge(&mut self, other: Self) {
        self.raw_values += other.raw_values;
        self.value_accesses += other.value_accesses;
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ApiOccurrence {
    declaration: String,
    shape: String,
    kind: ApiKind,
    inputs: usize,
    outputs: usize,
    access: usize,
}

impl ApiOccurrence {
    fn disposition(&self) -> ApiDisposition {
        if self.kind == ApiKind::TypeAlias {
            ApiDisposition::RegionalRepresentation
        } else if self.access != 0 {
            ApiDisposition::RegionalAccess
        } else if self
            .declaration
            .starts_with("src/core/managed/payload_edges")
            || self.declaration == "src/core/managed.rs::trace_compatibility_value_managed_edges"
            || self.declaration == "src/core/managed/recursive_cells.rs::trace_promise_assignment"
            || self.declaration == "src/eval/whnf.rs::trace_whnf_value"
            || self.declaration == "src/eval/whnf.rs::trace_whnf_values"
        {
            ApiDisposition::CollectorPrimitive
        } else {
            ApiDisposition::Violation
        }
    }

    fn record(&self) -> String {
        format!(
            "{}|shape={}|kind={}|in={}|out={}|access={}|disposition={}",
            self.declaration,
            self.shape,
            self.kind.label(),
            self.inputs,
            self.outputs,
            self.access,
            self.disposition().label(),
        )
    }

    fn remediation_assignment(&self) -> Option<RemediationAssignment> {
        if self.disposition() != ApiDisposition::Violation {
            return None;
        }

        let path = self
            .declaration
            .split("::")
            .next()
            .expect("an inventory declaration should begin with a source path");
        let assignment = if path == "src/core.rs" || path == "src/core_net.rs" {
            RemediationAssignment {
                owner: RemediationOwner::D2bCoreCompatibility,
                replacement: ReplacementShape::CoreStructuralOperation,
            }
        } else if path.starts_with("src/core/") {
            RemediationAssignment {
                owner: RemediationOwner::D2bCoreCompatibility,
                replacement: ReplacementShape::ManagedCellAccess,
            }
        } else if path == "src/runtime.rs" {
            RemediationAssignment {
                owner: RemediationOwner::D2bCoreCompatibility,
                replacement: ReplacementShape::RuntimeRootProjection,
            }
        } else if path == "src/eval.rs" || path.starts_with("src/eval/") {
            RemediationAssignment {
                owner: RemediationOwner::D2cEvaluator,
                replacement: ReplacementShape::EvaluatorQuantum,
            }
        } else if path == "src/evaluation.rs" || path.starts_with("src/evaluation/") {
            RemediationAssignment {
                owner: RemediationOwner::D2dOrchestration,
                replacement: ReplacementShape::RootedOrchestration,
            }
        } else if path == "src/g_syntax.rs" || path.starts_with("src/g_syntax/") {
            RemediationAssignment {
                owner: RemediationOwner::D2eFrontend,
                replacement: ReplacementShape::FrontendRegion,
            }
        } else if path == "src/reflection.rs" || path.starts_with("src/reflection/") {
            RemediationAssignment {
                owner: RemediationOwner::D2fReflection,
                replacement: ReplacementShape::ReflectionRegionOrRoot,
            }
        } else if path == "src/api.rs" || path.starts_with("src/api/") {
            RemediationAssignment {
                owner: RemediationOwner::D2gPublicCompilerDiagnostics,
                replacement: ReplacementShape::PublicDurableBoundary,
            }
        } else if matches!(
            path,
            "src/compiler.rs" | "src/diagnostic.rs" | "src/source.rs"
        ) {
            RemediationAssignment {
                owner: RemediationOwner::D2gPublicCompilerDiagnostics,
                replacement: ReplacementShape::CompilerDiagnosticRegion,
            }
        } else {
            panic!(
                "{} has no reviewed GCI11R-002D.2 remediation owner",
                self.declaration
            );
        };
        Some(assignment)
    }

    fn d2c_family(&self) -> Option<D2cFamily> {
        if self.remediation_assignment()?.owner != RemediationOwner::D2cEvaluator {
            return None;
        }
        let path = self
            .declaration
            .split("::")
            .next()
            .expect("an inventory declaration should begin with a source path");
        let family = match path {
            "src/eval/value.rs" => D2cFamily::ValueDemand,
            "src/eval/application.rs" | "src/eval/sequence.rs" => D2cFamily::ApplicationAndSequence,
            "src/eval/operator.rs" | "src/eval/net.rs" | "src/eval/whnf.rs" => {
                D2cFamily::OperatorAndNet
            }
            "src/eval/builtins.rs"
            | "src/eval/builtins/assertion.rs"
            | "src/eval/builtins/comparison.rs"
            | "src/eval/builtins/comparison/implementation.rs"
            | "src/eval/builtins/conditional.rs"
            | "src/eval/builtins/numeric.rs"
            | "src/eval/builtins/numeric/implementation.rs"
            | "src/eval/builtins/provenance.rs"
            | "src/eval/builtins/strategy.rs" => D2cFamily::DispatchScalarAndStrategy,
            "src/eval/builtins/dict.rs"
            | "src/eval/builtins/dict/basic.rs"
            | "src/eval/builtins/dict/merge.rs"
            | "src/eval/builtins/list.rs"
            | "src/eval/builtins/list/implementation.rs"
            | "src/eval/builtins/pattern.rs" => D2cFamily::CollectionsAndPatterns,
            "src/eval/builtins/annotation.rs"
            | "src/eval/builtins/annotation/implementation.rs"
            | "src/eval/builtins/effect.rs"
            | "src/eval/builtins/effect/implementation.rs"
            | "src/eval/builtins/list_effect.rs"
            | "src/eval/builtins/list_effect/implementation.rs"
            | "src/eval/list_effect_machine.rs" => D2cFamily::AnnotationsAndEffects,
            "src/eval/builtins/object.rs"
            | "src/eval/builtins/object/implementation.rs"
            | "src/eval/object_machine.rs" => D2cFamily::Objects,
            "src/eval/builtins/net.rs" | "src/eval/builtins/net/construction.rs" => {
                D2cFamily::NetBuiltins
            }
            _ => panic!("{} has no reviewed GCI11R-002D.2c family", self.declaration),
        };
        Some(family)
    }

    fn d2c_current_context(&self) -> Option<D2cCurrentContext> {
        self.d2c_family()?;
        Some(if self.shape.contains("EvaluatorStepContext") {
            D2cCurrentContext::EvaluatorStep
        } else if self.shape.contains("EvalContext") {
            D2cCurrentContext::DurableEval
        } else {
            D2cCurrentContext::ContextFree
        })
    }

    /// Conservative initial migration shape for D.2c.0.
    ///
    /// An existing step/durable context may cross a demand boundary, so it is
    /// a coordinator until its family checkpoint proves otherwise and splits
    /// out a regional leaf. Context-free raw-value operations must first gain
    /// regional authority; a later family checkpoint may instead eliminate
    /// one by narrowing it to immediate semantic data.
    fn d2c_execution_shape(&self) -> Option<D2cExecutionShape> {
        Some(match self.d2c_current_context()? {
            D2cCurrentContext::EvaluatorStep | D2cCurrentContext::DurableEval => {
                D2cExecutionShape::SuspendableCoordinator
            }
            D2cCurrentContext::ContextFree => D2cExecutionShape::RegionalOperation,
        })
    }

    fn d2c_checkpoint(&self) -> Option<D2cCheckpoint> {
        let family = self.d2c_family()?;
        let path = self
            .declaration
            .split("::")
            .next()
            .expect("an inventory declaration should begin with a source path");
        let name = self
            .declaration
            .rsplit("::")
            .next()
            .expect("an inventory declaration should end with a name");

        use D2cCheckpoint::*;
        use D2cFamily::*;
        let checkpoint = match (family, path, name) {
            (ValueDemand, _, "complete" | "follow_value" | "is_error_lazy_value") => {
                W6A0aLazyOwnerHandoff
            }
            (ValueDemand, _, "eval_number_in" | "eval_index_number_in") => W6A0bNumericProjection,
            (
                ValueDemand,
                _,
                "value_to_key_in" | "tagged_payload_in" | "is_semantically_undefined_in",
            ) => W6A0cKeyTagUndefined,
            (ValueDemand, _, "force_list_thunk_in" | "pop_list_front_in") => {
                W6A0dLazyListProjection
            }
            (
                ValueDemand,
                _,
                "eval_value"
                | "eval_value_in"
                | "eval_lazy_in"
                | "eval_promised_in"
                | "await_deferred_task"
                | "deferred_wait_result"
                | "produce_lazy_source_in",
            ) => W8ValueCompatibility,

            (
                ApplicationAndSequence,
                "src/eval/application.rs",
                "apply_effect_function_value" | "effect_value" | "non_callable_error",
            ) => W6A1ApplicationLeaves,
            (ApplicationAndSequence, "src/eval/application.rs", _) => W6A2ApplicationWork,
            (
                ApplicationAndSequence,
                "src/eval/sequence.rs",
                "append_sequence" | "append_values",
            ) => W6A3SequenceLeaves,
            (ApplicationAndSequence, "src/eval/sequence.rs", _) => W6A4SequenceWork,

            (
                OperatorAndNet,
                "src/eval/operator.rs",
                "apply_core_operator" | "constant_effect_in_step",
            ) => W6B2OperatorExecution,
            (OperatorAndNet, "src/eval/operator.rs", _) => W6B1OperatorDescriptors,
            (OperatorAndNet, "src/eval/net.rs", "callable" | "parts") => W6B3NetClaimProjection,
            (OperatorAndNet, "src/eval/net.rs", _) => W6B4NetApplication,
            (OperatorAndNet, "src/eval/whnf.rs", _) => W6B4NetApplication,

            (DispatchScalarAndStrategy, "src/eval/builtins.rs", _) => W6C1DispatchAndArity,
            (
                DispatchScalarAndStrategy,
                "src/eval/builtins/assertion.rs" | "src/eval/builtins/conditional.rs",
                _,
            ) => W6C2AssertionAndConditional,
            (
                DispatchScalarAndStrategy,
                "src/eval/builtins/comparison.rs"
                | "src/eval/builtins/comparison/implementation.rs",
                _,
            ) => W6C3Comparison,
            (
                DispatchScalarAndStrategy,
                "src/eval/builtins/numeric.rs" | "src/eval/builtins/numeric/implementation.rs",
                _,
            ) => W6C4Numeric,
            (DispatchScalarAndStrategy, "src/eval/builtins/provenance.rs", _) => W6C5Provenance,

            (
                CollectionsAndPatterns,
                "src/eval/builtins/dict.rs" | "src/eval/builtins/dict/basic.rs",
                _,
            ) => W6D1DictBasic,
            (CollectionsAndPatterns, "src/eval/builtins/dict/merge.rs", _) => W6D2DictMerge,
            (
                CollectionsAndPatterns,
                "src/eval/builtins/list/implementation.rs",
                "eval_list_at_builtin"
                | "eval_list_head_builtin"
                | "eval_list_len_builtin"
                | "eval_list_split_builtin"
                | "eval_list_split_end_builtin"
                | "eval_list_tail_builtin"
                | "eval_slice_builtin",
            ) => W6D3ListObservation,
            (
                CollectionsAndPatterns,
                "src/eval/builtins/list.rs" | "src/eval/builtins/list/implementation.rs",
                _,
            ) => W6D4ListTransformAndDispatch,
            (
                CollectionsAndPatterns,
                "src/eval/builtins/pattern.rs",
                "dict_is_logically_empty"
                | "pattern_dict_is_empty"
                | "pattern_dict_try_take"
                | "pattern_equal"
                | "pattern_is_dict"
                | "pattern_path_equal"
                | "pattern_path_keys"
                | "pattern_value_key"
                | "take_dict_path"
                | "value_is_logically_undefined",
            ) => W6D5aPatternDictAndPath,
            (
                CollectionsAndPatterns,
                "src/eval/builtins/pattern.rs",
                "list_item_value"
                | "pattern_is_list"
                | "pattern_list_is_empty"
                | "pattern_list_try_uncons"
                | "pattern_list_try_unsnoc",
            ) => W6D5bPatternList,
            (CollectionsAndPatterns, "src/eval/builtins/pattern.rs", _) => {
                W6D5cPatternEffectAndDispatch
            }

            (
                AnnotationsAndEffects,
                "src/eval/builtins/annotation/implementation.rs",
                "annotation_error_value"
                | "annotation_name"
                | "is_undefined_value"
                | "parse_assertion_annotation"
                | "parse_value_annotation"
                | "payload_is_unit"
                | "recognize_annotation",
            ) => W6E1AnnotationRecognition,
            (
                AnnotationsAndEffects,
                "src/eval/builtins/annotation/implementation.rs",
                "eval_array_annotation" | "eval_binary_annotation" | "eval_deque_annotation",
            ) => W6E2AnnotationCollections,
            (
                AnnotationsAndEffects,
                "src/eval/builtins/annotation/implementation.rs",
                "eval_metadata_pure_annotation"
                | "metadata_update_inputs"
                | "metadata_update_outputs",
            ) => W6E3MetadataPure,
            (
                AnnotationsAndEffects,
                "src/eval/builtins/annotation.rs"
                | "src/eval/builtins/annotation/implementation.rs",
                _,
            ) => W6E4AnnotationReflection,
            (AnnotationsAndEffects, "src/eval/builtins/effect.rs", _)
            | (
                AnnotationsAndEffects,
                "src/eval/builtins/effect/implementation.rs",
                "apply_effect_api" | "eval_fixpoint_builtin",
            ) => W6E5EffectDispatchAndFixpoint,
            (AnnotationsAndEffects, "src/eval/builtins/effect/implementation.rs", _) => {
                W6E6EffectMap
            }
            (AnnotationsAndEffects, "src/eval/list_effect_machine.rs", _) => W6E7ListEffectApi,
            (
                AnnotationsAndEffects,
                "src/eval/builtins/list_effect/implementation.rs",
                "cut_list_effect_results"
                | "eval_list_effect_alt_builtin"
                | "eval_list_effect_builtin"
                | "eval_list_effect_cut_builtin"
                | "eval_list_effect_seq_builtin"
                | "flat_map_list_effect_results",
            ) => W6E8ListEffectControl,
            (
                AnnotationsAndEffects,
                "src/eval/builtins/list_effect.rs"
                | "src/eval/builtins/list_effect/implementation.rs",
                _,
            ) => W6E9ListEffectSource,

            (
                Objects,
                "src/eval/builtins/object/implementation.rs",
                "default_object_defs_value"
                | "dict_object_spec"
                | "object_spec_from_parts"
                | "object_spec_name_value",
            ) => W6F1ObjectLeaves,
            (
                Objects,
                "src/eval/builtins/object/implementation.rs",
                "eval_diagnostic_object_builtin"
                | "eval_object_local_name_builtin"
                | "eval_object_spec_builtin"
                | "object_spec_dict",
            ) => W6F2ObjectSpecification,
            (
                Objects,
                "src/eval/builtins/object/implementation.rs",
                "eval_object_composed_defs_builtin"
                | "eval_object_override_defs_builtin"
                | "eval_object_with_defs_builtin"
                | "override_dict",
            ) => W6F3ObjectComposition,
            (Objects, _, _) => W6F4ObjectInstantiation,

            (NetBuiltins, "src/eval/builtins/net.rs", _) => W6F5NetDispatch,
            (NetBuiltins, "src/eval/builtins/net/construction.rs", "new" | "poll" | "replay") => {
                W6F6NetConstructionLifecycle
            }
            (NetBuiltins, "src/eval/builtins/net/construction.rs", _) => W6F7NetConstructionValues,
            _ => panic!(
                "{} has no reviewed resumable-WHNF W6/W8 checkpoint",
                self.declaration
            ),
        };
        Some(checkpoint)
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
    !relative.file_name().is_some_and(|name| {
        name == "tests.rs"
            || name == "test_support.rs"
            || name.to_string_lossy().ends_with("_inventory.rs")
    })
}

fn flatten_use(tree: &UseTree, prefix: &mut Vec<String>, imports: &mut Vec<(Vec<String>, String)>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_use(&path.tree, prefix, imports);
            prefix.pop();
        }
        UseTree::Name(name) => {
            let mut source = prefix.clone();
            source.push(name.ident.to_string());
            imports.push((source, name.ident.to_string()));
        }
        UseTree::Rename(rename) => {
            let mut source = prefix.clone();
            source.push(rename.ident.to_string());
            imports.push((source, rename.rename.to_string()));
        }
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use(item, prefix, imports);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn is_raw_value_path(
    path: &[String],
    core_child: bool,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
) -> bool {
    (path.len() >= 3
        && path[path.len() - 3..path.len() - 1] == ["crate", "core"]
        && canonical_core_names.contains(&path[path.len() - 1]))
        || (path.len() >= 2
            && path[path.len() - 2] == "core"
            && canonical_core_names.contains(&path[path.len() - 1]))
        || (core_child
            && path
                .last()
                .is_some_and(|name| canonical_core_names.contains(name))
            && path.first().is_some_and(|name| name == "super"))
        || (path.len() >= 2
            && path
                .last()
                .is_some_and(|name| cross_file_alias_names.contains(name))
            && !is_api_value_path(path))
}

fn is_api_value_path(path: &[String]) -> bool {
    path.ends_with(&["crate".to_owned(), "api".to_owned(), "Value".to_owned()])
        || path.ends_with(&["api".to_owned(), "Value".to_owned()])
        || path.ends_with(&["glam".to_owned(), "Value".to_owned()])
}

fn initial_raw_names(
    relative: &Path,
    syntax: &syn::File,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
) -> BTreeSet<String> {
    let core_source = relative == Path::new("src/core.rs");
    let core_child = relative.starts_with("src/core/");
    let mut names = BTreeSet::new();
    let mut imports_raw_value = false;
    let mut imports_public_value = false;

    for item in &syntax.items {
        let Item::Use(item) = item else {
            continue;
        };
        let mut imports = Vec::new();
        flatten_use(&item.tree, &mut Vec::new(), &mut imports);
        for (source, local) in imports {
            let imports_api_value = is_api_value_path(&source)
                || ((relative.starts_with("src/api/") || relative == Path::new("src/api.rs"))
                    && source.last().is_some_and(|name| name == "Value")
                    && source
                        .first()
                        .is_some_and(|name| matches!(name.as_str(), "self" | "super")));
            if imports_api_value && local == "Value" {
                imports_public_value = true;
            } else if is_raw_value_path(
                &source,
                core_child,
                canonical_core_names,
                cross_file_alias_names,
            ) {
                imports_raw_value |=
                    source.last().is_some_and(|name| name == "Value") && local == "Value";
                names.insert(local);
            }
        }
    }

    if core_source
        || core_child
        || imports_raw_value
        || (!imports_public_value
            && !relative.starts_with("src/api/")
            && relative != Path::new("src/api.rs")
            && relative != Path::new("src/list.rs"))
    {
        names.insert("Value".to_owned());
    } else {
        names.remove("Value");
    }
    names
}

struct TypeSignalVisitor<'names> {
    raw_names: &'names BTreeSet<String>,
    canonical_core_names: &'names BTreeSet<String>,
    cross_file_alias_names: &'names BTreeSet<String>,
    access_names: &'names BTreeSet<String>,
    signals: TypeSignals,
}

impl<'ast> Visit<'ast> for TypeSignalVisitor<'_> {
    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        let segments = node
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        let terminal = segments.last().map(String::as_str);
        let explicit_core = segments.len() >= 2
            && segments[segments.len() - 2] == "core"
            && self
                .canonical_core_names
                .contains(&segments[segments.len() - 1]);
        let explicit_alias = segments.len() >= 2
            && self
                .cross_file_alias_names
                .contains(&segments[segments.len() - 1]);
        if !is_api_value_path(&segments)
            && (explicit_core
                || explicit_alias
                || terminal.is_some_and(|name| self.raw_names.contains(name)))
        {
            self.signals.raw_values += 1;
        }
        if terminal.is_some_and(|name| self.access_names.contains(name)) {
            self.signals.value_accesses += 1;
        }
        visit::visit_type_path(self, node);
    }
}

fn type_signals(
    ty: &Type,
    raw_names: &BTreeSet<String>,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
    access_names: &BTreeSet<String>,
) -> TypeSignals {
    let mut visitor = TypeSignalVisitor {
        raw_names,
        canonical_core_names,
        cross_file_alias_names,
        access_names,
        signals: TypeSignals::default(),
    };
    visitor.visit_type(ty);
    visitor.signals
}

fn generic_signals(
    generics: &syn::Generics,
    raw_names: &BTreeSet<String>,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
    access_names: &BTreeSet<String>,
) -> TypeSignals {
    let mut visitor = TypeSignalVisitor {
        raw_names,
        canonical_core_names,
        cross_file_alias_names,
        access_names,
        signals: TypeSignals::default(),
    };
    visitor.visit_generics(generics);
    visitor.signals
}

fn discover_raw_aliases(
    items: &[Item],
    names: &mut BTreeSet<String>,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
    access_names: &BTreeSet<String>,
) -> bool {
    let mut changed = false;
    for item in items {
        match item {
            Item::Type(item) if !is_test_only(&item.attrs) => {
                if type_signals(
                    &item.ty,
                    names,
                    canonical_core_names,
                    cross_file_alias_names,
                    access_names,
                )
                .raw_values
                    != 0
                {
                    changed |= names.insert(item.ident.to_string());
                }
            }
            Item::Mod(item) if !is_test_only(&item.attrs) => {
                if let Some((_, items)) = &item.content {
                    changed |= discover_raw_aliases(
                        items,
                        names,
                        canonical_core_names,
                        cross_file_alias_names,
                        access_names,
                    );
                }
            }
            _ => {}
        }
    }
    changed
}

fn collect_raw_alias_names(
    items: &[Item],
    raw_names: &BTreeSet<String>,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
    access_names: &BTreeSet<String>,
    aliases: &mut BTreeSet<String>,
) -> bool {
    let mut changed = false;
    for item in items {
        match item {
            Item::Type(item) if !is_test_only(&item.attrs) => {
                if type_signals(
                    &item.ty,
                    raw_names,
                    canonical_core_names,
                    cross_file_alias_names,
                    access_names,
                )
                .raw_values
                    != 0
                {
                    changed |= aliases.insert(item.ident.to_string());
                }
            }
            Item::Mod(item) if !is_test_only(&item.attrs) => {
                if let Some((_, items)) = &item.content {
                    changed |= collect_raw_alias_names(
                        items,
                        raw_names,
                        canonical_core_names,
                        cross_file_alias_names,
                        access_names,
                        aliases,
                    );
                }
            }
            _ => {}
        }
    }
    changed
}

struct AccessNameVisitor<'names> {
    access_names: &'names BTreeSet<String>,
    found: bool,
}

impl<'ast> Visit<'ast> for AccessNameVisitor<'_> {
    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        if node
            .path
            .segments
            .last()
            .is_some_and(|segment| self.access_names.contains(&segment.ident.to_string()))
        {
            self.found = true;
        }
        visit::visit_type_path(self, node);
    }
}

fn type_contains_access(ty: &Type, access_names: &BTreeSet<String>) -> bool {
    let mut visitor = AccessNameVisitor {
        access_names,
        found: false,
    };
    visitor.visit_type(ty);
    visitor.found
}

fn discover_access_carriers(items: &[Item], names: &mut BTreeSet<String>) -> bool {
    let mut changed = false;
    for item in items {
        match item {
            Item::Struct(item) if !is_test_only(&item.attrs) => {
                if item
                    .fields
                    .iter()
                    .any(|field| type_contains_access(&field.ty, names))
                {
                    changed |= names.insert(item.ident.to_string());
                }
            }
            Item::Enum(item) if !is_test_only(&item.attrs) => {
                if item
                    .variants
                    .iter()
                    .flat_map(|variant| &variant.fields)
                    .any(|field| type_contains_access(&field.ty, names))
                {
                    changed |= names.insert(item.ident.to_string());
                }
            }
            Item::Type(item) if !is_test_only(&item.attrs) => {
                if type_contains_access(&item.ty, names) {
                    changed |= names.insert(item.ident.to_string());
                }
            }
            Item::Mod(item) if !is_test_only(&item.attrs) => {
                if let Some((_, items)) = &item.content {
                    changed |= discover_access_carriers(items, names);
                }
            }
            _ => {}
        }
    }
    changed
}

fn signature_input_signals(
    signature: &Signature,
    raw_names: &BTreeSet<String>,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
    access_names: &BTreeSet<String>,
    owner_raw: bool,
    owner_access: bool,
) -> TypeSignals {
    let mut inputs = generic_signals(
        &signature.generics,
        raw_names,
        canonical_core_names,
        cross_file_alias_names,
        access_names,
    );
    if owner_raw {
        inputs.raw_values += usize::from(signature.receiver().is_some());
    }
    if owner_access && signature.receiver().is_some() {
        inputs.value_accesses += 1;
    }
    for input in &signature.inputs {
        if let syn::FnArg::Typed(input) = input {
            inputs.merge(type_signals(
                &input.ty,
                raw_names,
                canonical_core_names,
                cross_file_alias_names,
                access_names,
            ));
        }
    }
    inputs
}

fn signature_output_signals(
    signature: &Signature,
    raw_names: &BTreeSet<String>,
    canonical_core_names: &BTreeSet<String>,
    cross_file_alias_names: &BTreeSet<String>,
    access_names: &BTreeSet<String>,
) -> TypeSignals {
    match &signature.output {
        ReturnType::Default => TypeSignals::default(),
        ReturnType::Type(_, ty) => type_signals(
            ty,
            raw_names,
            canonical_core_names,
            cross_file_alias_names,
            access_names,
        ),
    }
}

fn simple_type_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

struct ApiVisitor<'path, 'names> {
    path: &'path Path,
    raw_names: &'names BTreeSet<String>,
    canonical_core_names: &'names BTreeSet<String>,
    cross_file_alias_names: &'names BTreeSet<String>,
    access_names: &'names BTreeSet<String>,
    modules: Vec<String>,
    owner: Option<String>,
    owner_raw: bool,
    owner_access: bool,
    occurrences: Vec<ApiOccurrence>,
}

impl ApiVisitor<'_, '_> {
    fn declaration(&self, name: &str) -> String {
        let mut parts = vec![self.path.display().to_string()];
        parts.extend(self.modules.iter().cloned());
        if let Some(owner) = &self.owner {
            parts.push(owner.clone());
        }
        parts.push(name.to_owned());
        parts.join("::")
    }

    fn record_signature(&mut self, signature: &Signature) {
        let inputs = signature_input_signals(
            signature,
            self.raw_names,
            self.canonical_core_names,
            self.cross_file_alias_names,
            self.access_names,
            self.owner_raw,
            self.owner_access,
        );
        let outputs = signature_output_signals(
            signature,
            self.raw_names,
            self.canonical_core_names,
            self.cross_file_alias_names,
            self.access_names,
        );
        if inputs.raw_values == 0 && outputs.raw_values == 0 {
            return;
        }
        self.occurrences.push(ApiOccurrence {
            declaration: self.declaration(&signature.ident.to_string()),
            shape: signature.to_token_stream().to_string(),
            kind: ApiKind::Function,
            inputs: inputs.raw_values,
            outputs: outputs.raw_values,
            access: inputs.value_accesses + outputs.value_accesses,
        });
    }

    fn record_alias(&mut self, name: &str, ty: &Type) {
        let signals = type_signals(
            ty,
            self.raw_names,
            self.canonical_core_names,
            self.cross_file_alias_names,
            self.access_names,
        );
        if signals.raw_values != 0 {
            self.occurrences.push(ApiOccurrence {
                declaration: self.declaration(&format!("type {name}")),
                shape: ty.to_token_stream().to_string(),
                kind: ApiKind::TypeAlias,
                inputs: 0,
                outputs: signals.raw_values,
                access: signals.value_accesses,
            });
        }
    }
}

impl<'ast> Visit<'ast> for ApiVisitor<'_, '_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if is_test_only(&item.attrs) {
            return;
        }
        self.modules.push(item.ident.to_string());
        visit::visit_item_mod(self, item);
        self.modules.pop();
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if is_test_only(&item.attrs) {
            return;
        }
        let owner = simple_type_name(&item.self_ty);
        let owner_signals = type_signals(
            &item.self_ty,
            self.raw_names,
            self.canonical_core_names,
            self.cross_file_alias_names,
            self.access_names,
        );
        let prior_owner = std::mem::replace(&mut self.owner, owner.clone());
        let prior_raw = std::mem::replace(
            &mut self.owner_raw,
            owner_signals.raw_values != 0
                || owner
                    .as_ref()
                    .is_some_and(|name| self.raw_names.contains(name)),
        );
        let prior_access = std::mem::replace(
            &mut self.owner_access,
            owner_signals.value_accesses != 0
                || owner
                    .as_ref()
                    .is_some_and(|name| self.access_names.contains(name)),
        );
        for member in &item.items {
            match member {
                ImplItem::Fn(function) if !is_test_only(&function.attrs) => {
                    self.record_signature(&function.sig);
                }
                ImplItem::Type(alias) if !is_test_only(&alias.attrs) => {
                    self.record_alias(&alias.ident.to_string(), &alias.ty);
                }
                _ => {}
            }
        }
        self.owner = prior_owner;
        self.owner_raw = prior_raw;
        self.owner_access = prior_access;
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !is_test_only(&item.attrs) {
            self.record_signature(&item.sig);
        }
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if is_test_only(&item.attrs) {
            return;
        }
        let prior = self.owner.replace(format!("trait {}", item.ident));
        for member in &item.items {
            match member {
                TraitItem::Fn(function) if !is_test_only(&function.attrs) => {
                    self.record_signature(&function.sig);
                }
                TraitItem::Type(alias) if !is_test_only(&alias.attrs) => {
                    if let Some((_, ty)) = &alias.default {
                        self.record_alias(&alias.ident.to_string(), ty);
                    }
                }
                _ => {}
            }
        }
        self.owner = prior;
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if !is_test_only(&item.attrs) {
            self.record_alias(&item.ident.to_string(), &item.ty);
        }
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        if is_test_only(&item.attrs) || !self.raw_names.contains(&item.ident.to_string()) {
            return;
        }
        for attribute in &item.attrs {
            if !attribute.path().is_ident("derive") {
                continue;
            }
            let Ok(traits) = attribute.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            ) else {
                continue;
            };
            for derived in traits {
                let Some(name) = derived
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                else {
                    continue;
                };
                let (inputs, outputs) = match name.as_str() {
                    "Clone" => (1, 1),
                    "PartialEq" => (2, 0),
                    // `Eq` is a marker but still records the standard-trait
                    // representation promise made by the raw value type.
                    "Eq" => (1, 0),
                    _ => continue,
                };
                self.occurrences.push(ApiOccurrence {
                    declaration: self.declaration(&format!("derive {name}")),
                    shape: format!("#[derive({name})] {}", item.ident),
                    kind: ApiKind::DerivedTrait,
                    inputs,
                    outputs,
                    access: 0,
                });
            }
        }
    }
}

fn collect_occurrences(manifest: &Path) -> Vec<ApiOccurrence> {
    let mut sources = Vec::new();
    collect_rust_sources(&manifest.join("src"), &mut sources);
    sources.sort();

    let mut parsed = Vec::new();
    for source_path in sources {
        let relative = source_path
            .strip_prefix(manifest)
            .expect("a source path should belong to this package");
        if !is_production_source(relative) {
            continue;
        }
        let source = fs::read_to_string(&source_path).expect("Rust source should be readable");
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} should parse as Rust: {error}", relative.display()));
        parsed.push((relative.to_path_buf(), syntax));
    }

    let core_access_names = BTreeSet::from([
        "RuntimeValueAccess".to_owned(),
        "EvaluationValueAccess".to_owned(),
    ]);

    let mut canonical_core_names = BTreeSet::from(["Value".to_owned()]);
    let no_cross_file_aliases = BTreeSet::new();
    loop {
        let core = parsed
            .iter()
            .find(|(path, _)| path == Path::new("src/core.rs"))
            .expect("the core value source must be inventoried");
        let prior = canonical_core_names.clone();
        let changed = discover_raw_aliases(
            &core.1.items,
            &mut canonical_core_names,
            &prior,
            &no_cross_file_aliases,
            &core_access_names,
        );
        if !changed {
            break;
        }
    }

    let mut cross_file_alias_names = canonical_core_names.clone();
    loop {
        let mut changed = false;
        let known_alias_names = cross_file_alias_names.clone();
        for (path, syntax) in &parsed {
            let mut access_names = core_access_names.clone();
            while discover_access_carriers(&syntax.items, &mut access_names) {}
            let mut names =
                initial_raw_names(path, syntax, &canonical_core_names, &known_alias_names);
            while discover_raw_aliases(
                &syntax.items,
                &mut names,
                &canonical_core_names,
                &known_alias_names,
                &access_names,
            ) {}
            changed |= collect_raw_alias_names(
                &syntax.items,
                &names,
                &canonical_core_names,
                &known_alias_names,
                &access_names,
                &mut cross_file_alias_names,
            );
        }
        if !changed {
            break;
        }
    }

    let mut occurrences = Vec::new();
    for (path, syntax) in parsed {
        let mut access_names = core_access_names.clone();
        while discover_access_carriers(&syntax.items, &mut access_names) {}
        let mut names = initial_raw_names(
            &path,
            &syntax,
            &canonical_core_names,
            &cross_file_alias_names,
        );
        while discover_raw_aliases(
            &syntax.items,
            &mut names,
            &canonical_core_names,
            &cross_file_alias_names,
            &access_names,
        ) {}
        let mut visitor = ApiVisitor {
            path: &path,
            raw_names: &names,
            canonical_core_names: &canonical_core_names,
            cross_file_alias_names: &cross_file_alias_names,
            access_names: &access_names,
            modules: Vec::new(),
            owner: None,
            owner_raw: false,
            owner_access: false,
            occurrences: Vec::new(),
        };
        visitor.visit_file(&syntax);
        occurrences.extend(visitor.occurrences);
    }
    occurrences.sort();
    occurrences
}

fn occurrence_fingerprint(occurrences: &[ApiOccurrence]) -> u64 {
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

fn occurrence_summary(occurrences: &[ApiOccurrence]) -> BTreeMap<(ApiKind, ApiDisposition), usize> {
    occurrences
        .iter()
        .fold(BTreeMap::new(), |mut counts, occurrence| {
            *counts
                .entry((occurrence.kind, occurrence.disposition()))
                .or_default() += 1;
            counts
        })
}

fn occurrence_file_summary(occurrences: &[ApiOccurrence]) -> BTreeMap<String, (usize, usize)> {
    occurrences
        .iter()
        .fold(BTreeMap::new(), |mut counts, occurrence| {
            let path = occurrence
                .declaration
                .split("::")
                .next()
                .expect("an inventory declaration should begin with a source path");
            let count = counts.entry(path.to_owned()).or_insert((0, 0));
            if occurrence.disposition() == ApiDisposition::Violation {
                count.1 += 1;
            } else {
                count.0 += 1;
            }
            counts
        })
}

fn remediation_summary(
    occurrences: &[ApiOccurrence],
) -> BTreeMap<(RemediationOwner, ReplacementShape), usize> {
    occurrences
        .iter()
        .filter_map(ApiOccurrence::remediation_assignment)
        .fold(BTreeMap::new(), |mut counts, assignment| {
            *counts
                .entry((assignment.owner, assignment.replacement))
                .or_default() += 1;
            counts
        })
}

fn d2c_occurrences(occurrences: &[ApiOccurrence]) -> Vec<&ApiOccurrence> {
    occurrences
        .iter()
        .filter(|occurrence| occurrence.d2c_family().is_some())
        .collect()
}

#[test]
fn raw_core_value_api_inventory_is_complete() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let actual = collect_occurrences(manifest);

    if std::env::var_os("GLAM_DUMP_RAW_VALUE_API_INVENTORY").is_some() {
        for occurrence in &actual {
            eprintln!("{}", occurrence.record());
        }
    }

    assert_eq!(
        actual.len(),
        536,
        "inventory count drifted: {:#?}",
        occurrence_summary(&actual)
    );
    assert_eq!(
        occurrence_fingerprint(&actual),
        10_568_053_404_139_570_714,
        "inventory fingerprint drifted: {:#?}",
        occurrence_file_summary(&actual),
    );
}

#[test]
fn raw_core_value_api_inventory_has_reviewed_dispositions() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let actual = collect_occurrences(manifest);
    let expected = BTreeMap::from([
        ((ApiKind::Function, ApiDisposition::RegionalAccess), 130),
        ((ApiKind::Function, ApiDisposition::CollectorPrimitive), 28),
        ((ApiKind::Function, ApiDisposition::Violation), 368),
        (
            (ApiKind::TypeAlias, ApiDisposition::RegionalRepresentation),
            7,
        ),
        ((ApiKind::DerivedTrait, ApiDisposition::Violation), 3),
    ]);

    assert_eq!(
        occurrence_summary(&actual),
        expected,
        "raw core-value APIs require a reviewed access/disposition classification"
    );

    assert!(actual.iter().all(|occurrence| {
        occurrence.declaration != "src/evaluation/session.rs::EvalContext::evaluate_whnf"
    }));
    let compatibility_evaluate_whnf = actual
        .iter()
        .find(|occurrence| {
            occurrence.declaration
                == "src/evaluation/session.rs::EvalContext::evaluate_compatibility_whnf"
        })
        .expect("the narrowed raw WHNF compatibility facade remains assigned to D.2");
    assert_eq!(
        compatibility_evaluate_whnf.disposition(),
        ApiDisposition::Violation
    );

    assert!(actual.iter().any(|occurrence| {
        occurrence.declaration == "src/api/assembly.rs::Assembler::compile_diagnostic_emitter"
            && occurrence.disposition() == ApiDisposition::Violation
    }));
    assert!(actual.iter().any(|occurrence| {
        occurrence.declaration == "src/g_syntax/resolve/scope.rs::NameScope::resolved"
            && occurrence.disposition() == ApiDisposition::Violation
    }));
    assert!(
        actual
            .iter()
            .all(|occurrence| !occurrence.declaration.starts_with("src/bin/")),
        "the binary crate should expose only durable public Value handles"
    );
    assert!(actual.iter().all(|occurrence| {
        occurrence.disposition() != ApiDisposition::CollectorPrimitive
            || occurrence
                .declaration
                .starts_with("src/core/managed/payload_edges")
            || occurrence.declaration
                == "src/core/managed/recursive_cells.rs::trace_promise_assignment"
            || occurrence.declaration
                == "src/core/managed.rs::trace_compatibility_value_managed_edges"
            || occurrence.declaration == "src/eval/whnf.rs::trace_whnf_value"
            || occurrence.declaration == "src/eval/whnf.rs::trace_whnf_values"
    }));
}

#[test]
fn every_raw_value_violation_has_one_reviewed_remediation_assignment() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let actual = collect_occurrences(manifest);
    let expected = BTreeMap::from([
        (
            (
                RemediationOwner::D2bCoreCompatibility,
                ReplacementShape::CoreStructuralOperation,
            ),
            39,
        ),
        (
            (
                RemediationOwner::D2cEvaluator,
                ReplacementShape::EvaluatorQuantum,
            ),
            112,
        ),
        (
            (
                RemediationOwner::D2dOrchestration,
                ReplacementShape::RootedOrchestration,
            ),
            12,
        ),
        (
            (
                RemediationOwner::D2eFrontend,
                ReplacementShape::FrontendRegion,
            ),
            137,
        ),
        (
            (
                RemediationOwner::D2fReflection,
                ReplacementShape::ReflectionRegionOrRoot,
            ),
            26,
        ),
        (
            (
                RemediationOwner::D2gPublicCompilerDiagnostics,
                ReplacementShape::PublicDurableBoundary,
            ),
            11,
        ),
        (
            (
                RemediationOwner::D2gPublicCompilerDiagnostics,
                ReplacementShape::CompilerDiagnosticRegion,
            ),
            34,
        ),
    ]);

    assert_eq!(
        remediation_summary(&actual),
        expected,
        "each raw-value violation needs exactly one checkpoint owner and replacement shape"
    );
    assert_eq!(
        actual
            .iter()
            .filter(|occurrence| occurrence.disposition() == ApiDisposition::Violation)
            .count(),
        expected.values().sum(),
        "the remediation manifest must account for every violation"
    );
}

#[test]
fn d2b_core_compatibility_declarations_are_exact() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let actual = collect_occurrences(manifest)
        .into_iter()
        .filter(|occurrence| {
            occurrence
                .remediation_assignment()
                .is_some_and(|assignment| {
                    assignment.owner == RemediationOwner::D2bCoreCompatibility
                })
        })
        .map(|occurrence| occurrence.declaration)
        .collect::<BTreeSet<_>>();
    let expected = [
        "src/core.rs::CoreValueFactory::atom",
        "src/core.rs::CoreValueFactory::clone_cached_root",
        "src/core.rs::CoreValueFactory::error",
        "src/core.rs::CoreValueFactory::info",
        "src/core.rs::CoreValueFactory::initial_metadata",
        "src/core.rs::CoreValueFactory::key_value",
        "src/core.rs::CoreValueFactory::object_reflection_guard",
        "src/core.rs::CoreValueFactory::tuple",
        "src/core.rs::CoreValueFactory::unit",
        "src/core.rs::CoreValueFactory::warn",
        "src/core.rs::EvaluatedValue::into_value",
        "src/core.rs::EvaluatedValue::try_from",
        "src/core.rs::EvaluationFailure::contexts",
        "src/core.rs::EvaluationFailure::emission",
        "src/core.rs::EvaluationFailure::emission_value",
        "src/core.rs::EvaluationFailure::visit_direct_values",
        "src/core.rs::EvaluationFailure::with_context",
        "src/core.rs::HostCallProducer::captures",
        "src/core.rs::HostCallRootBundle::from_captures",
        "src/core.rs::Key::from_value",
        "src/core.rs::Key::to_value_with",
        "src/core.rs::LazyApplication::arguments",
        "src/core.rs::LazyApplication::function",
        "src/core.rs::MetadataCarrier::associated_metadata",
        "src/core.rs::MetadataCarrier::new",
        "src/core.rs::ReflectionComputation::gate",
        "src/core.rs::ReflectionComputation::new",
        "src/core.rs::ReflectionComputation::return_value",
        "src/core.rs::ReflectionComputation::target",
        "src/core.rs::Value::associated_metadata",
        "src/core.rs::Value::diagnostic_kind_name",
        "src/core.rs::Value::fmt",
        "src/core.rs::Value::metadata_carrier",
        "src/core.rs::Value::singleton_list",
        "src/core.rs::derive Clone",
        "src/core.rs::derive Eq",
        "src/core.rs::derive PartialEq",
        "src/core.rs::immediate_failure_text",
        "src/core.rs::list_to_key_items",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "D.2b.4 permits only the exact compatibility declarations handed to D.2c-D.2g and P4"
    );
}

#[test]
fn d2c_evaluator_boundary_manifest_is_exact() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let inventory = collect_occurrences(manifest);
    let occurrences = d2c_occurrences(&inventory);

    let family_counts = occurrences
        .iter()
        .fold(BTreeMap::new(), |mut counts, occurrence| {
            *counts
                .entry(
                    occurrence
                        .d2c_family()
                        .expect("D.2c occurrence needs a family"),
                )
                .or_default() += 1;
            counts
        });
    assert_eq!(
        family_counts,
        BTreeMap::from([
            (D2cFamily::ValueDemand, 12),
            (D2cFamily::ApplicationAndSequence, 7),
            (D2cFamily::DispatchScalarAndStrategy, 2),
            (D2cFamily::CollectionsAndPatterns, 30),
            (D2cFamily::AnnotationsAndEffects, 36),
            (D2cFamily::Objects, 17),
            (D2cFamily::NetBuiltins, 8),
        ]),
        "each raw evaluator operation needs one stable D.2c family"
    );

    let context_counts = occurrences
        .iter()
        .fold(BTreeMap::new(), |mut counts, occurrence| {
            *counts
                .entry(
                    occurrence
                        .d2c_current_context()
                        .expect("D.2c occurrence needs a current context shape"),
                )
                .or_default() += 1;
            counts
        });
    assert_eq!(
        context_counts,
        BTreeMap::from([
            (D2cCurrentContext::EvaluatorStep, 94),
            (D2cCurrentContext::DurableEval, 2),
            (D2cCurrentContext::ContextFree, 16),
        ]),
        "the D.2c signature baseline drifted"
    );

    let execution_counts = occurrences
        .iter()
        .fold([0_usize; 3], |mut counts, occurrence| {
            let index = match occurrence
                .d2c_execution_shape()
                .expect("D.2c occurrence needs an execution shape")
            {
                D2cExecutionShape::RegionalOperation => 0,
                D2cExecutionShape::SuspendableCoordinator => 1,
                D2cExecutionShape::ImmediateDataHelper => 2,
            };
            counts[index] += 1;
            counts
        });
    assert_eq!(
        execution_counts,
        [16, 96, 0],
        "D.2c starts conservatively: context-free operations need regional authority, while context-bearing operations remain coordinators until audited"
    );
}

#[test]
fn d2c_family_fingerprints_are_exact() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let inventory = collect_occurrences(manifest);
    let mut groups = BTreeMap::<D2cFamily, Vec<ApiOccurrence>>::new();
    for occurrence in d2c_occurrences(&inventory) {
        groups
            .entry(
                occurrence
                    .d2c_family()
                    .expect("D.2c occurrence needs a family"),
            )
            .or_default()
            .push(occurrence.clone());
    }
    let actual = groups
        .iter()
        .map(|(family, occurrences)| (*family, occurrence_fingerprint(occurrences)))
        .collect::<BTreeMap<_, _>>();
    let expected = BTreeMap::from([
        (D2cFamily::ValueDemand, 16_603_655_789_842_265_632),
        (
            D2cFamily::ApplicationAndSequence,
            11_551_636_234_692_466_987,
        ),
        (
            D2cFamily::DispatchScalarAndStrategy,
            5_656_025_884_590_611_233,
        ),
        (D2cFamily::CollectionsAndPatterns, 1_071_875_561_606_341_435),
        (D2cFamily::AnnotationsAndEffects, 5_637_277_238_344_972_318),
        (D2cFamily::Objects, 14_243_874_767_540_971_701),
        (D2cFamily::NetBuiltins, 13_862_576_417_551_920_128),
    ]);

    assert_eq!(
        actual, expected,
        "a D.2c declaration or signature moved without updating its family checkpoint"
    );
}

#[test]
fn d2c_w6_checkpoint_manifest_is_exact() {
    use D2cCheckpoint::*;

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let inventory = collect_occurrences(manifest);
    let mut groups = BTreeMap::<D2cCheckpoint, Vec<ApiOccurrence>>::new();
    for occurrence in d2c_occurrences(&inventory) {
        groups
            .entry(
                occurrence
                    .d2c_checkpoint()
                    .expect("D.2c occurrence needs a W6 or W8 checkpoint"),
            )
            .or_default()
            .push(occurrence.clone());
    }

    let actual_counts = groups
        .iter()
        .map(|(checkpoint, occurrences)| (*checkpoint, occurrences.len()))
        .collect::<BTreeMap<_, _>>();
    let expected_counts = BTreeMap::from([
        (W6A0cKeyTagUndefined, 3),
        (W6A0dLazyListProjection, 2),
        (W8ValueCompatibility, 7),
        (W6A1ApplicationLeaves, 1),
        (W6A2ApplicationWork, 4),
        (W6A4SequenceWork, 2),
        (W6C1DispatchAndArity, 1),
        (W6C2AssertionAndConditional, 1),
        (W6D3ListObservation, 7),
        (W6D4ListTransformAndDispatch, 4),
        (W6D5aPatternDictAndPath, 10),
        (W6D5bPatternList, 5),
        (W6D5cPatternEffectAndDispatch, 4),
        (W6E1AnnotationRecognition, 7),
        (W6E2AnnotationCollections, 3),
        (W6E3MetadataPure, 3),
        (W6E4AnnotationReflection, 5),
        (W6E5EffectDispatchAndFixpoint, 4),
        (W6E6EffectMap, 3),
        (W6E7ListEffectApi, 1),
        (W6E8ListEffectControl, 6),
        (W6E9ListEffectSource, 4),
        (W6F1ObjectLeaves, 4),
        (W6F2ObjectSpecification, 4),
        (W6F3ObjectComposition, 4),
        (W6F4ObjectInstantiation, 5),
        (W6F5NetDispatch, 2),
        (W6F6NetConstructionLifecycle, 3),
        (W6F7NetConstructionValues, 3),
    ]);
    assert_eq!(
        actual_counts, expected_counts,
        "every D.2c declaration needs one bounded W6/W8 implementation checkpoint"
    );

    let actual_fingerprints = groups
        .iter()
        .map(|(checkpoint, occurrences)| (*checkpoint, occurrence_fingerprint(occurrences)))
        .collect::<BTreeMap<_, _>>();
    let expected_fingerprints = BTreeMap::from([
        (W6A0cKeyTagUndefined, 13_459_851_214_100_325_735),
        (W6A0dLazyListProjection, 7_696_218_449_487_864_870),
        (W8ValueCompatibility, 15_067_824_851_424_263_475),
        (W6A1ApplicationLeaves, 4_928_394_332_436_527_125),
        (W6A2ApplicationWork, 4_925_520_474_408_746_135),
        (W6A4SequenceWork, 15_567_590_830_686_767_581),
        (W6C1DispatchAndArity, 183_834_627_390_525_313),
        (W6C2AssertionAndConditional, 11_178_720_452_358_268_189),
        (W6D3ListObservation, 1_141_124_844_497_395_933),
        (W6D4ListTransformAndDispatch, 937_907_731_239_525_859),
        (W6D5aPatternDictAndPath, 4_701_694_396_283_851_948),
        (W6D5bPatternList, 13_481_016_508_580_392_350),
        (W6D5cPatternEffectAndDispatch, 7_938_764_653_056_697_587),
        (W6E1AnnotationRecognition, 15_863_802_739_852_521_816),
        (W6E2AnnotationCollections, 5_152_331_018_857_617_448),
        (W6E3MetadataPure, 8_965_714_601_448_224_494),
        (W6E4AnnotationReflection, 9_119_478_075_463_295_836),
        (W6E5EffectDispatchAndFixpoint, 2_047_908_102_180_799_399),
        (W6E6EffectMap, 32_026_630_358_957_828),
        (W6E7ListEffectApi, 1_964_017_467_539_357_249),
        (W6E8ListEffectControl, 5_599_189_213_790_407_454),
        (W6E9ListEffectSource, 16_763_173_768_808_435_452),
        (W6F1ObjectLeaves, 17_471_107_961_031_657_988),
        (W6F2ObjectSpecification, 17_557_287_365_619_708_655),
        (W6F3ObjectComposition, 12_124_644_744_746_303_865),
        (W6F4ObjectInstantiation, 10_760_564_769_723_657_478),
        (W6F5NetDispatch, 4_216_785_241_083_672_531),
        (W6F6NetConstructionLifecycle, 16_082_646_699_173_186_379),
        (W6F7NetConstructionValues, 12_233_159_576_254_963_310),
    ]);
    assert_eq!(
        actual_fingerprints, expected_fingerprints,
        "a D.2c declaration or signature moved between W6/W8 checkpoints"
    );
}

#[test]
fn d2c_context_is_not_mistaken_for_active_access() {
    let occurrence = |shape: &str| ApiOccurrence {
        declaration: "src/eval/value.rs::fixture".to_owned(),
        shape: shape.to_owned(),
        kind: ApiKind::Function,
        inputs: 1,
        outputs: 1,
        access: 0,
    };

    let step = occurrence("fn fixture(context: &EvaluatorStepContext<'_>, value: Value) -> Value");
    assert_eq!(
        step.d2c_current_context(),
        Some(D2cCurrentContext::EvaluatorStep)
    );
    assert_eq!(
        step.d2c_execution_shape(),
        Some(D2cExecutionShape::SuspendableCoordinator)
    );

    let durable = occurrence("fn fixture(context: &EvalContext, value: Value) -> Value");
    assert_eq!(
        durable.d2c_current_context(),
        Some(D2cCurrentContext::DurableEval)
    );
    assert_eq!(
        durable.d2c_execution_shape(),
        Some(D2cExecutionShape::SuspendableCoordinator)
    );

    let regional = occurrence("fn fixture(value: Value) -> Value");
    assert_eq!(
        regional.d2c_current_context(),
        Some(D2cCurrentContext::ContextFree)
    );
    assert_eq!(
        regional.d2c_execution_shape(),
        Some(D2cExecutionShape::RegionalOperation)
    );

    let _future_narrowing = D2cExecutionShape::ImmediateDataHelper;
}

#[test]
fn raw_core_value_type_scanner_covers_wrappers_callbacks_aliases_and_bounds() {
    let raw_names = BTreeSet::from(["Value".to_owned()]);
    let canonical_core_names = raw_names.clone();
    let cross_file_alias_names =
        BTreeSet::from(["CompileDiagnosticEmitter".to_owned(), "Value".to_owned()]);
    let access_names = BTreeSet::from([
        "RuntimeValueAccess".to_owned(),
        "EvaluationValueAccess".to_owned(),
    ]);

    let wrapped: Type =
        syn::parse_str("Option<Result<Vec<Arc<[Value]>>, Box<dyn Fn(&Value) -> Value>>>")
            .expect("the wrapper fixture should parse");
    assert_eq!(
        type_signals(
            &wrapped,
            &raw_names,
            &canonical_core_names,
            &cross_file_alias_names,
            &access_names,
        ),
        TypeSignals {
            raw_values: 3,
            value_accesses: 0,
        }
    );

    let item: syn::ItemFn = syn::parse_str(
        "fn constrained<F>(operation: F) where F: Fn(&Value) -> Result<Value, ()> {}",
    )
    .expect("the generic callback fixture should parse");
    let constrained = signature_input_signals(
        &item.sig,
        &raw_names,
        &canonical_core_names,
        &cross_file_alias_names,
        &access_names,
        false,
        false,
    );
    assert_eq!(constrained.raw_values, 2);

    let imported_alias: Type = syn::parse_str("crate::compiler::CompileDiagnosticEmitter")
        .expect("the cross-file alias fixture should parse");
    assert_ne!(
        type_signals(
            &imported_alias,
            &BTreeSet::new(),
            &canonical_core_names,
            &cross_file_alias_names,
            &access_names,
        )
        .raw_values,
        0
    );

    let public_value: Type =
        syn::parse_str("glam::Value").expect("the public value fixture should parse");
    assert_eq!(
        type_signals(
            &public_value,
            &raw_names,
            &canonical_core_names,
            &cross_file_alias_names,
            &access_names,
        )
        .raw_values,
        0,
        "the durable public facade must not be mistaken for raw core::Value"
    );

    for admitted in [
        "&RuntimeValueAccess<'_>",
        "Option<&EvaluationValueAccess<'_>>",
    ] {
        let value_type: Type = syn::parse_str(admitted).expect("access fixture should parse");
        assert_ne!(
            type_signals(
                &value_type,
                &raw_names,
                &canonical_core_names,
                &cross_file_alias_names,
                &access_names,
            )
            .value_accesses,
            0,
            "{admitted} must qualify a raw-value signature as regional"
        );
    }
    for rejected in ["&EvaluatorStepContext<'_>", "&EvalContext"] {
        let value_type: Type = syn::parse_str(rejected).expect("context fixture should parse");
        assert_eq!(
            type_signals(
                &value_type,
                &raw_names,
                &canonical_core_names,
                &cross_file_alias_names,
                &access_names,
            )
            .value_accesses,
            0,
            "{rejected} coordinates access but must not impersonate an active region"
        );
    }

    let access_carriers = syn::parse_file(
        r#"
        struct Regional<'scope> {
            access: &'scope EvaluationValueAccess<'scope>,
        }
        struct Step<'scope> {
            context: &'scope EvaluatorStepContext<'scope>,
        }
        "#,
    )
    .expect("access-carrier fixtures should parse");
    let mut discovered = access_names.clone();
    while discover_access_carriers(&access_carriers.items, &mut discovered) {}
    assert!(discovered.contains("Regional"));
    assert!(!discovered.contains("Step"));
}

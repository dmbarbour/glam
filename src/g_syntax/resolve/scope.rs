use std::ops::{Deref, DerefMut};

use super::super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) struct LocalName {
    pub(in crate::g_syntax) raw: String,
    pub(in crate::g_syntax) canonical: Option<String>,
    pub(in crate::g_syntax) suppress_unused_warning: bool,
    pub(in crate::g_syntax) binding: Option<BindingId>,
}

/// Per-lowering lexical state. Binding identities are meaningful only within
/// one resolver and are never allocated from process-global state.
#[derive(Debug, Default)]
pub(in crate::g_syntax) struct ResolverContext {
    locals: Vec<LocalName>,
    next_binding_id: u64,
}

impl ResolverContext {
    pub(in crate::g_syntax) fn fresh_binding(&mut self) -> BindingId {
        let binding = BindingId::from_local_index(self.next_binding_id);
        self.next_binding_id = self
            .next_binding_id
            .checked_add(1)
            .expect("g-syntax binding ID space exhausted");
        binding
    }

    pub(in crate::g_syntax) fn push_internal_binding(&mut self, raw: &str) -> BindingId {
        let binding = self.fresh_binding();
        let mut local = local_name_metadata(raw);
        local.binding = Some(binding);
        self.locals.push(local);
        binding
    }

    pub(in crate::g_syntax) fn extend_source_bindings<'a>(
        &mut self,
        names: impl IntoIterator<Item = &'a str>,
        line: usize,
    ) -> Result<Vec<BindingId>, Diagnostic> {
        let pending = names
            .into_iter()
            .map(local_name_metadata)
            .collect::<Vec<_>>();

        for (index, local) in pending.iter().enumerate() {
            let Some(canonical) = local.canonical.as_deref() else {
                continue;
            };
            let conflict = self
                .locals
                .iter()
                .chain(&pending[..index])
                .find(|existing| existing.canonical.as_deref() == Some(canonical));
            if let Some(existing) = conflict {
                return Err(Diagnostic::error(
                    line,
                    format!(
                        "local `{}` shadows existing local `{}`; local name shadowing is not allowed",
                        local.raw, existing.raw
                    ),
                ));
            }
        }

        Ok(pending
            .into_iter()
            .map(|mut local| {
                let binding = self.fresh_binding();
                local.binding = Some(binding);
                self.locals.push(local);
                binding
            })
            .collect())
    }
}

impl Deref for ResolverContext {
    type Target = Vec<LocalName>;

    fn deref(&self) -> &Self::Target {
        &self.locals
    }
}

impl DerefMut for ResolverContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.locals
    }
}

#[cfg(test)]
mod resolver_context_tests {
    use super::*;
    use crate::interaction_net::Node;

    fn unit() -> Value {
        Value::Dict(Dict::new_sync())
    }

    #[test]
    fn binding_ids_are_local_to_each_resolver() {
        let mut left = ResolverContext::default();
        let mut right = ResolverContext::default();

        let left_first = left.fresh_binding();
        let right_first = right.fresh_binding();
        let left_second = left.fresh_binding();

        assert_eq!(left_first, right_first);
        assert_ne!(left_first, left_second);
    }

    #[test]
    fn source_bindings_reject_canonical_shadowing_without_partial_extension() {
        let mut resolver = ResolverContext::default();
        resolver
            .extend_source_bindings(["outer"], 1)
            .expect("first local should bind");

        let error = resolver
            .extend_source_bindings(["fresh", "_outer"], 2)
            .expect_err("suppressed local spelling should not evade shadow checks");

        assert_eq!(
            error.message,
            "local `_outer` shadows existing local `outer`; local name shadowing is not allowed"
        );
        assert_eq!(resolver.len(), 1);
    }

    #[test]
    fn source_bindings_allow_repeated_inaccessible_drops() {
        let mut resolver = ResolverContext::default();
        let bindings = resolver
            .extend_source_bindings(["_", "_"], 1)
            .expect("drop binders do not introduce names that can shadow");

        assert_eq!(bindings.len(), 2);
        assert!(resolver.iter().all(|local| local.canonical.is_none()));
    }

    #[test]
    fn lambda_locals_remain_stable_resolved_bindings() {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let mut resolver = ResolverContext::default();
        let syntax = SyntaxExpr::Lambda(
            vec!["x".to_owned()],
            Box::new(SyntaxExpr::Name("x".to_owned())),
        );

        let resolved =
            syntax_expr_to_resolved_in_scope(&syntax, 1, &context, &scope, &mut resolver).unwrap();

        assert!(matches!(
            resolved,
            ResolvedExpr::Lambda { parameters, body }
                if matches!(body.as_ref(), ResolvedExpr::Local(binding)
                    if *binding == parameters[0])
        ));
    }

    #[test]
    fn direct_lambda_application_uses_the_fused_resolved_form() {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let mut resolver = ResolverContext::default();
        let syntax = SyntaxExpr::Apply(
            Box::new(SyntaxExpr::Lambda(
                vec!["x".to_owned()],
                Box::new(SyntaxExpr::Name("x".to_owned())),
            )),
            Box::new(SyntaxExpr::Unit),
        );

        let resolved =
            syntax_expr_to_resolved_in_scope(&syntax, 1, &context, &scope, &mut resolver).unwrap();

        assert!(matches!(
            resolved,
            ResolvedExpr::ApplyLambda {
                parameters,
                body,
                arguments,
            } if matches!(body.as_ref(), ResolvedExpr::Local(binding)
                if *binding == parameters[0])
                && matches!(arguments.as_slice(), [ResolvedExpr::Embedded(Value::Atom(_))])
        ));
    }

    #[test]
    fn lists_and_accesses_keep_local_binding_identity() {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let mut resolver = ResolverContext::default();
        let syntax = SyntaxExpr::Lambda(
            vec!["x".to_owned()],
            Box::new(SyntaxExpr::List(vec![
                SyntaxExpr::Name("x".to_owned()),
                SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    vec![SyntaxKeyExpr::Atom("field".to_owned())],
                ),
            ])),
        );

        let resolved =
            syntax_expr_to_resolved_in_scope(&syntax, 1, &context, &scope, &mut resolver).unwrap();

        assert!(matches!(
            resolved,
            ResolvedExpr::Lambda { parameters, body }
                if matches!(body.as_ref(), ResolvedExpr::List(items)
                    if matches!(&items[0], ResolvedExpr::Local(binding)
                        if *binding == parameters[0])
                    && matches!(&items[1], ResolvedExpr::Access { base, .. }
                        if matches!(base.as_ref(), ResolvedExpr::Local(binding)
                            if *binding == parameters[0])))
        ));
    }

    #[test]
    fn with_expression_keeps_its_fixpoint_local_in_resolved_ir() {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let mut resolver = ResolverContext::default();
        let syntax = SyntaxExpr::With {
            base: Box::new(SyntaxExpr::PathDict(
                vec![SyntaxKeyExpr::Atom("base".to_owned())],
                Box::new(SyntaxExpr::Unit),
            )),
            alias: None,
            body: vec![ObjectBodyDefinition {
                line: 1,
                text: "copy = self".to_owned(),
                kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                    target: "copy".to_owned(),
                    parameters: vec![],
                    kind: DefinitionKind::Introduce,
                    body: "self".to_owned(),
                    expr: Some(SyntaxExpr::Name("self".to_owned())),
                }),
            }],
        };

        let resolved =
            syntax_expr_to_resolved_in_scope(&syntax, 1, &context, &scope, &mut resolver).unwrap();

        assert!(matches!(
            resolved,
            ResolvedExpr::ApplyLambda { parameters, body, arguments }
                if parameters.len() == 1
                && arguments.len() == 1
                && matches!(body.as_ref(), ResolvedExpr::Apply { function, arguments }
                    if matches!(function.as_ref(),
                        ResolvedExpr::Embedded(Value::Builtin(Builtin::Fixpoint)))
                    && matches!(arguments.as_slice(),
                        [ResolvedExpr::Lambda { parameters, body }]
                        if parameters.len() == 1
                        && body.free_bindings().contains(&parameters[0])))
        ));
    }

    #[test]
    fn object_expression_keeps_self_pair_in_resolved_ir() {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        let mut resolver = ResolverContext::default();
        let syntax = SyntaxExpr::Object(ObjectExpr {
            name: None,
            alias: None,
            deps: Vec::new(),
            body: vec![ObjectBodyDefinition {
                line: 1,
                text: "copy = self".to_owned(),
                kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                    target: "copy".to_owned(),
                    parameters: vec![],
                    kind: DefinitionKind::Introduce,
                    body: "self".to_owned(),
                    expr: Some(SyntaxExpr::Name("self".to_owned())),
                }),
            }],
        });

        let resolved =
            syntax_expr_to_resolved_in_scope(&syntax, 1, &context, &scope, &mut resolver).unwrap();

        assert!(matches!(
            resolved,
            ResolvedExpr::Apply { function, arguments }
                if matches!(function.as_ref(),
                    ResolvedExpr::Embedded(Value::Builtin(Builtin::ObjectInstanceFromParts)))
                && matches!(arguments.as_slice(), [_, _, ResolvedExpr::Lambda { parameters, body }]
                    if parameters.len() == 2
                    && parameters.iter().all(|binding| body.free_bindings().contains(binding)))
        ));
    }

    #[test]
    fn direct_net_emitter_wires_identity_without_a_fan_or_erase() {
        let mut resolver = ResolverContext::default();
        let parameter = resolver.fresh_binding();
        let net =
            ResolvedNetLowerer::lower_template(vec![parameter], ResolvedExpr::Local(parameter));

        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Bind))
                .count(),
            1
        );
        assert!(
            !net.nodes()
                .iter()
                .any(|node| matches!(node, Node::Fan { .. }))
        );
        assert!(!net.nodes().iter().any(|node| matches!(node, Node::Erase)));
    }

    #[test]
    fn direct_net_emitter_erases_unused_parameters() {
        let mut resolver = ResolverContext::default();
        let parameter = resolver.fresh_binding();
        let net =
            ResolvedNetLowerer::lower_template(vec![parameter], ResolvedExpr::Embedded(unit()));

        assert!(net.nodes().iter().any(|node| matches!(node, Node::Erase)));
    }

    #[test]
    fn direct_net_emitter_fans_repeated_parameters_once() {
        let mut resolver = ResolverContext::default();
        let parameter = resolver.fresh_binding();
        let body = ResolvedExpr::apply(
            ResolvedExpr::Local(parameter),
            [ResolvedExpr::Local(parameter)],
        );
        let net = ResolvedNetLowerer::lower_template(vec![parameter], body);

        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Fan { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn direct_net_emitter_builds_one_bind_chain_for_curried_parameters() {
        let mut resolver = ResolverContext::default();
        let first = resolver.fresh_binding();
        let second = resolver.fresh_binding();
        let net =
            ResolvedNetLowerer::lower_template(vec![first, second], ResolvedExpr::Local(first));

        assert_eq!(
            net.nodes()
                .iter()
                .filter(|node| matches!(node, Node::Bind))
                .count(),
            2
        );
    }

    #[test]
    fn direct_net_emitter_lifts_only_free_bindings_as_captures() {
        let mut resolver = ResolverContext::default();
        let parameter = resolver.fresh_binding();
        let capture = resolver.fresh_binding();
        let (code, captures) =
            ResolvedNetLowerer::lower_code(vec![parameter], ResolvedExpr::Local(capture));

        assert_eq!(code.arity(), 1);
        assert_eq!(code.capture_count(), 1);
        assert_eq!(captures, vec![capture]);
    }
}

#[derive(Debug, Clone)]
pub(in crate::g_syntax) struct NameScope<V = Value> {
    pub(in crate::g_syntax) final_defs: V,
    pub(in crate::g_syntax) prior_defs: V,
    pub(in crate::g_syntax) module_final_defs: V,
    pub(in crate::g_syntax) module_prior_defs: V,
    pub(in crate::g_syntax) object_alias: Option<String>,
    pub(in crate::g_syntax) object_final_defs: Option<V>,
    pub(in crate::g_syntax) object_prior_defs: Option<V>,
    /// Final namespace and private heap key used by compiler-generated
    /// reflection demand boundaries. Expression-local objects deliberately
    /// leave this unset.
    pub(in crate::g_syntax) reflection: Option<ReflectionBoundary<V>>,
    pub(in crate::g_syntax) parent: Option<Box<NameScope<V>>>,
}

#[derive(Debug, Clone)]
pub(in crate::g_syntax) struct ReflectionBoundary<V> {
    pub(in crate::g_syntax) annotator: V,
}

/// A name root is deliberately atomic. Reusing it creates another local
/// reference or closed value occurrence, never a second copy of an expression
/// tree that the net emitter would lower again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) enum ResolvedRoot {
    Provided(Value),
    Local(BindingId),
}

impl ResolvedRoot {
    pub(in crate::g_syntax) fn expr(&self) -> ResolvedExpr<Value> {
        match self {
            Self::Provided(value) => ResolvedExpr::Provided(value.clone()),
            Self::Local(binding) => ResolvedExpr::Local(*binding),
        }
    }
}

#[derive(Default)]
pub(in crate::g_syntax) struct ResolvedBindings {
    bindings: Vec<(BindingId, ResolvedExpr<Value>)>,
}

impl ResolvedBindings {
    pub(in crate::g_syntax) fn bind(
        &mut self,
        locals: &mut ResolverContext,
        label: &str,
        value: ResolvedExpr<Value>,
    ) -> ResolvedRoot {
        let binding = locals.push_internal_binding(label);
        self.bindings.push((binding, value));
        ResolvedRoot::Local(binding)
    }

    pub(in crate::g_syntax) fn wrap(self, mut body: ResolvedExpr<Value>) -> ResolvedExpr<Value> {
        for (binding, value) in self.bindings.into_iter().rev() {
            body = ResolvedExpr::apply(ResolvedExpr::lambda(vec![binding], body), [value]);
        }
        body
    }
}

impl NameScope<Value> {
    #[cfg(test)]
    pub(in crate::g_syntax) fn module(
        context: &CompileContext,
        visible_definitions: Value,
    ) -> Self {
        let reflection = ReflectionBoundary {
            annotator: compiler_values::reflection_annotator_value(
                context.abstract_global_path("refl"),
                context.final_defs().clone(),
            ),
        };
        Self::module_with_reflection(context, visible_definitions, reflection)
    }

    pub(in crate::g_syntax) fn module_with_reflection(
        context: &CompileContext,
        visible_definitions: Value,
        reflection: ReflectionBoundary<Value>,
    ) -> Self {
        Self {
            final_defs: context.final_defs().clone(),
            prior_defs: visible_definitions.clone(),
            module_final_defs: context.final_defs().clone(),
            module_prior_defs: visible_definitions,
            object_alias: None,
            object_final_defs: None,
            object_prior_defs: None,
            reflection: Some(reflection),
            parent: None,
        }
    }

    pub(in crate::g_syntax) fn resolved(&self) -> NameScope<ResolvedRoot> {
        NameScope {
            final_defs: ResolvedRoot::Provided(self.final_defs.clone()),
            prior_defs: ResolvedRoot::Provided(self.prior_defs.clone()),
            module_final_defs: ResolvedRoot::Provided(self.module_final_defs.clone()),
            module_prior_defs: ResolvedRoot::Provided(self.module_prior_defs.clone()),
            object_alias: self.object_alias.clone(),
            object_final_defs: self.object_final_defs.clone().map(ResolvedRoot::Provided),
            object_prior_defs: self.object_prior_defs.clone().map(ResolvedRoot::Provided),
            reflection: self.reflection.as_ref().map(|boundary| ReflectionBoundary {
                annotator: ResolvedRoot::Provided(boundary.annotator.clone()),
            }),
            parent: self
                .parent
                .as_deref()
                .map(NameScope::resolved)
                .map(Box::new),
        }
    }
}

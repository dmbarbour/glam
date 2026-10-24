use super::super::*;

#[cfg(test)]
pub(in crate::g_syntax) fn syntax_expr_to_resolved_in_scope(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    context.values().with_boundary_access(|access| {
        syntax_expr_to_resolved_in_semantic_scope(
            &access,
            expr,
            line,
            context,
            &scope.resolved_in(&access),
            locals,
        )
    })
}

pub(in crate::g_syntax) fn syntax_expr_to_resolved_in_semantic_scope(
    access: &RuntimeValueAccess<'_>,
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    Ok(match expr {
        SyntaxExpr::Unit => ResolvedExpr::Embedded(context.unit_value(access)),
        SyntaxExpr::Embedded(value) => ResolvedExpr::Embedded(value.clone_core_with(access)),
        SyntaxExpr::Number(number) => ResolvedExpr::Embedded(Value::Number(number.clone())),
        SyntaxExpr::Text(text) => ResolvedExpr::Embedded(Value::binary_from_text(text)),
        SyntaxExpr::Atom(name) => ResolvedExpr::Embedded(Value::Atom(atom_from_str(name))),
        SyntaxExpr::Effect(path) => ResolvedExpr::Embedded(compiler_values::effect_path_value(
            access,
            &path.iter().map(String::as_str).collect::<Vec<_>>(),
        )),
        SyntaxExpr::AbstractGlobalPath {
            explicit_module,
            path,
        } => lower_abstract_global_path_resolved(
            access,
            *explicit_module,
            path,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::PathDict(path, value) => {
            lower_path_dict_resolved(access, path, value, line, context, scope, locals)?
        }
        SyntaxExpr::TaggedConstructor(path) => {
            lower_tagged_constructor_resolved(access, path, line, context, scope, locals)?
        }
        SyntaxExpr::DictLiteral(members) => {
            lower_dict_literal_resolved(access, members, line, context, scope, locals)?
        }
        SyntaxExpr::Name(name) => lower_name_expr_resolved(access, name, context, scope, locals),
        SyntaxExpr::PriorName(name) => {
            lower_prior_name_expr_resolved(access, name, line, scope, locals)?
        }
        SyntaxExpr::Escape(depth, expr) => {
            // An escape reads its target's lexical locals: those bound
            // before the `self` it stepped out of, and its own.
            let (escaped_scope, boundary) = escaped_name_scope(access, scope, *depth, line)?;
            let mut outer = locals.prefix(boundary);
            let lowered = syntax_expr_to_resolved_in_semantic_scope(
                access,
                expr,
                line,
                context,
                &escaped_scope,
                &mut outer,
            )?;
            locals.resume_after(&outer);
            lowered
        }
        SyntaxExpr::Access(base, parts) => ResolvedExpr::Access {
            base: Box::new(syntax_expr_to_resolved_in_semantic_scope(
                access, base, line, context, scope, locals,
            )?),
            path: parts
                .iter()
                .map(|part| {
                    syntax_key_expr_to_resolved_path(access, part, line, context, scope, locals)
                })
                .collect::<Result<Vec<_>, _>>()?,
        },
        SyntaxExpr::Object(object) => {
            lower_object_expr_resolved(access, object, line, context, scope, locals)?
        }
        SyntaxExpr::With { base, alias, body } => lower_dict_with_expr_resolved(
            access,
            base,
            alias.as_ref().map(Alias::name),
            body,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::Using { namespace, body } => {
            lower_using_expr_resolved(access, namespace, body, line, context, scope, locals)?
        }
        SyntaxExpr::List(items) => resolved_list(
            access,
            items
                .iter()
                .map(|expr| {
                    syntax_expr_to_resolved_in_semantic_scope(
                        access, expr, line, context, scope, locals,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SyntaxExpr::Tuple(items) => ResolvedExpr::apply(
            ResolvedExpr::Embedded(Value::Builtin(Builtin::DictSingleton)),
            [
                ResolvedExpr::Embedded(access.tuple()),
                resolved_list(
                    access,
                    items
                        .iter()
                        .map(|expr| {
                            syntax_expr_to_resolved_in_semantic_scope(
                                access, expr, line, context, scope, locals,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            ],
        ),
        SyntaxExpr::Lambda(params, body) => {
            lower_lambda_expr_resolved(access, params, body, line, context, scope, locals)?
        }
        SyntaxExpr::Do(do_expr) => lower_do_expr_resolved(access, do_expr, context, scope, locals)?,
        SyntaxExpr::If(if_expr) => super::conditional::lower_if_expr_resolved(
            access, if_expr, line, context, scope, locals,
        )?,
        SyntaxExpr::Match(match_expr) => super::conditional::lower_match_expr_resolved(
            access, match_expr, context, scope, locals,
        )?,
        SyntaxExpr::MatchWhen(match_when) => super::conditional::lower_match_when_expr_resolved(
            access, match_when, context, scope, locals,
        )?,
        SyntaxExpr::Let { bindings, body } => {
            lower_let_expr_resolved(access, bindings, body, line, context, scope, locals)?
        }
        SyntaxExpr::Apply(function, argument) => lower_application_expr_resolved(
            access, function, argument, line, context, scope, locals,
        )?,
        SyntaxExpr::OperatorApply {
            operator,
            left,
            right,
        } => lower_syntax_operator_expr_resolved(
            access, *operator, left, right, line, context, scope, locals,
        )?,
        SyntaxExpr::ComparisonChain { first, rest } => {
            lower_comparison_chain_resolved(access, first, rest, line, context, scope, locals)?
        }
        SyntaxExpr::OperatorSection {
            operator,
            left,
            right,
        } => lower_operator_section_resolved(
            access, *operator, left, right, line, context, scope, locals,
        )?,
        SyntaxExpr::Multiply(left, right) => lower_builtin_expr_resolved(
            access,
            Builtin::Multiply,
            left,
            right,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::Divide(left, right) => lower_builtin_expr_resolved(
            access,
            Builtin::Divide,
            left,
            right,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::Add(left, right) => lower_builtin_expr_resolved(
            access,
            Builtin::Add,
            left,
            right,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::Subtract(left, right) => lower_builtin_expr_resolved(
            access,
            Builtin::Subtract,
            left,
            right,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::Append(left, right) => lower_builtin_expr_resolved(
            access,
            Builtin::Append,
            left,
            right,
            line,
            context,
            scope,
            locals,
        )?,
    })
}

fn lower_path_dict_resolved(
    access: &RuntimeValueAccess<'_>,
    path: &[SyntaxKeyExpr],
    value: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let path = syntax_path_resolved(access, path, line, context, scope, locals)?;
    let value =
        syntax_expr_to_resolved_in_semantic_scope(access, value, line, context, scope, locals)?;
    Ok(build_path_dict_resolved(access, path, value))
}

/// A tag: `value` introduced at `path` in `{}`, kept even where it is `{}`
/// (`dict-path-fusion`).
fn build_path_dict_resolved(
    _access: &RuntimeValueAccess<'_>,
    path: ResolvedExpr<Value>,
    value: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::DictIntroduce)),
        [
            path,
            value,
            ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
        ],
    )
}

fn lower_tagged_constructor_resolved(
    access: &RuntimeValueAccess<'_>,
    path: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    // Resolve the path in the surrounding scope before introducing the
    // constructor's inaccessible, hygienic argument binding.
    let path = syntax_path_resolved(access, path, line, context, scope, locals)?;
    let payload = locals.fresh_binding();
    let body = build_path_dict_resolved(access, path, ResolvedExpr::Local(payload));
    Ok(ResolvedExpr::lambda(vec![payload], body))
}

pub(in crate::g_syntax) fn lower_object_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    object: &ObjectExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let name = match &object.name {
        Some(name) => {
            syntax_expr_to_resolved_in_semantic_scope(access, name, line, context, scope, locals)?
        }
        None => ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
    };
    let deps = object_parents_resolved(access, &object.deps, line, context, scope, locals)?;
    let defs = object_body_defs_resolved_in_scope(
        access,
        &object.body,
        object.alias.as_ref().map(Alias::name),
        line,
        context,
        scope.duplicate_in(access),
        locals,
        false,
    )?;
    Ok(object_from_parts_resolved(
        access,
        object.realization,
        name,
        ResolvedExpr::List(deps),
        defs,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn lower_dict_with_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    base: &SyntaxExpr,
    alias: Option<&str>,
    body: &[ObjectBodyDefinition],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let base_len = locals.len();
    let prior_value =
        syntax_expr_to_resolved_in_semantic_scope(access, base, line, context, scope, locals)?;
    let prior_binding = locals.push_internal_binding("<with-prior-defs>");
    let final_binding = locals.push_internal_binding("<with-final-defs>");
    let final_defs = ResolvedRoot::Local(final_binding);
    let mut definitions = ResolvedRoot::Local(prior_binding);
    let mut body_bindings = ResolvedBindings::default();

    for body_definition in body {
        let body_scope = dict_with_body_scope(
            access,
            alias,
            final_defs.duplicate_in(access),
            definitions.duplicate_in(access),
            scope.duplicate_in(access),
            base_len,
        );
        if let ObjectBodyDefinitionKind::Let(group) = &body_definition.kind {
            body_bindings.bind_group(
                access,
                lower_let_group_resolved(
                    access,
                    group,
                    body_definition.line,
                    context,
                    &body_scope,
                    locals,
                )?,
            );
            continue;
        }
        let updated = lower_object_body_item_resolved(
            access,
            body_definition,
            context,
            &definitions,
            &body_scope,
            false,
            locals,
        )?;
        definitions = body_bindings.bind(access, locals, "<with-visible-defs>", updated);
    }

    let lambda_body = body_bindings.wrap(access, definitions.expr(access));
    let extension_defs = ResolvedExpr::lambda(vec![prior_binding, final_binding], lambda_body);
    let extended = ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::ObjectWithDefs)),
        [prior_value, extension_defs],
    );
    locals.truncate(base_len);
    Ok(extended)
}

pub(in crate::g_syntax) fn dict_with_body_scope(
    access: &RuntimeValueAccess<'_>,
    alias: Option<&str>,
    dict_final_defs: ResolvedRoot,
    dict_prior_defs: ResolvedRoot,
    parent: NameScope<ResolvedRoot>,
    body_locals: usize,
) -> NameScope<ResolvedRoot> {
    let object_alias = alias
        .map(local_name_metadata)
        .and_then(|alias| alias.canonical);
    let object_final_defs = Some(dict_final_defs.duplicate_in(access));
    let object_prior_defs = Some(dict_prior_defs.duplicate_in(access));
    // `alias self` localizes an update's names, hiding the locals outside
    // its body as an object's do.
    let localized = object_alias.as_deref() == Some("self");
    let local_floor = if localized {
        body_locals
    } else {
        parent.local_floor
    };
    let (final_defs, prior_defs) = if localized {
        (dict_final_defs, dict_prior_defs)
    } else {
        (
            parent.final_defs.duplicate_in(access),
            parent.prior_defs.duplicate_in(access),
        )
    };
    // Only `alias self` makes the update `self`; otherwise `self` keeps
    // its host's meaning.
    let (self_final_defs, self_prior_defs) = if localized {
        (
            final_defs.duplicate_in(access),
            prior_defs.duplicate_in(access),
        )
    } else {
        (
            parent.self_final_defs.duplicate_in(access),
            parent.self_prior_defs.duplicate_in(access),
        )
    };

    NameScope {
        final_defs,
        prior_defs,
        module_final_defs: parent.module_final_defs.duplicate_in(access),
        module_prior_defs: parent.module_prior_defs.duplicate_in(access),
        object_alias,
        object_final_defs,
        object_prior_defs,
        self_final_defs,
        self_prior_defs,
        binds_self: localized,
        local_floor,
        sees_file_lets: !localized && parent.sees_file_lets,
        reflection: None,
        parent: Some(Box::new(parent)),
    }
}

fn lower_using_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    namespace: &SyntaxExpr,
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    parent_scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let namespace = syntax_expr_to_resolved_in_semantic_scope(
        access,
        namespace,
        line,
        context,
        parent_scope,
        locals,
    )?;
    let base_len = locals.len();
    let namespace_binding = locals.push_internal_binding("<using-namespace>");
    let namespace_root = ResolvedRoot::Local(namespace_binding);
    // A `using` scope localizes names as an object does, hiding the locals
    // outside it behind `^`.
    let using_scope = object_body_scope_resolved(
        access,
        None,
        namespace_root,
        ResolvedRoot::Provided(Value::Dict(Dict::new_sync())),
        parent_scope.duplicate_in(access),
        None,
        base_len,
    );
    let body = syntax_expr_to_resolved_in_semantic_scope(
        access,
        body,
        line,
        context,
        &using_scope,
        locals,
    )?;
    locals.truncate(base_len);

    Ok(ResolvedExpr::apply(
        ResolvedExpr::lambda(vec![namespace_binding], body),
        [namespace],
    ))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn lower_builtin_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    builtin: Builtin,
    left: &SyntaxExpr,
    right: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    Ok(ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(builtin)),
        [
            syntax_expr_to_resolved_in_semantic_scope(access, left, line, context, scope, locals)?,
            syntax_expr_to_resolved_in_semantic_scope(access, right, line, context, scope, locals)?,
        ],
    ))
}

pub(in crate::g_syntax) fn lower_effect_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    name: &str,
) -> ResolvedExpr<Value> {
    ResolvedExpr::Embedded(compiler_values::effect_value(access, name))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn lower_operator_section_resolved(
    access: &RuntimeValueAccess<'_>,
    operator: SyntaxOperator,
    left: &Option<Box<SyntaxExpr>>,
    right: &Option<Box<SyntaxExpr>>,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    match (left, right) {
        (None, None) => {
            return Ok(lower_syntax_operator_function_resolved(
                access, operator, locals,
            ));
        }
        (Some(left), Some(right)) => {
            return lower_syntax_operator_expr_resolved(
                access, operator, left, right, line, context, scope, locals,
            );
        }
        _ => {}
    }

    let base_len = locals.len();
    let parameter = locals.push_internal_binding("<operator-section>");
    let left = left
        .as_deref()
        .map(|expr| {
            syntax_expr_to_resolved_in_semantic_scope(access, expr, line, context, scope, locals)
        })
        .transpose()?;
    let right = right
        .as_deref()
        .map(|expr| {
            syntax_expr_to_resolved_in_semantic_scope(access, expr, line, context, scope, locals)
        })
        .transpose()?;
    let argument = ResolvedExpr::Local(parameter);
    let body = match (left, right) {
        (None, Some(right)) => {
            lower_syntax_operator_values_resolved(access, operator, argument, right, locals)
        }
        (Some(left), None) => {
            lower_syntax_operator_values_resolved(access, operator, left, argument, locals)
        }
        _ => unreachable!("operator section arity was handled before lowering operands"),
    };
    locals.truncate(base_len);
    Ok(ResolvedExpr::lambda(vec![parameter], body))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::g_syntax) fn lower_syntax_operator_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    operator: SyntaxOperator,
    left: &SyntaxExpr,
    right: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let left =
        syntax_expr_to_resolved_in_semantic_scope(access, left, line, context, scope, locals)?;
    let right =
        syntax_expr_to_resolved_in_semantic_scope(access, right, line, context, scope, locals)?;
    Ok(lower_syntax_operator_values_resolved(
        access, operator, left, right, locals,
    ))
}

pub(in crate::g_syntax) fn lower_syntax_operator_function_resolved(
    access: &RuntimeValueAccess<'_>,
    operator: SyntaxOperator,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    if let SyntaxOperator::Builtin(builtin) = operator {
        return ResolvedExpr::Embedded(Value::Builtin(builtin));
    }

    let base_len = locals.len();
    let left = locals.push_internal_binding("<operator-left>");
    let right = locals.push_internal_binding("<operator-right>");
    let body = lower_syntax_operator_values_resolved(
        access,
        operator,
        ResolvedExpr::Local(left),
        ResolvedExpr::Local(right),
        locals,
    );
    locals.truncate(base_len);
    ResolvedExpr::lambda(vec![left, right], body)
}

pub(in crate::g_syntax) fn lower_syntax_operator_values_resolved(
    access: &RuntimeValueAccess<'_>,
    operator: SyntaxOperator,
    left: ResolvedExpr<Value>,
    right: ResolvedExpr<Value>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    match operator {
        SyntaxOperator::Builtin(builtin) => ResolvedExpr::apply(
            ResolvedExpr::Embedded(Value::Builtin(builtin)),
            [left, right],
        ),
        SyntaxOperator::BoolAnd => {
            effect_then_resolved(access, left, right, "`and` left operand", locals)
        }
        SyntaxOperator::BoolOr => effect_call_resolved(access, "alt", [left, right]),
        SyntaxOperator::PipeForward => ResolvedExpr::apply(right, [left]),
        SyntaxOperator::PipeBackward => ResolvedExpr::apply(left, [right]),
        SyntaxOperator::ApplicativeForward => {
            applicative_resolved(access, left, right, false, locals)
        }
        SyntaxOperator::ApplicativeBackward => {
            applicative_resolved(access, left, right, true, locals)
        }
        SyntaxOperator::ComposeForward => compose_resolved(access, left, right, locals),
        SyntaxOperator::ComposeBackward => compose_resolved(access, right, left, locals),
        SyntaxOperator::EffectBind => effect_call_resolved(access, "seq", [left, right]),
        SyntaxOperator::KleisliCompose => kleisli_compose_resolved(access, left, right, locals),
        SyntaxOperator::EffectThen => {
            effect_then_resolved(access, left, right, "`=>>` discarded result", locals)
        }
    }
}

/// Sequences two effects in source order, then applies the function produced
/// by one to the value produced by the other and returns that result.
pub(in crate::g_syntax) fn applicative_resolved(
    access: &RuntimeValueAccess<'_>,
    first_operation: ResolvedExpr<Value>,
    second_operation: ResolvedExpr<Value>,
    first_is_function: bool,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let first_result = locals.push_internal_binding("<applicative-first>");
    let second_result = locals.push_internal_binding("<applicative-second>");
    let (function, argument) = if first_is_function {
        (
            ResolvedExpr::Local(first_result),
            ResolvedExpr::Local(second_result),
        )
    } else {
        (
            ResolvedExpr::Local(second_result),
            ResolvedExpr::Local(first_result),
        )
    };
    let result = ResolvedExpr::apply(function, [argument]);
    let returned = effect_call_resolved(access, "r", [result]);
    let second_continuation = ResolvedExpr::lambda(vec![second_result], returned);
    let after_first = effect_call_resolved(access, "seq", [second_operation, second_continuation]);
    let first_continuation = ResolvedExpr::lambda(vec![first_result], after_first);
    locals.truncate(base_len);
    effect_call_resolved(access, "seq", [first_operation, first_continuation])
}

pub(in crate::g_syntax) fn compose_resolved(
    _access: &RuntimeValueAccess<'_>,
    first: ResolvedExpr<Value>,
    second: ResolvedExpr<Value>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let input = locals.push_internal_binding("<composition-input>");
    let body = ResolvedExpr::apply(
        second,
        [ResolvedExpr::apply(first, [ResolvedExpr::Local(input)])],
    );
    locals.truncate(base_len);
    ResolvedExpr::lambda(vec![input], body)
}

pub(in crate::g_syntax) fn kleisli_compose_resolved(
    access: &RuntimeValueAccess<'_>,
    first: ResolvedExpr<Value>,
    second: ResolvedExpr<Value>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let input = locals.push_internal_binding("<kleisli-input>");
    let operation = ResolvedExpr::apply(first, [ResolvedExpr::Local(input)]);
    let body = effect_call_resolved(access, "seq", [operation, second]);
    locals.truncate(base_len);
    ResolvedExpr::lambda(vec![input], body)
}

pub(in crate::g_syntax) fn effect_then_resolved(
    access: &RuntimeValueAccess<'_>,
    operation: ResolvedExpr<Value>,
    next: ResolvedExpr<Value>,
    diagnostic_context: &'static str,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let result = locals.push_internal_binding("<effect-result>");
    let body = assert_unit_resolved(
        access,
        diagnostic_context,
        ResolvedExpr::Local(result),
        next,
    );
    let continuation = ResolvedExpr::lambda(vec![result], body);
    locals.truncate(base_len);
    effect_call_resolved(access, "seq", [operation, continuation])
}

pub(in crate::g_syntax) fn effect_call_resolved(
    access: &RuntimeValueAccess<'_>,
    name: &str,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(lower_effect_expr_resolved(access, name), arguments)
}

pub(in crate::g_syntax) fn assert_unit_resolved(
    _access: &RuntimeValueAccess<'_>,
    diagnostic_context: &'static str,
    value: ResolvedExpr<Value>,
    target: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::AssertUnit)),
        [
            ResolvedExpr::Embedded(Value::binary_from_text(diagnostic_context)),
            value,
            target,
        ],
    )
}

pub(in crate::g_syntax) fn lower_comparison_chain_resolved(
    access: &RuntimeValueAccess<'_>,
    first: &SyntaxExpr,
    rest: &[(SyntaxOperator, SyntaxExpr)],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let left =
        syntax_expr_to_resolved_in_semantic_scope(access, first, line, context, scope, locals)?;
    let rest = rest
        .iter()
        .map(|(operator, expr)| {
            if !is_comparison_operator(*operator) {
                return Err(Diagnostic::error(
                    line,
                    "internal error: comparison chain contained a non-comparison operator",
                ));
            }
            Ok((
                *operator,
                syntax_expr_to_resolved_in_semantic_scope(
                    access, expr, line, context, scope, locals,
                )?,
            ))
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    Ok(lower_comparison_chain_values_resolved(
        access,
        left,
        rest.into_iter(),
        locals,
    ))
}

pub(in crate::g_syntax) fn lower_comparison_chain_values_resolved(
    access: &RuntimeValueAccess<'_>,
    left: ResolvedExpr<Value>,
    mut rest: std::vec::IntoIter<(SyntaxOperator, ResolvedExpr<Value>)>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let Some((operator, right)) = rest.next() else {
        return left;
    };
    if rest.len() == 0 {
        return lower_syntax_operator_values_resolved(access, operator, left, right, locals);
    }

    let base_len = locals.len();
    let right_binding = locals.push_internal_binding("<comparison-right>");
    let first_condition = lower_syntax_operator_values_resolved(
        access,
        operator,
        left,
        ResolvedExpr::Local(right_binding),
        locals,
    );
    let remaining_condition = lower_comparison_chain_values_resolved(
        access,
        ResolvedExpr::Local(right_binding),
        rest,
        locals,
    );
    let body = lower_syntax_operator_values_resolved(
        access,
        SyntaxOperator::BoolAnd,
        first_condition,
        remaining_condition,
        locals,
    );
    locals.truncate(base_len);
    ResolvedExpr::apply(ResolvedExpr::lambda(vec![right_binding], body), [right])
}

pub(in crate::g_syntax) fn syntax_key_expr_to_resolved_path(
    access: &RuntimeValueAccess<'_>,
    key: &SyntaxKeyExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedPathPart<Value>, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => ResolvedPathPart::Key(name_as_key(name)),
        SyntaxKeyExpr::Index(expr) => ResolvedPathPart::Index(Box::new(
            syntax_expr_to_resolved_in_semantic_scope(access, expr, line, context, scope, locals)?,
        )),
        SyntaxKeyExpr::PathIndex(expr) => ResolvedPathPart::PathIndex(Box::new(
            syntax_expr_to_resolved_in_semantic_scope(access, expr, line, context, scope, locals)?,
        )),
    })
}

pub(in crate::g_syntax) fn syntax_path_resolved(
    access: &RuntimeValueAccess<'_>,
    parts: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut result: Option<ResolvedExpr<Value>> = None;
    let mut pending = Vec::new();

    for part in parts {
        match part {
            SyntaxKeyExpr::PathIndex(expr) => {
                let prefix = ResolvedExpr::List(std::mem::take(&mut pending));
                let combined = match result {
                    Some(result) => {
                        apply_builtin_resolved(access, Builtin::Append, [result, prefix])
                    }
                    None => prefix,
                };
                let splice = syntax_expr_to_resolved_in_semantic_scope(
                    access, expr, line, context, scope, locals,
                )?;
                result = Some(apply_builtin_resolved(
                    access,
                    Builtin::Append,
                    [combined, splice],
                ));
            }
            SyntaxKeyExpr::Atom(name) => {
                pending.push(ResolvedExpr::Embedded(Value::Atom(atom_from_str(name))))
            }
            SyntaxKeyExpr::Index(expr) => pending.push(syntax_expr_to_resolved_in_semantic_scope(
                access, expr, line, context, scope, locals,
            )?),
        }
    }

    let tail = ResolvedExpr::List(pending);
    Ok(match result {
        Some(result) => apply_builtin_resolved(access, Builtin::Append, [result, tail]),
        None => tail,
    })
}

/// A dict literal's member, resolved.
enum ResolvedDictMember {
    Path(ResolvedExpr<Value>, ResolvedExpr<Value>),
    Optional(ResolvedExpr<Value>, ResolvedExpr<Value>),
    Dict(ResolvedExpr<Value>),
}

/// A dict literal (`dict-path-fusion`): its members introduced in written
/// order, the inverse of a dict pattern's takes. A literal of closed data at
/// static paths folds here, where a conflict is an error the compiler
/// reports; any other builds at run time, applying its members in order,
/// each a function of the dict built so far.
pub(in crate::g_syntax) fn lower_dict_literal_resolved(
    access: &RuntimeValueAccess<'_>,
    members: &[DictMember],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    check_static_member_order(members, line)?;
    let mut resolved = Vec::with_capacity(members.len());
    for member in members {
        let mut value = |value| {
            syntax_expr_to_resolved_in_semantic_scope(access, value, line, context, scope, locals)
        };
        resolved.push(match member {
            DictMember::Path(path, member_value) => {
                let member_value = value(member_value)?;
                ResolvedDictMember::Path(
                    syntax_path_resolved(access, path, line, context, scope, locals)?,
                    member_value,
                )
            }
            DictMember::Optional(path, member_value) => {
                let member_value = value(member_value)?;
                ResolvedDictMember::Optional(
                    syntax_path_resolved(access, path, line, context, scope, locals)?,
                    member_value,
                )
            }
            DictMember::Dict(member_value) => ResolvedDictMember::Dict(value(member_value)?),
        });
    }
    if let Some(dict) = closed_dict(access, &resolved, line)? {
        return Ok(ResolvedExpr::Embedded(Value::Dict(dict)));
    }
    let members = resolved
        .into_iter()
        .map(|member| {
            let (builtin, arguments) = match member {
                ResolvedDictMember::Path(path, value) => {
                    (Builtin::DictIntroduce, vec![path, value])
                }
                ResolvedDictMember::Optional(path, list) => {
                    (Builtin::DictIntroduceOptional, vec![path, list])
                }
                ResolvedDictMember::Dict(dict) => (Builtin::DictShallowUnion, vec![dict]),
            };
            ResolvedExpr::apply(ResolvedExpr::Embedded(Value::Builtin(builtin)), arguments)
        })
        .collect();
    Ok(ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::DictBuild)),
        [ResolvedExpr::List(members)],
    ))
}

/// Reports what the compiler can see conflict in a literal's static paths
/// (`dict-path-fusion`): a path introduced twice, or a path introduced after
/// a deeper one under it, which the literal should write first.
fn check_static_member_order(members: &[DictMember], line: usize) -> Result<(), Diagnostic> {
    fn static_path(path: &[SyntaxKeyExpr]) -> Option<Vec<&str>> {
        path.iter()
            .map(|key| match key {
                SyntaxKeyExpr::Atom(name) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }
    let mut earlier: Vec<Vec<&str>> = Vec::new();
    for member in members {
        let DictMember::Path(path, _) = member else {
            continue;
        };
        let Some(path) = static_path(path) else {
            continue;
        };
        for prior in &earlier {
            if *prior == path {
                return Err(Diagnostic::error(
                    line,
                    format!("the dict literal introduces `{}` twice", path.join(".")),
                ));
            }
            if prior.starts_with(&path) {
                return Err(Diagnostic::error(
                    line,
                    format!(
                        "the dict literal introduces `{}` after `{}`, which defines it: write `{}` first",
                        path.join("."),
                        prior.join("."),
                        path.join(".")
                    ),
                ));
            }
        }
        earlier.push(path);
    }
    Ok(())
}

/// The dict a literal builds, where every member is closed data at a
/// static path of atom, number or text keys: introduced in order, as at run
/// time, a conflict an error here. `None` where some member is not.
fn closed_dict(
    access: &RuntimeValueAccess<'_>,
    members: &[ResolvedDictMember],
    line: usize,
) -> Result<Option<Dict>, Diagnostic> {
    let mut dict = Dict::new_sync();
    for member in members {
        let ResolvedDictMember::Path(ResolvedExpr::List(path), ResolvedExpr::Embedded(value)) =
            member
        else {
            return Ok(None);
        };
        let mut keys = Vec::with_capacity(path.len());
        for key in path {
            let ResolvedExpr::Embedded(
                key @ (Value::Atom(_) | Value::Number(_) | Value::Binary(_)),
            ) = key
            else {
                return Ok(None);
            };
            let Some(key) = access.key_from_value(key) else {
                return Ok(None);
            };
            keys.push(key);
        }
        if keys.is_empty() {
            return Ok(None);
        }
        match closed_introduce(access, &dict, &keys, value) {
            Ok(Some(introduced)) => dict = introduced,
            Ok(None) => return Ok(None),
            Err(message) => return Err(Diagnostic::error(line, message)),
        }
    }
    Ok(Some(dict))
}

/// `dict` with `value` introduced at `keys`, as `dict.introduce` would:
/// `None` where a dict on the way is not closed data.
fn closed_introduce(
    access: &RuntimeValueAccess<'_>,
    dict: &Dict,
    keys: &[Key],
    value: &Value,
) -> Result<Option<Dict>, String> {
    let name = || {
        keys.iter()
            .map(crate::eval::format_key_name)
            .collect::<Vec<_>>()
            .join(".")
    };
    let [head, rest @ ..] = keys else {
        unreachable!("a closed path has a key")
    };
    if rest.is_empty() {
        if dict.contains_key(head) {
            return Err(format!("the dict literal introduces `{}` twice", name()));
        }
        return Ok(Some(
            dict.insert(head.clone(), access.duplicate_value(value)),
        ));
    }
    let nested = match dict.get(head) {
        None => Dict::new_sync(),
        Some(Value::Dict(nested)) => nested.clone(),
        Some(Value::Lazy(_) | Value::Promised(_)) => return Ok(None),
        Some(_) => {
            return Err(format!(
                "the dict literal's path `{}` traverses a non-dictionary value",
                name()
            ));
        }
    };
    Ok(closed_introduce(access, &nested, rest, value)?
        .map(|nested| dict.insert(head.clone(), Value::Dict(nested))))
}

pub(in crate::g_syntax) fn lower_lambda_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    params: &[String],
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let base_len = locals.len();
    let parameters = locals.extend_source_bindings(params.iter().map(String::as_str), line)?;
    let lowered =
        syntax_expr_to_resolved_in_semantic_scope(access, body, line, context, scope, locals)?;
    locals.truncate(base_len);

    Ok(ResolvedExpr::lambda(parameters, lowered))
}

pub(in crate::g_syntax) fn lower_application_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    function: &SyntaxExpr,
    argument: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut head = function;
    let mut arguments = vec![argument];
    while let SyntaxExpr::Apply(next, argument) = head {
        arguments.push(argument);
        head = next;
    }
    arguments.reverse();

    let function = match head {
        SyntaxExpr::Lambda(params, body) => {
            lower_lambda_expr_resolved(access, params, body, line, context, scope, locals)?
        }
        head => {
            syntax_expr_to_resolved_in_semantic_scope(access, head, line, context, scope, locals)?
        }
    };
    let arguments = arguments
        .into_iter()
        .map(|argument| {
            syntax_expr_to_resolved_in_semantic_scope(
                access, argument, line, context, scope, locals,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedExpr::apply(function, arguments))
}

fn lower_abstract_global_path_resolved(
    access: &RuntimeValueAccess<'_>,
    explicit_module: bool,
    path: &[String],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let target = path.join(".");
    if explicit_module {
        return Ok(ResolvedExpr::Embedded(
            context.abstract_global_path(access, &target),
        ));
    }

    let root = path
        .first()
        .expect("abstract global paths must contain at least one name");
    if locals
        .iter()
        .rev()
        .any(|candidate| candidate.canonical.as_deref() == Some(root))
        || locals.file_let(root).is_some()
    {
        return Err(Diagnostic::error(
            line,
            format!(
                "`abstract_global_path {target}` resolves `{root}` as a local; abstract global paths must bind to the module namespace"
            ),
        ));
    }
    if scope.object_alias.as_deref() == Some(root) {
        return Err(Diagnostic::error(
            line,
            format!(
                "`abstract_global_path {target}` resolves `{root}` as an object alias; write `abstract_global_path module.{target}` for the module namespace"
            ),
        ));
    }
    if !scope
        .final_defs
        .same_representation_in(&scope.module_final_defs, access)
    {
        return Err(Diagnostic::error(
            line,
            format!(
                "`abstract_global_path {target}` resolves through an object or `using` namespace; write `abstract_global_path module.{target}` for the module namespace"
            ),
        ));
    }

    Ok(ResolvedExpr::Embedded(
        context.abstract_global_path(access, &target),
    ))
}

pub(in crate::g_syntax) fn lower_let_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    bindings: &[(String, SyntaxExpr)],
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let base_len = locals.len();
    let binders = lower_let_group_resolved(access, bindings, line, context, scope, locals)?;
    let mut lowered =
        syntax_expr_to_resolved_in_semantic_scope(access, body, line, context, scope, locals)?;
    locals.truncate(base_len);
    for (parameters, values) in binders.into_iter().rev() {
        lowered = ResolvedExpr::apply(ResolvedExpr::lambda(parameters, lowered), values);
    }
    Ok(lowered)
}

/// The binders of one `let` group, outermost first, each the parameters of
/// a lambda applied to its values around what the group scopes; the group's
/// names stay bound in `locals`.
pub(in crate::g_syntax) fn lower_let_group_resolved(
    access: &RuntimeValueAccess<'_>,
    bindings: &[(String, SyntaxExpr)],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<Vec<Binder>, Diagnostic> {
    if bindings.is_empty() {
        return Ok(Vec::new());
    }

    // Check the group's names together: no duplicate, no shadowing.
    let base_len = locals.len();
    locals.extend_source_bindings(bindings.iter().map(|(name, _)| name.as_str()), line)?;
    locals.truncate(base_len);

    // The group is mutually recursive. Its strongly connected components
    // bind in dependency order, each enclosing the next: one without a cycle
    // as a plain application, one with a cycle through a fixpoint. The edges
    // of the groups in its values, found with its own, last until its values
    // are resolved.
    let (dependencies, walk) = locals.group_dependencies(bindings);
    let binders = lower_group_components(
        access,
        bindings,
        &dependencies,
        line,
        context,
        scope,
        locals,
    );
    if let Some(walk) = walk {
        locals.forget_group_dependencies(walk);
    }
    binders
}

/// Binds a group's strongly connected components in dependency order.
fn lower_group_components(
    access: &RuntimeValueAccess<'_>,
    bindings: &[(String, SyntaxExpr)],
    dependencies: &[Vec<usize>],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<Vec<Binder>, Diagnostic> {
    let mut binders = Vec::new();
    for component in dependency_components(dependencies) {
        let names = component.iter().map(|&index| bindings[index].0.as_str());
        let resolve = |locals: &mut ResolverContext, index: usize| {
            syntax_expr_to_resolved_in_semantic_scope(
                access,
                &bindings[index].1,
                line,
                context,
                scope,
                locals,
            )
        };
        match component.as_slice() {
            [index] if !dependencies[*index].contains(index) => {
                let value = resolve(locals, *index)?;
                binders.push((locals.extend_source_bindings(names, line)?, vec![value]));
            }
            [index] => {
                // `fixpoint f` is the lazy value `x` with `x = f x`.
                let inner_len = locals.len();
                let inner = locals.extend_source_bindings(names.clone(), line)?;
                let value = resolve(locals, *index)?;
                locals.truncate(inner_len);
                let fixpoint = ResolvedExpr::apply(
                    ResolvedExpr::Embedded(Value::Builtin(Builtin::Fixpoint)),
                    [ResolvedExpr::lambda(inner, value)],
                );
                binders.push((locals.extend_source_bindings(names, line)?, vec![fixpoint]));
            }
            members => {
                // A fixpoint over one dictionary of the members' values, each
                // member bound to its own entry.
                let key = |position: usize| format!("binding{position}");
                let entries = |group: BindingId| {
                    (0..members.len())
                        .map(|position| ResolvedExpr::Access {
                            base: Box::new(ResolvedExpr::Local(group)),
                            path: vec![ResolvedPathPart::Key(name_as_key(&key(position)))],
                        })
                        .collect::<Vec<_>>()
                };
                let inner_len = locals.len();
                let inner_group = locals.push_internal_binding("<let-group>");
                let inner = locals.extend_source_bindings(names.clone(), line)?;
                let values = members
                    .iter()
                    .map(|&index| resolve(locals, index))
                    .collect::<Result<Vec<_>, _>>()?;
                locals.truncate(inner_len);
                let record = values
                    .into_iter()
                    .enumerate()
                    .map(|(position, value)| {
                        ResolvedExpr::apply(
                            ResolvedExpr::Embedded(Value::Builtin(Builtin::DictSingleton)),
                            [
                                ResolvedExpr::Embedded(Value::Atom(atom_from_str(&key(position)))),
                                value,
                            ],
                        )
                    })
                    .reduce(|left, right| {
                        ResolvedExpr::apply(
                            ResolvedExpr::Embedded(Value::Builtin(Builtin::DictUnion)),
                            [left, right],
                        )
                    })
                    .expect("a cycle has members");
                let fixpoint = ResolvedExpr::apply(
                    ResolvedExpr::Embedded(Value::Builtin(Builtin::Fixpoint)),
                    [ResolvedExpr::lambda(
                        vec![inner_group],
                        ResolvedExpr::apply(
                            ResolvedExpr::lambda(inner, record),
                            entries(inner_group),
                        ),
                    )],
                );
                let group = locals.push_internal_binding("<let-group>");
                binders.push((vec![group], vec![fixpoint]));
                binders.push((locals.extend_source_bindings(names, line)?, entries(group)));
            }
        }
    }
    Ok(binders)
}

/// The strongly connected components of a binding group's dependency
/// graph, each listed in source order, dependencies before the components
/// that use them (Tarjan's algorithm, iteratively).
fn dependency_components(dependencies: &[Vec<usize>]) -> Vec<Vec<usize>> {
    const UNVISITED: usize = usize::MAX;
    let count = dependencies.len();
    let mut order = vec![UNVISITED; count];
    let mut low = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut stack = Vec::new();
    let mut components = Vec::new();
    let mut next = 0;
    for root in 0..count {
        if order[root] != UNVISITED {
            continue;
        }
        order[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        // Each frame is a node and the next of its dependencies to visit.
        let mut frames = vec![(root, 0)];
        while let Some(frame) = frames.last_mut() {
            let (node, edge) = *frame;
            if let Some(&dependency) = dependencies[node].get(edge) {
                frame.1 += 1;
                if order[dependency] == UNVISITED {
                    order[dependency] = next;
                    low[dependency] = next;
                    next += 1;
                    stack.push(dependency);
                    on_stack[dependency] = true;
                    frames.push((dependency, 0));
                } else if on_stack[dependency] {
                    low[node] = low[node].min(order[dependency]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == order[node] {
                let mut component = Vec::new();
                loop {
                    let member = stack.pop().expect("a component's root is on the stack");
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                component.sort_unstable();
                components.push(component);
            }
        }
    }
    components
}

pub(in crate::g_syntax) fn lower_name_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    name: &str,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &ResolverContext,
) -> ResolvedExpr<Value> {
    match name {
        "module" => return scope.module_final_defs.expr(access),
        "module_origin" => {
            return ResolvedExpr::Embedded(
                context
                    .opaque_origin(access)
                    .unwrap_or_else(|| context.unavailable_origin(access)),
            );
        }
        "self" => return scope.self_final_defs.expr(access),
        _ => {}
    }

    if let Some(local) = scope
        .visible_locals(locals)
        .iter()
        .rev()
        .find(|candidate| candidate.canonical.as_deref() == Some(name))
    {
        return ResolvedExpr::Local(
            local
                .binding
                .expect("lowering locals must have stable binding identities"),
        );
    }
    if scope.sees_file_lets
        && let Some(binding) = locals.file_let(name)
    {
        return ResolvedExpr::Local(binding);
    }

    if scope.object_alias.as_deref() == Some(name)
        && let Some(object_final_defs) = &scope.object_final_defs
    {
        return object_final_defs.expr(access);
    }

    ResolvedExpr::Access {
        base: Box::new(scope.final_defs.expr(access)),
        path: vec![ResolvedPathPart::Key(Key::atom_from_text(name))],
    }
}

pub(in crate::g_syntax) fn lower_prior_name_expr_resolved(
    access: &RuntimeValueAccess<'_>,
    name: &str,
    line: usize,
    scope: &NameScope<ResolvedRoot>,
    locals: &ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    if name.is_empty() {
        return Err(Diagnostic::error(
            line,
            "prior name expression must have a name",
        ));
    }
    // One name has one meaning in a file, so `_x` cannot reach past a
    // local `x` to a prior definition.
    if scope
        .visible_locals(locals)
        .iter()
        .any(|local| local.canonical.as_deref() == Some(name))
        || (scope.sees_file_lets && locals.file_let(name).is_some())
    {
        return Err(Diagnostic::error(
            line,
            format!("`_{name}` names the local `{name}`, which has no prior definition"),
        ));
    }

    match name {
        "module" => return Ok(scope.module_prior_defs.expr(access)),
        "self" => return Ok(scope.self_prior_defs.expr(access)),
        _ => {}
    }

    if scope.object_alias.as_deref() == Some(name)
        && let Some(object_prior_defs) = &scope.object_prior_defs
    {
        return Ok(object_prior_defs.expr(access));
    }

    Ok(ResolvedExpr::Access {
        base: Box::new(scope.prior_defs.expr(access)),
        path: vec![ResolvedPathPart::Key(Key::atom_from_text(name))],
    })
}

/// Where `depth` escapes lead, and the boundary below which the locals
/// they reach were bound: the floor of the last `self` they stepped out of.
pub(in crate::g_syntax) fn escaped_name_scope(
    access: &RuntimeValueAccess<'_>,
    scope: &NameScope<ResolvedRoot>,
    depth: usize,
    line: usize,
) -> Result<(NameScope<ResolvedRoot>, usize), Diagnostic> {
    let mut escaped = scope.duplicate_in(access);
    let mut boundary = escaped.local_floor;
    for level in 0..depth {
        // Each `^` steps out of the innermost `self`: a scope that is not
        // its own `self` is transparent.
        while !escaped.binds_self {
            let parent = escaped
                .parent
                .as_deref()
                .expect("a scope that is not its own `self` has a host");
            escaped = parent.duplicate_in(access);
        }
        boundary = escaped.local_floor;
        let Some(parent) = escaped.parent.as_deref() else {
            return Err(Diagnostic::error(
                line,
                format!(
                    "scope escape depth `{depth}` exceeds available parent scopes at level `{}`",
                    level + 1
                ),
            ));
        };
        escaped = parent.duplicate_in(access);
    }
    Ok((escaped, boundary))
}

pub(in crate::g_syntax) fn local_name_metadata(raw: &str) -> LocalName {
    match raw {
        "_" => LocalName {
            raw: raw.to_owned(),
            canonical: None,
            suppress_unused_warning: true,
            binding: None,
        },
        suppressed if suppressed.starts_with('_') => LocalName {
            raw: suppressed.to_owned(),
            canonical: Some(suppressed[1..].to_owned()),
            suppress_unused_warning: true,
            binding: None,
        },
        name => LocalName {
            raw: name.to_owned(),
            canonical: Some(name.to_owned()),
            suppress_unused_warning: false,
            binding: None,
        },
    }
}

pub(in crate::g_syntax) fn name_as_key(name: &str) -> Key {
    // 'name as dict key or tag
    Key::Atom(atom_from_str(name))
}

pub(in crate::g_syntax) fn atom_from_str(name: &str) -> Atom {
    // 'name atom, i.e. ["name"]:()
    Atom::from_key(&Key::binary_from_text(name))
}

/// A list literal. One whose items are all closed data is closed data too,
/// so it resolves to the list value itself. Built at run time instead, it
/// would take one partial application of the list operator per item, each
/// costing time linear in the list's length.
fn resolved_list(
    _access: &RuntimeValueAccess<'_>,
    items: Vec<ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    if !items
        .iter()
        .all(|item| matches!(item, ResolvedExpr::Embedded(_)))
    {
        return ResolvedExpr::List(items);
    }
    let values = items
        .into_iter()
        .map(|item| match item {
            ResolvedExpr::Embedded(value) => value,
            _ => unreachable!("every item is closed data"),
        })
        .collect();
    ResolvedExpr::Embedded(Value::List(crate::core::List::from_values(values)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Components come out dependencies first, each in source order.
    #[test]
    fn binding_group_components_order_dependencies_first() {
        // 0 names 1, which forms a cycle with 2; 3 stands alone.
        let dependencies = vec![vec![1], vec![2], vec![1], vec![]];
        assert_eq!(
            dependency_components(&dependencies),
            vec![vec![1, 2], vec![0], vec![3]]
        );
    }

    fn resolve(expr: &SyntaxExpr) -> ResolvedExpr<Value> {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        syntax_expr_to_resolved_in_scope(expr, 1, &context, &scope, &mut ResolverContext::default())
            .expect("list literal should resolve")
    }

    fn number(value: i64) -> SyntaxExpr {
        SyntaxExpr::Number(value.into())
    }

    /// A literal of closed data, nested lists included, resolves to its
    /// list value; one with a computed item stays a list to build.
    #[test]
    fn closed_list_literals_resolve_to_their_values() {
        let closed = SyntaxExpr::List(vec![
            number(1),
            SyntaxExpr::List(vec![number(2), SyntaxExpr::Text("three".into())]),
        ]);
        let ResolvedExpr::Embedded(Value::List(list)) = resolve(&closed) else {
            panic!("a closed literal should resolve to its list");
        };
        assert_eq!(list.len(), 2);

        let computed = SyntaxExpr::List(vec![
            number(1),
            SyntaxExpr::Add(Box::new(number(1)), Box::new(number(1))),
        ]);
        assert!(
            matches!(resolve(&computed), ResolvedExpr::List(items) if items.len() == 2),
            "a literal with a computed item should stay a list to build"
        );
    }

    fn entry(key: &str, value: SyntaxExpr) -> SyntaxExpr {
        SyntaxExpr::PathDict(vec![SyntaxKeyExpr::Atom(key.into())], Box::new(value))
    }

    fn entry_path(path: &[&str], value: SyntaxExpr) -> SyntaxExpr {
        SyntaxExpr::PathDict(
            path.iter()
                .map(|key| SyntaxKeyExpr::Atom((*key).into()))
                .collect(),
            Box::new(value),
        )
    }

    fn resolve_error(expr: &SyntaxExpr) -> String {
        let context = CompileContext::default();
        let scope = NameScope::module(&context, Value::Dict(Dict::new_sync()));
        syntax_expr_to_resolved_in_scope(expr, 1, &context, &scope, &mut ResolverContext::default())
            .map(|_| ())
            .expect_err("the literal should not resolve")
            .message
            .clone()
    }

    /// A literal of closed data at static paths resolves to its dictionary,
    /// introducing its members in order, an empty dict kept as a value
    /// (`dict-path-fusion`). The compiler reports a path introduced twice,
    /// or after a deeper one under it. Computed values build at run time,
    /// one build of all the members, not a chain as deep as the literal.
    #[test]
    fn closed_dict_literals_resolve_to_their_values() {
        let closed = SyntaxExpr::dict_literal(vec![
            entry("a", number(1)),
            entry("b", SyntaxExpr::Text("two".into())),
            entry("empty", SyntaxExpr::dict_literal(Vec::new())),
        ]);
        let ResolvedExpr::Embedded(Value::Dict(dict)) = resolve(&closed) else {
            panic!("a closed literal should resolve to its dictionary");
        };
        assert_eq!(dict.size(), 3, "an empty dict is a value");

        let fused = SyntaxExpr::dict_literal(vec![
            entry("a", SyntaxExpr::dict_literal(vec![entry("b", number(1))])),
            entry_path(&["a", "c"], number(2)),
            entry_path(&["d", "e"], number(3)),
        ]);
        let ResolvedExpr::Embedded(Value::Dict(dict)) = resolve(&fused) else {
            panic!("static paths should fuse into a closed dictionary");
        };
        let Some(Value::Dict(a)) = dict.get(&Key::atom_from_text("a")) else {
            panic!("`a` should hold a dictionary");
        };
        assert_eq!(a.size(), 2, "`a.c` joins what `a` introduced");

        let duplicate =
            SyntaxExpr::dict_literal(vec![entry("a", number(1)), entry("a", number(2))]);
        assert!(resolve_error(&duplicate).contains("introduces `a` twice"));
        let reversed = SyntaxExpr::dict_literal(vec![
            entry_path(&["a", "c"], number(2)),
            entry("a", number(1)),
        ]);
        assert!(resolve_error(&reversed).contains("write `a` first"));

        let computed = SyntaxExpr::dict_literal(
            (0..64)
                .map(|index| {
                    entry(
                        &format!("k{index}"),
                        SyntaxExpr::Add(Box::new(number(index)), Box::new(number(0))),
                    )
                })
                .collect(),
        );
        // One build applies every member; each member is one application
        // of its own around its sum.
        let resolved = resolve(&computed);
        assert!(
            matches!(&resolved, ResolvedExpr::Apply { arguments, .. }
                if matches!(arguments.as_slice(), [ResolvedExpr::List(members)] if members.len() == 64)),
            "64 computed entries should build in one step"
        );
    }
}

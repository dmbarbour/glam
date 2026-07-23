use super::super::*;

#[cfg(test)]
pub(in crate::g_syntax) fn syntax_expr_to_resolved_in_scope(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    syntax_expr_to_resolved_in_semantic_scope(expr, line, context, &scope.resolved(), locals)
}

pub(in crate::g_syntax) fn syntax_expr_to_resolved_in_semantic_scope(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    Ok(match expr {
        SyntaxExpr::Unit => ResolvedExpr::Embedded(context.unit_value()),
        SyntaxExpr::Number(number) => ResolvedExpr::Embedded(Value::Number(number.clone())),
        SyntaxExpr::Text(text) => ResolvedExpr::Embedded(Value::binary_from_text(text)),
        SyntaxExpr::Atom(name) => ResolvedExpr::Embedded(Value::Atom(atom_from_str(name))),
        SyntaxExpr::Effect(path) => ResolvedExpr::Embedded(compiler_values::effect_path_value(
            &path.iter().map(String::as_str).collect::<Vec<_>>(),
        )),
        SyntaxExpr::PathDict(path, value) => {
            lower_path_dict_resolved(path, value, line, context, scope, locals)?
        }
        SyntaxExpr::TaggedConstructor(path) => {
            lower_tagged_constructor_resolved(path, line, context, scope, locals)?
        }
        SyntaxExpr::DictUnion(items) => {
            lower_dict_union_resolved(items, line, context, scope, locals)?
        }
        SyntaxExpr::Name(name) => lower_name_expr_resolved(name, scope, locals),
        SyntaxExpr::PriorName(name) => lower_prior_name_expr_resolved(name, line, scope)?,
        SyntaxExpr::Escape(depth, expr) => {
            let escaped_scope = escaped_name_scope(scope, *depth, line)?;
            syntax_expr_to_resolved_in_semantic_scope(expr, line, context, &escaped_scope, locals)?
        }
        SyntaxExpr::Access(base, parts) => ResolvedExpr::Access {
            base: Box::new(syntax_expr_to_resolved_in_semantic_scope(
                base, line, context, scope, locals,
            )?),
            path: parts
                .iter()
                .map(|part| syntax_key_expr_to_resolved_path(part, line, context, scope, locals))
                .collect::<Result<Vec<_>, _>>()?,
        },
        SyntaxExpr::Object(object) => {
            lower_object_expr_resolved(object, line, context, scope, locals)?
        }
        SyntaxExpr::With { base, alias, body } => lower_dict_with_expr_resolved(
            base,
            alias.as_deref(),
            body,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::List(items) => ResolvedExpr::List(
            items
                .iter()
                .map(|expr| {
                    syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SyntaxExpr::Tuple(items) => ResolvedExpr::apply(
            ResolvedExpr::Embedded(Value::Builtin(Builtin::DictSingleton)),
            [
                ResolvedExpr::Embedded((*keys::TUPLE_VALUE).clone()),
                ResolvedExpr::List(
                    items
                        .iter()
                        .map(|expr| {
                            syntax_expr_to_resolved_in_semantic_scope(
                                expr, line, context, scope, locals,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            ],
        ),
        SyntaxExpr::Lambda(params, body) => {
            lower_lambda_expr_resolved(params, body, line, context, scope, locals)?
        }
        SyntaxExpr::Do(do_expr) => lower_do_expr_resolved(do_expr, context, scope, locals)?,
        SyntaxExpr::Let { bindings, body } => {
            lower_let_expr_resolved(bindings, body, line, context, scope, locals)?
        }
        SyntaxExpr::Apply(function, argument) => {
            lower_application_expr_resolved(function, argument, line, context, scope, locals)?
        }
        SyntaxExpr::OperatorApply {
            operator,
            left,
            right,
        } => lower_syntax_operator_expr_resolved(
            *operator, left, right, line, context, scope, locals,
        )?,
        SyntaxExpr::ComparisonChain { first, rest } => {
            lower_comparison_chain_resolved(first, rest, line, context, scope, locals)?
        }
        SyntaxExpr::OperatorSection {
            operator,
            left,
            right,
        } => lower_operator_section_resolved(*operator, left, right, line, context, scope, locals)?,
        SyntaxExpr::Multiply(left, right) => lower_builtin_expr_resolved(
            Builtin::Multiply,
            left,
            right,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::Divide(left, right) => {
            lower_builtin_expr_resolved(Builtin::Divide, left, right, line, context, scope, locals)?
        }
        SyntaxExpr::Add(left, right) => {
            lower_builtin_expr_resolved(Builtin::Add, left, right, line, context, scope, locals)?
        }
        SyntaxExpr::Subtract(left, right) => lower_builtin_expr_resolved(
            Builtin::Subtract,
            left,
            right,
            line,
            context,
            scope,
            locals,
        )?,
        SyntaxExpr::Append(left, right) => {
            lower_builtin_expr_resolved(Builtin::Append, left, right, line, context, scope, locals)?
        }
    })
}

enum ResolvedDictPath {
    Key(ResolvedExpr<Value>),
    Path(ResolvedExpr<Value>),
}

fn lower_path_dict_resolved(
    path: &[SyntaxKeyExpr],
    value: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let path = resolve_dict_path(path, line, context, scope, locals)?;
    let value = syntax_expr_to_resolved_in_semantic_scope(value, line, context, scope, locals)?;
    Ok(build_path_dict_resolved(path, value))
}

fn resolve_dict_path(
    path: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedDictPath, Diagnostic> {
    if let [key] = path
        && !matches!(key, SyntaxKeyExpr::PathIndex(_))
    {
        return Ok(ResolvedDictPath::Key(syntax_key_expr_to_resolved_value(
            key, line, context, scope, locals,
        )?));
    }

    Ok(ResolvedDictPath::Path(syntax_path_resolved(
        path, line, context, scope, locals,
    )?))
}

fn build_path_dict_resolved(
    path: ResolvedDictPath,
    value: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    match path {
        ResolvedDictPath::Key(key) => ResolvedExpr::apply(
            ResolvedExpr::Embedded(Value::Builtin(Builtin::DictSingleton)),
            [key, value],
        ),
        ResolvedDictPath::Path(path) => ResolvedExpr::apply(
            ResolvedExpr::Embedded(Value::Builtin(Builtin::DictUpdate)),
            [
                path,
                value,
                ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
            ],
        ),
    }
}

fn lower_tagged_constructor_resolved(
    path: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    // Resolve the path in the surrounding scope before introducing the
    // constructor's inaccessible, hygienic argument binding.
    let path = resolve_dict_path(path, line, context, scope, locals)?;
    let payload = locals.fresh_binding();
    let body = build_path_dict_resolved(path, ResolvedExpr::Local(payload));
    Ok(ResolvedExpr::lambda(vec![payload], body))
}

pub(in crate::g_syntax) fn lower_object_expr_resolved(
    object: &ObjectExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let name = match &object.name {
        Some(name) => {
            syntax_expr_to_resolved_in_semantic_scope(name, line, context, scope, locals)?
        }
        None => ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())),
    };
    let deps = object_parents_resolved(&object.deps, line, context, scope, locals)?;
    let defs = object_body_defs_resolved_in_scope(
        &object.body,
        object.alias.as_deref(),
        line,
        context,
        scope.clone(),
        locals,
        false,
    )?;
    Ok(object_instance_from_parts_resolved(
        name,
        ResolvedExpr::List(deps),
        defs,
    ))
}

pub(in crate::g_syntax) fn lower_dict_with_expr_resolved(
    base: &SyntaxExpr,
    alias: Option<&str>,
    body: &[ObjectBodyDefinition],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let base_len = locals.len();
    let mut outer_bindings = ResolvedBindings::default();
    let prior_value =
        syntax_expr_to_resolved_in_semantic_scope(base, line, context, scope, locals)?;
    let prior_defs = outer_bindings.bind(locals, "<with-prior-defs>", prior_value);
    let final_binding = locals.push_internal_binding("<with-final-defs>");
    let final_defs = ResolvedRoot::Local(final_binding);
    let mut definitions = prior_defs;
    let mut body_bindings = ResolvedBindings::default();

    for body_definition in body {
        let body_scope = dict_with_body_scope(
            alias,
            final_defs.clone(),
            definitions.clone(),
            scope.clone(),
        );
        let updated = lower_object_body_item_resolved(
            body_definition,
            context,
            &definitions,
            &body_scope,
            locals,
        )?;
        definitions = body_bindings.bind(locals, "<with-visible-defs>", updated);
    }

    let lambda_body = body_bindings.wrap(definitions.expr());
    let fixed = ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::Fixpoint)),
        [ResolvedExpr::lambda(vec![final_binding], lambda_body)],
    );
    let result = outer_bindings.wrap(fixed);
    locals.truncate(base_len);
    Ok(result)
}

pub(in crate::g_syntax) fn dict_with_body_scope(
    alias: Option<&str>,
    dict_final_defs: ResolvedRoot,
    dict_prior_defs: ResolvedRoot,
    parent: NameScope<ResolvedRoot>,
) -> NameScope<ResolvedRoot> {
    let object_alias = alias
        .map(local_name_metadata)
        .and_then(|alias| alias.canonical);
    let object_final_defs = Some(dict_final_defs.clone());
    let object_prior_defs = Some(dict_prior_defs.clone());
    let (final_defs, prior_defs) = if object_alias.as_deref() == Some("self") {
        (dict_final_defs, dict_prior_defs)
    } else {
        (parent.final_defs.clone(), parent.prior_defs.clone())
    };

    NameScope {
        final_defs,
        prior_defs,
        module_final_defs: parent.module_final_defs.clone(),
        module_prior_defs: parent.module_prior_defs.clone(),
        object_alias,
        object_final_defs,
        object_prior_defs,
        reflection: None,
        parent: Some(Box::new(parent)),
    }
}

pub(in crate::g_syntax) fn lower_builtin_expr_resolved(
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
            syntax_expr_to_resolved_in_semantic_scope(left, line, context, scope, locals)?,
            syntax_expr_to_resolved_in_semantic_scope(right, line, context, scope, locals)?,
        ],
    ))
}

pub(in crate::g_syntax) fn lower_effect_expr_resolved(name: &str) -> ResolvedExpr<Value> {
    ResolvedExpr::Embedded(compiler_values::effect_value(name))
}

pub(in crate::g_syntax) fn lower_operator_section_resolved(
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
            return Ok(lower_syntax_operator_function_resolved(operator, locals));
        }
        (Some(left), Some(right)) => {
            return lower_syntax_operator_expr_resolved(
                operator, left, right, line, context, scope, locals,
            );
        }
        _ => {}
    }

    let base_len = locals.len();
    let parameter = locals.push_internal_binding("<operator-section>");
    let left = left
        .as_deref()
        .map(|expr| syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals))
        .transpose()?;
    let right = right
        .as_deref()
        .map(|expr| syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals))
        .transpose()?;
    let argument = ResolvedExpr::Local(parameter);
    let body = match (left, right) {
        (None, Some(right)) => {
            lower_syntax_operator_values_resolved(operator, argument, right, locals)
        }
        (Some(left), None) => {
            lower_syntax_operator_values_resolved(operator, left, argument, locals)
        }
        _ => unreachable!("operator section arity was handled before lowering operands"),
    };
    locals.truncate(base_len);
    Ok(ResolvedExpr::lambda(vec![parameter], body))
}

pub(in crate::g_syntax) fn lower_syntax_operator_expr_resolved(
    operator: SyntaxOperator,
    left: &SyntaxExpr,
    right: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let left = syntax_expr_to_resolved_in_semantic_scope(left, line, context, scope, locals)?;
    let right = syntax_expr_to_resolved_in_semantic_scope(right, line, context, scope, locals)?;
    Ok(lower_syntax_operator_values_resolved(
        operator, left, right, locals,
    ))
}

pub(in crate::g_syntax) fn lower_syntax_operator_function_resolved(
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
        operator,
        ResolvedExpr::Local(left),
        ResolvedExpr::Local(right),
        locals,
    );
    locals.truncate(base_len);
    ResolvedExpr::lambda(vec![left, right], body)
}

pub(in crate::g_syntax) fn lower_syntax_operator_values_resolved(
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
        SyntaxOperator::BoolAnd => effect_then_resolved(left, right, locals),
        SyntaxOperator::BoolOr => effect_call_resolved("alt", [left, right]),
        SyntaxOperator::PipeForward => ResolvedExpr::apply(right, [left]),
        SyntaxOperator::PipeBackward => ResolvedExpr::apply(left, [right]),
        SyntaxOperator::ApplicativeForward => applicative_resolved(left, right, false, locals),
        SyntaxOperator::ApplicativeBackward => applicative_resolved(left, right, true, locals),
        SyntaxOperator::ComposeForward => compose_resolved(left, right, locals),
        SyntaxOperator::ComposeBackward => compose_resolved(right, left, locals),
        SyntaxOperator::EffectBind => effect_call_resolved("seq", [left, right]),
        SyntaxOperator::KleisliCompose => kleisli_compose_resolved(left, right, locals),
        SyntaxOperator::EffectThen => effect_then_resolved(left, right, locals),
    }
}

/// Sequences two effects in source order, then applies the function produced
/// by one to the value produced by the other and returns that result.
pub(in crate::g_syntax) fn applicative_resolved(
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
    let returned = effect_call_resolved("r", [result]);
    let second_continuation = ResolvedExpr::lambda(vec![second_result], returned);
    let after_first = effect_call_resolved("seq", [second_operation, second_continuation]);
    let first_continuation = ResolvedExpr::lambda(vec![first_result], after_first);
    locals.truncate(base_len);
    effect_call_resolved("seq", [first_operation, first_continuation])
}

pub(in crate::g_syntax) fn compose_resolved(
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
    first: ResolvedExpr<Value>,
    second: ResolvedExpr<Value>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let input = locals.push_internal_binding("<kleisli-input>");
    let operation = ResolvedExpr::apply(first, [ResolvedExpr::Local(input)]);
    let body = effect_call_resolved("seq", [operation, second]);
    locals.truncate(base_len);
    ResolvedExpr::lambda(vec![input], body)
}

pub(in crate::g_syntax) fn effect_then_resolved(
    operation: ResolvedExpr<Value>,
    next: ResolvedExpr<Value>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let base_len = locals.len();
    let result = locals.push_internal_binding("<effect-result>");
    let body = annotate_assert_unit_resolved(ResolvedExpr::Local(result), next);
    let continuation = ResolvedExpr::lambda(vec![result], body);
    locals.truncate(base_len);
    effect_call_resolved("seq", [operation, continuation])
}

pub(in crate::g_syntax) fn effect_call_resolved(
    name: &str,
    arguments: impl IntoIterator<Item = ResolvedExpr<Value>>,
) -> ResolvedExpr<Value> {
    ResolvedExpr::apply(lower_effect_expr_resolved(name), arguments)
}

pub(in crate::g_syntax) fn annotate_assert_unit_resolved(
    value: ResolvedExpr<Value>,
    target: ResolvedExpr<Value>,
) -> ResolvedExpr<Value> {
    let singleton = || ResolvedExpr::Embedded(Value::Builtin(Builtin::DictSingleton));
    let payload = ResolvedExpr::apply(
        singleton(),
        [
            ResolvedExpr::Embedded(Value::Atom(atom_from_str("value"))),
            value,
        ],
    );
    let annotation = ResolvedExpr::apply(
        singleton(),
        [
            ResolvedExpr::Embedded(Value::Atom(atom_from_str("assert_unit"))),
            payload,
        ],
    );
    ResolvedExpr::apply(
        ResolvedExpr::Embedded(Value::Builtin(Builtin::Anno)),
        [annotation, target],
    )
}

pub(in crate::g_syntax) fn lower_comparison_chain_resolved(
    first: &SyntaxExpr,
    rest: &[(SyntaxOperator, SyntaxExpr)],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let left = syntax_expr_to_resolved_in_semantic_scope(first, line, context, scope, locals)?;
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
                syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)?,
            ))
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    Ok(lower_comparison_chain_values_resolved(
        left,
        rest.into_iter(),
        locals,
    ))
}

pub(in crate::g_syntax) fn lower_comparison_chain_values_resolved(
    left: ResolvedExpr<Value>,
    mut rest: std::vec::IntoIter<(SyntaxOperator, ResolvedExpr<Value>)>,
    locals: &mut ResolverContext,
) -> ResolvedExpr<Value> {
    let Some((operator, right)) = rest.next() else {
        return left;
    };
    if rest.len() == 0 {
        return lower_syntax_operator_values_resolved(operator, left, right, locals);
    }

    let base_len = locals.len();
    let right_binding = locals.push_internal_binding("<comparison-right>");
    let first_condition = lower_syntax_operator_values_resolved(
        operator,
        left,
        ResolvedExpr::Local(right_binding),
        locals,
    );
    let remaining_condition =
        lower_comparison_chain_values_resolved(ResolvedExpr::Local(right_binding), rest, locals);
    let body = lower_syntax_operator_values_resolved(
        SyntaxOperator::BoolAnd,
        first_condition,
        remaining_condition,
        locals,
    );
    locals.truncate(base_len);
    ResolvedExpr::apply(ResolvedExpr::lambda(vec![right_binding], body), [right])
}

pub(in crate::g_syntax) fn syntax_key_expr_to_resolved_value(
    key: &SyntaxKeyExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    match key {
        SyntaxKeyExpr::Atom(name) => Ok(ResolvedExpr::Embedded(Value::Atom(atom_from_str(name)))),
        SyntaxKeyExpr::Index(expr) => {
            syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)
        }
        SyntaxKeyExpr::PathIndex(_) => Err(Diagnostic::error(
            line,
            "list-valued path expressions are not valid dictionary keys",
        )),
    }
}

pub(in crate::g_syntax) fn syntax_key_expr_to_resolved_path(
    key: &SyntaxKeyExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedPathPart<Value>, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => ResolvedPathPart::Key(name_as_key(name)),
        SyntaxKeyExpr::Index(expr) => ResolvedPathPart::Index(Box::new(
            syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)?,
        )),
        SyntaxKeyExpr::PathIndex(expr) => ResolvedPathPart::PathIndex(Box::new(
            syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)?,
        )),
    })
}

pub(in crate::g_syntax) fn syntax_path_resolved(
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
                    Some(result) => apply_builtin_resolved(Builtin::Append, [result, prefix]),
                    None => prefix,
                };
                let splice =
                    syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)?;
                result = Some(apply_builtin_resolved(Builtin::Append, [combined, splice]));
            }
            SyntaxKeyExpr::Atom(name) => {
                pending.push(ResolvedExpr::Embedded(Value::Atom(atom_from_str(name))))
            }
            SyntaxKeyExpr::Index(expr) => pending.push(syntax_expr_to_resolved_in_semantic_scope(
                expr, line, context, scope, locals,
            )?),
        }
    }

    let tail = ResolvedExpr::List(pending);
    Ok(match result {
        Some(result) => apply_builtin_resolved(Builtin::Append, [result, tail]),
        None => tail,
    })
}

pub(in crate::g_syntax) fn lower_dict_union_resolved(
    items: &[SyntaxExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let mut items = items.iter();
    let Some(first) = items.next() else {
        return Ok(ResolvedExpr::Embedded(Value::Dict(Dict::new_sync())));
    };

    let mut value = syntax_expr_to_resolved_in_semantic_scope(first, line, context, scope, locals)?;
    for item in items {
        value = ResolvedExpr::apply(
            ResolvedExpr::Embedded(Value::Builtin(Builtin::DictUnion)),
            [
                value,
                syntax_expr_to_resolved_in_semantic_scope(item, line, context, scope, locals)?,
            ],
        );
    }
    Ok(value)
}

pub(in crate::g_syntax) fn lower_lambda_expr_resolved(
    params: &[String],
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    let base_len = locals.len();
    let parameters = locals.extend_source_bindings(params.iter().map(String::as_str), line)?;
    let lowered = syntax_expr_to_resolved_in_semantic_scope(body, line, context, scope, locals)?;
    locals.truncate(base_len);

    Ok(ResolvedExpr::lambda(parameters, lowered))
}

pub(in crate::g_syntax) fn lower_application_expr_resolved(
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
            lower_lambda_expr_resolved(params, body, line, context, scope, locals)?
        }
        head => syntax_expr_to_resolved_in_semantic_scope(head, line, context, scope, locals)?,
    };
    let arguments = arguments
        .into_iter()
        .map(|argument| {
            syntax_expr_to_resolved_in_semantic_scope(argument, line, context, scope, locals)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ResolvedExpr::apply(function, arguments))
}

pub(in crate::g_syntax) fn lower_let_expr_resolved(
    bindings: &[(String, SyntaxExpr)],
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope<ResolvedRoot>,
    locals: &mut ResolverContext,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    if bindings.is_empty() {
        return syntax_expr_to_resolved_in_semantic_scope(body, line, context, scope, locals);
    }

    let values = bindings
        .iter()
        .map(|(_, expr)| {
            syntax_expr_to_resolved_in_semantic_scope(expr, line, context, scope, locals)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let base_len = locals.len();
    let parameters =
        locals.extend_source_bindings(bindings.iter().map(|(name, _)| name.as_str()), line)?;
    let lowered = syntax_expr_to_resolved_in_semantic_scope(body, line, context, scope, locals)?;
    locals.truncate(base_len);

    Ok(ResolvedExpr::apply(
        ResolvedExpr::lambda(parameters, lowered),
        values,
    ))
}

pub(in crate::g_syntax) fn lower_name_expr_resolved(
    name: &str,
    scope: &NameScope<ResolvedRoot>,
    locals: &ResolverContext,
) -> ResolvedExpr<Value> {
    match name {
        "module" => return scope.module_final_defs.expr(),
        "self" => {
            return scope
                .object_final_defs
                .as_ref()
                .unwrap_or(&scope.module_final_defs)
                .expr();
        }
        _ => {}
    }

    if let Some(local) = locals
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

    if scope.object_alias.as_deref() == Some(name)
        && let Some(object_final_defs) = &scope.object_final_defs
    {
        return object_final_defs.expr();
    }

    ResolvedExpr::Access {
        base: Box::new(scope.final_defs.expr()),
        path: vec![ResolvedPathPart::Key(Key::atom_from_text(name))],
    }
}

pub(in crate::g_syntax) fn lower_prior_name_expr_resolved(
    name: &str,
    line: usize,
    scope: &NameScope<ResolvedRoot>,
) -> Result<ResolvedExpr<Value>, Diagnostic> {
    if name.is_empty() {
        return Err(Diagnostic::error(
            line,
            "prior name expression must have a name",
        ));
    }

    match name {
        "module" => return Ok(scope.module_prior_defs.expr()),
        "self" => {
            return Ok(scope
                .object_prior_defs
                .as_ref()
                .unwrap_or(&scope.module_prior_defs)
                .expr());
        }
        _ => {}
    }

    if scope.object_alias.as_deref() == Some(name)
        && let Some(object_prior_defs) = &scope.object_prior_defs
    {
        return Ok(object_prior_defs.expr());
    }

    Ok(ResolvedExpr::Access {
        base: Box::new(scope.prior_defs.expr()),
        path: vec![ResolvedPathPart::Key(Key::atom_from_text(name))],
    })
}

pub(in crate::g_syntax) fn escaped_name_scope<V: Clone>(
    scope: &NameScope<V>,
    depth: usize,
    line: usize,
) -> Result<NameScope<V>, Diagnostic> {
    let mut escaped = scope.clone();
    for level in 0..depth {
        let Some(parent) = escaped.parent.as_deref() else {
            return Err(Diagnostic::error(
                line,
                format!(
                    "scope escape depth `{depth}` exceeds available parent scopes at level `{}`",
                    level + 1
                ),
            ));
        };
        escaped = parent.clone();
    }
    Ok(escaped)
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

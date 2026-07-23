use super::*;

pub(super) fn warn_unused_locals(
    expr: &SyntaxExpr,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    analyze_expr_locals(expr, line, diagnostics);
}

fn analyze_expr_locals(expr: &SyntaxExpr, line: usize, diagnostics: &mut Vec<Diagnostic>) {
    match expr {
        SyntaxExpr::Unit
        | SyntaxExpr::Number(_)
        | SyntaxExpr::Text(_)
        | SyntaxExpr::Atom(_)
        | SyntaxExpr::Effect(_) => {}
        SyntaxExpr::Name(_) | SyntaxExpr::PriorName(_) => {}
        SyntaxExpr::Escape(_, expr) => analyze_expr_locals(expr, line, diagnostics),
        SyntaxExpr::Access(base, parts) => {
            analyze_expr_locals(base, line, diagnostics);
            for part in parts {
                analyze_key_expr_locals(part, line, diagnostics);
            }
        }
        SyntaxExpr::Object(object) => {
            if let Some(name) = &object.name {
                analyze_expr_locals(name, line, diagnostics);
            }
            for dep in &object.deps {
                analyze_expr_locals(dep, line, diagnostics);
            }
            if let Some(alias) = &object.alias {
                warn_unused_with_alias(alias, &object.body, line, diagnostics);
            }
            analyze_object_body_locals(&object.body, diagnostics);
        }
        SyntaxExpr::With { base, alias, body } => {
            analyze_expr_locals(base, line, diagnostics);
            if let Some(alias) = alias {
                warn_unused_with_alias(alias, body, line, diagnostics);
            }
            analyze_object_body_locals(body, diagnostics);
        }
        SyntaxExpr::PathDict(path, value) => {
            for key in path {
                analyze_key_expr_locals(key, line, diagnostics);
            }
            analyze_expr_locals(value, line, diagnostics);
        }
        SyntaxExpr::TaggedConstructor(path) => {
            for key in path {
                analyze_key_expr_locals(key, line, diagnostics);
            }
        }
        SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) | SyntaxExpr::Tuple(items) => {
            for item in items {
                analyze_expr_locals(item, line, diagnostics);
            }
        }
        SyntaxExpr::Lambda(params, body) => {
            let params = params
                .iter()
                .map(|param| local_name_metadata(param))
                .collect::<Vec<_>>();
            let mut used = vec![false; params.len()];
            mark_used_locals(body, &params, &mut used);
            for (param, used) in params.iter().zip(used) {
                if !used && param.canonical.is_some() && !param.suppress_unused_warning {
                    diagnostics.push(Diagnostic::warn(
                        line,
                        format!("unused local `{}`", param.raw),
                    ));
                }
            }
            analyze_expr_locals(body, line, diagnostics);
        }
        SyntaxExpr::Do(do_expr) => {
            analyze_do_expr_locals(do_expr, diagnostics);
        }
        SyntaxExpr::Let { bindings, body } => {
            let params = bindings
                .iter()
                .map(|(name, _)| local_name_metadata(name))
                .collect::<Vec<_>>();
            let mut used = vec![false; params.len()];
            mark_used_locals(body, &params, &mut used);
            for (param, used) in params.iter().zip(used) {
                if !used && param.canonical.is_some() && !param.suppress_unused_warning {
                    diagnostics.push(Diagnostic::warn(
                        line,
                        format!("unused local `{}`", param.raw),
                    ));
                }
            }
            for (_, value) in bindings {
                analyze_expr_locals(value, line, diagnostics);
            }
            analyze_expr_locals(body, line, diagnostics);
        }
        SyntaxExpr::OperatorSection { left, right, .. } => {
            if let Some(left) = left {
                analyze_expr_locals(left, line, diagnostics);
            }
            if let Some(right) = right {
                analyze_expr_locals(right, line, diagnostics);
            }
        }
        SyntaxExpr::ComparisonChain { first, rest } => {
            analyze_expr_locals(first, line, diagnostics);
            for (_, expr) in rest {
                analyze_expr_locals(expr, line, diagnostics);
            }
        }
        SyntaxExpr::OperatorApply { left, right, .. }
        | SyntaxExpr::Apply(left, right)
        | SyntaxExpr::Multiply(left, right)
        | SyntaxExpr::Divide(left, right)
        | SyntaxExpr::Add(left, right)
        | SyntaxExpr::Subtract(left, right)
        | SyntaxExpr::Append(left, right) => {
            analyze_expr_locals(left, line, diagnostics);
            analyze_expr_locals(right, line, diagnostics);
        }
    }
}

pub(super) fn warn_unused_with_alias(
    alias: &str,
    body: &[ObjectBodyDefinition],
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if alias == "self" {
        return;
    }
    let alias = local_name_metadata(alias);
    if alias.canonical.is_none() || alias.suppress_unused_warning {
        return;
    }

    let mut used = vec![false];
    for item in body {
        mark_used_body_item_locals(item, std::slice::from_ref(&alias), &mut used);
        mark_used_body_item_prior_alias(item, alias.canonical.as_deref(), &mut used[0]);
    }
    if !used[0] {
        diagnostics.push(Diagnostic::warn(
            line,
            format!("unused local `{}`", alias.raw),
        ));
    }
}

fn analyze_object_body_locals(body: &[ObjectBodyDefinition], diagnostics: &mut Vec<Diagnostic>) {
    for item in body {
        if let Some(definition) = item.definition()
            && let Some(expr) = &definition.expr
        {
            analyze_expr_locals(expr, item.line, diagnostics);
        }
        if let Some(object) = item.object() {
            if let Some(alias) = &object.alias {
                warn_unused_with_alias(alias, &object.body, item.line, diagnostics);
            }
            analyze_object_body_locals(&object.body, diagnostics);
        }
    }
}

fn analyze_key_expr_locals(key: &SyntaxKeyExpr, line: usize, diagnostics: &mut Vec<Diagnostic>) {
    match key {
        SyntaxKeyExpr::Atom(_) => {}
        SyntaxKeyExpr::Index(expr) | SyntaxKeyExpr::PathIndex(expr) => {
            analyze_expr_locals(expr, line, diagnostics)
        }
    }
}

fn mark_used_prior_alias(expr: &SyntaxExpr, alias: Option<&str>, used: &mut bool) {
    match expr {
        SyntaxExpr::PriorName(name) if Some(name.as_str()) == alias => *used = true,
        SyntaxExpr::Unit
        | SyntaxExpr::Number(_)
        | SyntaxExpr::Text(_)
        | SyntaxExpr::Atom(_)
        | SyntaxExpr::Effect(_)
        | SyntaxExpr::Name(_)
        | SyntaxExpr::PriorName(_) => {}
        SyntaxExpr::Escape(_, expr) => mark_used_prior_alias(expr, alias, used),
        SyntaxExpr::Access(base, parts) => {
            mark_used_prior_alias(base, alias, used);
            for part in parts {
                mark_used_prior_alias_in_key(part, alias, used);
            }
        }
        SyntaxExpr::Object(object) => {
            if let Some(name) = &object.name {
                mark_used_prior_alias(name, alias, used);
            }
            for dep in &object.deps {
                mark_used_prior_alias(dep, alias, used);
            }
            for item in &object.body {
                mark_used_body_item_prior_alias(item, alias, used);
            }
        }
        SyntaxExpr::With { base, body, .. } => {
            mark_used_prior_alias(base, alias, used);
            for item in body {
                mark_used_body_item_prior_alias(item, alias, used);
            }
        }
        SyntaxExpr::PathDict(path, value) => {
            for key in path {
                mark_used_prior_alias_in_key(key, alias, used);
            }
            mark_used_prior_alias(value, alias, used);
        }
        SyntaxExpr::TaggedConstructor(path) => {
            for key in path {
                mark_used_prior_alias_in_key(key, alias, used);
            }
        }
        SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) | SyntaxExpr::Tuple(items) => {
            for item in items {
                mark_used_prior_alias(item, alias, used);
            }
        }
        SyntaxExpr::Lambda(_, body) => mark_used_prior_alias(body, alias, used),
        SyntaxExpr::Do(do_expr) => {
            for step in &do_expr.steps {
                if let Some(expr) = do_step_expr(step) {
                    mark_used_prior_alias(expr, alias, used);
                }
            }
            mark_used_prior_alias(&do_expr.result, alias, used);
        }
        SyntaxExpr::Let { bindings, body } => {
            for (_, value) in bindings {
                mark_used_prior_alias(value, alias, used);
            }
            mark_used_prior_alias(body, alias, used);
        }
        SyntaxExpr::OperatorSection { left, right, .. } => {
            if let Some(left) = left {
                mark_used_prior_alias(left, alias, used);
            }
            if let Some(right) = right {
                mark_used_prior_alias(right, alias, used);
            }
        }
        SyntaxExpr::ComparisonChain { first, rest } => {
            mark_used_prior_alias(first, alias, used);
            for (_, expr) in rest {
                mark_used_prior_alias(expr, alias, used);
            }
        }
        SyntaxExpr::OperatorApply { left, right, .. }
        | SyntaxExpr::Apply(left, right)
        | SyntaxExpr::Multiply(left, right)
        | SyntaxExpr::Divide(left, right)
        | SyntaxExpr::Add(left, right)
        | SyntaxExpr::Subtract(left, right)
        | SyntaxExpr::Append(left, right) => {
            mark_used_prior_alias(left, alias, used);
            mark_used_prior_alias(right, alias, used);
        }
    }
}

fn mark_used_body_item_prior_alias(
    item: &ObjectBodyDefinition,
    alias: Option<&str>,
    used: &mut bool,
) {
    if let Some(definition) = item.definition()
        && let Some(expr) = &definition.expr
    {
        mark_used_prior_alias(expr, alias, used);
    }
    if let Some(object) = item.object() {
        for item in &object.body {
            mark_used_body_item_prior_alias(item, alias, used);
        }
    }
}

fn mark_used_prior_alias_in_key(key: &SyntaxKeyExpr, alias: Option<&str>, used: &mut bool) {
    match key {
        SyntaxKeyExpr::Atom(_) => {}
        SyntaxKeyExpr::Index(expr) | SyntaxKeyExpr::PathIndex(expr) => {
            mark_used_prior_alias(expr, alias, used)
        }
    }
}

fn mark_used_locals(expr: &SyntaxExpr, locals: &[LocalName], used: &mut [bool]) {
    match expr {
        SyntaxExpr::Unit
        | SyntaxExpr::Number(_)
        | SyntaxExpr::Text(_)
        | SyntaxExpr::Atom(_)
        | SyntaxExpr::Effect(_) => {}
        SyntaxExpr::Name(name) => {
            if let Some(index) = locals
                .iter()
                .rposition(|local| local.canonical.as_deref() == Some(name.as_str()))
            {
                used[index] = true;
            }
        }
        SyntaxExpr::PriorName(_) => {}
        SyntaxExpr::Escape(_, expr) => mark_used_locals(expr, locals, used),
        SyntaxExpr::Access(base, parts) => {
            mark_used_locals(base, locals, used);
            for part in parts {
                mark_used_key_expr(part, locals, used);
            }
        }
        SyntaxExpr::Object(object) => {
            if let Some(name) = &object.name {
                mark_used_locals(name, locals, used);
            }
            for dep in &object.deps {
                mark_used_locals(dep, locals, used);
            }
            for item in &object.body {
                mark_used_body_item_locals(item, locals, used);
            }
        }
        SyntaxExpr::With { base, body, .. } => {
            mark_used_locals(base, locals, used);
            for item in body {
                mark_used_body_item_locals(item, locals, used);
            }
        }
        SyntaxExpr::PathDict(path, value) => {
            for key in path {
                mark_used_key_expr(key, locals, used);
            }
            mark_used_locals(value, locals, used);
        }
        SyntaxExpr::TaggedConstructor(path) => {
            for key in path {
                mark_used_key_expr(key, locals, used);
            }
        }
        SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) | SyntaxExpr::Tuple(items) => {
            for item in items {
                mark_used_locals(item, locals, used);
            }
        }
        SyntaxExpr::Lambda(params, body) => {
            let nested = params
                .iter()
                .map(|param| local_name_metadata(param))
                .collect::<Vec<_>>();
            let mut combined = Vec::with_capacity(locals.len() + nested.len());
            combined.extend_from_slice(locals);
            combined.extend(nested);
            let mut nested_used = vec![false; combined.len()];
            nested_used[..locals.len()].copy_from_slice(used);
            mark_used_locals(body, &combined, &mut nested_used);
            used.copy_from_slice(&nested_used[..locals.len()]);
        }
        SyntaxExpr::Do(do_expr) => {
            mark_used_do_locals(do_expr, locals, used);
        }
        SyntaxExpr::Let { bindings, body } => {
            for (_, value) in bindings {
                mark_used_locals(value, locals, used);
            }
            let nested = bindings
                .iter()
                .map(|(name, _)| local_name_metadata(name))
                .collect::<Vec<_>>();
            let mut combined = Vec::with_capacity(locals.len() + nested.len());
            combined.extend_from_slice(locals);
            combined.extend(nested);
            let mut nested_used = vec![false; combined.len()];
            nested_used[..locals.len()].copy_from_slice(used);
            mark_used_locals(body, &combined, &mut nested_used);
            used.copy_from_slice(&nested_used[..locals.len()]);
        }
        SyntaxExpr::OperatorSection { left, right, .. } => {
            if let Some(left) = left {
                mark_used_locals(left, locals, used);
            }
            if let Some(right) = right {
                mark_used_locals(right, locals, used);
            }
        }
        SyntaxExpr::ComparisonChain { first, rest } => {
            mark_used_locals(first, locals, used);
            for (_, expr) in rest {
                mark_used_locals(expr, locals, used);
            }
        }
        SyntaxExpr::OperatorApply { left, right, .. }
        | SyntaxExpr::Apply(left, right)
        | SyntaxExpr::Multiply(left, right)
        | SyntaxExpr::Divide(left, right)
        | SyntaxExpr::Add(left, right)
        | SyntaxExpr::Subtract(left, right)
        | SyntaxExpr::Append(left, right) => {
            mark_used_locals(left, locals, used);
            mark_used_locals(right, locals, used);
        }
    }
}

fn analyze_do_expr_locals(do_expr: &DoExpr, diagnostics: &mut Vec<Diagnostic>) {
    if let Ok(plan) = recursive_do::RecursiveDoPlan::build(do_expr) {
        diagnostics.extend(plan.promotion_warnings(do_expr));
    }

    let mut locals = Vec::new();
    let mut used = Vec::new();
    let mut binding_lines = Vec::new();
    let mut unresolved_abstracts = Vec::new();

    for step in &do_expr.steps {
        if let Some(expr) = do_step_expr(step) {
            mark_used_locals(expr, &locals, &mut used);
            analyze_expr_locals(expr, step.line, diagnostics);
        }

        match &step.kind {
            DoStepKind::Abstract(names) => {
                for name in names {
                    let local = local_name_metadata(name);
                    if let Some(canonical) = &local.canonical {
                        unresolved_abstracts.push(canonical.clone());
                    }
                    locals.push(local);
                    used.push(false);
                    binding_lines.push(step.line);
                }
            }
            DoStepKind::Bind { name, .. } | DoStepKind::ValueBind { name, .. } => {
                if !fulfills_abstract(name, &mut unresolved_abstracts) {
                    locals.push(local_name_metadata(name));
                    used.push(false);
                    binding_lines.push(step.line);
                }
            }
            DoStepKind::Then(_) => {}
        }
    }

    mark_used_locals(&do_expr.result, &locals, &mut used);
    analyze_expr_locals(&do_expr.result, do_expr.result_line, diagnostics);

    for ((local, used), line) in locals.iter().zip(used).zip(binding_lines) {
        if !used && local.canonical.is_some() && !local.suppress_unused_warning {
            diagnostics.push(Diagnostic::warn(
                line,
                format!("unused local `{}`", local.raw),
            ));
        }
    }
}

fn mark_used_do_locals(do_expr: &DoExpr, locals: &[LocalName], used: &mut [bool]) {
    let outer_len = locals.len();
    let mut combined = Vec::with_capacity(outer_len + do_expr.steps.len());
    combined.extend_from_slice(locals);
    let mut combined_used = Vec::with_capacity(outer_len + do_expr.steps.len());
    combined_used.extend_from_slice(used);
    let mut unresolved_abstracts = Vec::new();

    for step in &do_expr.steps {
        if let Some(expr) = do_step_expr(step) {
            mark_used_locals(expr, &combined, &mut combined_used);
        }
        match &step.kind {
            DoStepKind::Abstract(names) => {
                for name in names {
                    let local = local_name_metadata(name);
                    if let Some(canonical) = &local.canonical {
                        unresolved_abstracts.push(canonical.clone());
                    }
                    combined.push(local);
                    combined_used.push(false);
                }
            }
            DoStepKind::Bind { name, .. } | DoStepKind::ValueBind { name, .. } => {
                if !fulfills_abstract(name, &mut unresolved_abstracts) {
                    combined.push(local_name_metadata(name));
                    combined_used.push(false);
                }
            }
            DoStepKind::Then(_) => {}
        }
    }
    mark_used_locals(&do_expr.result, &combined, &mut combined_used);
    used.copy_from_slice(&combined_used[..outer_len]);
}

fn do_step_expr(step: &DoStep) -> Option<&SyntaxExpr> {
    match &step.kind {
        DoStepKind::Abstract(_) => None,
        DoStepKind::Bind { operation, .. } => Some(operation),
        DoStepKind::ValueBind { value, .. } => Some(value),
        DoStepKind::Then(expr) => Some(expr),
    }
}

fn fulfills_abstract(name: &str, unresolved: &mut Vec<String>) -> bool {
    let canonical = local_name_metadata(name).canonical;
    let Some(index) = unresolved
        .iter()
        .rposition(|abstract_name| Some(abstract_name) == canonical.as_ref())
    else {
        return false;
    };
    unresolved.remove(index);
    true
}

fn mark_used_body_item_locals(
    item: &ObjectBodyDefinition,
    locals: &[LocalName],
    used: &mut [bool],
) {
    if let Some(definition) = item.definition()
        && let Some(expr) = &definition.expr
    {
        mark_used_locals(expr, locals, used);
    }
    if let Some(object) = item.object() {
        for item in &object.body {
            mark_used_body_item_locals(item, locals, used);
        }
    }
}

fn mark_used_key_expr(key: &SyntaxKeyExpr, locals: &[LocalName], used: &mut [bool]) {
    match key {
        SyntaxKeyExpr::Atom(_) => {}
        SyntaxKeyExpr::Index(expr) | SyntaxKeyExpr::PathIndex(expr) => {
            mark_used_locals(expr, locals, used)
        }
    }
}

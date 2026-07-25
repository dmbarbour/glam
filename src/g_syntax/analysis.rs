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
        SyntaxExpr::If(if_expr) => {
            analyze_guard_branch_locals(&if_expr.guards, &if_expr.then_result, line, diagnostics);
            analyze_expr_locals(&if_expr.else_result, line, diagnostics);
        }
        SyntaxExpr::Match(match_expr) => {
            analyze_expr_locals(&match_expr.subject, line, diagnostics);
            for arm in &match_expr.arms {
                analyze_pattern_guard_outcome_branch_locals(
                    &arm.pattern,
                    &arm.guards,
                    &arm.outcome,
                    arm.line,
                    diagnostics,
                );
            }
        }
        SyntaxExpr::MatchWhen(match_when) => {
            for arm in &match_when.arms {
                analyze_when_branch_locals(arm, diagnostics);
            }
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
            for parent in &object.deps {
                analyze_expr_locals(parent, item.line, diagnostics);
            }
            if let Some(alias) = &object.alias {
                warn_unused_with_alias(alias, &object.body, item.line, diagnostics);
            }
            analyze_object_body_locals(&object.body, diagnostics);
        }
        if let Some(extend) = item.extend() {
            if let Some(alias) = &extend.alias {
                warn_unused_with_alias(alias, &extend.body, item.line, diagnostics);
            }
            analyze_object_body_locals(&extend.body, diagnostics);
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
        SyntaxExpr::If(if_expr) => {
            for guard in &if_expr.guards {
                guard.visit_scope_events(&mut |event| {
                    if let SyntaxPatternScopeEvent::Expression(expr) = event {
                        mark_used_prior_alias(expr, alias, used);
                    }
                });
            }
            mark_used_prior_alias(&if_expr.then_result, alias, used);
            mark_used_prior_alias(&if_expr.else_result, alias, used);
        }
        SyntaxExpr::Match(match_expr) => {
            mark_used_prior_alias(&match_expr.subject, alias, used);
            for arm in &match_expr.arms {
                arm.pattern.visit_scope_events(&mut |event| {
                    if let SyntaxPatternScopeEvent::Expression(expr) = event {
                        mark_used_prior_alias(expr, alias, used);
                    }
                });
                for guard in &arm.guards {
                    guard.visit_scope_events(&mut |event| {
                        if let SyntaxPatternScopeEvent::Expression(expr) = event {
                            mark_used_prior_alias(expr, alias, used);
                        }
                    });
                }
                mark_used_prior_alias_in_outcome(&arm.outcome, alias, used);
            }
        }
        SyntaxExpr::MatchWhen(match_when) => {
            for arm in &match_when.arms {
                for guard in &arm.guards {
                    guard.visit_scope_events(&mut |event| {
                        if let SyntaxPatternScopeEvent::Expression(expr) = event {
                            mark_used_prior_alias(expr, alias, used);
                        }
                    });
                }
                mark_used_prior_alias_in_outcome(&arm.outcome, alias, used);
            }
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
        for parent in &object.deps {
            mark_used_prior_alias(parent, alias, used);
        }
        for item in &object.body {
            mark_used_body_item_prior_alias(item, alias, used);
        }
    }
    if let Some(extend) = item.extend() {
        for item in &extend.body {
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
        SyntaxExpr::If(if_expr) => {
            mark_used_guard_branch(&if_expr.guards, &if_expr.then_result, locals, used);
            mark_used_locals(&if_expr.else_result, locals, used);
        }
        SyntaxExpr::Match(match_expr) => {
            mark_used_locals(&match_expr.subject, locals, used);
            for arm in &match_expr.arms {
                mark_used_pattern_guard_outcome_branch(
                    &arm.pattern,
                    &arm.guards,
                    &arm.outcome,
                    locals,
                    used,
                );
            }
        }
        SyntaxExpr::MatchWhen(match_when) => {
            for arm in &match_when.arms {
                mark_used_when_branch(arm, locals, used);
            }
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

fn analyze_guard_branch_locals(
    guards: &[SyntaxGuardClause],
    result: &SyntaxExpr,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    analyze_branch_locals(None, guards, result, line, diagnostics);
}

fn analyze_pattern_guard_outcome_branch_locals(
    pattern: &SyntaxPattern,
    guards: &[SyntaxGuardClause],
    outcome: &MatchOutcome,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    analyze_outcome_branch_locals(Some(pattern), guards, outcome, line, diagnostics);
}

fn analyze_branch_locals(
    pattern: Option<&SyntaxPattern>,
    guards: &[SyntaxGuardClause],
    result: &SyntaxExpr,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut locals = Vec::new();
    let mut used = Vec::new();
    if let Some(pattern) = pattern {
        pattern.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, &locals, &mut used);
                analyze_expr_locals(expr, line, diagnostics);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                locals.push(local_name_metadata(name));
                used.push(false);
            }
        });
    }
    for guard in guards {
        guard.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, &locals, &mut used);
                analyze_expr_locals(expr, line, diagnostics);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                locals.push(local_name_metadata(name));
                used.push(false);
            }
        });
    }
    mark_used_locals(result, &locals, &mut used);
    analyze_expr_locals(result, line, diagnostics);
    for (local, used) in locals.iter().zip(used) {
        if !used && local.canonical.is_some() && !local.suppress_unused_warning {
            diagnostics.push(Diagnostic::warn(
                line,
                format!("unused local `{}`", local.raw),
            ));
        }
    }
}

fn analyze_when_branch_locals(arm: &WhenArm, diagnostics: &mut Vec<Diagnostic>) {
    analyze_outcome_branch_locals(None, &arm.guards, &arm.outcome, arm.line, diagnostics);
}

fn analyze_outcome_branch_locals(
    pattern: Option<&SyntaxPattern>,
    guards: &[SyntaxGuardClause],
    outcome: &MatchOutcome,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut locals = Vec::new();
    let mut used = Vec::new();
    visit_branch_prefix(pattern, guards, line, &mut locals, &mut used, diagnostics);
    analyze_outcome_locals(outcome, &locals, &mut used, diagnostics);
    warn_unused_branch_locals(&locals, &used, line, diagnostics);
}

fn analyze_outcome_locals(
    outcome: &MatchOutcome,
    locals: &[LocalName],
    used: &mut [bool],
    diagnostics: &mut Vec<Diagnostic>,
) {
    match outcome {
        MatchOutcome::Result {
            line, expression, ..
        } => {
            mark_used_locals(expression, locals, used);
            analyze_expr_locals(expression, *line, diagnostics);
        }
        MatchOutcome::Nested(arms) => {
            for arm in arms {
                mark_used_when_branch(arm, locals, used);
                analyze_when_branch_locals(arm, diagnostics);
            }
        }
    }
}

fn visit_branch_prefix(
    pattern: Option<&SyntaxPattern>,
    guards: &[SyntaxGuardClause],
    line: usize,
    locals: &mut Vec<LocalName>,
    used: &mut Vec<bool>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(pattern) = pattern {
        pattern.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, locals, used);
                analyze_expr_locals(expr, line, diagnostics);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                locals.push(local_name_metadata(name));
                used.push(false);
            }
        });
    }
    for guard in guards {
        guard.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, locals, used);
                analyze_expr_locals(expr, line, diagnostics);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                locals.push(local_name_metadata(name));
                used.push(false);
            }
        });
    }
}

fn warn_unused_branch_locals(
    locals: &[LocalName],
    used: &[bool],
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (local, used) in locals.iter().zip(used) {
        if !used && local.canonical.is_some() && !local.suppress_unused_warning {
            diagnostics.push(Diagnostic::warn(
                line,
                format!("unused local `{}`", local.raw),
            ));
        }
    }
}

fn mark_used_guard_branch(
    guards: &[SyntaxGuardClause],
    result: &SyntaxExpr,
    locals: &[LocalName],
    used: &mut [bool],
) {
    mark_used_branch(None, guards, result, locals, used);
}

fn mark_used_pattern_guard_outcome_branch(
    pattern: &SyntaxPattern,
    guards: &[SyntaxGuardClause],
    outcome: &MatchOutcome,
    locals: &[LocalName],
    used: &mut [bool],
) {
    mark_used_outcome_branch(Some(pattern), guards, outcome, locals, used);
}

fn mark_used_branch(
    pattern: Option<&SyntaxPattern>,
    guards: &[SyntaxGuardClause],
    result: &SyntaxExpr,
    locals: &[LocalName],
    used: &mut [bool],
) {
    let outer_len = locals.len();
    let mut combined = locals.to_vec();
    let mut combined_used = used.to_vec();
    if let Some(pattern) = pattern {
        pattern.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, &combined, &mut combined_used);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                combined.push(local_name_metadata(name));
                combined_used.push(false);
            }
        });
    }
    for guard in guards {
        guard.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, &combined, &mut combined_used);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                combined.push(local_name_metadata(name));
                combined_used.push(false);
            }
        });
    }
    mark_used_locals(result, &combined, &mut combined_used);
    used.copy_from_slice(&combined_used[..outer_len]);
}

fn mark_used_when_branch(arm: &WhenArm, locals: &[LocalName], used: &mut [bool]) {
    mark_used_outcome_branch(None, &arm.guards, &arm.outcome, locals, used);
}

fn mark_used_outcome_branch(
    pattern: Option<&SyntaxPattern>,
    guards: &[SyntaxGuardClause],
    outcome: &MatchOutcome,
    locals: &[LocalName],
    used: &mut [bool],
) {
    let outer_len = locals.len();
    let mut combined = locals.to_vec();
    let mut combined_used = used.to_vec();
    if let Some(pattern) = pattern {
        pattern.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, &combined, &mut combined_used);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                combined.push(local_name_metadata(name));
                combined_used.push(false);
            }
        });
    }
    for guard in guards {
        guard.visit_scope_events(&mut |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => {
                mark_used_locals(expr, &combined, &mut combined_used);
            }
            SyntaxPatternScopeEvent::Capture(name) => {
                combined.push(local_name_metadata(name));
                combined_used.push(false);
            }
        });
    }
    match outcome {
        MatchOutcome::Result { expression, .. } => {
            mark_used_locals(expression, &combined, &mut combined_used);
        }
        MatchOutcome::Nested(arms) => {
            for arm in arms {
                mark_used_when_branch(arm, &combined, &mut combined_used);
            }
        }
    }
    used.copy_from_slice(&combined_used[..outer_len]);
}

fn mark_used_prior_alias_in_outcome(outcome: &MatchOutcome, alias: Option<&str>, used: &mut bool) {
    match outcome {
        MatchOutcome::Result { expression, .. } => mark_used_prior_alias(expression, alias, used),
        MatchOutcome::Nested(arms) => {
            for arm in arms {
                for guard in &arm.guards {
                    guard.visit_scope_events(&mut |event| {
                        if let SyntaxPatternScopeEvent::Expression(expr) = event {
                            mark_used_prior_alias(expr, alias, used);
                        }
                    });
                }
                mark_used_prior_alias_in_outcome(&arm.outcome, alias, used);
            }
        }
    }
}

fn analyze_do_expr_locals(do_expr: &DoExpr, diagnostics: &mut Vec<Diagnostic>) {
    if let Ok(plan) = preview_recursive_do_plan(do_expr) {
        diagnostics.extend(plan.promotion_warnings());
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
            DoStepKind::Bind { pattern, .. } | DoStepKind::ValueBind { pattern, .. } => {
                pattern.visit_scope_events(&mut |event| match event {
                    SyntaxPatternScopeEvent::Expression(expr) => {
                        mark_used_locals(expr, &locals, &mut used);
                        analyze_expr_locals(expr, step.line, diagnostics);
                    }
                    SyntaxPatternScopeEvent::Capture(name) => {
                        if !fulfills_abstract(name, &mut unresolved_abstracts) {
                            locals.push(local_name_metadata(name));
                            used.push(false);
                            binding_lines.push(step.line);
                        }
                    }
                });
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

/// Builds only the primitive recursive provenance needed to preview warnings.
///
/// Resolution builds the authoritative stream by decorating resolved effect
/// steps in the do adapter. This source-level preview exists because
/// unused-local analysis runs before resolution; `RecursiveDoPlan` itself
/// remains pattern-agnostic.
fn preview_recursive_do_plan(
    do_expr: &DoExpr,
) -> Result<recursive_do::RecursiveDoPlan, Diagnostic> {
    let mut registry = recursive_do::ForwardNameRegistry::default();
    let mut steps = Vec::new();
    for step in &do_expr.steps {
        match &step.kind {
            DoStepKind::Abstract(names) => {
                let ids = registry.declare(names, step.line)?;
                steps.push(recursive_do::RecursiveDoStep {
                    line: step.line,
                    event: recursive_do::RecursiveDoEvent::Declare(ids),
                });
            }
            DoStepKind::Bind { pattern, .. } | DoStepKind::ValueBind { pattern, .. } => {
                pattern.visit_primitive_events(&mut |capture| {
                    let event = capture.and_then(|name| registry.fulfill(name)).map_or(
                        recursive_do::RecursiveDoEvent::None,
                        recursive_do::RecursiveDoEvent::Fulfill,
                    );
                    steps.push(recursive_do::RecursiveDoStep {
                        line: step.line,
                        event,
                    });
                });
            }
            DoStepKind::Then(_) => steps.push(recursive_do::RecursiveDoStep {
                line: step.line,
                event: recursive_do::RecursiveDoEvent::None,
            }),
        }
    }
    recursive_do::RecursiveDoPlan::build(steps.iter(), registry.into_forwards())
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
            DoStepKind::Bind { pattern, .. } | DoStepKind::ValueBind { pattern, .. } => {
                pattern.visit_scope_events(&mut |event| match event {
                    SyntaxPatternScopeEvent::Expression(expr) => {
                        mark_used_locals(expr, &combined, &mut combined_used);
                    }
                    SyntaxPatternScopeEvent::Capture(name) => {
                        if !fulfills_abstract(name, &mut unresolved_abstracts) {
                            combined.push(local_name_metadata(name));
                            combined_used.push(false);
                        }
                    }
                });
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
        for parent in &object.deps {
            mark_used_locals(parent, locals, used);
        }
        for item in &object.body {
            mark_used_body_item_locals(item, locals, used);
        }
    }
    if let Some(extend) = item.extend() {
        for item in &extend.body {
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

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
        | SyntaxExpr::Embedded(_)
        | SyntaxExpr::Number(_)
        | SyntaxExpr::Text(_)
        | SyntaxExpr::Atom(_)
        | SyntaxExpr::Effect(_)
        | SyntaxExpr::AbstractGlobalPath { .. } => {}
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
        SyntaxExpr::Using { namespace, body } => {
            analyze_expr_locals(namespace, line, diagnostics);
            analyze_expr_locals(body, line, diagnostics);
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
            // The group is mutually recursive: a binding the body reaches
            // uses the siblings its value names.
            let dependencies = binding_group_dependencies(bindings);
            let mut reached = (0..params.len())
                .filter(|&index| used[index])
                .collect::<Vec<_>>();
            while let Some(index) = reached.pop() {
                for &dependency in &dependencies[index] {
                    if !used[dependency] {
                        used[dependency] = true;
                        reached.push(dependency);
                    }
                }
            }
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
        | SyntaxExpr::Embedded(_)
        | SyntaxExpr::Number(_)
        | SyntaxExpr::Text(_)
        | SyntaxExpr::Atom(_)
        | SyntaxExpr::Effect(_)
        | SyntaxExpr::AbstractGlobalPath { .. }
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
        SyntaxExpr::Using { namespace, body } => {
            mark_used_prior_alias(namespace, alias, used);
            mark_used_prior_alias(body, alias, used);
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

/// For each binding of one `let` or `where` group, the positions of the
/// group's bindings its value names. The group is mutually recursive, so
/// these are its dependency edges.
pub(in crate::g_syntax) fn binding_group_dependencies(
    bindings: &[(String, SyntaxExpr)],
) -> Vec<Vec<usize>> {
    let names = bindings
        .iter()
        .map(|(name, _)| local_name_metadata(name))
        .collect::<Vec<_>>();
    bindings
        .iter()
        .map(|(_, value)| {
            let mut used = vec![false; names.len()];
            mark_used_locals(value, &names, &mut used);
            (0..names.len()).filter(|&index| used[index]).collect()
        })
        .collect()
}

/// The canonical name a source binder binds, as `local_name_metadata`
/// gives it, without allocating.
fn binder_canonical(raw: &str) -> Option<&str> {
    match raw {
        "_" => None,
        suppressed if suppressed.starts_with('_') => Some(&suppressed[1..]),
        name => Some(name),
    }
}

/// The outer locals one marking walk tracks, as a nested scope sees them.
/// Only the outer locals' uses are read, so a nested binder matters only
/// where it rebinds an outer local's name, hiding that local in its scope.
/// Nothing is copied until a nested binder shadows an outer local, so a
/// walk costs time linear in the expression it visits, however deeply its
/// binders nest.
struct NestedLocals<'outer> {
    outer: &'outer [LocalName],
    /// `outer` with each shadowed local's name removed, once one is.
    hidden: Option<Vec<LocalName>>,
}

impl<'outer> NestedLocals<'outer> {
    fn new(outer: &'outer [LocalName]) -> Self {
        Self {
            outer,
            hidden: None,
        }
    }

    fn bind(&mut self, raw: &str) {
        let Some(canonical) = binder_canonical(raw) else {
            return;
        };
        let shadows = |local: &LocalName| local.canonical.as_deref() == Some(canonical);
        if !self.get().iter().any(shadows) {
            return;
        }
        let outer = self.outer;
        let hidden = self.hidden.get_or_insert_with(|| outer.to_vec());
        for local in hidden.iter_mut().filter(|local| shadows(local)) {
            local.canonical = None;
        }
    }

    fn get(&self) -> &[LocalName] {
        self.hidden.as_deref().unwrap_or(self.outer)
    }
}

fn mark_used_locals(expr: &SyntaxExpr, locals: &[LocalName], used: &mut [bool]) {
    match expr {
        SyntaxExpr::Unit
        | SyntaxExpr::Embedded(_)
        | SyntaxExpr::Number(_)
        | SyntaxExpr::Text(_)
        | SyntaxExpr::Atom(_)
        | SyntaxExpr::Effect(_)
        | SyntaxExpr::AbstractGlobalPath { .. } => {}
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
        SyntaxExpr::Using { namespace, body } => {
            mark_used_locals(namespace, locals, used);
            mark_used_locals(body, locals, used);
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
            let mut nested = NestedLocals::new(locals);
            for param in params {
                nested.bind(param);
            }
            mark_used_locals(body, nested.get(), used);
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
            // The group's names are in scope in every value as well as the
            // body.
            let mut nested = NestedLocals::new(locals);
            for (name, _) in bindings {
                nested.bind(name);
            }
            for (_, value) in bindings {
                mark_used_locals(value, nested.get(), used);
            }
            mark_used_locals(body, nested.get(), used);
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
    let mut nested = NestedLocals::new(locals);
    mark_used_branch_prefix(pattern, guards, &mut nested, used);
    mark_used_locals(result, nested.get(), used);
}

/// Marks a branch's pattern and guard expressions, binding their captures in
/// `nested` as they appear.
fn mark_used_branch_prefix(
    pattern: Option<&SyntaxPattern>,
    guards: &[SyntaxGuardClause],
    nested: &mut NestedLocals<'_>,
    used: &mut [bool],
) {
    let mut visit = |event| match event {
        SyntaxPatternScopeEvent::Expression(expr) => mark_used_locals(expr, nested.get(), used),
        SyntaxPatternScopeEvent::Capture(name) => nested.bind(name),
    };
    if let Some(pattern) = pattern {
        pattern.visit_scope_events(&mut visit);
    }
    for guard in guards {
        guard.visit_scope_events(&mut visit);
    }
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
    let mut nested = NestedLocals::new(locals);
    mark_used_branch_prefix(pattern, guards, &mut nested, used);
    match outcome {
        MatchOutcome::Result { expression, .. } => {
            mark_used_locals(expression, nested.get(), used);
        }
        MatchOutcome::Nested(arms) => {
            for arm in arms {
                mark_used_when_branch(arm, nested.get(), used);
            }
        }
    }
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
    let mut nested = NestedLocals::new(locals);
    let mut unresolved_abstracts = Vec::new();

    for step in &do_expr.steps {
        if let Some(expr) = do_step_expr(step) {
            mark_used_locals(expr, nested.get(), used);
        }
        match &step.kind {
            DoStepKind::Abstract(names) => {
                for name in names {
                    if let Some(canonical) = binder_canonical(name) {
                        unresolved_abstracts.push(canonical.to_owned());
                    }
                    nested.bind(name);
                }
            }
            DoStepKind::Bind { pattern, .. } | DoStepKind::ValueBind { pattern, .. } => {
                pattern.visit_scope_events(&mut |event| match event {
                    SyntaxPatternScopeEvent::Expression(expr) => {
                        mark_used_locals(expr, nested.get(), used);
                    }
                    SyntaxPatternScopeEvent::Capture(name) => {
                        if !fulfills_abstract(name, &mut unresolved_abstracts) {
                            nested.bind(name);
                        }
                    }
                });
            }
            DoStepKind::Then(_) => {}
        }
    }
    mark_used_locals(&do_expr.result, nested.get(), used);
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

//! Unused-local warnings, and the dependency edges of `let` groups.
//!
//! One walk with a scope stack serves every analysis here
//! (`parser-scope-analysis`). A name resolves to its innermost binder as the
//! walk meets it, through an index of the binders in scope by name, so the
//! cost is linear in the source however deeply binders nest. A binder's
//! unused locals are known when its scope ends; a binder that warns before
//! the warnings inside its scope reserves its place in the output, so
//! warnings keep the order of a walk per binder.
//!
//! Scopes follow the source, not resolution: a localized body does not hide
//! the locals around it here, so a name it reads as `self.name` still counts
//! as a use of an outer local of that name.

use std::collections::HashMap;

use super::*;

/// Warns for each local bound in `expr` that its scope does not use.
pub(super) fn warn_unused_locals(
    expr: &SyntaxExpr,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut walk = Walk::reporting();
    walk.expr(expr, line);
    walk.finish(diagnostics);
}

/// Warns for each binding of a body's `let` groups that the members after
/// its group do not use. The members report their own locals as they parse.
pub(super) fn warn_unused_body_lets(
    body: &[ObjectBodyDefinition],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut walk = Walk::quiet();
    walk.body(None, body, Report::Lets);
    walk.finish(diagnostics);
}

/// Warns, on its member's line, for an alias its body does not use.
pub(super) fn warn_unused_with_alias(
    alias: &Alias,
    body: &[ObjectBodyDefinition],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut walk = Walk::quiet();
    walk.body(Some(alias), body, Report::Alias);
    walk.finish(diagnostics);
}

/// Warns for each binding of a file's `let` groups that neither the
/// declarations after its group nor a macro invocation, by its name in
/// `macro_uses`, uses.
pub(super) fn warn_unused_file_lets(
    declarations: &[Declaration],
    macro_uses: &[String],
) -> Vec<Diagnostic> {
    let mut walk = Walk::quiet();
    let start = walk.groups.len();
    for declaration in declarations {
        let line = declaration.line;
        match &declaration.kind {
            DeclarationKind::Let(bindings) => {
                let slot = walk.reserve();
                let first = walk.open_group(bindings, line, Some(slot));
                for (offset, (name, _)) in bindings.iter().enumerate() {
                    walk.locals[first + offset].used = macro_uses.contains(name);
                }
                walk.group_values(bindings, line);
            }
            DeclarationKind::Definition(definition) => {
                walk.keys(&definition.target, line);
                if let Some(expr) = &definition.expr {
                    walk.expr(expr, line);
                }
            }
            DeclarationKind::Object(object) => {
                for parent in &object.deps {
                    walk.expr(parent, line);
                }
                walk.body(object.alias.as_ref(), &object.body, Report::None);
            }
            DeclarationKind::Extend(extend) => {
                walk.body(extend.alias.as_ref(), &extend.body, Report::None);
            }
            DeclarationKind::Language(_)
            | DeclarationKind::Import(_)
            | DeclarationKind::Declare(_)
            | DeclarationKind::Assume(_)
            | DeclarationKind::Unique(_)
            | DeclarationKind::Unknown => {}
        }
    }
    walk.close_groups(start);
    let mut diagnostics = Vec::new();
    walk.finish(&mut diagnostics);
    diagnostics
}

/// The dependency edges of `let` groups: for each binding of a group, the
/// positions of the group's bindings its value names. A group is mutually
/// recursive, so these order its strongly connected components.
///
/// One walk computes the edges of a group and of every group nested in its
/// values, and keeps the nested groups' edges until they are asked for, so
/// nested groups cost one walk in all. Groups are known by their bindings'
/// address in the source being resolved, which outlives the walk's users and
/// is never rebuilt while they run.
#[derive(Debug, Default)]
pub(in crate::g_syntax) struct GroupDependencies {
    known: HashMap<GroupKey, Vec<Vec<usize>>>,
}

impl GroupDependencies {
    pub(in crate::g_syntax) fn of(&mut self, bindings: &[(String, SyntaxExpr)]) -> Vec<Vec<usize>> {
        let key = group_key(bindings);
        if let Some(edges) = self.known.remove(&key) {
            return edges;
        }
        let mut walk = Walk::quiet();
        walk.dependencies = Some(std::mem::take(&mut self.known));
        let start = walk.groups.len();
        walk.open_group(bindings, 0, None);
        walk.group_values(bindings, 0);
        walk.close_groups(start);
        self.known = walk.dependencies.take().expect("the walk kept the edges");
        self.known
            .remove(&key)
            .expect("the walk kept the group's edges")
    }
}

/// A group's bindings by address and length.
type GroupKey = (usize, usize);

fn group_key(bindings: &[(String, SyntaxExpr)]) -> GroupKey {
    (bindings.as_ptr() as usize, bindings.len())
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

/// A local in scope.
struct Local<'a> {
    raw: &'a str,
    canonical: Option<&'a str>,
    /// Where its warning goes.
    line: usize,
    used: bool,
    /// An object alias, which `_alias` uses too.
    alias: bool,
    /// Its `let` group and position there. A use from one of the group's
    /// values is a dependency edge, not a use.
    member: Option<(usize, usize)>,
}

impl Local<'_> {
    /// Whether it warns as unused: a `_` prefix suppresses the warning, and
    /// `_` binds nothing.
    fn warns(&self, used: bool) -> bool {
        !used && !self.raw.starts_with('_')
    }

    fn warning(&self) -> Diagnostic {
        Diagnostic::warn(self.line, format!("unused local `{}`", self.raw))
    }
}

/// An open `let` group: a `let` expression's, a body's or a file's.
struct Group {
    /// Where its members start among the locals.
    first: usize,
    /// The binding whose value the walk is in.
    current: Option<usize>,
    /// For each binding, the members its value uses.
    edges: Vec<Vec<usize>>,
    key: GroupKey,
    /// Where its warnings go, if it reports.
    slot: Option<usize>,
}

/// The output: warnings, and places reserved for warnings a scope's end
/// gives.
enum Out {
    Warning(Diagnostic),
    Reserved(Vec<Diagnostic>),
}

/// Which binders of a body report, in a walk that is otherwise quiet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Report {
    None,
    Alias,
    Lets,
}

struct Walk<'a> {
    locals: Vec<Local<'a>>,
    /// The locals in scope by canonical name, innermost last.
    by_name: HashMap<&'a str, Vec<usize>>,
    groups: Vec<Group>,
    out: Vec<Out>,
    /// Whether binders report their unused locals. A quiet walk reports only
    /// the binders it is asked about.
    reporting: bool,
    /// Each closed group's edges, when kept.
    dependencies: Option<HashMap<GroupKey, Vec<Vec<usize>>>>,
}

impl<'a> Walk<'a> {
    fn reporting() -> Self {
        Self {
            locals: Vec::new(),
            by_name: HashMap::new(),
            groups: Vec::new(),
            out: Vec::new(),
            reporting: true,
            dependencies: None,
        }
    }

    fn quiet() -> Self {
        Self {
            reporting: false,
            ..Self::reporting()
        }
    }

    fn finish(self, diagnostics: &mut Vec<Diagnostic>) {
        for out in self.out {
            match out {
                Out::Warning(warning) => diagnostics.push(warning),
                Out::Reserved(warnings) => diagnostics.extend(warnings),
            }
        }
    }

    /// Reserves a place for warnings a scope's end gives.
    fn reserve(&mut self) -> usize {
        self.out.push(Out::Reserved(Vec::new()));
        self.out.len() - 1
    }

    fn fill(&mut self, slot: usize, warnings: impl IntoIterator<Item = Diagnostic>) {
        let Out::Reserved(reserved) = &mut self.out[slot] else {
            unreachable!("a reserved place holds warnings");
        };
        reserved.extend(warnings);
    }

    fn bind(&mut self, raw: &'a str, line: usize) -> usize {
        let index = self.locals.len();
        let canonical = binder_canonical(raw);
        self.locals.push(Local {
            raw,
            canonical,
            line,
            used: false,
            alias: false,
            member: None,
        });
        if let Some(canonical) = canonical {
            self.by_name.entry(canonical).or_default().push(index);
        }
        index
    }

    /// Ends the scope of the locals from `start`.
    fn unbind(&mut self, start: usize) -> Vec<Local<'a>> {
        let locals = self.locals.split_off(start);
        for local in locals.iter().rev() {
            if let Some(canonical) = local.canonical {
                let binders = self
                    .by_name
                    .get_mut(canonical)
                    .expect("a bound name is indexed");
                binders.pop();
                if binders.is_empty() {
                    self.by_name.remove(canonical);
                }
            }
        }
        locals
    }

    /// Ends the scope of the locals from `start`, warning for the unused at
    /// `slot`, or in place.
    fn close(&mut self, start: usize, slot: Option<usize>) {
        let locals = self.unbind(start);
        if !self.reporting {
            return;
        }
        let warnings = locals
            .iter()
            .filter(|local| local.warns(local.used))
            .map(Local::warning);
        match slot {
            Some(slot) => self.fill(slot, warnings),
            None => self.out.extend(warnings.map(Out::Warning)),
        }
    }

    fn use_name(&mut self, name: &str) {
        let Some(&index) = self.by_name.get(name).and_then(|binders| binders.last()) else {
            return;
        };
        if let Some((group, member)) = self.locals[index].member
            && let Some(current) = self.groups[group].current
        {
            self.groups[group].edges[current].push(member);
        } else {
            self.locals[index].used = true;
        }
    }

    /// `_name` uses an alias of that name.
    fn use_prior(&mut self, name: &str) {
        if let Some(&index) = self.by_name.get(name).and_then(|binders| binders.last())
            && self.locals[index].alias
        {
            self.locals[index].used = true;
        }
    }

    /// Opens a group of `bindings`, warning at `line` into `slot` when it
    /// reports. Returns where its members start.
    fn open_group(
        &mut self,
        bindings: &'a [(String, SyntaxExpr)],
        line: usize,
        slot: Option<usize>,
    ) -> usize {
        let group = self.groups.len();
        let first = self.locals.len();
        for (member, (name, _)) in bindings.iter().enumerate() {
            let index = self.bind(name, line);
            self.locals[index].member = Some((group, member));
        }
        self.groups.push(Group {
            first,
            current: None,
            edges: vec![Vec::new(); bindings.len()],
            key: group_key(bindings),
            slot,
        });
        first
    }

    /// Walks the values of the innermost group, each the group's current
    /// binding while the walk is in it.
    fn group_values(&mut self, bindings: &'a [(String, SyntaxExpr)], line: usize) {
        let group = self.groups.len() - 1;
        for (member, (_, value)) in bindings.iter().enumerate() {
            self.groups[group].current = Some(member);
            self.expr(value, line);
        }
        self.groups[group].current = None;
    }

    /// Closes the groups from `start`, innermost first. A member is used if
    /// its scope uses it, or a used member's value does.
    fn close_groups(&mut self, start: usize) {
        while self.groups.len() > start {
            let group = self.groups.pop().expect("a group is open");
            let members = self.unbind(group.first);
            let mut used = members.iter().map(|member| member.used).collect::<Vec<_>>();
            let mut reached = (0..used.len())
                .filter(|&member| used[member])
                .collect::<Vec<_>>();
            while let Some(member) = reached.pop() {
                for &dependency in &group.edges[member] {
                    if !used[dependency] {
                        used[dependency] = true;
                        reached.push(dependency);
                    }
                }
            }
            if let Some(slot) = group.slot {
                let warnings = members
                    .iter()
                    .zip(used)
                    .filter(|(member, used)| member.warns(*used))
                    .map(|(member, _)| member.warning())
                    .collect::<Vec<_>>();
                self.fill(slot, warnings);
            }
            if let Some(dependencies) = &mut self.dependencies {
                let edges = group
                    .edges
                    .into_iter()
                    .map(|mut edges| {
                        edges.sort_unstable();
                        edges.dedup();
                        edges
                    })
                    .collect();
                dependencies.insert(group.key, edges);
            }
        }
    }

    fn keys(&mut self, keys: &'a [SyntaxKeyExpr], line: usize) {
        for key in keys {
            match key {
                SyntaxKeyExpr::Atom(_) => {}
                SyntaxKeyExpr::Index(expr) | SyntaxKeyExpr::PathIndex(expr) => {
                    self.expr(expr, line)
                }
            }
        }
    }

    fn expr(&mut self, expr: &'a SyntaxExpr, line: usize) {
        match expr {
            SyntaxExpr::Unit
            | SyntaxExpr::Embedded(_)
            | SyntaxExpr::Number(_)
            | SyntaxExpr::Text(_)
            | SyntaxExpr::Atom(_)
            | SyntaxExpr::Effect(_)
            | SyntaxExpr::AbstractGlobalPath { .. } => {}
            SyntaxExpr::Name(name) => self.use_name(name),
            SyntaxExpr::PriorName(name) => self.use_prior(name),
            SyntaxExpr::Escape(_, expr) => self.expr(expr, line),
            SyntaxExpr::Access(base, parts) => {
                self.expr(base, line);
                self.keys(parts, line);
            }
            SyntaxExpr::Object(object) => {
                if let Some(name) = &object.name {
                    self.expr(name, line);
                }
                for dep in &object.deps {
                    self.expr(dep, line);
                }
                self.body(object.alias.as_ref(), &object.body, Report::None);
            }
            SyntaxExpr::With { base, alias, body } => {
                self.expr(base, line);
                self.body(alias.as_ref(), body, Report::None);
            }
            SyntaxExpr::Using { namespace, body } => {
                self.expr(namespace, line);
                self.expr(body, line);
            }
            SyntaxExpr::PathDict(path, value) => {
                self.keys(path, line);
                self.expr(value, line);
            }
            SyntaxExpr::TaggedConstructor(path) => self.keys(path, line),
            SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) | SyntaxExpr::Tuple(items) => {
                for item in items {
                    self.expr(item, line);
                }
            }
            SyntaxExpr::Lambda(params, body) => {
                let slot = self.reporting.then(|| self.reserve());
                let start = self.locals.len();
                for param in params {
                    self.bind(param, line);
                }
                self.expr(body, line);
                self.close(start, slot);
            }
            SyntaxExpr::Do(do_expr) => self.do_block(do_expr),
            SyntaxExpr::If(if_expr) => {
                self.branch(None, &if_expr.guards, line, |walk| {
                    walk.expr(&if_expr.then_result, line)
                });
                self.expr(&if_expr.else_result, line);
            }
            SyntaxExpr::Match(match_expr) => {
                self.expr(&match_expr.subject, line);
                for arm in &match_expr.arms {
                    self.branch(Some(&arm.pattern), &arm.guards, arm.line, |walk| {
                        walk.outcome(&arm.outcome)
                    });
                }
            }
            SyntaxExpr::MatchWhen(match_when) => {
                for arm in &match_when.arms {
                    self.when_arm(arm);
                }
            }
            SyntaxExpr::Let { bindings, body } => {
                let slot = self.reporting.then(|| self.reserve());
                let start = self.groups.len();
                self.open_group(bindings, line, slot);
                self.group_values(bindings, line);
                self.expr(body, line);
                self.close_groups(start);
            }
            SyntaxExpr::OperatorSection { left, right, .. } => {
                if let Some(left) = left {
                    self.expr(left, line);
                }
                if let Some(right) = right {
                    self.expr(right, line);
                }
            }
            SyntaxExpr::ComparisonChain { first, rest } => {
                self.expr(first, line);
                for (_, expr) in rest {
                    self.expr(expr, line);
                }
            }
            SyntaxExpr::OperatorApply { left, right, .. }
            | SyntaxExpr::Apply(left, right)
            | SyntaxExpr::Multiply(left, right)
            | SyntaxExpr::Divide(left, right)
            | SyntaxExpr::Add(left, right)
            | SyntaxExpr::Subtract(left, right)
            | SyntaxExpr::Append(left, right) => {
                self.expr(left, line);
                self.expr(right, line);
            }
        }
    }

    /// An object's or update's body. Its alias is in scope for every member,
    /// and each `let` group from its own member on. They warn before the
    /// members' own locals, in a reporting walk or as `report` asks.
    fn body(
        &mut self,
        alias: Option<&'a Alias>,
        items: &'a [ObjectBodyDefinition],
        report: Report,
    ) {
        let report_alias = self.reporting || report == Report::Alias;
        let report_lets = self.reporting || report == Report::Lets;
        let start = self.locals.len();
        let alias_slot = match alias.filter(|alias| alias.name != "self") {
            Some(alias) => {
                let slot = report_alias.then(|| self.reserve());
                let index = self.bind(&alias.name, alias.line);
                self.locals[index].alias = true;
                Some(slot)
            }
            None => None,
        };
        let mut let_slots = items
            .iter()
            .filter(|item| matches!(item.kind, ObjectBodyDefinitionKind::Let(_)))
            .map(|_| report_lets.then(|| self.reserve()))
            .collect::<Vec<_>>()
            .into_iter();
        let groups = self.groups.len();
        for item in items {
            let line = item.line;
            if let Some(definition) = item.definition()
                && let Some(expr) = &definition.expr
            {
                self.expr(expr, line);
            }
            if let ObjectBodyDefinitionKind::Let(bindings) = &item.kind {
                let slot = let_slots.next().expect("each `let` member has a place");
                self.open_group(bindings, line, slot);
                self.group_values(bindings, line);
            }
            if let Some(object) = item.object() {
                for parent in &object.deps {
                    self.expr(parent, line);
                }
                self.body(object.alias.as_ref(), &object.body, Report::None);
            }
            if let Some(extend) = item.extend() {
                self.body(extend.alias.as_ref(), &extend.body, Report::None);
            }
        }
        self.close_groups(groups);
        if let Some(slot) = alias_slot {
            let alias = self.unbind(start).pop().expect("the alias is bound");
            if let Some(slot) = slot
                && alias.warns(alias.used)
            {
                self.fill(slot, [alias.warning()]);
            }
        }
    }

    fn do_block(&mut self, do_expr: &'a DoExpr) {
        if self.reporting
            && let Ok(plan) = preview_recursive_do_plan(do_expr)
        {
            self.out
                .extend(plan.promotion_warnings().into_iter().map(Out::Warning));
        }
        let start = self.locals.len();
        let mut unresolved_abstracts = Vec::new();
        for step in &do_expr.steps {
            if let Some(expr) = do_step_expr(step) {
                self.expr(expr, step.line);
            }
            match &step.kind {
                DoStepKind::Declare(names) => {
                    for name in names {
                        if let Some(canonical) = binder_canonical(name) {
                            unresolved_abstracts.push(canonical);
                        }
                        self.bind(name, step.line);
                    }
                }
                DoStepKind::Bind { pattern, .. } | DoStepKind::ValueBind { pattern, .. } => {
                    pattern.visit_scope_events(&mut |event| match event {
                        SyntaxPatternScopeEvent::Expression(expr) => self.expr(expr, step.line),
                        SyntaxPatternScopeEvent::Capture(name) => {
                            if !fulfills_abstract(name, &mut unresolved_abstracts) {
                                self.bind(name, step.line);
                            }
                        }
                    });
                }
                DoStepKind::Then(_) => {}
            }
        }
        self.expr(&do_expr.result, do_expr.result_line);
        self.close(start, None);
    }

    /// A branch: a pattern's and guards' captures, in scope for the guards
    /// after them and for `rest`, warning at `line` after the branch's own
    /// warnings.
    fn branch(
        &mut self,
        pattern: Option<&'a SyntaxPattern>,
        guards: &'a [SyntaxGuardClause],
        line: usize,
        rest: impl FnOnce(&mut Self),
    ) {
        let start = self.locals.len();
        let mut visit = |event| match event {
            SyntaxPatternScopeEvent::Expression(expr) => self.expr(expr, line),
            SyntaxPatternScopeEvent::Capture(name) => {
                self.bind(name, line);
            }
        };
        if let Some(pattern) = pattern {
            pattern.visit_scope_events(&mut visit);
        }
        for guard in guards {
            guard.visit_scope_events(&mut visit);
        }
        rest(self);
        self.close(start, None);
    }

    fn when_arm(&mut self, arm: &'a WhenArm) {
        self.branch(None, &arm.guards, arm.line, |walk| {
            walk.outcome(&arm.outcome)
        });
    }

    fn outcome(&mut self, outcome: &'a MatchOutcome) {
        match outcome {
            MatchOutcome::Result {
                line, expression, ..
            } => self.expr(expression, *line),
            MatchOutcome::Nested(arms) => {
                for arm in arms {
                    self.when_arm(arm);
                }
            }
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
            DoStepKind::Declare(names) => {
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

fn do_step_expr(step: &DoStep) -> Option<&SyntaxExpr> {
    match &step.kind {
        DoStepKind::Declare(_) => None,
        DoStepKind::Bind { operation, .. } => Some(operation),
        DoStepKind::ValueBind { value, .. } => Some(value),
        DoStepKind::Then(expr) => Some(expr),
    }
}

/// Whether `name` fulfills a `declare`d name still waiting, which it then
/// stops waiting for.
fn fulfills_abstract(name: &str, unresolved: &mut Vec<&str>) -> bool {
    let canonical = binder_canonical(name);
    let Some(index) = unresolved
        .iter()
        .rposition(|abstract_name| Some(*abstract_name) == canonical)
    else {
        return false;
    };
    unresolved.remove(index);
    true
}

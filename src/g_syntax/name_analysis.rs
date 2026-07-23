//! File-wide checks that require more context than one lexical resolver owns.
//!
//! Ordinary local-to-local shadowing remains a resolver invariant. This pass
//! indexes only source-written namespace introductions and global-root uses so
//! that local/global conflicts do not depend on declaration order.

use std::collections::BTreeMap;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NamespaceId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolutionScopeId(usize);

#[derive(Debug, Default)]
struct Namespace {
    children: BTreeMap<String, NamespaceId>,
    introduced: BTreeMap<String, usize>,
    used: BTreeMap<String, usize>,
}

#[derive(Debug)]
struct ResolutionScope {
    definitions: NamespaceId,
    lookup: NamespaceId,
    object_alias: Option<String>,
    parent: Option<ResolutionScopeId>,
}

#[derive(Debug)]
struct SourceBinder {
    raw: String,
    canonical: String,
    line: usize,
    visible_namespaces: Vec<NamespaceId>,
}

struct FileNameAnalysis {
    namespaces: Vec<Namespace>,
    scopes: Vec<ResolutionScope>,
    binders: Vec<SourceBinder>,
}

impl FileNameAnalysis {
    fn new() -> Self {
        let root_namespace = NamespaceId(0);
        Self {
            namespaces: vec![Namespace::default()],
            scopes: vec![ResolutionScope {
                definitions: root_namespace,
                lookup: root_namespace,
                object_alias: None,
                parent: None,
            }],
            binders: Vec::new(),
        }
    }

    fn analyze(mut self, declarations: &[Declaration]) -> Vec<Diagnostic> {
        let root = ResolutionScopeId(0);
        let mut locals = Vec::new();
        for declaration in declarations {
            self.visit_declaration(declaration, root, &mut locals);
        }
        self.finish()
    }

    fn finish(self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for binder in self.binders {
            let introduction = binder.visible_namespaces.iter().find_map(|namespace| {
                self.namespaces[namespace.0]
                    .introduced
                    .get(&binder.canonical)
                    .copied()
            });
            if let Some(line) = introduction {
                diagnostics.push(Diagnostic::error(
                    binder.line,
                    format!(
                        "local `{}` shadows external `{}` defined on line {line}",
                        binder.raw, binder.canonical
                    ),
                ));
                continue;
            }

            let use_line = binder.visible_namespaces.iter().find_map(|namespace| {
                self.namespaces[namespace.0]
                    .used
                    .get(&binder.canonical)
                    .copied()
            });
            if let Some(line) = use_line {
                diagnostics.push(Diagnostic::error(
                    binder.line,
                    format!(
                        "local `{}` shadows external `{}` used by this file on line {line}",
                        binder.raw, binder.canonical
                    ),
                ));
            }
        }
        diagnostics
    }

    fn visit_declaration(
        &mut self,
        declaration: &Declaration,
        scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) {
        let line = declaration.line;
        match &declaration.kind {
            DeclarationKind::Import(import) => {
                if let ImportPlacement::As(target) = &import.placement {
                    self.introduce_static_path(self.scopes[scope.0].definitions, target, line);
                }
            }
            DeclarationKind::Abstract(paths) | DeclarationKind::Unique(paths) => {
                for path in paths {
                    self.introduce_static_path(self.scopes[scope.0].definitions, path, line);
                }
            }
            DeclarationKind::Object(object) => {
                self.visit_object_declaration(object, line, scope, locals);
            }
            DeclarationKind::Extend(extend) => {
                self.visit_extend_declaration(extend, line, scope, locals);
            }
            DeclarationKind::Definition(definition) => {
                self.visit_definition(definition, line, scope, locals);
            }
            DeclarationKind::Language(_) | DeclarationKind::Unknown => {}
        }
    }

    fn visit_definition(
        &mut self,
        definition: &DefinitionDecl,
        line: usize,
        scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) {
        self.introduce_definition_path(self.scopes[scope.0].definitions, &definition.target, line);
        for key in &definition.target {
            self.visit_key_expr(key, line, scope, locals);
        }
        if let Some(expr) = &definition.expr {
            self.visit_expr(expr, line, scope, locals);
        }
    }

    fn visit_object_declaration(
        &mut self,
        object: &ObjectDecl,
        line: usize,
        parent_scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) {
        for dependency in &object.deps {
            self.record_static_path_use(dependency, line, parent_scope, locals);
        }
        let object_namespace = self.introduce_static_path(
            self.scopes[parent_scope.0].definitions,
            &object.target,
            line,
        );
        self.visit_object_body(
            &object.body,
            BodyScopeSpec {
                alias: object.alias.as_deref(),
                line,
                parent: parent_scope,
                namespace: object_namespace,
                kind: BodyScopeKind::Object,
            },
            locals,
        );
    }

    fn visit_extend_declaration(
        &mut self,
        extend: &ObjectExtendDecl,
        line: usize,
        parent_scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) {
        let object_namespace =
            self.ensure_static_path(self.scopes[parent_scope.0].definitions, &extend.target);
        self.visit_object_body(
            &extend.body,
            BodyScopeSpec {
                alias: extend.alias.as_deref(),
                line,
                parent: parent_scope,
                namespace: object_namespace,
                kind: BodyScopeKind::Object,
            },
            locals,
        );
    }

    fn visit_object_body(
        &mut self,
        body: &[ObjectBodyDefinition],
        spec: BodyScopeSpec<'_>,
        locals: &mut Vec<String>,
    ) {
        let alias = spec.alias.map(local_name_metadata);
        let canonical_alias = alias.as_ref().and_then(|alias| alias.canonical.clone());
        let parent_lookup = self.scopes[spec.parent.0].lookup;
        let lookup = match spec.kind {
            BodyScopeKind::Object if canonical_alias.is_none() => spec.namespace,
            BodyScopeKind::Dictionary if canonical_alias.as_deref() == Some("self") => {
                spec.namespace
            }
            BodyScopeKind::Object | BodyScopeKind::Dictionary => parent_lookup,
        };
        let scope = self.push_scope(ResolutionScope {
            definitions: spec.namespace,
            lookup,
            object_alias: canonical_alias.clone(),
            parent: Some(spec.parent),
        });
        if let Some(alias) = alias
            && let Some(canonical) = alias.canonical
        {
            self.record_binder_with_raw(&alias.raw, canonical, spec.line, scope);
        }

        for item in body {
            match &item.kind {
                ObjectBodyDefinitionKind::Definition(definition) => {
                    self.visit_definition(definition, item.line, scope, locals);
                }
                ObjectBodyDefinitionKind::Object(object) => {
                    self.visit_object_declaration(object, item.line, scope, locals);
                }
                ObjectBodyDefinitionKind::Extend(extend) => {
                    self.visit_extend_declaration(extend, item.line, scope, locals);
                }
            }
        }
    }

    fn visit_expr(
        &mut self,
        expr: &SyntaxExpr,
        line: usize,
        scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) {
        match expr {
            SyntaxExpr::Unit
            | SyntaxExpr::Number(_)
            | SyntaxExpr::Text(_)
            | SyntaxExpr::Atom(_)
            | SyntaxExpr::Effect(_)
            | SyntaxExpr::PriorName(_) => {}
            SyntaxExpr::Name(name) => self.record_name_use(name, line, scope, locals),
            SyntaxExpr::Escape(depth, expr) => {
                let escaped = self.escape_scope(scope, *depth);
                self.visit_expr(expr, line, escaped, locals);
            }
            SyntaxExpr::Access(base, path) => {
                self.visit_expr(base, line, scope, locals);
                for key in path {
                    self.visit_key_expr(key, line, scope, locals);
                }
            }
            SyntaxExpr::Object(object) => {
                if let Some(name) = &object.name {
                    self.visit_expr(name, line, scope, locals);
                }
                for dependency in &object.deps {
                    self.visit_expr(dependency, line, scope, locals);
                }
                let namespace = self.push_namespace();
                self.visit_object_body(
                    &object.body,
                    BodyScopeSpec {
                        alias: object.alias.as_deref(),
                        line,
                        parent: scope,
                        namespace,
                        kind: BodyScopeKind::Object,
                    },
                    locals,
                );
            }
            SyntaxExpr::With { base, alias, body } => {
                self.visit_expr(base, line, scope, locals);
                let namespace = self.push_namespace();
                self.visit_object_body(
                    body,
                    BodyScopeSpec {
                        alias: alias.as_deref(),
                        line,
                        parent: scope,
                        namespace,
                        kind: BodyScopeKind::Dictionary,
                    },
                    locals,
                );
            }
            SyntaxExpr::PathDict(path, value) => {
                for key in path {
                    self.visit_key_expr(key, line, scope, locals);
                }
                self.visit_expr(value, line, scope, locals);
            }
            SyntaxExpr::TaggedConstructor(path) => {
                for key in path {
                    self.visit_key_expr(key, line, scope, locals);
                }
            }
            SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) | SyntaxExpr::Tuple(items) => {
                for item in items {
                    self.visit_expr(item, line, scope, locals);
                }
            }
            SyntaxExpr::Lambda(parameters, body) => {
                let base_len = locals.len();
                for parameter in parameters {
                    self.push_source_local(parameter, line, scope, locals);
                }
                self.visit_expr(body, line, scope, locals);
                locals.truncate(base_len);
            }
            SyntaxExpr::Do(do_expr) => self.visit_do_expr(do_expr, scope, locals),
            SyntaxExpr::Let { bindings, body } => {
                for (_, value) in bindings {
                    self.visit_expr(value, line, scope, locals);
                }
                let base_len = locals.len();
                for (name, _) in bindings {
                    self.push_source_local(name, line, scope, locals);
                }
                self.visit_expr(body, line, scope, locals);
                locals.truncate(base_len);
            }
            SyntaxExpr::OperatorSection { left, right, .. } => {
                if let Some(left) = left {
                    self.visit_expr(left, line, scope, locals);
                }
                if let Some(right) = right {
                    self.visit_expr(right, line, scope, locals);
                }
            }
            SyntaxExpr::ComparisonChain { first, rest } => {
                self.visit_expr(first, line, scope, locals);
                for (_, expr) in rest {
                    self.visit_expr(expr, line, scope, locals);
                }
            }
            SyntaxExpr::OperatorApply { left, right, .. }
            | SyntaxExpr::Apply(left, right)
            | SyntaxExpr::Multiply(left, right)
            | SyntaxExpr::Divide(left, right)
            | SyntaxExpr::Add(left, right)
            | SyntaxExpr::Subtract(left, right)
            | SyntaxExpr::Append(left, right) => {
                self.visit_expr(left, line, scope, locals);
                self.visit_expr(right, line, scope, locals);
            }
        }
    }

    fn visit_do_expr(
        &mut self,
        do_expr: &DoExpr,
        scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) {
        let base_len = locals.len();
        let mut unresolved = Vec::new();
        for step in &do_expr.steps {
            match &step.kind {
                DoStepKind::Abstract(names) => {
                    for name in names {
                        if let Some(canonical) =
                            self.push_source_local(name, step.line, scope, locals)
                        {
                            unresolved.push(canonical);
                        }
                    }
                }
                DoStepKind::Bind { name, operation } => {
                    self.visit_expr(operation, step.line, scope, locals);
                    if !fulfills_abstract_name(name, &mut unresolved) {
                        self.push_source_local(name, step.line, scope, locals);
                    }
                }
                DoStepKind::ValueBind { name, value } => {
                    self.visit_expr(value, step.line, scope, locals);
                    if !fulfills_abstract_name(name, &mut unresolved) {
                        self.push_source_local(name, step.line, scope, locals);
                    }
                }
                DoStepKind::Then(expr) => {
                    self.visit_expr(expr, step.line, scope, locals);
                }
            }
        }
        self.visit_expr(&do_expr.result, do_expr.result_line, scope, locals);
        locals.truncate(base_len);
    }

    fn visit_key_expr(
        &mut self,
        key: &SyntaxKeyExpr,
        line: usize,
        scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) {
        match key {
            SyntaxKeyExpr::Atom(_) => {}
            SyntaxKeyExpr::Index(expr) | SyntaxKeyExpr::PathIndex(expr) => {
                self.visit_expr(expr, line, scope, locals);
            }
        }
    }

    fn record_static_path_use(
        &mut self,
        path: &str,
        line: usize,
        scope: ResolutionScopeId,
        locals: &[String],
    ) {
        if let Some(root) = path.split('.').next() {
            self.record_name_use(root, line, scope, locals);
        }
    }

    fn record_name_use(
        &mut self,
        name: &str,
        line: usize,
        scope: ResolutionScopeId,
        locals: &[String],
    ) {
        if matches!(name, "module" | "self")
            || locals.iter().rev().any(|local| local == name)
            || self.scopes[scope.0].object_alias.as_deref() == Some(name)
        {
            return;
        }
        let namespace = self.scopes[scope.0].lookup;
        self.namespaces[namespace.0]
            .used
            .entry(name.to_owned())
            .or_insert(line);
    }

    fn push_source_local(
        &mut self,
        raw: &str,
        line: usize,
        scope: ResolutionScopeId,
        locals: &mut Vec<String>,
    ) -> Option<String> {
        let canonical = local_name_metadata(raw).canonical?;
        self.record_binder_with_raw(raw, canonical.clone(), line, scope);
        locals.push(canonical.clone());
        Some(canonical)
    }

    fn record_binder_with_raw(
        &mut self,
        raw: &str,
        canonical: String,
        line: usize,
        scope: ResolutionScopeId,
    ) {
        self.binders.push(SourceBinder {
            raw: raw.to_owned(),
            canonical,
            line,
            visible_namespaces: self.visible_namespaces(scope),
        });
    }

    fn visible_namespaces(&self, mut scope: ResolutionScopeId) -> Vec<NamespaceId> {
        let mut visible = Vec::new();
        loop {
            let current = &self.scopes[scope.0];
            if !visible.contains(&current.lookup) {
                visible.push(current.lookup);
            }
            let Some(parent) = current.parent else {
                break;
            };
            scope = parent;
        }
        visible
    }

    fn escape_scope(&self, mut scope: ResolutionScopeId, depth: usize) -> ResolutionScopeId {
        for _ in 0..depth {
            let Some(parent) = self.scopes[scope.0].parent else {
                return scope;
            };
            scope = parent;
        }
        scope
    }

    fn introduce_definition_path(
        &mut self,
        start: NamespaceId,
        path: &[SyntaxKeyExpr],
        line: usize,
    ) -> NamespaceId {
        let mut namespace = start;
        for key in path {
            let SyntaxKeyExpr::Atom(name) = key else {
                break;
            };
            namespace = self.introduce_name(namespace, name, line);
        }
        namespace
    }

    fn introduce_static_path(
        &mut self,
        start: NamespaceId,
        path: &str,
        line: usize,
    ) -> NamespaceId {
        let mut namespace = start;
        for name in path.split('.') {
            namespace = self.introduce_name(namespace, name, line);
        }
        namespace
    }

    fn introduce_name(&mut self, namespace: NamespaceId, name: &str, line: usize) -> NamespaceId {
        self.namespaces[namespace.0]
            .introduced
            .entry(name.to_owned())
            .or_insert(line);
        self.child_namespace(namespace, name)
    }

    fn ensure_static_path(&mut self, start: NamespaceId, path: &str) -> NamespaceId {
        path.split('.').fold(start, |namespace, name| {
            self.child_namespace(namespace, name)
        })
    }

    fn child_namespace(&mut self, parent: NamespaceId, name: &str) -> NamespaceId {
        if let Some(namespace) = self.namespaces[parent.0].children.get(name) {
            return *namespace;
        }
        let namespace = self.push_namespace();
        self.namespaces[parent.0]
            .children
            .insert(name.to_owned(), namespace);
        namespace
    }

    fn push_namespace(&mut self) -> NamespaceId {
        let namespace = NamespaceId(self.namespaces.len());
        self.namespaces.push(Namespace::default());
        namespace
    }

    fn push_scope(&mut self, scope: ResolutionScope) -> ResolutionScopeId {
        let id = ResolutionScopeId(self.scopes.len());
        self.scopes.push(scope);
        id
    }
}

#[derive(Debug, Clone, Copy)]
enum BodyScopeKind {
    Object,
    Dictionary,
}

struct BodyScopeSpec<'a> {
    alias: Option<&'a str>,
    line: usize,
    parent: ResolutionScopeId,
    namespace: NamespaceId,
    kind: BodyScopeKind,
}

fn fulfills_abstract_name(name: &str, unresolved: &mut Vec<String>) -> bool {
    let Some(canonical) = local_name_metadata(name).canonical else {
        return false;
    };
    let Some(index) = unresolved
        .iter()
        .rposition(|unresolved| unresolved == &canonical)
    else {
        return false;
    };
    unresolved.remove(index);
    true
}

pub(super) fn check_file_global_local_shadowing(declarations: &[Declaration]) -> Vec<Diagnostic> {
    FileNameAnalysis::new().analyze(declarations)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(name: &str, expr: SyntaxExpr, line: usize) -> Declaration {
        Declaration {
            line,
            preview: String::new(),
            kind: DeclarationKind::Definition(DefinitionDecl {
                target: vec![SyntaxKeyExpr::Atom(name.to_owned())],
                parameters: Vec::new(),
                kind: DefinitionKind::Introduce,
                expr: Some(expr),
            }),
        }
    }

    #[test]
    fn inaccessible_drop_binders_are_not_recorded() {
        let declarations = [
            definition("_", SyntaxExpr::Unit, 1),
            definition(
                "function",
                SyntaxExpr::Lambda(vec!["_".to_owned()], Box::new(SyntaxExpr::Unit)),
                2,
            ),
        ];

        assert_eq!(check_file_global_local_shadowing(&declarations), []);
    }
}

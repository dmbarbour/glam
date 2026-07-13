use std::sync::Arc;

use chumsky::prelude::*;

use crate::compiler::CompileContext;
use crate::core::Builtin;
use crate::core::{Atom, Dict, Expr as CoreExpr, Key, KeyExpr as CoreKeyExpr, Value};
use crate::diagnostic::Severity;
use crate::number::Number;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

impl SourceFile {
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: text.into(),
        }
    }

    pub fn parse(&self) -> ParsedSource {
        parse_source(self)
    }

    pub fn parse_with_context(&self, context: &CompileContext) -> ParsedSource {
        parse_source_with_context(self, context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSource {
    pub declarations: Vec<Declaration>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub line: usize,
    pub kind: DeclarationKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclarationKind {
    Language(LanguageDecl),
    Import(ImportDecl),
    Abstract(Vec<String>),
    Unique(Vec<String>),
    Object(ObjectDecl),
    Extend(ObjectExtendDecl),
    Definition(DefinitionDecl),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageDecl {
    pub base: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDecl {
    pub reference: ImportReference,
    pub binary: bool,
    pub placement: ImportPlacement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectDecl {
    pub target: String,
    pub alias: Option<String>,
    pub deps: Vec<String>,
    pub body: Vec<ObjectBodyDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectBodyDefinition {
    pub line: usize,
    pub text: String,
    pub kind: ObjectBodyDefinitionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectBodyDefinitionKind {
    Definition(DefinitionDecl),
    Object(ObjectDecl),
}

impl ObjectBodyDefinition {
    fn definition(&self) -> Option<&DefinitionDecl> {
        match &self.kind {
            ObjectBodyDefinitionKind::Definition(definition) => Some(definition),
            ObjectBodyDefinitionKind::Object(_) => None,
        }
    }

    fn object(&self) -> Option<&ObjectDecl> {
        match &self.kind {
            ObjectBodyDefinitionKind::Definition(_) => None,
            ObjectBodyDefinitionKind::Object(object) => Some(object),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectExtendDecl {
    pub target: String,
    pub alias: Option<String>,
    pub body: Vec<ObjectBodyDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectExpr {
    pub name: Option<Box<SyntaxExpr>>,
    pub alias: Option<String>,
    pub deps: Vec<SyntaxExpr>,
    pub body: Vec<ObjectBodyDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportReference {
    Local(String),
    Builtin(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportPlacement {
    Inline,
    As(String),
    At(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionDecl {
    pub target: String,
    pub kind: DefinitionKind,
    pub body: String,
    pub expr: Option<SyntaxExpr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionKind {
    Introduce,
    Override,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxExpr {
    Unit,
    Number(Number),
    Text(String),
    Atom(String),
    Effect(String),
    Name(String),
    PriorName(String),
    Escape(usize, Box<SyntaxExpr>),
    Access(Box<SyntaxExpr>, Vec<SyntaxKeyExpr>),
    Object(ObjectExpr),
    With {
        base: Box<SyntaxExpr>,
        alias: Option<String>,
        body: Vec<ObjectBodyDefinition>,
    },
    SingletonDict(SyntaxKeyExpr, Box<SyntaxExpr>),
    DictUnion(Vec<SyntaxExpr>),
    List(Vec<SyntaxExpr>),
    Lambda(Vec<String>, Box<SyntaxExpr>),
    Let {
        bindings: Vec<(String, SyntaxExpr)>,
        body: Box<SyntaxExpr>,
    },
    Apply(Box<SyntaxExpr>, Box<SyntaxExpr>),
    OperatorApply {
        operator: SyntaxOperator,
        left: Box<SyntaxExpr>,
        right: Box<SyntaxExpr>,
    },
    ComparisonChain {
        first: Box<SyntaxExpr>,
        rest: Vec<(SyntaxOperator, SyntaxExpr)>,
    },
    OperatorSection {
        operator: SyntaxOperator,
        left: Option<Box<SyntaxExpr>>,
        right: Option<Box<SyntaxExpr>>,
    },
    Multiply(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Divide(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Add(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Subtract(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Append(Box<SyntaxExpr>, Box<SyntaxExpr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxOperator {
    Builtin(Builtin),
    BoolAnd,
    BoolOr,
    PipeForward,
    PipeBackward,
    ComposeForward,
    ComposeBackward,
    EffectBind,
    KleisliCompose,
    EffectThen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxKeyExpr {
    Atom(String),
    Index(Box<SyntaxExpr>),
    PathIndex(Box<SyntaxExpr>),
}

fn is_comparison_operator(operator: SyntaxOperator) -> bool {
    matches!(
        operator,
        SyntaxOperator::Builtin(
            Builtin::Greater
                | Builtin::GreaterEqual
                | Builtin::Equal
                | Builtin::NotEqual
                | Builtin::LessEqual
                | Builtin::Less
        )
    )
}

#[derive(Debug, Clone)]
enum PathSuffix {
    Single(SyntaxKeyExpr),
    Expand(Vec<SyntaxKeyExpr>),
}

fn flatten_path_suffixes(suffixes: Vec<PathSuffix>) -> Vec<SyntaxKeyExpr> {
    let mut parts = Vec::new();
    for suffix in suffixes {
        match suffix {
            PathSuffix::Single(part) => parts.push(part),
            PathSuffix::Expand(items) => parts.extend(items),
        }
    }
    parts
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalName {
    raw: String,
    canonical: Option<String>,
    suppress_unused_warning: bool,
}

#[derive(Debug, Clone)]
struct NameScope {
    final_defs: Value,
    prior_defs: Value,
    module_final_defs: Value,
    module_prior_defs: Value,
    object_alias: Option<String>,
    object_final_defs: Option<Value>,
    object_prior_defs: Option<Value>,
    parent: Option<Box<NameScope>>,
}

impl NameScope {
    fn module(context: &CompileContext, visible_definitions: Value) -> Self {
        Self {
            final_defs: context.final_defs.clone(),
            prior_defs: visible_definitions.clone(),
            module_final_defs: context.final_defs.clone(),
            module_prior_defs: visible_definitions,
            object_alias: None,
            object_final_defs: None,
            object_prior_defs: None,
            parent: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSource {
    pub definitions: Value, // open fixpoint, i.e. \ self -> Dict
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub line: usize,
    pub message: String,
}

impl Diagnostic {
    fn warn(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            line,
            message: message.into(),
        }
    }

    fn error(line: usize, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            line,
            message: message.into(),
        }
    }
}

pub fn parse_source(source: &SourceFile) -> ParsedSource {
    let context = CompileContext::default().with_source_binary(source.text.as_bytes());
    parse_source_with_context(source, &context)
}

pub fn parse_source_with_context(source: &SourceFile, context: &CompileContext) -> ParsedSource {
    let text = match context.source_text(source.text.as_str()) {
        Ok(text) => text,
        Err(err) => {
            return ParsedSource {
                declarations: Vec::new(),
                diagnostics: vec![Diagnostic::error(
                    1,
                    format!("source is not valid UTF-8: {err}"),
                )],
            };
        }
    };

    let mut diagnostics = line_ending_diagnostics(text.as_ref());
    let physical_lines = split_lines(text.as_ref());
    let mut declarations = Vec::new();
    let mut index = 0;

    while index < physical_lines.len() {
        let line = physical_lines[index];
        let trimmed = strip_comment(line.text).trim();

        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        if is_indented(line.text) {
            diagnostics.push(Diagnostic::error(
                line.number,
                "continuation line without a preceding declaration",
            ));
            index += 1;
            continue;
        }

        let start_line = line.number;
        let mut text = String::from(trimmed);
        index += 1;
        let mut continuation_indent = None;

        while index < physical_lines.len() {
            let next = physical_lines[index];
            let next_trimmed = strip_comment(next.text).trim();

            if next_trimmed.is_empty() {
                index += 1;
                continue;
            }

            if !is_indented(next.text) && !is_dedent_closer(next_trimmed) {
                break;
            }

            if is_indented(next.text) && continuation_indent.is_none() {
                continuation_indent = Some(indentation_width(next.text));
            }

            let next_text = strip_comment(next.text).trim_end();
            let next_text = continuation_indent
                .map(|indent| strip_indent_width(next_text, indent))
                .unwrap_or(next_trimmed);
            text.push('\n');
            text.push_str(next_text.trim_end());
            index += 1;
        }

        declarations.push(Declaration {
            line: start_line,
            kind: classify_declaration(&text, start_line, &mut diagnostics),
            text,
        });
    }

    validate_language_position(&declarations, &mut diagnostics);

    ParsedSource {
        declarations,
        diagnostics,
    }
}

pub fn lower_to_core_with_context(
    parsed: &ParsedSource,
    context: &CompileContext,
) -> LoweredSource {
    // note: we'll extend 'prior' within the 'body' of an implicit lambda
    let mut definitions = context.prior_defs.clone();
    let mut diagnostics = parsed.diagnostics.clone();

    for declaration in &parsed.declarations {
        match &declaration.kind {
            DeclarationKind::Import(import) => {
                if let Err(diagnostic) =
                    lower_import(import, declaration.line, context, &mut definitions)
                {
                    diagnostics.push(diagnostic);
                }
            }
            DeclarationKind::Unique(names) => {
                if let Err(diagnostic) =
                    lower_unique(names, declaration.line, context, &mut definitions)
                {
                    diagnostics.push(diagnostic);
                }
            }
            DeclarationKind::Definition(definition) => {
                let scope = NameScope::module(context, definitions.clone());
                if let Err(diagnostic) = lower_definition(
                    definition,
                    declaration.text.as_str(),
                    declaration.line,
                    context,
                    &mut definitions,
                    &scope,
                ) {
                    diagnostics.push(diagnostic);
                }
            }
            DeclarationKind::Object(object) => {
                if let Err(diagnostic) =
                    lower_object(object, declaration.line, context, &mut definitions)
                {
                    diagnostics.push(diagnostic);
                }
            }
            DeclarationKind::Extend(extend) => {
                if let Err(diagnostic) =
                    lower_extend(extend, declaration.line, context, &mut definitions)
                {
                    diagnostics.push(diagnostic);
                }
            }
            _ => {}
        }
    }

    LoweredSource {
        definitions,
        diagnostics,
    }
}

fn lower_import(
    import: &ImportDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    match &import.reference {
        ImportReference::Builtin(name) => {
            if import.binary {
                return Err(Diagnostic::error(
                    line,
                    "built-in imports cannot use the `binary` modifier",
                ));
            }
            lower_builtin_import(name, &import.placement, line, context, definitions)
        }
        ImportReference::Local(reference) if import.binary => {
            lower_local_binary_import(reference, &import.placement, line, context, definitions)
        }
        ImportReference::Local(reference) => {
            lower_local_import(reference, &import.placement, context, definitions)
        }
    }
}

fn lower_builtin_import(
    name: &str,
    placement: &ImportPlacement,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let module = builtin_module_value(context, name)
        .ok_or_else(|| Diagnostic::error(line, format!("unknown built-in module `'{name}`")))?;

    *definitions = match placement {
        ImportPlacement::Inline => update_module_dict_value(definitions.clone(), module, context),
        ImportPlacement::As(target) => update_module_value(
            definitions.clone(),
            target,
            module_object_value(target, module, context),
            context,
        ),
        ImportPlacement::At(target) => {
            let object = extend_object_with_defs(
                target,
                constant_object_defs(module, context),
                context,
                definitions.clone(),
            )?;
            update_module_value(definitions.clone(), target, object, context)
        }
    };

    Ok(())
}

fn lower_local_import(
    reference: &str,
    placement: &ImportPlacement,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    match placement {
        ImportPlacement::Inline => {
            let args = context.local_module_load_args(
                reference,
                context.module_path.clone(),
                definitions.clone(),
                context.final_defs.clone(),
            );
            *definitions = context.value_load_local_module(args);
        }
        ImportPlacement::As(target) => {
            let prior_defs = import_as_prior_defs(target, definitions.clone(), context)?;
            let loaded = scoped_local_import_value(reference, target, prior_defs, context)?;
            *definitions = update_module_value(
                definitions.clone(),
                target,
                module_object_value(target, loaded, context),
                context,
            );
        }
        ImportPlacement::At(target) => {
            let scoped_prior = path_value_in_definitions(target, definitions.clone(), context)?;
            let loaded = scoped_local_import_value(reference, target, scoped_prior, context)?;
            let object = extend_object_with_defs(
                target,
                constant_object_defs(loaded, context),
                context,
                definitions.clone(),
            )?;
            *definitions = update_module_value(definitions.clone(), target, object, context);
        }
    };

    Ok(())
}

fn lower_local_binary_import(
    reference: &str,
    placement: &ImportPlacement,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let ImportPlacement::As(target) = placement else {
        return Err(Diagnostic::error(
            line,
            "`import ... binary` requires `as name` in the current spike",
        ));
    };

    let loaded = context.value_load_local_binary(context.local_binary_load_args(reference));
    *definitions = update_module_value(definitions.clone(), target, loaded, context);
    Ok(())
}

fn scoped_local_import_value(
    reference: &str,
    target: &str,
    prior_defs: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let final_defs = path_value_in_definitions(target, context.final_defs.clone(), context)?;
    let args = context.local_module_load_args(
        reference,
        scoped_module_path(context, target),
        prior_defs,
        final_defs,
    );
    Ok(context.value_load_local_module(args))
}

fn import_as_prior_defs(
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let env = inherited_import_env_object_value(target, definitions, context)?;
    Ok(update_module_value(
        context.empty_dict_value(),
        "env",
        env,
        context,
    ))
}

fn inherited_import_env_object_value(
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let parent_env = path_value_in_definitions("env", definitions, context)?;
    let name = context.abstract_global_path_value(
        context
            .abstract_global_path(&format!("{target}.env"))
            .as_ref(),
    );
    let deps = context.value_list(vec![object_spec_value(parent_env, context)]);
    Ok(object_instance_from_parts_value(
        name,
        deps,
        empty_object_defs(context),
        context,
    ))
}

fn empty_object_defs(context: &CompileContext) -> Value {
    context.value_lambda(
        context.value_lambda(remove_object_spec_value(context.value_local(1), context)),
    )
}

fn module_object_value(target: &str, module: Value, context: &CompileContext) -> Value {
    let spec = Dict::new_sync()
        .insert(
            name_as_key("name"),
            context.abstract_global_path_value(context.abstract_global_path(target).as_ref()),
        )
        .insert(name_as_key("deps"), context.value_list(Vec::new()))
        .insert(name_as_key("defs"), constant_object_defs(module, context));

    context.value_apply(
        context.value_builtin(Builtin::ObjectInstance),
        context.value_dict(spec),
    )
}

fn constant_object_defs(value: Value, context: &CompileContext) -> Value {
    context.value_lambda(context.value_lambda(value))
}

fn lower_unique(
    names: &[String],
    _line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    for name in names {
        let path = context.abstract_global_path(name);
        let value = context.abstract_global_path_value(path.as_ref());
        *definitions = update_module_value(definitions.clone(), name, value, context);
    }
    Ok(())
}

fn builtin_module_value(context: &CompileContext, name: &str) -> Option<Value> {
    match name {
        "math" => Some(context.value_dict(builtin_math_module(context))),
        "list" => Some(context.value_dict(builtin_list_module(context))),
        "std" | "prelude" => Some(context.value_dict(builtin_std_module(context))),
        _ => None,
    }
}

fn builtin_math_module(context: &CompileContext) -> Dict {
    Dict::new_sync()
        .insert(name_as_key("floor"), context.value_builtin(Builtin::Floor))
        .insert(name_as_key("mod"), context.value_builtin(Builtin::Mod))
}

fn builtin_list_module(context: &CompileContext) -> Dict {
    Dict::new_sync()
        .insert(name_as_key("slice"), context.value_builtin(Builtin::Slice))
        .insert(
            name_as_key("split"),
            context.value_builtin(Builtin::ListSplit),
        )
        .insert(
            name_as_key("split_end"),
            context.value_builtin(Builtin::ListSplitEnd),
        )
        .insert(name_as_key("map"), context.value_builtin(Builtin::Map))
        .insert(name_as_key("len"), context.value_builtin(Builtin::ListLen))
        .insert(
            name_as_key("head"),
            context.value_builtin(Builtin::ListHead),
        )
        .insert(
            name_as_key("tail"),
            context.value_builtin(Builtin::ListTail),
        )
        .insert(
            name_as_key("pure"),
            context.value_builtin(Builtin::ListEffect),
        )
}

fn builtin_std_module(context: &CompileContext) -> Dict {
    Dict::new_sync()
        .insert(name_as_key("anno"), context.value_builtin(Builtin::Anno))
        .insert(name_as_key("not"), builtin_not_value(context))
        .insert(name_as_key("could"), builtin_could_value(context))
        .insert(
            name_as_key("math"),
            context.value_dict(builtin_math_module(context)),
        )
        .insert(
            name_as_key("list"),
            context.value_dict(builtin_list_module(context)),
        )
}

fn builtin_not_value(context: &CompileContext) -> Value {
    let condition = context.value_local(0);
    let fail_operation = lower_effect_expr("fail", context);
    let true_operation = effect_return_value(context.unit_value(), context);
    let fail_if_condition_succeeds = effect_then_values(
        condition,
        effect_return_value(fail_operation, context),
        context,
    );
    let succeed_if_condition_fails = effect_return_value(true_operation, context);
    let select_operation = effect_call_value(
        "cut",
        vec![effect_call_value(
            "alt",
            vec![fail_if_condition_succeeds, succeed_if_condition_fails],
            context,
        )],
        context,
    );
    let run_selected_operation = context.value_lambda(context.value_local(0));
    context.value_lambda(effect_call_value(
        "seq",
        vec![select_operation, run_selected_operation],
        context,
    ))
}

fn builtin_could_value(context: &CompileContext) -> Value {
    let not = builtin_not_value(context);
    let condition = context.value_local(0);
    context.value_lambda(context.value_apply(not.clone(), context.value_apply(not, condition)))
}

fn effect_return_value(value: Value, context: &CompileContext) -> Value {
    context.value_apply(lower_effect_expr("r", context), value)
}

fn lower_definition(
    definition: &DefinitionDecl,
    declaration_text: &str,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
    scope: &NameScope,
) -> Result<(), Diagnostic> {
    lower_definition_with_locals(
        definition,
        declaration_text,
        line,
        context,
        definitions,
        scope,
        &mut Vec::new(),
    )
}

fn lower_definition_with_locals(
    definition: &DefinitionDecl,
    declaration_text: &str,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<(), Diagnostic> {
    let Some(expr) = &definition.expr else {
        return Ok(());
    };

    let value = syntax_expr_to_value_in_scope(expr, line, context, scope, locals)?;
    let target_scope = definition_target_scope(scope, definitions.clone());
    let value = match definition.kind {
        DefinitionKind::Introduce => annotate_definition_value(
            BuiltinAssertion::Undefined,
            &definition.target,
            definitions.clone(),
            value,
            line,
            context,
            &target_scope,
            locals,
        )?,
        DefinitionKind::Override => annotate_definition_value(
            BuiltinAssertion::Defined,
            &definition.target,
            definitions.clone(),
            value,
            line,
            context,
            &target_scope,
            locals,
        )?,
        DefinitionKind::Update => lower_update_definition(
            &definition.target,
            definitions.clone(),
            value,
            definition_param_count(definition, declaration_text, line)?,
            line,
            context,
            &target_scope,
            locals,
        )?,
    };
    *definitions = update_definition_target_value(
        definitions.clone(),
        &definition.target,
        value,
        line,
        context,
        &target_scope,
        locals,
    )?;

    Ok(())
}

fn definition_target_scope(scope: &NameScope, visible_definitions: Value) -> NameScope {
    if scope.object_final_defs.is_some() {
        return scope.clone();
    }

    let mut scope = scope.clone();
    scope.final_defs = visible_definitions.clone();
    scope.prior_defs = visible_definitions.clone();
    scope.module_final_defs = visible_definitions.clone();
    scope.module_prior_defs = visible_definitions;
    scope
}

fn lower_object(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let object_value = object_decl_value(object, line, context, definitions.clone())?;
    let scope = NameScope::module(context, definitions.clone());
    let mut locals = Vec::new();
    let object_value = annotate_definition_value(
        BuiltinAssertion::Undefined,
        &object.target,
        definitions.clone(),
        object_value,
        line,
        context,
        &scope,
        &mut locals,
    )?;

    *definitions = update_module_value(definitions.clone(), &object.target, object_value, context);
    Ok(())
}

fn object_decl_value(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    visible_definitions: Value,
) -> Result<Value, Diagnostic> {
    let parent_scope = NameScope::module(context, visible_definitions.clone());
    let name =
        context.abstract_global_path_value(context.abstract_global_path(&object.target).as_ref());
    object_decl_value_in_scope(object, line, context, parent_scope, &mut Vec::new(), name)
}

fn object_decl_value_in_scope(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    parent_scope: NameScope,
    locals: &mut Vec<LocalName>,
    name: Value,
) -> Result<Value, Diagnostic> {
    let body_parent_scope = shift_name_scope_locals(&parent_scope, 2);
    let defs = object_body_defs_value_in_scope(
        &object.body,
        object.alias.as_deref(),
        line,
        context,
        body_parent_scope,
        locals,
    )?;
    let deps = object
        .deps
        .iter()
        .map(|dep| {
            let dep_object = path_value_in_scope(dep, line, context, &parent_scope, locals);
            Ok(object_spec_value(dep_object, context))
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    Ok(object_instance_from_parts_value(
        name,
        context.value_list(deps),
        defs,
        context,
    ))
}

fn shift_name_scope_locals(scope: &NameScope, amount: usize) -> NameScope {
    NameScope {
        final_defs: shift_value_locals(&scope.final_defs, amount, 0),
        prior_defs: shift_value_locals(&scope.prior_defs, amount, 0),
        module_final_defs: shift_value_locals(&scope.module_final_defs, amount, 0),
        module_prior_defs: shift_value_locals(&scope.module_prior_defs, amount, 0),
        object_alias: scope.object_alias.clone(),
        object_final_defs: scope
            .object_final_defs
            .as_ref()
            .map(|value| shift_value_locals(value, amount, 0)),
        object_prior_defs: scope
            .object_prior_defs
            .as_ref()
            .map(|value| shift_value_locals(value, amount, 0)),
        parent: scope
            .parent
            .as_ref()
            .map(|parent| Box::new(shift_name_scope_locals(parent, amount))),
    }
}

fn shift_value_locals(value: &Value, amount: usize, cutoff: usize) -> Value {
    match value {
        Value::Expr(thunk) if thunk.env.is_empty() => {
            Value::expr(shift_expr_locals(thunk.expr.as_ref(), amount, cutoff))
        }
        other => other.clone(),
    }
}

fn shift_expr_locals(expr: &CoreExpr, amount: usize, cutoff: usize) -> CoreExpr {
    match expr {
        CoreExpr::Value(value) => CoreExpr::Value(shift_value_locals(value, amount, cutoff)),
        CoreExpr::List(items) => CoreExpr::List(Arc::from(
            items
                .iter()
                .map(|item| Arc::new(shift_expr_locals(item, amount, cutoff)))
                .collect::<Vec<_>>(),
        )),
        CoreExpr::Apply(function, argument) => CoreExpr::Apply(
            Arc::new(shift_expr_locals(function, amount, cutoff)),
            Arc::new(shift_expr_locals(argument, amount, cutoff)),
        ),
        CoreExpr::Lambda(body) => {
            CoreExpr::Lambda(Arc::new(shift_expr_locals(body, amount, cutoff + 1)))
        }
        CoreExpr::Local(index) if *index >= cutoff => CoreExpr::Local(index + amount),
        CoreExpr::Local(index) => CoreExpr::Local(*index),
        CoreExpr::Access(base, path) => CoreExpr::Access(
            Arc::new(shift_expr_locals(base, amount, cutoff)),
            Arc::from(
                path.iter()
                    .map(|key| shift_key_expr_locals(key, amount, cutoff))
                    .collect::<Vec<_>>(),
            ),
        ),
        CoreExpr::Future(ivar) => CoreExpr::Future(ivar.clone()),
        CoreExpr::Deferred(deferred) => CoreExpr::Deferred(deferred.clone()),
        CoreExpr::Error(message) => CoreExpr::Error(message.clone()),
    }
}

fn shift_key_expr_locals(key: &CoreKeyExpr, amount: usize, cutoff: usize) -> CoreKeyExpr {
    match key {
        CoreKeyExpr::Key(key) => CoreKeyExpr::Key(key.clone()),
        CoreKeyExpr::Index(expr) => {
            CoreKeyExpr::Index(Arc::new(shift_expr_locals(expr, amount, cutoff)))
        }
        CoreKeyExpr::PathIndex(expr) => {
            CoreKeyExpr::PathIndex(Arc::new(shift_expr_locals(expr, amount, cutoff)))
        }
    }
}

fn object_spec_value(value: Value, context: &CompileContext) -> Value {
    context.value_apply(context.value_builtin(Builtin::ObjectSpec), value)
}

fn object_instance_from_parts_value(
    name: Value,
    deps: Value,
    defs: Value,
    context: &CompileContext,
) -> Value {
    context.builtin_apply3_value(Builtin::ObjectInstanceFromParts, name, deps, defs)
}

fn object_body_defs_value(
    body: &[ObjectBodyDefinition],
    alias: Option<&str>,
    line: usize,
    context: &CompileContext,
    parent_scope: NameScope,
) -> Result<Value, Diagnostic> {
    object_body_defs_value_in_scope(body, alias, line, context, parent_scope, &mut Vec::new())
}

fn object_body_defs_value_in_scope(
    body: &[ObjectBodyDefinition],
    alias: Option<&str>,
    _line: usize,
    context: &CompileContext,
    parent_scope: NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let mut definitions = remove_object_spec_value(context.value_local(1), context);
    let object_final_defs = context.value_local(0);

    for body_definition in body {
        let scope = object_body_scope(
            alias,
            object_final_defs.clone(),
            definitions.clone(),
            parent_scope.clone(),
        );
        lower_object_body_item(body_definition, context, &mut definitions, &scope, locals)?;
        definitions = remove_object_spec_value(definitions, context);
    }

    Ok(context.value_lambda(context.value_lambda(definitions)))
}

fn lower_object_body_item(
    item: &ObjectBodyDefinition,
    context: &CompileContext,
    definitions: &mut Value,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<(), Diagnostic> {
    match &item.kind {
        ObjectBodyDefinitionKind::Definition(definition) => lower_definition_with_locals(
            definition,
            item.text.as_str(),
            item.line,
            context,
            definitions,
            scope,
            locals,
        ),
        ObjectBodyDefinitionKind::Object(object) => {
            lower_nested_object(object, item.line, context, definitions, scope, locals)
        }
    }
}

fn lower_nested_object(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<(), Diagnostic> {
    let name = hierarchical_object_name_value(&object.target, line, context, scope)?;
    let object_value =
        object_decl_value_in_scope(object, line, context, scope.clone(), locals, name)?;
    let object_value = annotate_definition_value(
        BuiltinAssertion::Undefined,
        &object.target,
        definitions.clone(),
        object_value,
        line,
        context,
        scope,
        locals,
    )?;

    *definitions = update_module_value(definitions.clone(), &object.target, object_value, context);
    Ok(())
}

fn hierarchical_object_name_value(
    target: &str,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
) -> Result<Value, Diagnostic> {
    let Some(host) = &scope.object_final_defs else {
        return Err(Diagnostic::error(
            line,
            "nested object declaration requires an object scope",
        ));
    };
    let parts = context.value_list(
        target
            .split('.')
            .map(|part| context.value_atom(atom_from_str(part)))
            .collect(),
    );
    Ok(context.builtin_apply2_value(Builtin::ObjectLocalName, host.clone(), parts))
}

fn remove_object_spec_value(value: Value, context: &CompileContext) -> Value {
    context.builtin_apply3_value(
        Builtin::DictUpdate,
        path_value("spec", context),
        context.empty_dict_value(),
        value,
    )
}

fn object_body_scope(
    alias: Option<&str>,
    object_final_defs: Value,
    object_prior_defs: Value,
    parent: NameScope,
) -> NameScope {
    let object_alias = alias
        .map(local_name_metadata)
        .and_then(|alias| alias.canonical);
    let (final_defs, prior_defs) = if object_alias.is_some() {
        (parent.final_defs.clone(), parent.prior_defs.clone())
    } else {
        (object_final_defs.clone(), object_prior_defs.clone())
    };

    NameScope {
        final_defs,
        prior_defs,
        module_final_defs: parent.module_final_defs.clone(),
        module_prior_defs: parent.module_prior_defs.clone(),
        object_alias,
        object_final_defs: Some(object_final_defs),
        object_prior_defs: Some(object_prior_defs),
        parent: Some(Box::new(parent)),
    }
}

fn lower_extend(
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let object_value = extend_object_value(extend, line, context, definitions.clone())?;
    let scope = NameScope::module(context, definitions.clone());
    let mut locals = Vec::new();
    let object_value = annotate_definition_value(
        BuiltinAssertion::Defined,
        &extend.target,
        definitions.clone(),
        object_value,
        line,
        context,
        &scope,
        &mut locals,
    )?;

    *definitions = update_module_value(definitions.clone(), &extend.target, object_value, context);
    Ok(())
}

fn extend_object_value(
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    visible_definitions: Value,
) -> Result<Value, Diagnostic> {
    let extension_defs = object_body_defs_value(
        &extend.body,
        extend.alias.as_deref(),
        line,
        context,
        NameScope::module(context, visible_definitions.clone()),
    )?;
    extend_object_with_defs(&extend.target, extension_defs, context, visible_definitions)
}

fn extend_object_with_defs(
    target: &str,
    extension_defs: Value,
    context: &CompileContext,
    visible_definitions: Value,
) -> Result<Value, Diagnostic> {
    let prior_object = path_value_in_definitions(target, visible_definitions, context)?;
    let prior_spec = context.value_access(
        prior_object,
        vec![context.key_expr_key(name_as_key("spec"))],
    );
    let prior_defs = context.value_access(
        prior_spec.clone(),
        vec![context.key_expr_key(name_as_key("defs"))],
    );
    let spec = Dict::new_sync()
        .insert(
            name_as_key("name"),
            context.value_access(
                prior_spec.clone(),
                vec![context.key_expr_key(name_as_key("name"))],
            ),
        )
        .insert(
            name_as_key("deps"),
            context.value_access(prior_spec, vec![context.key_expr_key(name_as_key("deps"))]),
        )
        .insert(
            name_as_key("defs"),
            compose_object_defs(prior_defs, extension_defs, context),
        );

    Ok(context.value_apply(
        context.value_builtin(Builtin::ObjectInstance),
        context.value_dict(spec),
    ))
}

fn compose_object_defs(
    prior_defs: Value,
    extension_defs: Value,
    context: &CompileContext,
) -> Value {
    let prior_self = context.value_apply(
        context.value_apply(prior_defs, context.value_local(1)),
        context.value_local(0),
    );
    let body = context.value_apply(
        context.value_apply(extension_defs, prior_self),
        context.value_local(0),
    );
    context.value_lambda(context.value_lambda(body))
}

fn lower_update_definition(
    target: &str,
    visible_definitions: Value,
    update: Value,
    sugar_param_count: usize,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let prior =
        definition_target_access_value(target, visible_definitions, line, context, scope, locals)?;
    let mut lowered = update;

    for _ in 0..sugar_param_count {
        let Some(body) = context.value_lambda_body(&lowered) else {
            return Err(Diagnostic::error(
                line,
                "internal error lowering update definition arguments",
            ));
        };
        lowered = body;
    }

    lowered = context.value_apply(lowered, prior);

    for _ in 0..sugar_param_count {
        lowered = context.value_lambda(lowered);
    }

    Ok(lowered)
}

#[derive(Clone, Copy)]
enum BuiltinAssertion {
    Defined,
    Undefined,
}

fn annotate_definition_value(
    assertion: BuiltinAssertion,
    target: &str,
    visible_definitions: Value,
    value: Value,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let tag = match assertion {
        BuiltinAssertion::Defined => "assert_defined",
        BuiltinAssertion::Undefined => "assert_undefined",
    };
    let payload = context.builtin_apply2_value(
        Builtin::DictUnion,
        context.builtin_apply2_value(
            Builtin::DictSingleton,
            context.value_atom(atom_from_str("name")),
            context.value_binary(target),
        ),
        context.builtin_apply2_value(
            Builtin::DictSingleton,
            context.value_atom(atom_from_str("value")),
            definition_target_access_value(
                target,
                visible_definitions,
                line,
                context,
                scope,
                locals,
            )?,
        ),
    );
    let annotation = context.builtin_apply2_value(
        Builtin::DictSingleton,
        context.value_atom(atom_from_str(tag)),
        payload,
    );

    Ok(context.builtin_apply2_value(Builtin::Anno, annotation, value))
}

fn update_module_value(
    definitions: Value,
    target: &str,
    value: Value,
    context: &CompileContext,
) -> Value {
    // Module definitions are ordered updates over the incoming namespace.
    // Ordinary dictionary literals still lower through DictUnion.
    context.builtin_apply3_value(
        Builtin::DictUpdate,
        path_value(target, context),
        value,
        definitions,
    )
}

fn update_definition_target_value(
    definitions: Value,
    target: &str,
    value: Value,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    Ok(context.builtin_apply3_value(
        Builtin::DictUpdate,
        definition_target_path_value(target, line, context, scope, locals)?,
        value,
        definitions,
    ))
}

fn update_module_dict_value(definitions: Value, item: Value, context: &CompileContext) -> Value {
    match item {
        Value::Dict(dict) => update_module_dict_entries(definitions, Vec::new(), &dict, context),
        _ => definitions,
    }
}

fn update_module_dict_entries(
    definitions: Value,
    prefix: Vec<Value>,
    dict: &Dict,
    context: &CompileContext,
) -> Value {
    dict.iter().fold(definitions, |definitions, (key, value)| {
        let mut path = prefix.clone();
        path.push(key_to_value(key, context));
        match value {
            Value::Dict(nested) if !nested.is_empty() => {
                update_module_dict_entries(definitions, path, nested, context)
            }
            _ => context.builtin_apply3_value(
                Builtin::DictUpdate,
                context.value_list(path),
                value.clone(),
                definitions,
            ),
        }
    })
}

fn path_value(target: &str, context: &CompileContext) -> Value {
    context.value_list(
        target
            .split('.')
            .map(|part| context.value_atom(atom_from_str(part)))
            .collect(),
    )
}

fn definition_target_parts(target: &str, line: usize) -> Result<Vec<SyntaxKeyExpr>, Diagnostic> {
    definition_target_path()
        .then_ignore(end())
        .parse(target)
        .into_result()
        .map_err(|errors| {
            Diagnostic::error(
                line,
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })
}

fn definition_target_access_value(
    target: &str,
    definitions: Value,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let path = definition_target_parts(target, line)?
        .iter()
        .map(|part| syntax_key_expr_to_core(part, line, context, scope, locals))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(context.value_access(definitions, path))
}

fn definition_target_path_value(
    target: &str,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let parts = definition_target_parts(target, line)?;
    syntax_path_value(&parts, line, context, scope, locals)
}

fn syntax_path_value(
    parts: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let mut result: Option<Value> = None;
    let mut pending = Vec::new();

    for part in parts {
        match part {
            SyntaxKeyExpr::PathIndex(expr) => {
                result = append_path_segments(
                    result,
                    std::mem::take(&mut pending),
                    syntax_expr_to_value_in_scope(expr, line, context, scope, locals)?,
                    context,
                );
            }
            SyntaxKeyExpr::Atom(name) => pending.push(context.value_atom(atom_from_str(name))),
            SyntaxKeyExpr::Index(expr) => {
                pending.push(syntax_expr_to_value_in_scope(
                    expr, line, context, scope, locals,
                )?);
            }
        }
    }

    Ok(match result {
        Some(result) => {
            let tail = context.value_list(pending);
            context.builtin_apply2_value(Builtin::Append, result, tail)
        }
        None => context.value_list(pending),
    })
}

fn append_path_segments(
    result: Option<Value>,
    pending: Vec<Value>,
    splice: Value,
    context: &CompileContext,
) -> Option<Value> {
    let prefix = context.value_list(pending);
    let combined = match result {
        Some(result) => context.builtin_apply2_value(Builtin::Append, result, prefix),
        None => prefix,
    };
    Some(context.builtin_apply2_value(Builtin::Append, combined, splice))
}

fn key_to_value(key: &Key, context: &CompileContext) -> Value {
    match key {
        Key::Atom(atom) => context.value_atom(*atom),
        Key::Number(number) => context.value_number(number.clone()),
        Key::Binary(bytes) => Value::Binary(bytes.clone()),
        Key::AbstractGlobalPath(parts) => {
            context.value_atom(Atom::from_key(&Key::AbstractGlobalPath(parts.clone())))
        }
        Key::List(items) => context.value_list(
            items
                .iter()
                .map(|item| key_to_value(item, context))
                .collect(),
        ),
        Key::Dict(entries) => {
            context.value_dict(entries.iter().fold(Dict::new_sync(), |dict, (key, value)| {
                dict.insert(key.clone(), key_to_value(value, context))
            }))
        }
    }
}

fn path_value_in_definitions(
    target: &str,
    definitions: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let path = target
        .split('.')
        .map(|part| context.key_expr_key(name_as_key(part)))
        .collect::<Vec<_>>();
    Ok(context.value_access(definitions, path))
}

fn path_value_in_scope(
    target: &str,
    _line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Value {
    let mut parts = target.split('.');
    let Some(first) = parts.next() else {
        return context.empty_dict_value();
    };
    let mut value = lower_name_expr(first, context, scope, locals);
    let path = parts
        .map(|part| context.key_expr_key(name_as_key(part)))
        .collect::<Vec<_>>();
    if !path.is_empty() {
        value = context.value_access(value, path);
    }
    value
}

fn scoped_module_path(context: &CompileContext, target: &str) -> std::sync::Arc<[String]> {
    let mut parts = context.module_path.iter().cloned().collect::<Vec<_>>();
    parts.extend(target.split('.').map(ToOwned::to_owned));
    std::sync::Arc::from(parts.into_boxed_slice())
}

fn definition_param_count(
    definition: &DefinitionDecl,
    declaration_text: &str,
    line: usize,
) -> Result<usize, Diagnostic> {
    let operator = match definition.kind {
        DefinitionKind::Introduce => "=",
        DefinitionKind::Override => ":=",
        DefinitionKind::Update => "::=",
    };
    let suffix = declaration_text
        .strip_prefix(definition.target.as_str())
        .ok_or_else(|| {
            Diagnostic::error(line, "internal error extracting definition parameters")
        })?;
    let (params, _) = suffix.split_once(operator).ok_or_else(|| {
        Diagnostic::error(line, "internal error extracting definition parameters")
    })?;
    Ok(params.split_whitespace().count())
}

fn syntax_expr_to_value_in_scope(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    Ok(match expr {
        SyntaxExpr::Unit => context.unit_value(),
        SyntaxExpr::Number(number) => context.value_number(number.clone()),
        SyntaxExpr::Text(text) => context.value_binary(text),
        SyntaxExpr::Atom(name) => context.value_atom(atom_from_str(name)),
        SyntaxExpr::Effect(name) => lower_effect_expr(name, context),
        SyntaxExpr::SingletonDict(key, value) => context.builtin_apply2_value(
            Builtin::DictSingleton,
            syntax_key_expr_to_value(key, line, context, scope, locals)?,
            syntax_expr_to_value_in_scope(value, line, context, scope, locals)?,
        ),
        SyntaxExpr::DictUnion(items) => lower_dict_union(items, line, context, scope, locals)?,
        SyntaxExpr::Name(name) => lower_name_expr(name, context, scope, locals),
        SyntaxExpr::PriorName(name) => lower_prior_name_expr(name, line, context, scope)?,
        SyntaxExpr::Escape(depth, expr) => {
            let escaped_scope = escaped_name_scope(scope, *depth, line)?;
            syntax_expr_to_value_in_scope(expr, line, context, &escaped_scope, locals)?
        }
        SyntaxExpr::Access(base, parts) => context.value_access(
            syntax_expr_to_value_in_scope(base, line, context, scope, locals)?,
            parts
                .iter()
                .map(|part| syntax_key_expr_to_core(part, line, context, scope, locals))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SyntaxExpr::Object(object) => lower_object_expr(object, line, context, scope, locals)?,
        SyntaxExpr::With { base, alias, body } => {
            lower_dict_with_expr(base, alias.as_deref(), body, line, context, scope, locals)?
        }
        SyntaxExpr::List(items) => context.value_list(
            items
                .iter()
                .map(|expr| syntax_expr_to_value_in_scope(expr, line, context, scope, locals))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SyntaxExpr::Lambda(params, body) => {
            lower_lambda_expr(params, body, line, context, scope, locals)?
        }
        SyntaxExpr::Let { bindings, body } => {
            lower_let_expr(bindings, body, line, context, scope, locals)?
        }
        SyntaxExpr::Apply(function, argument) => context.value_apply(
            syntax_expr_to_value_in_scope(function, line, context, scope, locals)?,
            syntax_expr_to_value_in_scope(argument, line, context, scope, locals)?,
        ),
        SyntaxExpr::OperatorApply {
            operator,
            left,
            right,
        } => lower_syntax_operator_expr(*operator, left, right, line, context, scope, locals)?,
        SyntaxExpr::ComparisonChain { first, rest } => {
            lower_comparison_chain(first, rest, line, context, scope, locals)?
        }
        SyntaxExpr::OperatorSection {
            operator,
            left,
            right,
        } => lower_operator_section(*operator, left, right, line, context, scope, locals)?,
        SyntaxExpr::Multiply(left, right) => {
            lower_builtin_expr(Builtin::Multiply, left, right, line, context, scope, locals)?
        }
        SyntaxExpr::Divide(left, right) => {
            lower_builtin_expr(Builtin::Divide, left, right, line, context, scope, locals)?
        }
        SyntaxExpr::Add(left, right) => {
            lower_builtin_expr(Builtin::Add, left, right, line, context, scope, locals)?
        }
        SyntaxExpr::Subtract(left, right) => {
            lower_builtin_expr(Builtin::Subtract, left, right, line, context, scope, locals)?
        }
        SyntaxExpr::Append(left, right) => {
            lower_builtin_expr(Builtin::Append, left, right, line, context, scope, locals)?
        }
    })
}

fn lower_object_expr(
    object: &ObjectExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let name = match &object.name {
        Some(name) => syntax_expr_to_value_in_scope(name, line, context, scope, locals)?,
        None => context.empty_dict_value(),
    };
    let deps = object
        .deps
        .iter()
        .map(|dep| {
            let dep_object = syntax_expr_to_value_in_scope(dep, line, context, scope, locals)?;
            Ok(object_spec_value(dep_object, context))
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    let defs = object_body_defs_value_in_scope(
        &object.body,
        object.alias.as_deref(),
        line,
        context,
        scope.clone(),
        locals,
    )?;
    Ok(object_instance_from_parts_value(
        name,
        context.value_list(deps),
        defs,
        context,
    ))
}

fn lower_dict_with_expr(
    base: &SyntaxExpr,
    alias: Option<&str>,
    body: &[ObjectBodyDefinition],
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let prior_defs = syntax_expr_to_value_in_scope(base, line, context, scope, locals)?;
    let final_defs = context.value_local(0);
    let mut definitions = prior_defs.clone();

    for body_definition in body {
        let body_scope = dict_with_body_scope(
            alias,
            final_defs.clone(),
            definitions.clone(),
            scope.clone(),
        );
        lower_object_body_item(
            body_definition,
            context,
            &mut definitions,
            &body_scope,
            locals,
        )?;
    }

    Ok(context.value_apply(
        context.value_builtin(Builtin::Fixpoint),
        context.value_lambda(definitions),
    ))
}

fn dict_with_body_scope(
    alias: Option<&str>,
    dict_final_defs: Value,
    dict_prior_defs: Value,
    parent: NameScope,
) -> NameScope {
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
        parent: Some(Box::new(parent)),
    }
}

fn lower_builtin_expr(
    builtin: Builtin,
    left: &SyntaxExpr,
    right: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    Ok(context.builtin_apply2_value(
        builtin,
        syntax_expr_to_value_in_scope(left, line, context, scope, locals)?,
        syntax_expr_to_value_in_scope(right, line, context, scope, locals)?,
    ))
}

fn lower_effect_expr(name: &str, context: &CompileContext) -> Value {
    let body = context.value_access(
        context.value_local(0),
        vec![context.key_expr_key(Key::atom_from_text(name))],
    );
    context.builtin_apply2_value(
        Builtin::DictSingleton,
        context.value_atom(atom_from_str("eff")),
        context.value_lambda(body),
    )
}

fn lower_operator_section(
    operator: SyntaxOperator,
    left: &Option<Box<SyntaxExpr>>,
    right: &Option<Box<SyntaxExpr>>,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    match (left, right) {
        (None, None) => return Ok(lower_syntax_operator_function(operator, context)),
        (Some(left), Some(right)) => {
            return lower_syntax_operator_expr(operator, left, right, line, context, scope, locals);
        }
        _ => {}
    }

    let shifted_scope = shift_name_scope_locals(scope, 1);
    let base_len = locals.len();
    locals.push(LocalName {
        raw: "<operator-section>".to_owned(),
        canonical: None,
        suppress_unused_warning: true,
    });
    let lowered_left = match left {
        Some(expr) => {
            match syntax_expr_to_value_in_scope(expr, line, context, &shifted_scope, locals) {
                Ok(value) => Some(value),
                Err(err) => {
                    locals.truncate(base_len);
                    return Err(err);
                }
            }
        }
        None => None,
    };
    let lowered_right = match right {
        Some(expr) => {
            match syntax_expr_to_value_in_scope(expr, line, context, &shifted_scope, locals) {
                Ok(value) => Some(value),
                Err(err) => {
                    locals.truncate(base_len);
                    return Err(err);
                }
            }
        }
        None => None,
    };
    locals.truncate(base_len);
    let section_arg = context.value_local(0);
    let body = match (lowered_left, lowered_right) {
        (None, Some(right)) => lower_syntax_operator_values(operator, section_arg, right, context),
        (Some(left), None) => lower_syntax_operator_values(operator, left, section_arg, context),
        _ => unreachable!("operator section arity was handled before lowering operands"),
    };
    Ok(context.value_lambda(body))
}

fn lower_syntax_operator_expr(
    operator: SyntaxOperator,
    left: &SyntaxExpr,
    right: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    match operator {
        SyntaxOperator::Builtin(builtin) => {
            lower_builtin_expr(builtin, left, right, line, context, scope, locals)
        }
        SyntaxOperator::PipeForward
        | SyntaxOperator::PipeBackward
        | SyntaxOperator::BoolAnd
        | SyntaxOperator::BoolOr
        | SyntaxOperator::EffectBind
        | SyntaxOperator::KleisliCompose
        | SyntaxOperator::EffectThen => Ok(lower_syntax_operator_values(
            operator,
            syntax_expr_to_value_in_scope(left, line, context, scope, locals)?,
            syntax_expr_to_value_in_scope(right, line, context, scope, locals)?,
            context,
        )),
        SyntaxOperator::ComposeForward | SyntaxOperator::ComposeBackward => {
            let shifted_scope = shift_name_scope_locals(scope, 1);
            let base_len = locals.len();
            locals.push(LocalName {
                raw: "<composition>".to_owned(),
                canonical: None,
                suppress_unused_warning: true,
            });
            let left =
                match syntax_expr_to_value_in_scope(left, line, context, &shifted_scope, locals) {
                    Ok(value) => value,
                    Err(err) => {
                        locals.truncate(base_len);
                        return Err(err);
                    }
                };
            let right =
                match syntax_expr_to_value_in_scope(right, line, context, &shifted_scope, locals) {
                    Ok(value) => value,
                    Err(err) => {
                        locals.truncate(base_len);
                        return Err(err);
                    }
                };
            locals.truncate(base_len);
            Ok(lower_syntax_operator_values(operator, left, right, context))
        }
    }
}

fn lower_comparison_chain(
    first: &SyntaxExpr,
    rest: &[(SyntaxOperator, SyntaxExpr)],
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let left = syntax_expr_to_value_in_scope(first, line, context, scope, locals)?;
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
                syntax_expr_to_value_in_scope(expr, line, context, scope, locals)?,
            ))
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    Ok(lower_comparison_chain_values(left, &rest, context))
}

fn lower_comparison_chain_values(
    left: Value,
    rest: &[(SyntaxOperator, Value)],
    context: &CompileContext,
) -> Value {
    let Some((operator, right)) = rest.first() else {
        return left;
    };

    if rest.len() == 1 {
        return lower_syntax_operator_values(*operator, left, right.clone(), context);
    }

    let right_local = context.value_local(0);
    let first_condition = lower_syntax_operator_values(
        *operator,
        shift_value_locals(&left, 1, 0),
        right_local.clone(),
        context,
    );
    let shifted_rest = rest[1..]
        .iter()
        .map(|(operator, value)| (*operator, shift_value_locals(value, 1, 0)))
        .collect::<Vec<_>>();
    let remaining_condition = lower_comparison_chain_values(right_local, &shifted_rest, context);
    let body = lower_syntax_operator_values(
        SyntaxOperator::BoolAnd,
        first_condition,
        remaining_condition,
        context,
    );
    context.value_apply(context.value_lambda(body), right.clone())
}

fn lower_syntax_operator_function(operator: SyntaxOperator, context: &CompileContext) -> Value {
    match operator {
        SyntaxOperator::Builtin(builtin) => context.value_builtin(builtin),
        SyntaxOperator::BoolAnd
        | SyntaxOperator::BoolOr
        | SyntaxOperator::PipeForward
        | SyntaxOperator::PipeBackward
        | SyntaxOperator::ComposeForward
        | SyntaxOperator::ComposeBackward
        | SyntaxOperator::EffectBind
        | SyntaxOperator::KleisliCompose
        | SyntaxOperator::EffectThen => {
            let left = context.value_local(1);
            let right = context.value_local(0);
            let body = lower_syntax_operator_values(operator, left, right, context);
            context.value_lambda(context.value_lambda(body))
        }
    }
}

fn lower_syntax_operator_values(
    operator: SyntaxOperator,
    left: Value,
    right: Value,
    context: &CompileContext,
) -> Value {
    match operator {
        SyntaxOperator::Builtin(builtin) => context.builtin_apply2_value(builtin, left, right),
        SyntaxOperator::BoolAnd => effect_then_values(left, right, context),
        SyntaxOperator::BoolOr => effect_call_value("alt", vec![left, right], context),
        SyntaxOperator::PipeForward => context.value_apply(right, left),
        SyntaxOperator::PipeBackward => context.value_apply(left, right),
        SyntaxOperator::ComposeForward => compose_values(left, right, context),
        SyntaxOperator::ComposeBackward => compose_values(right, left, context),
        SyntaxOperator::EffectBind => effect_call_value("seq", vec![left, right], context),
        SyntaxOperator::KleisliCompose => kleisli_compose_values(left, right, context),
        SyntaxOperator::EffectThen => effect_then_values(left, right, context),
    }
}

fn compose_values(first: Value, second: Value, context: &CompileContext) -> Value {
    let input = context.value_local(0);
    let first = shift_value_locals(&first, 1, 0);
    let second = shift_value_locals(&second, 1, 0);
    context.value_lambda(context.value_apply(second, context.value_apply(first, input)))
}

fn kleisli_compose_values(first: Value, second: Value, context: &CompileContext) -> Value {
    let input = context.value_local(0);
    let first = shift_value_locals(&first, 1, 0);
    let second = shift_value_locals(&second, 1, 0);
    let operation = context.value_apply(first, input);
    let body = effect_call_value("seq", vec![operation, second], context);
    context.value_lambda(body)
}

fn effect_then_values(operation: Value, next: Value, context: &CompileContext) -> Value {
    let result = context.value_local(0);
    let next = shift_value_locals(&next, 1, 0);
    let body = annotate_assert_unit_value(result, next, context);
    let continuation = context.value_lambda(body);
    effect_call_value("seq", vec![operation, continuation], context)
}

fn effect_call_value(name: &str, arguments: Vec<Value>, context: &CompileContext) -> Value {
    arguments
        .into_iter()
        .fold(lower_effect_expr(name, context), |function, argument| {
            context.value_apply(function, argument)
        })
}

fn annotate_assert_unit_value(value: Value, target: Value, context: &CompileContext) -> Value {
    let payload = context.builtin_apply2_value(
        Builtin::DictSingleton,
        context.value_atom(atom_from_str("value")),
        value,
    );
    let annotation = context.builtin_apply2_value(
        Builtin::DictSingleton,
        context.value_atom(atom_from_str("assert_unit")),
        payload,
    );
    context.builtin_apply2_value(Builtin::Anno, annotation, target)
}

fn syntax_key_expr_to_value(
    key: &SyntaxKeyExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => context.value_atom(atom_from_str(name)),
        SyntaxKeyExpr::Index(expr) => {
            syntax_expr_to_value_in_scope(expr, line, context, scope, locals)?
        }
        SyntaxKeyExpr::PathIndex(_) => {
            return Err(Diagnostic::error(
                line,
                "list-valued path expressions are not valid dictionary keys",
            ));
        }
    })
}

fn lower_dict_union(
    items: &[SyntaxExpr],
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let mut items = items.iter();
    let Some(first) = items.next() else {
        return Ok(context.empty_dict_value());
    };

    let mut value = syntax_expr_to_value_in_scope(first, line, context, scope, locals)?;
    for item in items {
        value = context.builtin_apply2_value(
            Builtin::DictUnion,
            value,
            syntax_expr_to_value_in_scope(item, line, context, scope, locals)?,
        );
    }
    Ok(value)
}

fn lower_lambda_expr(
    params: &[String],
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let base_len = locals.len();
    locals.extend(params.iter().map(|param| local_name_metadata(param)));
    let mut lowered = syntax_expr_to_value_in_scope(body, line, context, scope, locals)?;
    locals.truncate(base_len);

    for _ in params.iter().rev() {
        lowered = context.value_lambda(lowered);
    }

    Ok(lowered)
}

fn lower_let_expr(
    bindings: &[(String, SyntaxExpr)],
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    let values = bindings
        .iter()
        .map(|(_, expr)| syntax_expr_to_value_in_scope(expr, line, context, scope, locals))
        .collect::<Result<Vec<_>, _>>()?;

    let base_len = locals.len();
    locals.extend(bindings.iter().map(|(name, _)| local_name_metadata(name)));
    let mut lowered = syntax_expr_to_value_in_scope(body, line, context, scope, locals)?;
    locals.truncate(base_len);

    for _ in bindings.iter().rev() {
        lowered = context.value_lambda(lowered);
    }
    for value in values {
        lowered = context.value_apply(lowered, value);
    }

    Ok(lowered)
}

fn lower_name_expr(
    name: &str,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Value {
    match name {
        "module" => return scope.module_final_defs.clone(),
        "self" => {
            return scope
                .object_final_defs
                .clone()
                .unwrap_or_else(|| scope.module_final_defs.clone());
        }
        _ => {}
    }

    let Some(local_index) = local_binding_index(name, locals) else {
        if scope.object_alias.as_deref() == Some(name) {
            if let Some(object_final_defs) = &scope.object_final_defs {
                return object_final_defs.clone();
            }
        }
        return context.value_access(
            scope.final_defs.clone(),
            vec![context.key_expr_key(Key::atom_from_text(name))],
        );
    };

    context.value_local(local_index)
}

fn lower_prior_name_expr(
    name: &str,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
) -> Result<Value, Diagnostic> {
    if name.is_empty() {
        return Err(Diagnostic::error(
            line,
            "prior name expression must have a name",
        ));
    }

    match name {
        "module" => return Ok(scope.module_prior_defs.clone()),
        "self" => {
            return Ok(scope
                .object_prior_defs
                .clone()
                .unwrap_or_else(|| scope.module_prior_defs.clone()));
        }
        _ => {}
    }

    if scope.object_alias.as_deref() == Some(name) {
        if let Some(object_prior_defs) = &scope.object_prior_defs {
            return Ok(object_prior_defs.clone());
        }
    }

    Ok(context.value_access(
        scope.prior_defs.clone(),
        vec![context.key_expr_key(Key::atom_from_text(name))],
    ))
}

fn escaped_name_scope(
    scope: &NameScope,
    depth: usize,
    line: usize,
) -> Result<NameScope, Diagnostic> {
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

fn local_binding_index(name: &str, locals: &[LocalName]) -> Option<usize> {
    locals
        .iter()
        .rposition(|candidate| candidate.canonical.as_deref() == Some(name))
        .map(|position| locals.len() - 1 - position)
}

fn local_name_metadata(raw: &str) -> LocalName {
    match raw {
        "_" => LocalName {
            raw: raw.to_owned(),
            canonical: None,
            suppress_unused_warning: true,
        },
        suppressed if suppressed.starts_with('_') => LocalName {
            raw: suppressed.to_owned(),
            canonical: Some(suppressed[1..].to_owned()),
            suppress_unused_warning: true,
        },
        name => LocalName {
            raw: name.to_owned(),
            canonical: Some(name.to_owned()),
            suppress_unused_warning: false,
        },
    }
}

fn syntax_key_expr_to_core(
    key: &SyntaxKeyExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<CoreKeyExpr, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => context.key_expr_key(name_as_key(name)),
        SyntaxKeyExpr::Index(expr) => context.key_expr_index(syntax_expr_to_value_in_scope(
            expr, line, context, scope, locals,
        )?),
        SyntaxKeyExpr::PathIndex(expr) => context.key_expr_path_index(
            syntax_expr_to_value_in_scope(expr, line, context, scope, locals)?,
        ),
    })
}

fn name_as_key(name: &str) -> Key {
    // 'name as dict key or tag
    Key::Atom(atom_from_str(name))
}

fn atom_from_str(name: &str) -> Atom {
    // 'name atom, i.e. ["name"]:()
    Atom::from_key(&Key::binary_from_text(name))
}

fn validate_language_position(declarations: &[Declaration], diagnostics: &mut Vec<Diagnostic>) {
    let Some(first) = declarations.first() else {
        diagnostics.push(Diagnostic::error(
            1,
            "empty source has no language declaration",
        ));
        return;
    };

    if !matches!(first.kind, DeclarationKind::Language(_)) {
        diagnostics.push(Diagnostic::error(
            first.line,
            "first declaration should be a language version declaration",
        ));
    }

    for declaration in declarations.iter().skip(1) {
        if matches!(declaration.kind, DeclarationKind::Language(_)) {
            diagnostics.push(Diagnostic::error(
                declaration.line,
                "language declaration must appear before all other declarations",
            ));
        }
    }
}

fn classify_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    match first_word(text) {
        Some("object") => return classify_object_declaration(text, line, diagnostics),
        Some("extend") => return classify_extend_declaration(text, line, diagnostics),
        _ => {}
    }

    let (declaration, errors) = declaration_parser().parse(text).into_output_errors();

    for error in errors {
        diagnostics.push(Diagnostic::error(line, error.to_string()));
    }

    if let Some(declaration) = declaration {
        match declaration {
            DeclarationKind::Definition(definition) => {
                DeclarationKind::Definition(finalize_definition_expr(definition, line, diagnostics))
            }
            other => other,
        }
    } else {
        DeclarationKind::Unknown
    }
}

fn classify_object_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    match parse_object_declaration(text, line, diagnostics) {
        Some(object) => DeclarationKind::Object(object),
        None => DeclarationKind::Unknown,
    }
}

fn parse_object_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectDecl> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    let header = header.strip_prefix("object")?.trim();

    let (target, rest) = take_header_word(header).unwrap_or(("", ""));
    if target.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "object declaration requires a name",
        ));
        return None;
    }
    if target == "_" {
        diagnostics.push(Diagnostic::error(
            line,
            "anonymous object declarations are not supported by the current spike",
        ));
        return None;
    }
    if !path().parse(target).into_result().is_ok() {
        diagnostics.push(Diagnostic::error(
            line,
            "object declaration requires a path name",
        ));
        return None;
    }

    let header_tail = parse_object_header_tail(rest.trim(), line, diagnostics)?;
    if !body_lines.is_empty() && !header_tail.has_with {
        diagnostics.push(Diagnostic::error(
            line,
            "object body requires `with` in the declaration header",
        ));
        return None;
    }

    let body = parse_object_body(&body_lines, line + 1, diagnostics);
    if let Some(alias) = &header_tail.alias {
        warn_unused_with_alias(alias, &body, line, diagnostics);
    }

    Some(ObjectDecl {
        target: target.to_owned(),
        alias: header_tail.alias,
        deps: header_tail.deps,
        body,
    })
}

struct ObjectHeaderTail {
    alias: Option<String>,
    deps: Vec<String>,
    has_with: bool,
}

fn parse_object_header_tail(
    rest: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectHeaderTail> {
    let (alias, rest) = parse_optional_object_alias(rest, line, diagnostics)?;
    if rest.is_empty() {
        return Some(ObjectHeaderTail {
            alias,
            deps: Vec::new(),
            has_with: false,
        });
    }
    if rest == "with" {
        return Some(ObjectHeaderTail {
            alias,
            deps: Vec::new(),
            has_with: true,
        });
    }

    let Some(after_extends) = rest.strip_prefix("extends").map(str::trim) else {
        diagnostics.push(Diagnostic::error(
            line,
            "object declarations currently support only `extends ...` and `with` after the name",
        ));
        return None;
    };
    if after_extends.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "object `extends` requires at least one dependency",
        ));
        return None;
    }

    let (deps_text, has_with) = match after_extends.strip_suffix(" with") {
        Some(deps) => (deps.trim(), true),
        None if after_extends == "with" => {
            diagnostics.push(Diagnostic::error(
                line,
                "object `extends` requires at least one dependency",
            ));
            return None;
        }
        None => (after_extends, false),
    };
    let deps = deps_text
        .split(',')
        .map(str::trim)
        .filter(|dep| !dep.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if deps.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "object `extends` requires at least one dependency",
        ));
        return None;
    }
    for dep in &deps {
        if !path().parse(dep.as_str()).into_result().is_ok() {
            diagnostics.push(Diagnostic::error(
                line,
                format!("object dependency `{dep}` is not a path name"),
            ));
            return None;
        }
    }

    Some(ObjectHeaderTail {
        alias,
        deps,
        has_with,
    })
}

fn parse_optional_object_alias<'a>(
    rest: &'a str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(Option<String>, &'a str)> {
    let rest = rest.trim();
    let Some((first, tail)) = take_header_word(rest) else {
        return Some((None, ""));
    };
    if first != "as" {
        return Some((None, rest));
    }

    let Some((alias, tail)) = take_header_word(tail) else {
        diagnostics.push(Diagnostic::error(
            line,
            "`as` requires an object alias name",
        ));
        return None;
    };
    if !local_name().parse(alias).into_result().is_ok() {
        diagnostics.push(Diagnostic::error(
            line,
            format!("object alias `{alias}` is not a valid local name"),
        ));
        return None;
    }
    Some((Some(alias.to_owned()), tail.trim()))
}

fn parse_object_body(
    lines: &[&str],
    first_line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ObjectBodyDefinition> {
    let mut body = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }

        let line_number = first_line + index;
        if is_indented(line) {
            diagnostics.push(Diagnostic::error(
                line_number,
                "object body continuation line without a preceding nested declaration",
            ));
            index += 1;
            continue;
        }

        let mut text = trimmed.to_owned();
        index += 1;
        let mut continuation_indent = None;
        while index < lines.len() {
            let next = lines[index];
            let next_trimmed = next.trim();
            if next_trimmed.is_empty() {
                index += 1;
                continue;
            }
            if !is_indented(next) {
                break;
            }
            if continuation_indent.is_none() {
                continuation_indent = Some(indentation_width(next));
            }
            let next_text = continuation_indent
                .map(|indent| strip_indent_width(next.trim_end(), indent))
                .unwrap_or(next_trimmed);
            text.push('\n');
            text.push_str(next_text.trim_end());
            index += 1;
        }

        if let Some(definition) = parse_object_body_definition(&text, line_number, diagnostics) {
            body.push(definition);
        }
    }

    body
}

fn parse_object_body_definition(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectBodyDefinition> {
    if text.trim_start().starts_with("object ") {
        let Some(object) = parse_object_declaration(text, line, diagnostics) else {
            return None;
        };
        return Some(ObjectBodyDefinition {
            line,
            text: text.to_owned(),
            kind: ObjectBodyDefinitionKind::Object(object),
        });
    }

    let (declaration, errors) = definition_decl().parse(text).into_output_errors();
    for error in errors {
        diagnostics.push(Diagnostic::error(line, error.to_string()));
    }

    let Some(definition) = declaration else {
        return None;
    };
    Some(ObjectBodyDefinition {
        line,
        text: text.to_owned(),
        kind: ObjectBodyDefinitionKind::Definition(finalize_definition_expr(
            definition,
            line,
            diagnostics,
        )),
    })
}

fn classify_extend_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DeclarationKind {
    match parse_extend_declaration(text, line, diagnostics) {
        Some(extend) => DeclarationKind::Extend(extend),
        None => DeclarationKind::Unknown,
    }
}

fn parse_extend_declaration(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectExtendDecl> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    let header = header.strip_prefix("extend")?.trim();

    let (target, rest) = take_header_word(header).unwrap_or(("", ""));
    if target.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a name",
        ));
        return None;
    }
    let (alias, rest) = parse_optional_object_alias(rest, line, diagnostics)?;
    if rest != "with" {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declarations currently require `extend name (as alias)? with`",
        ));
        return None;
    }
    if !path().parse(target).into_result().is_ok() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a path name",
        ));
        return None;
    }
    if body_lines.is_empty() {
        diagnostics.push(Diagnostic::error(
            line,
            "extend declaration requires a body",
        ));
        return None;
    }

    let body = parse_object_body(&body_lines, line + 1, diagnostics);
    if let Some(alias) = &alias {
        warn_unused_with_alias(alias, &body, line, diagnostics);
    }

    Some(ObjectExtendDecl {
        target: target.to_owned(),
        alias,
        body,
    })
}

fn take_header_word(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return None;
    }
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    Some((&text[..end], &text[end..]))
}

fn declaration_parser<'src>()
-> impl Parser<'src, &'src str, DeclarationKind, extra::Err<Rich<'src, char>>> {
    choice((
        language_decl().map(DeclarationKind::Language),
        import_decl().map(DeclarationKind::Import),
        keyword_name_list("abstract").map(DeclarationKind::Abstract),
        keyword_name_list("unique").map(DeclarationKind::Unique),
        definition_decl().map(DeclarationKind::Definition),
    ))
    .then_ignore(end())
}

fn language_decl<'src>() -> impl Parser<'src, &'src str, LanguageDecl, extra::Err<Rich<'src, char>>>
{
    just("language")
        .or(just("lang"))
        .padded()
        .ignore_then(name())
        .then(
            just("with")
                .padded()
                .ignore_then(
                    name()
                        .separated_by(just(',').padded())
                        .at_least(1)
                        .collect::<Vec<_>>(),
                )
                .or_not(),
        )
        .map(|(base, extensions)| LanguageDecl {
            base,
            extensions: extensions.unwrap_or_default(),
        })
}

fn import_decl<'src>() -> impl Parser<'src, &'src str, ImportDecl, extra::Err<Rich<'src, char>>> {
    let reference = choice((
        quoted_text().map(ImportReference::Local),
        just('\'')
            .ignore_then(glam_name())
            .map(ImportReference::Builtin),
    ));
    let placement = just("as")
        .padded()
        .ignore_then(path())
        .map(ImportPlacement::As)
        .or(just("at")
            .padded()
            .ignore_then(path())
            .map(ImportPlacement::At))
        .or_not()
        .map(|placement| placement.unwrap_or(ImportPlacement::Inline));

    let binary = just("binary")
        .padded()
        .to(true)
        .or_not()
        .map(|v| v.unwrap_or(false));

    just("import")
        .padded()
        .ignore_then(reference)
        .then(binary)
        .then(placement)
        .map(|((reference, binary), placement)| ImportDecl {
            reference,
            binary,
            placement,
        })
}

fn keyword_name_list<'src>(
    keyword: &'static str,
) -> impl Parser<'src, &'src str, Vec<String>, extra::Err<Rich<'src, char>>> {
    just(keyword).padded().ignore_then(
        path()
            .separated_by(just(',').padded())
            .at_least(1)
            .collect::<Vec<_>>(),
    )
}

fn definition_decl<'src>()
-> impl Parser<'src, &'src str, DefinitionDecl, extra::Err<Rich<'src, char>>> {
    definition_target()
        .then(
            whitespace1().ignore_then(
                local_name()
                    .then_ignore(whitespace1())
                    .repeated()
                    .collect::<Vec<_>>(),
            ),
        )
        .then(definition_operator())
        .then_ignore(whitespace0())
        .then(rest_of_declaration())
        .try_map(|(((target, params), kind), body), span| {
            if body.is_empty() {
                Err(Rich::custom(span, "definition body cannot be empty"))
            } else {
                Ok(DefinitionDecl {
                    target,
                    kind,
                    body: desugar_definition_body(kind, &params, body),
                    expr: None,
                })
            }
        })
}

fn definition_target<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    definition_target_path().to_slice().map(ToOwned::to_owned)
}

fn definition_target_path<'src>()
-> impl Parser<'src, &'src str, Vec<SyntaxKeyExpr>, extra::Err<Rich<'src, char>>> {
    let name = glam_name().boxed();
    let expr = syntax_expr_parser().boxed();
    let single_key_expr = || {
        choice((
            just('\'')
                .ignore_then(name.clone())
                .map(SyntaxKeyExpr::Atom),
            expr.clone()
                .map(|expr| SyntaxKeyExpr::Index(Box::new(expr))),
        ))
    };
    let path_list_shorthand = single_key_expr()
        .padded()
        .separated_by(just(',').padded())
        .allow_leading()
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just('['), just(']'))
        .map(PathSuffix::Expand);
    let path_list_expr = expr
        .padded()
        .delimited_by(just('('), just(')'))
        .map(|expr| PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(expr))));
    let path_suffix_item = just('.').ignore_then(choice((
        path_list_shorthand,
        path_list_expr,
        name.clone()
            .map(SyntaxKeyExpr::Atom)
            .map(PathSuffix::Single),
    )));
    let path_suffix = path_suffix_item.clone().repeated().collect::<Vec<_>>();

    choice((
        name.clone()
            .map(SyntaxKeyExpr::Atom)
            .then(path_suffix.clone())
            .map(|(name, suffixes)| {
                let mut parts = vec![name];
                parts.extend(flatten_path_suffixes(suffixes));
                parts
            }),
        path_suffix_item
            .repeated()
            .at_least(1)
            .collect::<Vec<_>>()
            .map(flatten_path_suffixes),
    ))
}

fn definition_operator<'src>()
-> impl Parser<'src, &'src str, DefinitionKind, extra::Err<Rich<'src, char>>> {
    choice((
        just("::=").to(DefinitionKind::Update),
        just(":=").to(DefinitionKind::Override),
        just('=').to(DefinitionKind::Introduce),
    ))
}

fn path<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    name()
        .separated_by(just('.'))
        .at_least(1)
        .collect::<Vec<_>>()
        .map(|parts| parts.join("."))
}

fn name<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    text::ascii::ident().map(ToOwned::to_owned)
}

fn quoted_text<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    none_of('"')
        .repeated()
        .to_slice()
        .map(ToOwned::to_owned)
        .delimited_by(just('"'), just('"'))
}

fn rest_of_declaration<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>>
{
    any()
        .repeated()
        .to_slice()
        .map(|text: &str| text.trim().to_owned())
}

fn desugar_definition_body(kind: DefinitionKind, params: &[String], body: String) -> String {
    let _ = kind;
    if params.is_empty() {
        body
    } else {
        format!("\\ {} -> {}", params.join(" "), body)
    }
}

#[cfg(test)]
fn parse_expr_result(text: &str) -> Result<SyntaxExpr, String> {
    let mut diagnostics = Vec::new();
    parse_expr_result_with_diagnostics(text, 1, &mut diagnostics)
}

fn parse_expr_result_with_diagnostics(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<SyntaxExpr, String> {
    let text = text.trim();
    if let Some(result) = parse_let_expr_result(text, line, diagnostics) {
        return result;
    }
    if let Some(result) = parse_where_expr_result(text, line, diagnostics) {
        return result;
    }
    if let Some(result) = parse_object_expr_result(text, line, diagnostics) {
        return result;
    }
    if let Some(result) = parse_with_expr_result(text, line, diagnostics) {
        return result;
    }

    syntax_expr_parser()
        .then_ignore(end())
        .parse(text)
        .into_result()
        .map_err(|errors| {
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })
}

fn parse_let_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let rest = text.strip_prefix("let")?;
    if !rest
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_whitespace())
    {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        return Some(Err("let expression requires bindings and a body".to_owned()));
    }

    let (bindings_text, body_text) = match split_top_level_keyword(rest, "in", false) {
        Some((bindings, body)) => (bindings.trim(), body.trim()),
        None => match split_multiline_let(rest) {
            Ok((bindings, body)) => (bindings.trim(), body.trim()),
            Err(message) => return Some(Err(message)),
        },
    };

    Some(parse_let_expr_from_parts(
        bindings_text,
        body_text,
        line,
        diagnostics,
    ))
}

fn parse_where_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let (body_text, bindings_text) = split_top_level_keyword(text, "where", true)?;
    let body_text = body_text.trim();
    let bindings_text = bindings_text.trim();
    if body_text.is_empty() {
        return Some(Err("where expression requires a body".to_owned()));
    }
    Some(parse_let_expr_from_parts(
        bindings_text,
        body_text,
        line,
        diagnostics,
    ))
}

fn parse_let_expr_from_parts(
    bindings_text: &str,
    body_text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<SyntaxExpr, String> {
    if body_text.is_empty() {
        return Err("let expression requires a body".to_owned());
    }
    let bindings = parse_local_bindings(bindings_text, line, diagnostics)?;
    if bindings.is_empty() {
        return Err("let expression requires at least one binding".to_owned());
    }
    let body = parse_expr_result_with_diagnostics(body_text, line, diagnostics)?;
    Ok(SyntaxExpr::Let {
        bindings,
        body: Box::new(body),
    })
}

fn parse_local_bindings(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<(String, SyntaxExpr)>, String> {
    let binding_texts = if text.contains('\n') {
        parse_multiline_binding_texts(text)?
    } else {
        split_top_level_semicolons(text)
    };

    binding_texts
        .into_iter()
        .filter(|binding| !binding.trim().is_empty())
        .map(|binding| parse_local_binding(binding.trim(), line, diagnostics))
        .collect()
}

fn parse_local_binding(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(String, SyntaxExpr), String> {
    let Some((name, value)) = split_top_level_binding_equals(text) else {
        return Err(format!("local binding `{text}` must use `=`"));
    };
    let name = name.trim();
    if local_name().parse(name).into_result().is_err() || name.contains(char::is_whitespace) {
        return Err(format!("invalid local binding name `{name}`"));
    }
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("local binding `{name}` requires a value"));
    }
    Ok((
        name.to_owned(),
        parse_expr_result_with_diagnostics(value, line, diagnostics)?,
    ))
}

fn split_multiline_let(text: &str) -> Result<(&str, &str), String> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return Err("multi-line let expression requires a body or `in`".to_owned());
    }

    let mut binding_end = 0;
    let mut saw_binding = false;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            binding_end = index + 1;
            continue;
        }
        if is_indented(line) {
            binding_end = index + 1;
            continue;
        }
        if split_top_level_binding_equals(trimmed).is_some() {
            saw_binding = true;
            binding_end = index + 1;
            continue;
        }
        break;
    }

    if !saw_binding {
        return Err("let expression requires at least one binding".to_owned());
    }
    if binding_end >= lines.len() {
        return Err("multi-line let expression requires a body".to_owned());
    }

    let binding_offset: usize = lines[..binding_end].iter().map(|line| line.len() + 1).sum();
    Ok((
        &text[..binding_offset.saturating_sub(1)],
        &text[binding_offset..],
    ))
}

fn parse_multiline_binding_texts(text: &str) -> Result<Vec<&str>, String> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !is_indented(line)
            && split_top_level_binding_equals(trimmed).is_some()
        {
            starts.push(offset);
        }
        offset += line.len() + 1;
    }

    if starts.is_empty() {
        return Err("local binding block requires at least one binding".to_owned());
    }
    starts.push(text.len() + 1);

    let mut bindings = Vec::new();
    for pair in starts.windows(2) {
        let start = pair[0];
        let end = pair[1].saturating_sub(1).min(text.len());
        bindings.push(text[start..end].trim());
    }
    Ok(bindings)
}

fn split_top_level_keyword<'a>(
    text: &'a str,
    keyword: &str,
    from_end: bool,
) -> Option<(&'a str, &'a str)> {
    let matches = top_level_keyword_indices(text, keyword);
    let index = if from_end {
        matches.into_iter().last()?
    } else {
        matches.into_iter().next()?
    };
    Some((&text[..index], &text[index + keyword.len()..]))
}

fn top_level_keyword_indices(text: &str, keyword: &str) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;

    for (index, ch) in text.char_indices() {
        if in_string {
            if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && keyword_starts_at(text, index, keyword) => indices.push(index),
            _ => {}
        }
    }

    indices
}

fn keyword_starts_at(text: &str, index: usize, keyword: &str) -> bool {
    if !text[index..].starts_with(keyword) {
        return false;
    }
    let before = text[..index].chars().next_back();
    let after = text[index + keyword.len()..].chars().next();
    !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn split_top_level_semicolons(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for index in top_level_char_indices(text, ';') {
        parts.push(&text[start..index]);
        start = index + 1;
    }
    parts.push(&text[start..]);
    parts
}

fn split_top_level_binding_equals(text: &str) -> Option<(&str, &str)> {
    top_level_char_indices(text, '=')
        .into_iter()
        .find(|index| {
            let before = text[..*index].chars().next_back();
            let after = text[index + 1..].chars().next();
            !matches!(before, Some(':') | Some('<') | Some('>') | Some('='))
                && !matches!(after, Some('=') | Some('>') | Some('<'))
        })
        .map(|index| (&text[..index], &text[index + 1..]))
}

fn top_level_char_indices(text: &str, needle: char) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;

    for (index, ch) in text.char_indices() {
        if in_string {
            if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && ch == needle => indices.push(index),
            _ => {}
        }
    }

    indices
}

fn parse_object_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    let header = header.strip_prefix("object")?.trim();
    if header.is_empty() {
        return Some(Err(
            "object expression requires a name expression or `_`".to_owned()
        ));
    }

    let (header, has_with) = match header.strip_suffix(" with") {
        Some(header) => (header.trim(), true),
        None => (header, false),
    };
    if !body_lines.is_empty() && !has_with {
        return Some(Err(
            "object expression body requires `with` in the expression header".to_owned(),
        ));
    }

    let (name_text, alias, dep_texts) = match parse_object_expr_header(header) {
        Ok(parsed) => parsed,
        Err(message) => return Some(Err(message)),
    };
    let name = match name_text {
        Some(name_text) => match parse_expr_result_with_diagnostics(name_text, line, diagnostics) {
            Ok(name) => Some(Box::new(name)),
            Err(message) => return Some(Err(message)),
        },
        None => None,
    };
    let deps = match dep_texts
        .iter()
        .map(|dep| parse_expr_result_with_diagnostics(dep, line, diagnostics))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(deps) => deps,
        Err(message) => return Some(Err(message)),
    };

    let body = parse_object_body(&body_lines, line + 1, diagnostics);

    Some(Ok(SyntaxExpr::Object(ObjectExpr {
        name,
        alias,
        deps,
        body,
    })))
}

fn parse_object_expr_header(
    header: &str,
) -> Result<(Option<&str>, Option<String>, Vec<&str>), String> {
    let (name_text, rest) = split_before_object_expr_keyword(header);
    let name_text = name_text.trim();
    if name_text.is_empty() {
        return Err("object expression requires a name expression or `_`".to_owned());
    }
    let name = if name_text == "_" {
        None
    } else {
        Some(name_text)
    };

    let (alias, rest) = parse_optional_object_expr_alias(rest)?;
    let deps = if rest.is_empty() {
        Vec::new()
    } else {
        let Some(deps) = rest.strip_prefix("extends").map(str::trim) else {
            return Err(
                "object expressions currently support only `as ...`, `extends ...`, and `with` after the name"
                    .to_owned(),
            );
        };
        if deps.is_empty() {
            return Err("object expression `extends` requires at least one dependency".to_owned());
        }
        deps.split(',')
            .map(str::trim)
            .filter(|dep| !dep.is_empty())
            .collect::<Vec<_>>()
    };

    Ok((name, alias, deps))
}

fn split_before_object_expr_keyword(header: &str) -> (&str, &str) {
    let as_index = header.find(" as ");
    let extends_index = header.find(" extends ");
    let split = match (as_index, extends_index) {
        (Some(left), Some(right)) => left.min(right),
        (Some(index), None) | (None, Some(index)) => index,
        (None, None) => return (header, ""),
    };
    (&header[..split], header[split..].trim_start())
}

fn parse_optional_object_expr_alias(rest: &str) -> Result<(Option<String>, &str), String> {
    let Some(after_as) = rest.strip_prefix("as").map(str::trim_start) else {
        return Ok((None, rest));
    };
    let Some((alias, rest)) = take_header_word(after_as) else {
        return Err("`as` requires an object alias name".to_owned());
    };
    if !local_name().parse(alias).into_result().is_ok() {
        return Err(format!("object alias `{alias}` is not a valid local name"));
    }
    Ok((Some(alias.to_owned()), rest.trim()))
}

fn parse_with_expr_result(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Result<SyntaxExpr, String>> {
    let mut lines = text.lines();
    let header = lines.next()?.trim();
    let body_lines = lines.collect::<Vec<_>>();
    if body_lines.is_empty() {
        return None;
    }

    let base_and_alias = header.strip_suffix(" with")?.trim();
    let (base_text, alias) = parse_optional_with_alias(base_and_alias);
    if base_text.is_empty() {
        return Some(Err("with expression requires a base expression".to_owned()));
    }
    let base = match parse_expr_result_with_diagnostics(base_text, line, diagnostics) {
        Ok(base) => base,
        Err(message) => return Some(Err(message)),
    };

    let body = parse_object_body(&body_lines, line + 1, diagnostics);

    Some(Ok(SyntaxExpr::With {
        base: Box::new(base),
        alias,
        body,
    }))
}

fn parse_optional_with_alias(text: &str) -> (&str, Option<String>) {
    let Some((base, alias)) = text.rsplit_once(" as ") else {
        return (text, None);
    };
    if alias == "_" {
        return (base.trim(), None);
    }
    if local_name().parse(alias).into_result().is_ok() {
        (base.trim(), Some(alias.to_owned()))
    } else {
        (text, None)
    }
}

#[cfg(test)]
fn parse_expr(text: &str) -> Option<SyntaxExpr> {
    parse_expr_result(text).ok()
}

fn finalize_definition_expr(
    mut definition: DefinitionDecl,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DefinitionDecl {
    match parse_expr_result_with_diagnostics(definition.body.as_str(), line, diagnostics) {
        Ok(expr) => {
            warn_unused_locals(&expr, line, diagnostics);
            definition.expr = Some(expr);
        }
        Err(message) => diagnostics.push(Diagnostic::error(line, message)),
    }
    definition
}

fn warn_unused_locals(expr: &SyntaxExpr, line: usize, diagnostics: &mut Vec<Diagnostic>) {
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
            analyze_object_body_locals(&object.body, diagnostics);
        }
        SyntaxExpr::With { base, alias, body } => {
            analyze_expr_locals(base, line, diagnostics);
            if let Some(alias) = alias {
                warn_unused_with_alias(alias, body, line, diagnostics);
            }
            analyze_object_body_locals(body, diagnostics);
        }
        SyntaxExpr::SingletonDict(key, value) => {
            analyze_key_expr_locals(key, line, diagnostics);
            analyze_expr_locals(value, line, diagnostics);
        }
        SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) => {
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

fn warn_unused_with_alias(
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
        if let Some(definition) = item.definition() {
            if let Some(expr) = &definition.expr {
                analyze_expr_locals(expr, item.line, diagnostics);
            }
        }
        if let Some(object) = item.object() {
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
        SyntaxExpr::SingletonDict(key, value) => {
            mark_used_prior_alias_in_key(key, alias, used);
            mark_used_prior_alias(value, alias, used);
        }
        SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) => {
            for item in items {
                mark_used_prior_alias(item, alias, used);
            }
        }
        SyntaxExpr::Lambda(_, body) => mark_used_prior_alias(body, alias, used),
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
    if let Some(definition) = item.definition() {
        if let Some(expr) = &definition.expr {
            mark_used_prior_alias(expr, alias, used);
        }
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
        SyntaxExpr::SingletonDict(key, value) => {
            mark_used_key_expr(key, locals, used);
            mark_used_locals(value, locals, used);
        }
        SyntaxExpr::DictUnion(items) | SyntaxExpr::List(items) => {
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

fn mark_used_body_item_locals(
    item: &ObjectBodyDefinition,
    locals: &[LocalName],
    used: &mut [bool],
) {
    if let Some(definition) = item.definition() {
        if let Some(expr) = &definition.expr {
            mark_used_locals(expr, locals, used);
        }
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

fn syntax_expr_parser<'src>()
-> impl Parser<'src, &'src str, SyntaxExpr, extra::Err<Rich<'src, char>>> {
    #[allow(dead_code)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Associativity {
        Left,
        Right,
        None,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum OperatorRelation {
        Stronger,
        Weaker,
        Same(Associativity),
        Unrelated,
    }

    enum PartialExpr {
        Expr(SyntaxExpr),
        ComparisonChain {
            first: Box<SyntaxExpr>,
            rest: Vec<(SyntaxOperator, SyntaxExpr)>,
        },
    }

    impl PartialExpr {
        fn into_expr(self) -> SyntaxExpr {
            match self {
                PartialExpr::Expr(expr) => expr,
                PartialExpr::ComparisonChain { first, mut rest } if rest.len() == 1 => {
                    let (operator, right) = rest
                        .pop()
                        .expect("single-item comparison chain should contain one comparison");
                    syntax_binary_expr(operator, *first, right)
                }
                PartialExpr::ComparisonChain { first, rest } => {
                    SyntaxExpr::ComparisonChain { first, rest }
                }
            }
        }
    }

    fn resolve_infix_chain(
        first: SyntaxExpr,
        rest: Vec<(SyntaxOperator, SyntaxExpr)>,
    ) -> Result<SyntaxExpr, String> {
        let mut exprs = vec![PartialExpr::Expr(first)];
        let mut ops = Vec::new();

        for (next_op, next_expr) in rest {
            while let Some(previous_op) = ops.last().copied() {
                match infix_operator_relation(previous_op, next_op) {
                    OperatorRelation::Stronger | OperatorRelation::Same(Associativity::Left) => {
                        reduce_top_operator(&mut exprs, &mut ops)?
                    }
                    OperatorRelation::Weaker | OperatorRelation::Same(Associativity::Right) => {
                        break;
                    }
                    OperatorRelation::Same(Associativity::None)
                        if is_comparison_operator(previous_op)
                            && is_comparison_operator(next_op) =>
                    {
                        reduce_top_operator(&mut exprs, &mut ops)?
                    }
                    OperatorRelation::Same(Associativity::None) => {
                        return Err(format!(
                            "operator `{}` is non-associative; parenthesize this chain",
                            infix_operator_symbol(next_op)
                        ));
                    }
                    OperatorRelation::Unrelated => {
                        return Err(format!(
                            "operators `{}` and `{}` have no precedence relationship; parenthesize to disambiguate",
                            infix_operator_symbol(previous_op),
                            infix_operator_symbol(next_op)
                        ));
                    }
                }
            }

            ops.push(next_op);
            exprs.push(PartialExpr::Expr(next_expr));
        }

        while !ops.is_empty() {
            reduce_top_operator(&mut exprs, &mut ops)?;
        }

        exprs
            .pop()
            .map(PartialExpr::into_expr)
            .ok_or_else(|| "operator chain did not produce an expression".to_owned())
    }

    fn reduce_top_operator(
        exprs: &mut Vec<PartialExpr>,
        ops: &mut Vec<SyntaxOperator>,
    ) -> Result<(), String> {
        let right = exprs
            .pop()
            .map(PartialExpr::into_expr)
            .ok_or_else(|| "missing right operand in operator chain".to_owned())?;
        let left = exprs
            .pop()
            .ok_or_else(|| "missing left operand in operator chain".to_owned())?;
        let op = ops
            .pop()
            .ok_or_else(|| "missing operator in operator chain".to_owned())?;
        if is_comparison_operator(op) {
            match left {
                PartialExpr::Expr(left) => exprs.push(PartialExpr::ComparisonChain {
                    first: Box::new(left),
                    rest: vec![(op, right)],
                }),
                PartialExpr::ComparisonChain { first, mut rest } => {
                    rest.push((op, right));
                    exprs.push(PartialExpr::ComparisonChain { first, rest });
                }
            }
        } else {
            exprs.push(PartialExpr::Expr(syntax_binary_expr(
                op,
                left.into_expr(),
                right,
            )));
        }
        Ok(())
    }

    fn infix_operator_relation(left: SyntaxOperator, right: SyntaxOperator) -> OperatorRelation {
        use crate::core::Builtin::{
            Add, Append, Divide, Equal, Greater, GreaterEqual, Less, LessEqual, Multiply, NotEqual,
            Subtract,
        };
        use SyntaxOperator::{
            BoolAnd, BoolOr, Builtin, ComposeBackward, ComposeForward, EffectBind, EffectThen,
            KleisliCompose, PipeBackward, PipeForward,
        };

        match (left, right) {
            (BoolOr, BoolOr) | (BoolAnd, BoolAnd) => OperatorRelation::Same(Associativity::Left),
            (BoolOr, BoolAnd) => OperatorRelation::Weaker,
            (BoolAnd, BoolOr) => OperatorRelation::Stronger,
            (EffectBind, EffectBind)
            | (EffectBind, EffectThen)
            | (EffectThen, EffectBind)
            | (EffectThen, EffectThen) => OperatorRelation::Same(Associativity::Left),
            (KleisliCompose, KleisliCompose) => OperatorRelation::Same(Associativity::Right),
            (PipeForward, PipeForward) => OperatorRelation::Same(Associativity::Left),
            (PipeBackward, PipeBackward) => OperatorRelation::Same(Associativity::Right),
            (PipeForward, PipeBackward) | (PipeBackward, PipeForward) => {
                OperatorRelation::Unrelated
            }
            (ComposeForward, ComposeForward) => OperatorRelation::Same(Associativity::Left),
            (ComposeBackward, ComposeBackward) => OperatorRelation::Same(Associativity::Right),
            (ComposeForward, ComposeBackward) | (ComposeBackward, ComposeForward) => {
                OperatorRelation::Unrelated
            }
            (Builtin(Append), Builtin(Append)) => OperatorRelation::Same(Associativity::Left),
            (Builtin(Add), Builtin(Add)) => OperatorRelation::Same(Associativity::Left),
            (Builtin(Add), Builtin(Subtract)) | (Builtin(Subtract), Builtin(Add)) => {
                OperatorRelation::Unrelated
            }
            (Builtin(Subtract), Builtin(Subtract)) => OperatorRelation::Same(Associativity::None),
            (
                Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less),
                Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less),
            ) => OperatorRelation::Same(Associativity::None),
            (Builtin(Multiply), Builtin(Multiply))
            | (Builtin(Multiply), Builtin(Divide))
            | (Builtin(Divide), Builtin(Multiply)) => OperatorRelation::Same(Associativity::Left),
            (Builtin(Divide), Builtin(Divide)) => OperatorRelation::Same(Associativity::None),
            _ => match operator_precedence(left).cmp(&operator_precedence(right)) {
                std::cmp::Ordering::Greater => OperatorRelation::Stronger,
                std::cmp::Ordering::Less => OperatorRelation::Weaker,
                std::cmp::Ordering::Equal => OperatorRelation::Unrelated,
            },
        }
    }

    fn operator_precedence(operator: SyntaxOperator) -> u8 {
        use crate::core::Builtin::{
            Add, Append, Divide, Equal, Greater, GreaterEqual, Less, LessEqual, Multiply, NotEqual,
            Subtract,
        };
        use SyntaxOperator::{
            BoolAnd, BoolOr, Builtin, ComposeBackward, ComposeForward, EffectBind, EffectThen,
            KleisliCompose, PipeBackward, PipeForward,
        };

        match operator {
            BoolOr => 0,
            BoolAnd => 1,
            EffectBind | EffectThen => 2,
            PipeForward | PipeBackward => 3,
            ComposeForward | ComposeBackward | KleisliCompose => 4,
            Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less) => 5,
            Builtin(Append) => 6,
            Builtin(Add | Subtract) => 7,
            Builtin(Multiply | Divide) => 8,
            Builtin(_) => 9,
        }
    }

    fn infix_operator_symbol(operator: SyntaxOperator) -> &'static str {
        match operator {
            SyntaxOperator::BoolAnd => "and",
            SyntaxOperator::BoolOr => "or",
            SyntaxOperator::Builtin(crate::core::Builtin::Append) => "++",
            SyntaxOperator::Builtin(crate::core::Builtin::Add) => "+",
            SyntaxOperator::Builtin(crate::core::Builtin::Subtract) => "-",
            SyntaxOperator::Builtin(crate::core::Builtin::Multiply) => "*",
            SyntaxOperator::Builtin(crate::core::Builtin::Divide) => "/",
            SyntaxOperator::Builtin(crate::core::Builtin::Greater) => ">",
            SyntaxOperator::Builtin(crate::core::Builtin::GreaterEqual) => ">=",
            SyntaxOperator::Builtin(crate::core::Builtin::Equal) => "==",
            SyntaxOperator::Builtin(crate::core::Builtin::NotEqual) => "<>",
            SyntaxOperator::Builtin(crate::core::Builtin::LessEqual) => "=<",
            SyntaxOperator::Builtin(crate::core::Builtin::Less) => "<",
            SyntaxOperator::PipeForward => "|>",
            SyntaxOperator::PipeBackward => "<|",
            SyntaxOperator::ComposeForward => ">>",
            SyntaxOperator::ComposeBackward => "<<",
            SyntaxOperator::EffectBind => ">>=",
            SyntaxOperator::KleisliCompose => ">=>",
            SyntaxOperator::EffectThen => "=>>",
            SyntaxOperator::Builtin(crate::core::Builtin::Fixpoint) => "fixpoint",
            SyntaxOperator::Builtin(crate::core::Builtin::Anno) => "anno",
            SyntaxOperator::Builtin(crate::core::Builtin::MergeDuplicate) => "merge_duplicate",
            SyntaxOperator::Builtin(crate::core::Builtin::Floor) => "floor",
            SyntaxOperator::Builtin(crate::core::Builtin::Mod) => "mod",
            SyntaxOperator::Builtin(crate::core::Builtin::Slice) => "slice",
            SyntaxOperator::Builtin(crate::core::Builtin::Map) => "map",
            SyntaxOperator::Builtin(crate::core::Builtin::ListLen) => "list.len",
            SyntaxOperator::Builtin(crate::core::Builtin::ListSplit) => "list.split",
            SyntaxOperator::Builtin(crate::core::Builtin::ListSplitEnd) => "list.split_end",
            SyntaxOperator::Builtin(crate::core::Builtin::ListHead) => "list.head",
            SyntaxOperator::Builtin(crate::core::Builtin::ListTail) => "list.tail",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffect) => "list.pure",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectReturn) => "list.pure.r",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectSeq) => "list.pure.seq",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectAlt) => "list.pure.alt",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectCut) => "list.pure.cut",
            SyntaxOperator::Builtin(crate::core::Builtin::ListEffectFix) => "list.pure.fix",
            SyntaxOperator::Builtin(crate::core::Builtin::DictSingleton) => ":",
            SyntaxOperator::Builtin(crate::core::Builtin::DictUnion) => "{,}",
            SyntaxOperator::Builtin(crate::core::Builtin::DictUpdate) => "dict_update",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectSpec) => "object_spec",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectLocalName) => "object_local_name",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectInstanceFromParts) => {
                "object_instance_from_parts"
            }
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectInstance) => "object_instance",
        }
    }

    fn access_if_path(base: SyntaxExpr, suffixes: Vec<PathSuffix>) -> SyntaxExpr {
        match flatten_path_suffixes(suffixes) {
            parts if parts.is_empty() => base,
            parts => SyntaxExpr::Access(Box::new(base), parts),
        }
    }

    let parser = recursive(|expr| {
        let name = glam_name().boxed();
        let expr_name = glam_name()
            .try_map(|name, span| match name.as_str() {
                "and" | "or" => Err(Rich::custom(span, format!("`{name}` is a keyword"))),
                _ => Ok(name),
            })
            .boxed();
        let local = local_name().boxed();

        let single_key_expr = || {
            choice((
                just('\'')
                    .ignore_then(name.clone())
                    .map(SyntaxKeyExpr::Atom),
                expr.clone()
                    .map(|expr| SyntaxKeyExpr::Index(Box::new(expr))),
            ))
        };

        let path_list_shorthand = single_key_expr()
            .padded()
            .separated_by(just(',').padded())
            .allow_leading()
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just('['), just(']'))
            .map(PathSuffix::Expand);
        let path_list_expr = expr
            .clone()
            .padded()
            .delimited_by(just('('), just(')'))
            .map(|expr| PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(expr))));

        // Dotted paths stay lexically tight because `.` has other roles in the
        // language surface, such as future effect sugar like `.bar`.
        let path_suffix = just('.')
            .ignore_then(choice((
                path_list_shorthand,
                path_list_expr,
                expr_name
                    .clone()
                    .map(SyntaxKeyExpr::Atom)
                    .map(PathSuffix::Single),
            )))
            .repeated()
            .collect::<Vec<_>>();

        let prior_name = just('_')
            .ignore_then(expr_name.clone())
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(SyntaxExpr::PriorName(name), suffixes))
            .boxed();
        let escaped_expr = just('^')
            .repeated()
            .at_least(1)
            .collect::<Vec<_>>()
            .then(choice((
                expr.clone().padded().delimited_by(just('('), just(')')),
                expr_name
                    .clone()
                    .then(path_suffix.clone())
                    .map(|(name, suffixes)| access_if_path(SyntaxExpr::Name(name), suffixes)),
            )))
            .then(path_suffix.clone())
            .map(|((carets, escaped), suffixes)| {
                access_if_path(
                    SyntaxExpr::Escape(carets.len(), Box::new(escaped)),
                    suffixes,
                )
            })
            .boxed();
        let name_expr = expr_name
            .clone()
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(SyntaxExpr::Name(name), suffixes))
            .boxed();
        let effect_expr = just('.')
            .ignore_then(name.clone())
            .map(SyntaxExpr::Effect)
            .boxed();

        let number_literal = choice((
            just('_').then(one_of("0123456789")).ignored(),
            one_of("0123456789").ignored(),
        ))
        .then(one_of("0123456789_.xXbBeEaAcCdDfF").repeated().to_slice())
        .to_slice();
        let number = number_literal.try_map(|text: &str, span| {
            Number::parse(text).map(SyntaxExpr::Number).map_err(|err| {
                Rich::custom(span, format!("invalid number literal `{text}`: {err}"))
            })
        });
        let text = quoted_text().map(SyntaxExpr::Text);
        let atom_literal = just('\'').ignore_then(name.clone()).map(SyntaxExpr::Atom);
        let unit = just("()").to(SyntaxExpr::Unit);

        let list = expr
            .clone()
            .padded()
            .separated_by(just(',').padded())
            .allow_leading()
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just('['), just(']'))
            .map(SyntaxExpr::List);

        let dict_item_key = choice((
            name.clone().map(SyntaxKeyExpr::Atom),
            single_key_expr()
                .padded()
                .delimited_by(just('['), just(']')),
        ));
        let dict_item = choice((
            dict_item_key
                .then_ignore(just(':').padded())
                .then(expr.clone())
                .map(|(key, value)| SyntaxExpr::SingletonDict(key, Box::new(value))),
            expr.clone(),
        ));
        let dict = dict_item
            .padded()
            .separated_by(just(',').padded())
            .allow_leading()
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just('{'), just('}'))
            .map(SyntaxExpr::DictUnion);

        let infix_operator = choice((
            text::keyword("and").to(SyntaxOperator::BoolAnd),
            text::keyword("or").to(SyntaxOperator::BoolOr),
            just(">>=").to(SyntaxOperator::EffectBind),
            just(">=>").to(SyntaxOperator::KleisliCompose),
            just("=>>").to(SyntaxOperator::EffectThen),
            just(">=").to(SyntaxOperator::Builtin(crate::core::Builtin::GreaterEqual)),
            just("==").to(SyntaxOperator::Builtin(crate::core::Builtin::Equal)),
            just("<>").to(SyntaxOperator::Builtin(crate::core::Builtin::NotEqual)),
            just("=<").to(SyntaxOperator::Builtin(crate::core::Builtin::LessEqual)),
            just(">>")
                .then_ignore(just('=').not())
                .to(SyntaxOperator::ComposeForward),
            just("<<").to(SyntaxOperator::ComposeBackward),
            just("|>").to(SyntaxOperator::PipeForward),
            just("<|").to(SyntaxOperator::PipeBackward),
            just('>').to(SyntaxOperator::Builtin(crate::core::Builtin::Greater)),
            just('<').to(SyntaxOperator::Builtin(crate::core::Builtin::Less)),
            just("++").to(SyntaxOperator::Builtin(crate::core::Builtin::Append)),
            just('*').to(SyntaxOperator::Builtin(crate::core::Builtin::Multiply)),
            just('/').to(SyntaxOperator::Builtin(crate::core::Builtin::Divide)),
            just('+')
                .then_ignore(just('+').not())
                .to(SyntaxOperator::Builtin(crate::core::Builtin::Add)),
            just('-').to(SyntaxOperator::Builtin(crate::core::Builtin::Subtract)),
        ));
        let prefix_operator_section = infix_operator
            .clone()
            .padded()
            .then(expr.clone())
            .delimited_by(just('('), just(')'))
            .map(|(operator, right)| SyntaxExpr::OperatorSection {
                operator,
                left: None,
                right: Some(Box::new(right)),
            });
        let postfix_operator_section = expr
            .clone()
            .then(infix_operator.clone().padded())
            .delimited_by(just('('), just(')'))
            .map(|(left, operator)| SyntaxExpr::OperatorSection {
                operator,
                left: Some(Box::new(left)),
                right: None,
            });
        let bare_operator_section = infix_operator
            .clone()
            .padded()
            .delimited_by(just('('), just(')'))
            .map(|operator| SyntaxExpr::OperatorSection {
                operator,
                left: None,
                right: None,
            });
        let parenthesized = expr.clone().padded().delimited_by(just('('), just(')'));
        let lambda = just('\\')
            .padded()
            .ignore_then(
                local
                    .clone()
                    .padded()
                    .repeated()
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just("->").padded())
            .then(expr.clone())
            .map(|(params, body)| SyntaxExpr::Lambda(params, Box::new(body)));

        let literal_atom = choice((
            unit,
            text,
            atom_literal,
            list,
            dict,
            number,
            prefix_operator_section,
            postfix_operator_section,
            bare_operator_section,
            parenthesized,
        ))
        .boxed();
        let literal_expr = literal_atom
            .then(path_suffix.clone())
            .map(|(base, suffixes)| access_if_path(base, suffixes))
            .boxed();
        let atom = choice((
            literal_expr,
            escaped_expr,
            effect_expr,
            prior_name,
            name_expr,
        ))
        .boxed();
        let application = atom
            .clone()
            .then(
                whitespace1()
                    .ignore_then(atom.clone())
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(|(function, arguments)| {
                arguments.into_iter().fold(function, |function, argument| {
                    SyntaxExpr::Apply(Box::new(function), Box::new(argument))
                })
            })
            .boxed();
        choice((
            lambda,
            application
                .clone()
                .then(
                    infix_operator
                        .padded()
                        .then(application)
                        .repeated()
                        .collect::<Vec<_>>(),
                )
                .try_map(|(first, rest), span| {
                    resolve_infix_chain(first, rest).map_err(|message| Rich::custom(span, message))
                }),
        ))
    });

    parser
}

fn syntax_binary_expr(operator: SyntaxOperator, left: SyntaxExpr, right: SyntaxExpr) -> SyntaxExpr {
    match operator {
        SyntaxOperator::Builtin(builtin) => match builtin {
            crate::core::Builtin::Append => SyntaxExpr::Append(Box::new(left), Box::new(right)),
            crate::core::Builtin::Add => SyntaxExpr::Add(Box::new(left), Box::new(right)),
            crate::core::Builtin::Subtract => SyntaxExpr::Subtract(Box::new(left), Box::new(right)),
            crate::core::Builtin::Multiply => SyntaxExpr::Multiply(Box::new(left), Box::new(right)),
            crate::core::Builtin::Divide => SyntaxExpr::Divide(Box::new(left), Box::new(right)),
            _ => SyntaxExpr::OperatorApply {
                operator,
                left: Box::new(left),
                right: Box::new(right),
            },
        },
        SyntaxOperator::BoolAnd
        | SyntaxOperator::BoolOr
        | SyntaxOperator::PipeForward
        | SyntaxOperator::PipeBackward
        | SyntaxOperator::ComposeForward
        | SyntaxOperator::ComposeBackward
        | SyntaxOperator::EffectBind
        | SyntaxOperator::KleisliCompose
        | SyntaxOperator::EffectThen => SyntaxExpr::OperatorApply {
            operator,
            left: Box::new(left),
            right: Box::new(right),
        },
    }
}

fn glam_name<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    text::ascii::ident().try_map(|name: &str, span| {
        if name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        {
            Ok(name.to_owned())
        } else {
            Err(Rich::custom(span, "expected name"))
        }
    })
}

fn local_name<'src>() -> impl Parser<'src, &'src str, String, extra::Err<Rich<'src, char>>> {
    choice((
        just('_')
            .ignore_then(glam_name())
            .map(|name| format!("_{name}")),
        just('_').to("_".to_owned()),
        glam_name(),
    ))
}

fn whitespace0<'src>() -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> {
    one_of(" \t\r\n").repeated().ignored()
}

fn whitespace1<'src>() -> impl Parser<'src, &'src str, (), extra::Err<Rich<'src, char>>> {
    one_of(" \t\r\n").repeated().at_least(1).ignored()
}

fn first_word(text: &str) -> Option<&str> {
    text.split(|ch: char| ch.is_whitespace()).next()
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

fn indentation_width(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .map(char::len_utf8)
        .sum()
}

fn strip_indent_width(line: &str, width: usize) -> &str {
    let mut remaining = width;
    for (index, ch) in line.char_indices() {
        if remaining == 0 || !matches!(ch, ' ' | '\t') {
            return &line[index..];
        }
        remaining = remaining.saturating_sub(ch.len_utf8());
    }
    ""
}

fn is_dedent_closer(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.chars().all(|ch| matches!(ch, '}' | ']' | ')'))
}

fn line_ending_diagnostics(text: &str) -> Vec<Diagnostic> {
    let mut has_lf = false;
    let mut has_crlf = false;
    let mut has_cr = false;
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                has_crlf = true;
                index += 2;
            }
            b'\r' => {
                has_cr = true;
                index += 1;
            }
            b'\n' => {
                has_lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }

    let kinds = [has_lf, has_crlf, has_cr]
        .into_iter()
        .filter(|present| *present)
        .count();

    if kinds > 1 {
        vec![Diagnostic::warn(1, "source uses inconsistent line endings")]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct PhysicalLine<'a> {
    number: usize,
    text: &'a str,
}

fn split_lines(text: &str) -> Vec<PhysicalLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut number = 1;
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                lines.push(PhysicalLine {
                    number,
                    text: &text[start..index],
                });
                index += 2;
                start = index;
                number += 1;
            }
            b'\r' | b'\n' => {
                lines.push(PhysicalLine {
                    number,
                    text: &text[start..index],
                });
                index += 1;
                start = index;
                number += 1;
            }
            _ => index += 1,
        }
    }

    if start < text.len() || text.is_empty() {
        lines.push(PhysicalLine {
            number,
            text: &text[start..],
        });
    }

    lines
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::compiler::CompileContext;
    use crate::core::{Builtin, Dict, Expr as CoreExpr, Key, KeyExpr as CoreKeyExpr, Value};
    use crate::number::Number;

    fn core_global_access(context: &CompileContext, path: Vec<CoreKeyExpr>) -> CoreExpr {
        let Value::Expr(thunk) = &context.final_defs else {
            panic!("final module binding should be a lazy expression");
        };
        CoreExpr::Access(thunk.expr.clone(), Arc::from(path))
    }

    fn evaluated_module_value(context: &CompileContext, lowered: &LoweredSource) -> Value {
        let Value::Expr(thunk) = &context.final_defs else {
            panic!("final module binding should be a lazy expression");
        };
        let crate::core::Expr::Future(ivar) = &(*thunk.expr) else {
            panic!("final module binding should be a future expression");
        };
        ivar.set(lowered.definitions.clone())
            .expect("future should not be set yet");
        crate::eval::eval_value(&lowered.definitions).expect("lowered module should evaluate")
    }

    fn value_at_atom_path(definitions: &Value, path: &[&str]) -> Option<Value> {
        let mut current = definitions.clone();
        for part in path {
            let current_value = crate::eval::eval_value(&current).ok()?;
            let Value::Dict(dict) = current_value else {
                return None;
            };
            current = dict
                .get(&Key::Atom(Atom::from_key(&Key::binary_from_text(*part))))
                .cloned()?;
        }
        Some(current)
    }

    fn resolved_value_at_path(definitions: &Value, path: &[&str]) -> Value {
        let value = value_at_atom_path(definitions, path).expect("binding should exist");
        crate::eval::eval_value(&value).expect("binding should resolve")
    }

    fn fully_evaluated_value(mut value: Value) -> Value {
        while matches!(value, Value::Expr(_)) {
            value = crate::eval::eval_value(&value).expect("value should fully evaluate");
        }
        value
    }

    fn output_bytes(value: &Value) -> Vec<u8> {
        match value {
            Value::Binary(bytes) => bytes.to_vec(),
            Value::List(list) => {
                crate::eval::list_output_bytes(list).expect("output list should render as bytes")
            }
            other => panic!("expected binary output value, got {other:?}"),
        }
    }

    fn output_binary_result_list(value: &Value) -> Vec<u8> {
        let Value::List(list) = value else {
            panic!("expected list output value, got {value:?}");
        };
        let bytes = std::cell::RefCell::new(Vec::new());
        list.try_for_each_segment(
            &mut |segment| {
                bytes.borrow_mut().extend_from_slice(segment);
                Ok::<_, String>(())
            },
            &mut |values| {
                for value in values {
                    let value = fully_evaluated_value(
                        crate::eval::eval_value(value).map_err(|err| err.to_string())?,
                    );
                    bytes.borrow_mut().extend(output_bytes(&value));
                }
                Ok(())
            },
            &mut |thunk| match crate::eval::eval_value(&Value::Expr(thunk.clone()))
                .map_err(|err| err.to_string())?
            {
                Value::Binary(bytes) => Ok(crate::core::List::from_bytes(bytes)),
                Value::List(list) => Ok(list),
                other => Err(format!(
                    "lazy output chunk was not a list or binary: {other:?}"
                )),
            },
        )
        .expect("result list should render as binary values");
        bytes.into_inner()
    }

    use super::*;
    use crate::diagnostic::Severity;

    fn parse(text: &str) -> ParsedSource {
        SourceFile::new("test.g", text).parse()
    }

    fn parse_with_context(text: &str, context: &CompileContext) -> ParsedSource {
        SourceFile::new("test.g", text).parse_with_context(context)
    }

    fn lower_with_module_path(text: &str, module_path: &[&str]) -> LoweredSource {
        let parsed = parse(text);
        let context = CompileContext::from_module_path(module_path.iter().copied());
        lower_to_core_with_context(&parsed, &context)
    }

    fn abstract_path_atom(parts: &[&str]) -> Value {
        Value::Atom(Atom::from_key(&Key::abstract_global_path(
            parts.iter().copied(),
        )))
    }

    fn n(value: i64) -> Number {
        value.into()
    }

    #[test]
    fn parses_language_declaration_with_extensions() {
        let parsed = parse("language g0 with utf8, demo\nanswer = 42\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[0].kind,
            DeclarationKind::Language(LanguageDecl {
                base: "g0".to_owned(),
                extensions: vec!["utf8".to_owned(), "demo".to_owned()],
            })
        );
    }

    #[test]
    fn groups_indented_continuation_lines() {
        let parsed = parse("language g0\nfoo = do\n  .bar\n  .baz\nqux := 1\n");

        assert_eq!(parsed.declarations.len(), 3);
        assert_eq!(parsed.declarations[1].text, "foo = do\n.bar\n.baz");
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "qux".to_owned(),
                kind: DefinitionKind::Override,
                body: "1".to_owned(),
                expr: Some(SyntaxExpr::Number(n(1))),
            })
        );
    }

    #[test]
    fn parses_local_imports() {
        let parsed = parse("language g0\nimport \"minimal.g\" as conf\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Import(ImportDecl {
                reference: ImportReference::Local("minimal.g".to_owned()),
                binary: false,
                placement: ImportPlacement::As("conf".to_owned()),
            })
        );

        let parsed = parse("language g0\nimport \"payload.bin\" binary as payload\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Import(ImportDecl {
                reference: ImportReference::Local("payload.bin".to_owned()),
                binary: true,
                placement: ImportPlacement::As("payload".to_owned()),
            })
        );
    }

    #[test]
    fn parses_builtin_imports() {
        let parsed = parse("language g0\nimport 'std as std\nimport 'math\nimport 'list as list\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Import(ImportDecl {
                reference: ImportReference::Builtin("std".to_owned()),
                binary: false,
                placement: ImportPlacement::As("std".to_owned()),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Import(ImportDecl {
                reference: ImportReference::Builtin("math".to_owned()),
                binary: false,
                placement: ImportPlacement::Inline,
            })
        );
    }

    #[test]
    fn parses_unique_declarations() {
        let parsed = parse("language g0\nunique Foo, palette.Blue\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Unique(vec!["Foo".to_owned(), "palette.Blue".to_owned()])
        );
    }

    #[test]
    fn parses_named_object_declarations() {
        let parsed = parse(
            "language g0\nobject child extends base, mixin with\n  text = \"Hello\"\n  target := \"World\"\n",
        );

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Object(ObjectDecl {
                target: "child".to_owned(),
                alias: None,
                deps: vec!["base".to_owned(), "mixin".to_owned()],
                body: vec![
                    ObjectBodyDefinition {
                        line: 3,
                        text: "text = \"Hello\"".to_owned(),
                        kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                            target: "text".to_owned(),
                            kind: DefinitionKind::Introduce,
                            body: "\"Hello\"".to_owned(),
                            expr: Some(SyntaxExpr::Text("Hello".to_owned())),
                        }),
                    },
                    ObjectBodyDefinition {
                        line: 4,
                        text: "target := \"World\"".to_owned(),
                        kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                            target: "target".to_owned(),
                            kind: DefinitionKind::Override,
                            body: "\"World\"".to_owned(),
                            expr: Some(SyntaxExpr::Text("World".to_owned())),
                        }),
                    },
                ],
            })
        );
    }

    #[test]
    fn parses_extend_declarations() {
        let parsed =
            parse("language g0\nextend child with\n  text := _text ++ \"!\"\n  tail = \"done\"\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Extend(ObjectExtendDecl {
                target: "child".to_owned(),
                alias: None,
                body: vec![
                    ObjectBodyDefinition {
                        line: 3,
                        text: "text := _text ++ \"!\"".to_owned(),
                        kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                            target: "text".to_owned(),
                            kind: DefinitionKind::Override,
                            body: "_text ++ \"!\"".to_owned(),
                            expr: Some(SyntaxExpr::Append(
                                Box::new(SyntaxExpr::PriorName("text".to_owned())),
                                Box::new(SyntaxExpr::Text("!".to_owned())),
                            )),
                        }),
                    },
                    ObjectBodyDefinition {
                        line: 4,
                        text: "tail = \"done\"".to_owned(),
                        kind: ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                            target: "tail".to_owned(),
                            kind: DefinitionKind::Introduce,
                            body: "\"done\"".to_owned(),
                            expr: Some(SyntaxExpr::Text("done".to_owned())),
                        }),
                    },
                ],
            })
        );
    }

    #[test]
    fn parses_hierarchical_object_body_declarations() {
        let parsed = parse(
            "language g0\nobject parent with\n  object child with\n    text = \"Hello\"\n  tail = \"done\"\n",
        );

        assert_eq!(parsed.diagnostics, []);
        let DeclarationKind::Object(parent) = &parsed.declarations[1].kind else {
            panic!("parent should parse as an object declaration");
        };
        assert_eq!(parent.body.len(), 2);
        let ObjectBodyDefinitionKind::Object(child) = &parent.body[0].kind else {
            panic!("first parent body item should parse as a nested object");
        };
        assert_eq!(child.target, "child");
        assert_eq!(child.body.len(), 1);
        assert_eq!(child.body[0].text, "text = \"Hello\"");
    }

    #[test]
    fn parses_object_expressions() {
        let parsed = parse(
            "language g0\nhello = object \"hello\" as _h extends base with\n  text = h.target\n",
        );

        assert_eq!(parsed.diagnostics, []);
        assert!(matches!(
            &parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                expr: Some(SyntaxExpr::Object(ObjectExpr {
                    name: Some(_),
                    alias: Some(alias),
                    deps,
                    body,
                })),
                ..
            }) if alias == "_h" && deps.len() == 1 && body.len() == 1
        ));
    }

    #[test]
    fn parses_object_and_extend_aliases() {
        let parsed = parse(
            "language g0\nobject child as _c extends base with\n  text = c.base\nextend child as _c with\n  text := _c.text ++ \"!\"\n",
        );

        assert_eq!(parsed.diagnostics, []);
        match &parsed.declarations[1].kind {
            DeclarationKind::Object(object) => {
                assert_eq!(object.target, "child");
                assert_eq!(object.alias.as_deref(), Some("_c"));
                assert_eq!(object.deps, ["base".to_owned()]);
            }
            other => panic!("expected object declaration, got {other:?}"),
        }
        match &parsed.declarations[2].kind {
            DeclarationKind::Extend(extend) => {
                assert_eq!(extend.target, "child");
                assert_eq!(extend.alias.as_deref(), Some("_c"));
            }
            other => panic!("expected extend declaration, got {other:?}"),
        }
    }

    #[test]
    fn object_declaration_aliases_follow_local_unused_warning_rules() {
        let parsed = parse(
            "language g0\nobject a as unused with\n  x = 1\nobject b as _unused with\n  x = 1\nobject c as _ with\n  x = 1\nobject d as used with\n  x = used.y\nextend d as update_unused with\n  x := 2\nextend d as _update_unused with\n  x := 3\n",
        );

        assert_eq!(
            parsed
                .diagnostics
                .iter()
                .filter(|diag| diag.message.contains("unused local"))
                .count(),
            2
        );
        assert!(parsed.diagnostics.iter().any(|diag| {
            diag.severity == Severity::Warning
                && diag.line == 2
                && diag.message == "unused local `unused`"
        }));
        assert!(parsed.diagnostics.iter().any(|diag| {
            diag.severity == Severity::Warning
                && diag.line == 10
                && diag.message == "unused local `update_unused`"
        }));
    }

    #[test]
    fn reports_missing_language_declaration() {
        let parsed = parse("foo = 1\n");

        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(parsed.diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn warns_on_inconsistent_line_endings() {
        let parsed = parse("language g0\r\nfoo = 1\n");

        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("inconsistent line endings"))
        );
    }

    #[test]
    fn parses_definition_forms() {
        assert_eq!(
            definition_decl().parse("foo = 1").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "1".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("foo := 1").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Override,
                body: "1".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("foo ::= f").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Update,
                body: "f".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("foo x y = x + y").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\\ x y -> x + y".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("skip _ y = y").into_result(),
            Ok(DefinitionDecl {
                target: "skip".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\\ _ y -> y".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("keep _value = value").into_result(),
            Ok(DefinitionDecl {
                target: "keep".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\\ _value -> value".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("foo x := x").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Override,
                body: "\\ x -> x".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse("foo x ::= update").into_result(),
            Ok(DefinitionDecl {
                target: "foo".to_owned(),
                kind: DefinitionKind::Update,
                body: "\\ x -> update".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl().parse(".[idx] = value").into_result(),
            Ok(DefinitionDecl {
                target: ".[idx]".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "value".to_owned(),
                expr: None,
            })
        );
        assert_eq!(
            definition_decl()
                .parse("foo.([1,2] ++ [3]) := value")
                .into_result(),
            Ok(DefinitionDecl {
                target: "foo.([1,2] ++ [3])".to_owned(),
                kind: DefinitionKind::Override,
                body: "value".to_owned(),
                expr: None,
            })
        );
    }

    #[test]
    fn parses_inline_text_literal_expressions() {
        let parsed = parse("language g0\nasm.result = \"Hello, World!\"\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\"Hello, World!\"".to_owned(),
                expr: Some(SyntaxExpr::Text("Hello, World!".to_owned())),
            })
        );
    }

    #[test]
    fn parses_atom_literal_expressions() {
        let parsed = parse("language g0\nanswer = 'deque\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "answer".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "'deque".to_owned(),
                expr: Some(SyntaxExpr::Atom("deque".to_owned())),
            })
        );
    }

    #[test]
    fn parses_number_literals() {
        let parsed = parse(
            "language g0\nanswer = 42\nneg = _42\nhex = 0xc0de\nbits = 0b1011_1010\nscaled = 1.234e_7\nexact = 1/6\n",
        );

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "answer".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "42".to_owned(),
                expr: Some(SyntaxExpr::Number(n(42))),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "neg".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "_42".to_owned(),
                expr: Some(SyntaxExpr::Number(Number::parse("_42").unwrap())),
            })
        );
        assert_eq!(
            parsed.declarations[3].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "hex".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "0xc0de".to_owned(),
                expr: Some(SyntaxExpr::Number(Number::parse("0xc0de").unwrap())),
            })
        );
        assert_eq!(
            parsed.declarations[4].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "bits".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "0b1011_1010".to_owned(),
                expr: Some(SyntaxExpr::Number(Number::parse("0b1011_1010").unwrap())),
            })
        );
        assert_eq!(
            parsed.declarations[5].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "scaled".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "1.234e_7".to_owned(),
                expr: Some(SyntaxExpr::Number(Number::parse("1.234e_7").unwrap())),
            })
        );
        assert_eq!(
            parsed.declarations[6].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "exact".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "1/6".to_owned(),
                expr: Some(SyntaxExpr::Divide(
                    Box::new(SyntaxExpr::Number(n(1))),
                    Box::new(SyntaxExpr::Number(n(6))),
                )),
            })
        );
    }

    #[test]
    fn parses_list_and_append_expressions() {
        let parsed = parse("language g0\nbytes = [1, 2] ++ [3, 4]\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "bytes".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "[1, 2] ++ [3, 4]".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::List(vec![
                        SyntaxExpr::Number(n(1)),
                        SyntaxExpr::Number(n(2)),
                    ])),
                    Box::new(SyntaxExpr::List(vec![
                        SyntaxExpr::Number(n(3)),
                        SyntaxExpr::Number(n(4)),
                    ])),
                )),
            })
        );
    }

    #[test]
    fn parses_arithmetic_with_precedence() {
        let parsed = parse("language g0\nanswer = (1 + 2 * 3) - (4 / 5)\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "answer".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "(1 + 2 * 3) - (4 / 5)".to_owned(),
                expr: Some(SyntaxExpr::Subtract(
                    Box::new(SyntaxExpr::Add(
                        Box::new(SyntaxExpr::Number(n(1))),
                        Box::new(SyntaxExpr::Multiply(
                            Box::new(SyntaxExpr::Number(n(2))),
                            Box::new(SyntaxExpr::Number(n(3))),
                        )),
                    )),
                    Box::new(SyntaxExpr::Divide(
                        Box::new(SyntaxExpr::Number(n(4))),
                        Box::new(SyntaxExpr::Number(n(5))),
                    )),
                )),
            })
        );
    }

    #[test]
    fn parses_name_and_append_expressions() {
        let parsed = parse("language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\"\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "hello ++ \", \" ++ world ++ \"!\"".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Append(
                        Box::new(SyntaxExpr::Append(
                            Box::new(SyntaxExpr::Name("hello".to_owned())),
                            Box::new(SyntaxExpr::Text(", ".to_owned())),
                        )),
                        Box::new(SyntaxExpr::Name("world".to_owned())),
                    )),
                    Box::new(SyntaxExpr::Text("!".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn parses_escaped_object_scope_names() {
        assert_eq!(
            parse_expr("^prefix.value"),
            Some(SyntaxExpr::Escape(
                1,
                Box::new(SyntaxExpr::Access(
                    Box::new(SyntaxExpr::Name("prefix".to_owned())),
                    vec![SyntaxKeyExpr::Atom("value".to_owned())],
                )),
            ))
        );
        assert_eq!(
            parse_expr("^^prefix"),
            Some(SyntaxExpr::Escape(
                2,
                Box::new(SyntaxExpr::Name("prefix".to_owned())),
            ))
        );
        assert_eq!(
            parse_expr("^(prefix ++ suffix).tail"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::Escape(
                    1,
                    Box::new(SyntaxExpr::Append(
                        Box::new(SyntaxExpr::Name("prefix".to_owned())),
                        Box::new(SyntaxExpr::Name("suffix".to_owned())),
                    )),
                )),
                vec![SyntaxKeyExpr::Atom("tail".to_owned())],
            ))
        );
    }

    #[test]
    fn parses_prior_name_expressions_only_at_name_roots() {
        let parsed = parse("language g0\nasm.result = _hello ++ _world.tail\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "_hello ++ _world.tail".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::PriorName("hello".to_owned())),
                    Box::new(SyntaxExpr::Access(
                        Box::new(SyntaxExpr::PriorName("world".to_owned())),
                        vec![SyntaxKeyExpr::Atom("tail".to_owned())],
                    )),
                )),
            })
        );

        assert_eq!(parse_expr("foo._bar"), None);
        assert_eq!(parse_expr("_foo._bar"), None);
    }

    #[test]
    fn parses_lambda_and_application_expressions() {
        let parsed = parse("language g0\nasm.result = (\\x -> x) \"Hello\"\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "(\\x -> x) \"Hello\"".to_owned(),
                expr: Some(SyntaxExpr::Apply(
                    Box::new(SyntaxExpr::Lambda(
                        vec!["x".to_owned()],
                        Box::new(SyntaxExpr::Name("x".to_owned())),
                    )),
                    Box::new(SyntaxExpr::Text("Hello".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn parses_effect_shorthand_expressions() {
        assert_eq!(parse_expr("()"), Some(SyntaxExpr::Unit));
        assert_eq!(
            parse_expr(".emit"),
            Some(SyntaxExpr::Effect("emit".to_owned()))
        );
        assert_eq!(
            parse_expr(".emit 'eax 42"),
            Some(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Apply(
                    Box::new(SyntaxExpr::Effect("emit".to_owned())),
                    Box::new(SyntaxExpr::Atom("eax".to_owned())),
                )),
                Box::new(SyntaxExpr::Number(n(42))),
            ))
        );
    }

    #[test]
    fn parses_operator_sections() {
        assert_eq!(
            parse_expr("(+ 42)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Add),
                left: None,
                right: Some(Box::new(SyntaxExpr::Number(n(42)))),
            })
        );
        assert_eq!(
            parse_expr("(42 -)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Subtract),
                left: Some(Box::new(SyntaxExpr::Number(n(42)))),
                right: None,
            })
        );
        assert_eq!(
            parse_expr("(++ suffix)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Append),
                left: None,
                right: Some(Box::new(SyntaxExpr::Name("suffix".to_owned()))),
            })
        );
        assert_eq!(
            parse_expr("(+)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::Builtin(Builtin::Add),
                left: None,
                right: None,
            })
        );
    }

    #[test]
    fn parses_pipe_and_composition_operators() {
        assert_eq!(
            parse_expr("value |> f"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::PipeForward,
                left: Box::new(SyntaxExpr::Name("value".to_owned())),
                right: Box::new(SyntaxExpr::Name("f".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("f <| value"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::PipeBackward,
                left: Box::new(SyntaxExpr::Name("f".to_owned())),
                right: Box::new(SyntaxExpr::Name("value".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("f >> g"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::ComposeForward,
                left: Box::new(SyntaxExpr::Name("f".to_owned())),
                right: Box::new(SyntaxExpr::Name("g".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("g << f"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::ComposeBackward,
                left: Box::new(SyntaxExpr::Name("g".to_owned())),
                right: Box::new(SyntaxExpr::Name("f".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("op >>= k"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::EffectBind,
                left: Box::new(SyntaxExpr::Name("op".to_owned())),
                right: Box::new(SyntaxExpr::Name("k".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("k1 >=> k2"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::KleisliCompose,
                left: Box::new(SyntaxExpr::Name("k1".to_owned())),
                right: Box::new(SyntaxExpr::Name("k2".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("op =>> next"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::EffectThen,
                left: Box::new(SyntaxExpr::Name("op".to_owned())),
                right: Box::new(SyntaxExpr::Name("next".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("(|> f)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::PipeForward,
                left: None,
                right: Some(Box::new(SyntaxExpr::Name("f".to_owned()))),
            })
        );
        assert_eq!(
            parse_expr("(>>)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::ComposeForward,
                left: None,
                right: None,
            })
        );
        assert_eq!(
            parse_expr("(>>= k)"),
            Some(SyntaxExpr::OperatorSection {
                operator: SyntaxOperator::EffectBind,
                left: None,
                right: Some(Box::new(SyntaxExpr::Name("k".to_owned()))),
            })
        );
    }

    #[test]
    fn parses_comparison_and_boolean_operators() {
        assert_eq!(
            parse_expr("x < y"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::Builtin(Builtin::Less),
                left: Box::new(SyntaxExpr::Name("x".to_owned())),
                right: Box::new(SyntaxExpr::Name("y".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("x >= y"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::Builtin(Builtin::GreaterEqual),
                left: Box::new(SyntaxExpr::Name("x".to_owned())),
                right: Box::new(SyntaxExpr::Name("y".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("x and y or z"),
            Some(SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::BoolOr,
                left: Box::new(SyntaxExpr::OperatorApply {
                    operator: SyntaxOperator::BoolAnd,
                    left: Box::new(SyntaxExpr::Name("x".to_owned())),
                    right: Box::new(SyntaxExpr::Name("y".to_owned())),
                }),
                right: Box::new(SyntaxExpr::Name("z".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("x < y =< z"),
            Some(SyntaxExpr::ComparisonChain {
                first: Box::new(SyntaxExpr::Name("x".to_owned())),
                rest: vec![
                    (
                        SyntaxOperator::Builtin(Builtin::Less),
                        SyntaxExpr::Name("y".to_owned()),
                    ),
                    (
                        SyntaxOperator::Builtin(Builtin::LessEqual),
                        SyntaxExpr::Name("z".to_owned()),
                    ),
                ],
            })
        );
        assert_eq!(parse_expr("and"), None);
        assert_eq!(
            parse_expr("android"),
            Some(SyntaxExpr::Name("android".to_owned()))
        );
        assert_eq!(parse_expr("'and"), Some(SyntaxExpr::Atom("and".to_owned())));
    }

    #[test]
    fn parses_local_root_paths_inside_lambda_bodies() {
        let parsed = parse("language g0\nasm.result = \\x -> x.tail\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\\x -> x.tail".to_owned(),
                expr: Some(SyntaxExpr::Lambda(
                    vec!["x".to_owned()],
                    Box::new(SyntaxExpr::Access(
                        Box::new(SyntaxExpr::Name("x".to_owned())),
                        vec![SyntaxKeyExpr::Atom("tail".to_owned())],
                    )),
                )),
            })
        );
    }

    #[test]
    fn parses_explicit_lambda_underscore_local_conventions() {
        let parsed = parse("language g0\nasm.result = (\\ _value _ -> value) 1 2\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "(\\ _value _ -> value) 1 2".to_owned(),
                expr: Some(SyntaxExpr::Apply(
                    Box::new(SyntaxExpr::Apply(
                        Box::new(SyntaxExpr::Lambda(
                            vec!["_value".to_owned(), "_".to_owned()],
                            Box::new(SyntaxExpr::Name("value".to_owned())),
                        )),
                        Box::new(SyntaxExpr::Number(n(1))),
                    )),
                    Box::new(SyntaxExpr::Number(n(2))),
                )),
            })
        );
    }

    #[test]
    fn parses_let_and_where_expressions() {
        assert_eq!(
            parse_expr("let x = 1 in x + x"),
            Some(SyntaxExpr::Let {
                bindings: vec![("x".to_owned(), SyntaxExpr::Number(n(1)))],
                body: Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                )),
            })
        );
        assert_eq!(
            parse_expr("let x = 1; _y = 2 in x"),
            Some(SyntaxExpr::Let {
                bindings: vec![
                    ("x".to_owned(), SyntaxExpr::Number(n(1))),
                    ("_y".to_owned(), SyntaxExpr::Number(n(2))),
                ],
                body: Box::new(SyntaxExpr::Name("x".to_owned())),
            })
        );
        assert_eq!(
            parse_expr("let x = 1\ny = 2\nx + y"),
            Some(SyntaxExpr::Let {
                bindings: vec![
                    ("x".to_owned(), SyntaxExpr::Number(n(1))),
                    ("y".to_owned(), SyntaxExpr::Number(n(2))),
                ],
                body: Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    Box::new(SyntaxExpr::Name("y".to_owned())),
                )),
            })
        );
        assert_eq!(
            parse_expr("x + y where x = 1; y = 2"),
            Some(SyntaxExpr::Let {
                bindings: vec![
                    ("x".to_owned(), SyntaxExpr::Number(n(1))),
                    ("y".to_owned(), SyntaxExpr::Number(n(2))),
                ],
                body: Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                    Box::new(SyntaxExpr::Name("y".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn parses_definition_argument_sugar() {
        let parsed = parse("language g0\nid x = x\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "id".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\\ x -> x".to_owned(),
                expr: Some(SyntaxExpr::Lambda(
                    vec!["x".to_owned()],
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn parses_update_definition_argument_sugar() {
        let parsed = parse("language g0\nid x ::= x\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "id".to_owned(),
                kind: DefinitionKind::Update,
                body: "\\ x -> x".to_owned(),
                expr: Some(SyntaxExpr::Lambda(
                    vec!["x".to_owned()],
                    Box::new(SyntaxExpr::Name("x".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn warns_on_unused_locals_without_underscore_prefix() {
        let parsed = parse("language g0\nid x = 42\nasm.result = (\\y -> \"ok\") 1\n");

        assert!(parsed.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.line == 2
                && diagnostic.message.contains("unused local `x`")
        }));
        assert!(parsed.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.line == 3
                && diagnostic.message.contains("unused local `y`")
        }));
    }

    #[test]
    fn let_bindings_follow_local_unused_warning_rules() {
        let parsed =
            parse("language g0\nasm.result = let unused = 1; _suppressed = 2; _ = 3 in 4\n");

        let warnings = parsed
            .diagnostics
            .iter()
            .filter(|diag| diag.message.contains("unused local"))
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].message, "unused local `unused`");
    }

    #[test]
    fn underscore_locals_suppress_unused_warnings_and_drop_is_allowed() {
        let parsed = parse("language g0\nkeep _value = value\nskip _ y = y\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "keep".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\\ _value -> value".to_owned(),
                expr: Some(SyntaxExpr::Lambda(
                    vec!["_value".to_owned()],
                    Box::new(SyntaxExpr::Name("value".to_owned())),
                )),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "skip".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "\\ _ y -> y".to_owned(),
                expr: Some(SyntaxExpr::Lambda(
                    vec!["_".to_owned(), "y".to_owned()],
                    Box::new(SyntaxExpr::Name("y".to_owned())),
                )),
            })
        );
    }

    #[test]
    fn parses_dictionary_literals() {
        let parsed = parse("language g0\nd = { hello:\"Hello\", world:\"World\" }\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{ hello:\"Hello\", world:\"World\" }".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("hello".to_owned()),
                        Box::new(SyntaxExpr::Text("Hello".to_owned())),
                    ),
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("world".to_owned()),
                        Box::new(SyntaxExpr::Text("World".to_owned())),
                    ),
                ])),
            })
        );
    }

    #[test]
    fn parses_dictionary_unions() {
        let parsed = parse("language g0\nd = { left, right, hello:\"Hello\" }\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{ left, right, hello:\"Hello\" }".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![
                    SyntaxExpr::Name("left".to_owned()),
                    SyntaxExpr::Name("right".to_owned()),
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("hello".to_owned()),
                        Box::new(SyntaxExpr::Text("Hello".to_owned())),
                    ),
                ])),
            })
        );
    }

    #[test]
    fn parses_multiline_literals_with_leading_commas() {
        let parsed = parse(
            "language g0\nnums = [\n  , 1\n  , 2\n  ]\nd = {\n  , hello:\"Hello\"\n  , world:\"World\"\n  }\n",
        );

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "nums".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "[\n, 1\n, 2\n]".to_owned(),
                expr: Some(SyntaxExpr::List(vec![
                    SyntaxExpr::Number(n(1)),
                    SyntaxExpr::Number(n(2)),
                ])),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{\n, hello:\"Hello\"\n, world:\"World\"\n}".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("hello".to_owned()),
                        Box::new(SyntaxExpr::Text("Hello".to_owned())),
                    ),
                    SyntaxExpr::SingletonDict(
                        SyntaxKeyExpr::Atom("world".to_owned()),
                        Box::new(SyntaxExpr::Text("World".to_owned())),
                    ),
                ])),
            })
        );
    }

    #[test]
    fn parses_expression_indexed_names_and_keys() {
        let parsed =
            parse("language g0\nd = { [42]:\"World\" }\nasm.result = d.[42] ++ d.['tail]\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "d".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "{ [42]:\"World\" }".to_owned(),
                expr: Some(SyntaxExpr::DictUnion(vec![SyntaxExpr::SingletonDict(
                    SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(42)))),
                    Box::new(SyntaxExpr::Text("World".to_owned())),
                )])),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "d.[42] ++ d.['tail]".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Access(
                        Box::new(SyntaxExpr::Name("d".to_owned())),
                        vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(42))))],
                    )),
                    Box::new(SyntaxExpr::Access(
                        Box::new(SyntaxExpr::Name("d".to_owned())),
                        vec![SyntaxKeyExpr::Atom("tail".to_owned())],
                    )),
                )),
            })
        );
    }

    #[test]
    fn parses_path_list_shorthand_and_general_list_path_exprs() {
        let parsed = parse("language g0\nasm.result = foo.[1,2,3] ++ foo.([1,2] ++ [3])\n");

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "foo.[1,2,3] ++ foo.([1,2] ++ [3])".to_owned(),
                expr: Some(SyntaxExpr::Append(
                    Box::new(SyntaxExpr::Access(
                        Box::new(SyntaxExpr::Name("foo".to_owned())),
                        vec![
                            SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(1)))),
                            SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(2)))),
                            SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(3)))),
                        ],
                    )),
                    Box::new(SyntaxExpr::Access(
                        Box::new(SyntaxExpr::Name("foo".to_owned())),
                        vec![SyntaxKeyExpr::PathIndex(Box::new(SyntaxExpr::Append(
                            Box::new(SyntaxExpr::List(vec![
                                SyntaxExpr::Number(n(1)),
                                SyntaxExpr::Number(n(2)),
                            ])),
                            Box::new(SyntaxExpr::List(vec![SyntaxExpr::Number(n(3))])),
                        )))],
                    )),
                )),
            })
        );
    }

    #[test]
    fn dotted_paths_require_tight_dots() {
        assert!(matches!(
            parse_expr("foo.[  42  ].bar"),
            Some(SyntaxExpr::Access(_, _))
        ));
        assert!(matches!(
            parse_expr("foo.([1,2] ++ [3]).bar"),
            Some(SyntaxExpr::Access(_, _))
        ));

        assert_eq!(
            parse_expr("foo  .[42].bar"),
            None,
            "whitespace before `.` should be rejected"
        );
        assert_eq!(
            parse_expr("foo .bar"),
            Some(SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Name("foo".to_owned())),
                Box::new(SyntaxExpr::Effect("bar".to_owned())),
            )),
            "whitespace before `.` should parse `.bar` as a separate effect expression"
        );
        assert_eq!(
            parse_expr("foo.[42].  bar"),
            None,
            "whitespace after `.` should be rejected"
        );
        assert_eq!(
            parse_expr("foo. bar"),
            None,
            "whitespace after `.` should prevent dotted-path parsing"
        );
        assert_eq!(
            parse_expr("foo. [42].bar"),
            None,
            "whitespace between `.` and `[` should be rejected"
        );
        assert_eq!(
            parse_expr("foo. ([1,2] ++ [3]).bar"),
            None,
            "whitespace between `.` and `(` should be rejected"
        );
    }

    #[test]
    fn parses_dotted_paths_on_literal_expressions() {
        assert_eq!(
            parse_expr("{ hello:\"Hello\" }.hello"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::DictUnion(vec![SyntaxExpr::SingletonDict(
                    SyntaxKeyExpr::Atom("hello".to_owned()),
                    Box::new(SyntaxExpr::Text("Hello".to_owned())),
                )])),
                vec![SyntaxKeyExpr::Atom("hello".to_owned())],
            ))
        );
        assert_eq!(
            parse_expr("[\"Hello\"].[0]"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::List(vec![SyntaxExpr::Text("Hello".to_owned())])),
                vec![SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(0))))],
            ))
        );
        assert_eq!(
            parse_expr("(foo).bar"),
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::Name("foo".to_owned())),
                vec![SyntaxKeyExpr::Atom("bar".to_owned())],
            ))
        );
    }

    #[test]
    fn parses_dictionary_with_expressions() {
        assert!(matches!(
            parse_expr("{ x:1 } with\nx := 2\ny = x + 1"),
            Some(SyntaxExpr::With {
                alias: None,
                body,
                ..
            }) if body.len() == 2
        ));
        assert!(matches!(
            parse_expr("d as prior with\nx := _prior.x + 1"),
            Some(SyntaxExpr::With {
                alias: Some(alias),
                body,
                ..
            }) if alias == "prior" && body.len() == 1
        ));
        assert!(matches!(
            parse_expr("d as _prior with\nx := _prior.x + 1"),
            Some(SyntaxExpr::With {
                alias: Some(alias),
                body,
                ..
            }) if alias == "_prior" && body.len() == 1
        ));
        assert!(matches!(
            parse_expr("d as _ with\nx := 1"),
            Some(SyntaxExpr::With {
                alias: None,
                body,
                ..
            }) if body.len() == 1
        ));
    }

    #[test]
    fn reports_ambiguous_slash_chains_as_parse_errors() {
        let parsed = parse("language g0\nasm.result = 3/4/5\n");

        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diag| diag.line == 2 && diag.message.contains("non-associative"))
        );
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "3/4/5".to_owned(),
                expr: None,
            })
        );
    }

    #[test]
    fn reports_ambiguous_subtract_chains_as_parse_errors() {
        let parsed = parse("language g0\nasm.result = 3 - 4 - 5\n");

        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diag| diag.line == 2
                    && diag.message.contains("operator `-` is non-associative"))
        );
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "3 - 4 - 5".to_owned(),
                expr: None,
            })
        );
    }

    #[test]
    fn reports_mixed_add_subtract_chains_as_parse_errors() {
        let parsed = parse("language g0\nasm.result = 3 + 1 - 4 + 1 - 5 + 1\n");

        assert!(parsed.diagnostics.iter().any(|diag| {
            diag.line == 2
                && diag
                    .message
                    .contains("operators `+` and `-` have no precedence relationship")
        }));
        assert_eq!(
            parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target: "asm.result".to_owned(),
                kind: DefinitionKind::Introduce,
                body: "3 + 1 - 4 + 1 - 5 + 1".to_owned(),
                expr: None,
            })
        );
    }

    #[test]
    fn reports_mixed_pipe_and_composition_directions_as_parse_errors() {
        let parsed = parse("language g0\npipe = value |> f <| g\ncompose = f >> g << h\n");

        assert!(parsed.diagnostics.iter().any(|diag| {
            diag.line == 2
                && diag
                    .message
                    .contains("operators `|>` and `<|` have no precedence relationship")
        }));
        assert!(parsed.diagnostics.iter().any(|diag| {
            diag.line == 3
                && diag
                    .message
                    .contains("operators `>>` and `<<` have no precedence relationship")
        }));
    }

    #[test]
    fn parentheses_disambiguate_division_chains() {
        assert_eq!(parse_expr("3/4/5"), None);
        assert_eq!(parse_expr("3/4 / 5"), None);
        assert_eq!(
            parse_expr("(3/4) / 5"),
            Some(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Divide(
                    Box::new(SyntaxExpr::Number(n(3))),
                    Box::new(SyntaxExpr::Number(n(4))),
                )),
                Box::new(SyntaxExpr::Number(n(5))),
            ))
        );
        assert_eq!(
            parse_expr("3 / (4/5)"),
            Some(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Divide(
                    Box::new(SyntaxExpr::Number(n(4))),
                    Box::new(SyntaxExpr::Number(n(5))),
                )),
            ))
        );
        assert_eq!(
            parse_expr("2 * 3 / 4"),
            Some(SyntaxExpr::Divide(
                Box::new(SyntaxExpr::Multiply(
                    Box::new(SyntaxExpr::Number(n(2))),
                    Box::new(SyntaxExpr::Number(n(3))),
                )),
                Box::new(SyntaxExpr::Number(n(4))),
            ))
        );
        assert_eq!(parse_expr("3 - 4 - 5"), None);
        assert_eq!(
            parse_expr("(3 - 4) - 5"),
            Some(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Subtract(
                    Box::new(SyntaxExpr::Number(n(3))),
                    Box::new(SyntaxExpr::Number(n(4))),
                )),
                Box::new(SyntaxExpr::Number(n(5))),
            ))
        );
        assert_eq!(
            parse_expr("3 - (4 - 5)"),
            Some(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Subtract(
                    Box::new(SyntaxExpr::Number(n(4))),
                    Box::new(SyntaxExpr::Number(n(5))),
                )),
            ))
        );
        assert_eq!(parse_expr("3 + 4 - 5"), None);
        assert_eq!(
            parse_expr("(3 + 4) - 5"),
            Some(SyntaxExpr::Subtract(
                Box::new(SyntaxExpr::Add(
                    Box::new(SyntaxExpr::Number(n(3))),
                    Box::new(SyntaxExpr::Number(n(4))),
                )),
                Box::new(SyntaxExpr::Number(n(5))),
            ))
        );
        assert_eq!(
            parse_expr("3 + (4 - 5)"),
            Some(SyntaxExpr::Add(
                Box::new(SyntaxExpr::Number(n(3))),
                Box::new(SyntaxExpr::Subtract(
                    Box::new(SyntaxExpr::Number(n(4))),
                    Box::new(SyntaxExpr::Number(n(5))),
                )),
            ))
        );
    }

    #[test]
    fn lowers_list_expressions_to_core_terms() {
        let parsed = parse("language g0\nasm.result = [72, 101] ++ [108, 108, 111]\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello"
        );
    }

    #[test]
    fn lowers_name_expressions_to_core_terms() {
        let parsed = parse(
            "language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\"\nhello = \"Hello\"\nworld = \"World\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn lowers_prior_name_expressions_to_visible_module_accesses() {
        let parsed = parse("language g0\nasm.result = _hello ++ \"!\"\n");
        let context = CompileContext::default().with_prior_defs(Value::Dict(
            crate::core::Dict::new_sync().insert(
                Key::atom_from_text("hello"),
                Value::binary_from_text("Hello"),
            ),
        ));
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello!"
        );
    }

    #[test]
    fn object_declarations_evaluate_as_object_instances() {
        let parsed = parse(
            "language g0\nobject hello with\n  text = \"Hello, World!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_expressions_evaluate_as_object_instances() {
        let parsed = parse(
            "language g0\nhello = object \"hello\" with\n  text = \"Hello, World!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_dependencies_apply_inherited_defs_to_child_self() {
        let parsed = parse(
            "language g0\nobject base with\n  text = hello ++ \", \" ++ target ++ \"!\"\n  hello = \"Hello\"\n  target = \"Base\"\nobject child extends base with\n  target := \"World\"\nasm.result = child.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_expressions_can_extend_other_object_expressions() {
        let parsed = parse(
            "language g0\nbase = object \"base\" with\n  text = hello ++ \", \" ++ target ++ \"!\"\n  hello = \"Hello\"\n  target = \"Base\"\nhello = object \"hello\" extends base with\n  target := \"World\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_expression_aliases_default_to_parent_scope() {
        let parsed = parse(
            "language g0\nprefix = \"Hello\"\nhello = object \"hello\" as _h with\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn self_and_module_keywords_resolve_at_module_scope() {
        let parsed = parse(
            "language g0\nhello = \"Hello\"\nworld = \"World\"\nasm.result = self.hello ++ \", \" ++ module.world ++ \"!\"\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn prior_self_and_module_keywords_resolve_at_module_scope() {
        let parsed = parse(
            "language g0\nhello = \"Hello\"\nworld = \"World\"\nhello := _self.hello ++ \", \" ++ _module.world ++ \"!\"\nasm.result = hello\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn self_and_module_keywords_resolve_inside_aliased_object_scope() {
        let parsed = parse(
            "language g0\nprefix = \"Hello\"\nobject hello as self with\n  prefix = \"Nope\"\n  target = \"World\"\n  text = module.prefix ++ \", \" ++ self.target ++ \"!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn prior_self_keyword_resolves_inside_object_scope() {
        let parsed = parse(
            "language g0\nobject hello with\n  text = \"Hello, World\"\n  text := _self.text ++ \"!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_prior_names_resolve_against_inherited_self() {
        let parsed = parse(
            "language g0\nobject base with\n  text = \"Hello, World\"\nobject child extends base with\n  text := _text ++ \"!\"\nasm.result = child.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_dependencies_use_c3_deduplication() {
        let parsed = parse(
            "language g0\nobject root with\n  code = \"root\"\nobject left extends root with\n  code := _code ++ \"L\"\nobject right extends root with\n  code := _code ++ \"R\"\nobject child extends left, right with\n  code := _code ++ \"C\"\nasm.result = child.code\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"rootRLC"
        );
    }

    #[test]
    fn object_dependencies_can_inherit_from_dictionaries() {
        let parsed = parse(
            "language g0\nbase = { hello:\"Hello\", target:\"Base\" }\nobject child extends base with\n  target := \"World\"\n  text = hello ++ \", \" ++ target ++ \"!\"\nasm.result = child.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn module_definitions_can_use_expression_indexed_targets() {
        let parsed = parse(
            "language g0\nidx = 42\n.[idx] = \"Hello\"\nasm.result = module.[idx] ++ \", World!\"\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn module_definitions_can_use_path_list_targets() {
        let parsed = parse(
            "language g0\nroot.(['hello, 'target]) = \"World\"\nroot.hello.prefix = \"Hello\"\nasm.result = root.hello.prefix ++ \", \" ++ root.hello.target ++ \"!\"\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_definitions_can_use_expression_indexed_targets() {
        let parsed = parse(
            "language g0\nidx = 42\nobject hello as self with\n  .[idx] = \"Hello\"\n  text = self.[idx] ++ \", World!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn hierarchical_object_declarations_evaluate_in_host_scope() {
        let parsed = parse(
            "language g0\nobject parent with\n  prefix = \"Hello\"\n  object child with\n    text = ^prefix ++ \", World!\"\nasm.result = parent.child.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn hierarchical_object_declarations_can_extend_sibling_objects() {
        let parsed = parse(
            "language g0\nobject parent with\n  object base with\n    text = \"Hello, World\"\n  object child extends base with\n    text := _text ++ \"!\"\nasm.result = parent.child.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn hierarchical_object_declarations_inside_extend_evaluate_in_host_scope() {
        let parsed = parse(
            "language g0\nobject parent with\n  prefix = \"Hello\"\nextend parent with\n  object child with\n    text = ^prefix ++ \", World!\"\nasm.result = parent.child.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn hierarchical_object_names_are_local_to_the_host_object() {
        let parsed = parse(
            "language g0\nobject left with\n  object helper with\n    left = \"Hello\"\nobject right with\n  object helper with\n    right = \"World\"\nobject child extends left.helper, right.helper with\n  text = left ++ \", \" ++ right ++ \"!\"\nasm.result = child.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn repeated_anonymous_object_mixins_are_not_deduplicated() {
        let parsed = parse(
            "language g0\nobject base with\n  count = 0\ninc = object _ with\n  count := _count + 1\nobject child extends inc, inc, inc, base with\nasm.result = child.count\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", "result"])),
            Value::Number(3.into())
        );
    }

    #[test]
    fn anonymous_object_dependencies_can_have_anonymous_and_named_dependencies() {
        let parsed = parse(
            "language g0\nobject base with\n  code = \"B\"\nadd_a = object _ with\n  code := _code ++ \"A\"\nadd_m = object _ extends add_a, base with\n  code := _code ++ \"M\"\nobject child extends add_m with\n  code := _code ++ \"C\"\nasm.result = child.code\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"BAMC"
        );
    }

    #[test]
    fn anonymous_object_dependencies_follow_dependency_override_order() {
        let parsed = parse(
            "language g0\nobject base with\n  code = \"\"\nadd_a = object _ with\n  code := _code ++ \"A\"\nadd_b = object _ with\n  code := _code ++ \"B\"\nobject child extends add_a, add_b, base with\nasm.result = child.code\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"BA"
        );
    }

    #[test]
    fn extend_declarations_reinstantiate_objects() {
        let parsed = parse(
            "language g0\nobject hello with\n  text = \"Hello, World\"\nextend hello with\n  text := _text ++ \"!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_body_edits_do_not_observe_direct_spec_definitions() {
        let parsed = parse(
            "language g0\nobject hello with\n  spec = { bad:\"bad\" }\n  text = { [{}]:\"Hello, World!\" }.[_spec]\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn object_bodies_can_escape_to_module_scope() {
        let parsed = parse(
            "language g0\nprefix = \"Hello\"\nseparator = \", \"\nobject hello with\n  target = \"World\"\n  text = ^(prefix ++ separator) ++ target ++ \"!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn excessive_scope_escapes_report_lowering_errors() {
        let parsed = parse("language g0\nasm.result = ^foo\nobject hello with\n  text = ^^foo\n");
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);

        assert_eq!(lowered.diagnostics.len(), 2);
        assert!(lowered.diagnostics.iter().all(|diag| {
            diag.severity == Severity::Error
                && diag.message.contains("exceeds available parent scopes")
        }));
    }

    #[test]
    fn aliased_object_bodies_default_to_module_scope() {
        let parsed = parse(
            "language g0\nprefix = \"Hello\"\nobject hello as h with\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn suppressed_object_aliases_still_bind_canonical_names() {
        let parsed = parse(
            "language g0\nprefix = \"Hello\"\nobject hello as _h with\n  target = \"World\"\n  text = prefix ++ \", \" ++ h.target ++ \"!\"\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn aliased_extend_bodies_can_reference_prior_object_and_module_scope() {
        let parsed = parse(
            "language g0\nsuffix = \"!\"\nobject hello with\n  text = \"Hello, World\"\nextend hello as h with\n  text := _h.text ++ suffix\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn suppressed_extend_aliases_still_bind_canonical_prior_names() {
        let parsed = parse(
            "language g0\nsuffix = \"!\"\nobject hello with\n  text = \"Hello, World\"\nextend hello as _h with\n  text := _h.text ++ suffix\nasm.result = hello.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn dictionary_with_without_alias_uses_parent_scope() {
        let parsed = parse(
            "language g0\nhello = \"Hello\"\nworld = \"World\"\nd = { hello:\"Nope\", world:\"Nope\" } with\n  hello := \"Still Nope\"\n  text = hello ++ \", \" ++ world ++ \"!\"\nasm.result = d.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn dictionary_with_aliases_capture_prior_and_final_dictionaries() {
        let parsed = parse(
            "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base as b with\n  text := _b.text ++ suffix\n  result = b.text\nasm.result = d.result\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn dictionary_with_suppressed_aliases_still_bind_canonical_names() {
        let parsed = parse(
            "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base as _b with\n  text := _b.text ++ suffix\n  result = b.text\nasm.result = d.result\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn dictionary_with_aliases_follow_local_unused_warning_rules() {
        let parsed =
            parse("language g0\nd = {} as unused with\n  x = 1\ne = {} as _unused with\n  x = 1\n");

        assert_eq!(
            parsed
                .diagnostics
                .iter()
                .filter(|diag| diag.message.contains("unused local"))
                .count(),
            1
        );
        assert!(parsed.diagnostics.iter().any(|diag| {
            diag.severity == Severity::Warning
                && diag.line == 2
                && diag.message == "unused local `unused`"
        }));
    }

    #[test]
    fn dictionary_with_self_alias_uses_object_style_scope() {
        let parsed = parse(
            "language g0\nsuffix = \"!\"\nbase = { text:\"Hello, World\" }\nd = base as self with\n  text := _text ++ ^suffix\nasm.result = d.text\n",
        );
        let context = CompileContext::from_module_path(["assembly"]);
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn lowers_lambda_and_application_expressions_to_core_terms() {
        let parsed =
            parse("language g0\nd = { tail:\"Hello, World!\" }\nasm.result = (\\x -> x.tail) d\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn lowers_let_expressions_to_lambda_application() {
        let parsed = parse(
            "language g0\nasm.result = let hello = \"Hello\"; world = \"World\" in hello ++ \", \" ++ world ++ \"!\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn lowers_where_expressions_to_lambda_application() {
        let parsed = parse(
            "language g0\nasm.result = hello ++ \", \" ++ world ++ \"!\" where hello = \"Hello\"; world = \"World\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn lowers_multiline_let_expressions_to_lambda_application() {
        let parsed = parse(
            "language g0\nasm.result = let hello = \"Hello\"\n    world = \"World\"\n    hello ++ \", \" ++ world ++ \"!\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn effect_shorthand_builds_applicable_effect_values() {
        let parsed = parse(
            "language g0\napi = { emit:(\\x -> x ++ \"!\") }\neffect = .emit \"Hi\"\nasm.result = effect.eff api\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hi!"
        );
    }

    #[test]
    fn method_objects_apply_via_apply_member_from_syntax() {
        let parsed = parse(
            "language g0\nmethod = { apply:(\\x -> x ++ \"!\") }\nasm.result = method \"Hi\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hi!"
        );
    }

    #[test]
    fn operator_sections_evaluate_as_curried_functions() {
        let parsed = parse(
            "language g0\nadd_answer = (+ 42)\nsub_from_answer = (42 -)\nadd = (+)\nappend = (++)\nasm.sum = add_answer 8\nasm.diff = sub_from_answer 8\nasm.full_sum = add 8 42\nasm.full_append = append \"Hello\" \"!\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", "sum"])),
            Value::Number(n(50))
        );
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", "diff"])),
            Value::Number(n(34))
        );
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", "full_sum"])),
            Value::Number(n(50))
        );
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "full_append"]
            ))),
            b"Hello!"
        );
    }

    #[test]
    fn pipe_and_composition_operators_evaluate_as_syntax_sugar() {
        let parsed = parse(
            "language g0\nid x = x\nbang x = x ++ \"!\"\nhello = \"Hello\"\npipe_section = (|> bang)\npipe_function = (|>)\ncompose_section = (>> bang)\ncompose_function = (>>)\nasm.pipe_forward = hello |> bang\nasm.pipe_backward = bang <| hello\nasm.compose_forward = (id >> bang) hello\nasm.compose_backward = (bang << id) hello\nasm.pipe_section = pipe_section hello\nasm.pipe_function = pipe_function hello bang\nasm.compose_section = (compose_section id) hello\nasm.compose_function = compose_function id bang hello\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        for path in [
            "pipe_forward",
            "pipe_backward",
            "compose_forward",
            "compose_backward",
            "pipe_section",
            "pipe_function",
            "compose_section",
            "compose_function",
        ] {
            assert_eq!(
                output_bytes(&fully_evaluated_value(resolved_value_at_path(
                    &value,
                    &["asm", path]
                ))),
                b"Hello!",
                "{path}"
            );
        }
    }

    #[test]
    fn effect_operators_evaluate_as_syntax_sugar() {
        let parsed = parse(
            "language g0\napi = { r:(\\x -> x), seq:(\\op k -> (k (op.eff api)).eff api) }\nop = .r \"Hello\"\nk x = .r (x ++ \", World!\")\nf x = .r (x ++ \", World\")\ng x = .r (x ++ \"!\")\nop_unit = .r ()\nbind_section = (>>= k)\nbind_function = (>>=)\nthen_function = (=>>)\nkleisli_function = (>=>)\nasm.bind = (op >>= k).eff api\nasm.bind_section = (bind_section op).eff api\nasm.bind_function = (bind_function op k).eff api\nasm.kleisli = ((f >=> g) \"Hello\").eff api\nasm.kleisli_function = (kleisli_function f g \"Hello\").eff api\nasm.then = (op_unit =>> .r \"Hello, World!\").eff api\nasm.then_function = (then_function op_unit (.r \"Hello, World!\")).eff api\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        for path in [
            "bind",
            "bind_section",
            "bind_function",
            "kleisli",
            "kleisli_function",
            "then",
            "then_function",
        ] {
            assert_eq!(
                output_bytes(&fully_evaluated_value(resolved_value_at_path(
                    &value,
                    &["asm", path]
                ))),
                b"Hello, World!",
                "{path}"
            );
        }
    }

    #[test]
    fn effect_then_requires_unit_result_when_observed() {
        let parsed = parse(
            "language g0\napi = { r:(\\x -> x), seq:(\\op k -> (k (op.eff api)).eff api) }\nbad = .r \"not unit\" =>> .r \"unreachable\"\nasm.result = bad.eff api\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        let mut result =
            value_at_atom_path(&value, &["asm", "result"]).expect("result should exist");
        let err = loop {
            match crate::eval::eval_value(&result) {
                Ok(Value::Expr(next)) => result = Value::Expr(next),
                Ok(other) => panic!("non-unit result should not evaluate to {other:?}"),
                Err(err) => break err,
            }
        };
        assert!(
            err.to_string()
                .contains("requires discarded effect results to be unit")
        );
    }

    #[test]
    fn comparisons_and_boolean_operators_evaluate_as_effects() {
        let parsed = parse(
            "language g0\nimport 'std\ntuple_left = { tuple:[1,2] }\ntuple_right = { tuple:[1,3] }\nasm.gt = list.pure ((3 > 2) =>> .r \"G\")\nasm.ge = list.pure ((3 >= 3) =>> .r \"E\")\nasm.eq = list.pure ((3 == 3) =>> .r \"Q\")\nasm.ne = list.pure ((3 <> 4) =>> .r \"N\")\nasm.le = list.pure ((3 =< 3) =>> .r \"L\")\nasm.lt = list.pure ((2 < 3) =>> .r \"T\")\nasm.fail = list.pure ((3 < 2) =>> .r \"bad\")\nasm.chain = list.pure ((1 < 2 =< 2 <> 3) =>> .r \"H\")\nasm.chain_fail = list.pure ((1 < 3 < 2) =>> .r \"bad\")\nasm.chain_raw = 1 < (2 + 0) < 3\nasm.list = list.pure (([1,2] < [1,3]) =>> .r \"S\")\nasm.binary_list = list.pure ((\"AB\" == [65,66]) =>> .r \"B\")\nasm.tuple = list.pure ((tuple_left < tuple_right) =>> .r \"U\")\nasm.dict = list.pure (({ a:1, b:{} } == { a:1 }) =>> .r \"D\")\nasm.and = list.pure ((3 > 2 and \"A\" == [65]) =>> .r \"A\")\nasm.or = list.pure ((3 < 2 or 3 == 3) =>> .r \"O\")\nasm.not_true = list.pure ((not (3 > 2)) =>> .r \"bad\")\nasm.not_false = list.pure ((not (3 < 2)) =>> .r \"F\")\nasm.could_true = list.pure ((could (.alt .fail (3 == 3))) =>> .r \"C\")\nasm.could_false = list.pure ((could .fail) =>> .r \"bad\")\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        for (path, expected) in [
            ("gt", b"G".as_slice()),
            ("ge", b"E"),
            ("eq", b"Q"),
            ("ne", b"N"),
            ("le", b"L"),
            ("lt", b"T"),
            ("fail", b""),
            ("chain", b"H"),
            ("chain_fail", b""),
            ("list", b"S"),
            ("binary_list", b"B"),
            ("tuple", b"U"),
            ("dict", b"D"),
            ("and", b"A"),
            ("or", b"O"),
            ("not_true", b""),
            ("not_false", b"F"),
            ("could_true", b"C"),
            ("could_false", b""),
        ] {
            assert_eq!(
                output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                    &value,
                    &["asm", path]
                ))),
                expected,
                "{path}"
            );
        }

        value_at_atom_path(&value, &["asm", "chain_raw"]).expect("raw chain should exist");
    }

    #[test]
    fn list_effect_handler_runs_standard_backtracking_effects() {
        let parsed = parse(
            "language g0\nimport 'list as list\nchoices = (.alt (.r \"A\") (.alt .fail (.r \"B\"))) >>= (\\x -> .r (x ++ \"!\"))\ncut = .cut (.alt (.r \"C\") (.r \"D\"))\ncut_bad = .cut (.alt (.r \"G\") 42)\ncut_seq_bad = .cut ((.alt (.r \"S\") 42) >>= (\\x -> .r (x ++ \"!\")))\nobject_effect = { eff:(.r \"E\").eff, meta:1 }\nfixed = .fix (\\self -> .r { text:\"F\", self:self })\nasm.choices = list.pure choices\nasm.cut = list.pure cut\nasm.cut_fail = list.pure (.cut .fail)\nasm.cut_bad = list.pure cut_bad\nasm.cut_seq_bad = list.pure cut_seq_bad\nasm.object = list.pure object_effect\nasm.fixed = (list.head (list.pure fixed)).text\nasm.head = list.head \"Hi\"\nasm.tail = list.tail \"Hi\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "choices"]
            ))),
            b"A!B!"
        );
        assert_eq!(
            output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "cut"]
            ))),
            b"C"
        );
        assert_eq!(
            output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "cut_fail"]
            ))),
            b""
        );
        assert_eq!(
            output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "cut_bad"]
            ))),
            b"G"
        );
        assert_eq!(
            output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "cut_seq_bad"]
            ))),
            b"S!"
        );
        assert_eq!(
            output_binary_result_list(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "object"]
            ))),
            b"E"
        );
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "fixed"]
            ))),
            b"F"
        );
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["asm", "head"])),
            Value::Number(n(72))
        );
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "tail"]
            ))),
            b"i"
        );
    }

    #[test]
    fn operator_section_operands_resolve_module_scope_names() {
        let parsed = parse(
            "language g0\nsuffix = \"!\"\nadd_suffix = (++ suffix)\nasm.result = add_suffix \"Hello\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello!"
        );
    }

    #[test]
    fn lowers_definition_argument_sugar_to_lambda_terms() {
        let parsed = parse("language g0\nid x = x\nasm.result = id \"Hello, World!\"\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn update_definition_argument_sugar_applies_body_to_prior_definition() {
        let parsed = parse(
            "language g0\nhello who = \"Hello, \" ++ who\nhello who ::= \\prior -> prior who ++ \"!\"\nasm.result = hello \"World\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn prior_names_observe_prior_module_state_within_current_module_scope() {
        let parsed = parse("language g0\nhello = \"Hello\"\nasm.result = _hello ++ \", World!\"\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn lowers_suppressed_local_names_to_canonical_body_references() {
        let parsed =
            parse("language g0\nkeep _value = value\nasm.result = keep \"Hello, World!\"\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &value,
                &["asm", "result"]
            ))),
            b"Hello, World!"
        );
    }

    #[test]
    fn lowers_dictionary_literals_to_lazy_values() {
        let parsed = parse(
            "language g0\nd = { hello:\"Hello\", world:other ++ \"!\" }\nother = \"World\"\n",
        );
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        let dictionary = fully_evaluated_value(resolved_value_at_path(&value, &["d"]));
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &dictionary,
                &["hello"]
            ))),
            b"Hello"
        );
        assert_eq!(
            output_bytes(&fully_evaluated_value(resolved_value_at_path(
                &dictionary,
                &["world"]
            ))),
            b"World!"
        );
    }

    #[test]
    fn lowering_starts_from_prior_dictionary() {
        let parsed = parse("language g0\nworld = \"World\"\n");
        let context = CompileContext::default().with_prior_defs(Value::Dict(
            crate::core::Dict::new_sync().insert(
                Key::atom_from_text("hello"),
                Value::binary_from_text("Hello"),
            ),
        ));
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            value.get_atom_path(&[Atom::from_key(&Key::binary_from_text("hello"))]),
            Some(&Value::binary_from_text("Hello"))
        );
        assert_eq!(
            resolved_value_at_path(&value, &["world"]),
            Value::binary_from_text("World")
        );
    }

    #[test]
    fn lowers_builtin_imports_to_module_dictionaries() {
        let parsed = parse("language g0\nimport 'std as std\nimport 'math\nimport 'list as list\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        let std = value
            .get_atom_path(&[Atom::from_key(&Key::binary_from_text("std"))])
            .expect("std import should exist");
        let std = crate::eval::eval_value(std).expect("std import should evaluate to a dictionary");
        let floor = value
            .get_atom_path(&[Atom::from_key(&Key::binary_from_text("floor"))])
            .expect("inline math import should expose floor");
        let mod_fn = value
            .get_atom_path(&[Atom::from_key(&Key::binary_from_text("mod"))])
            .expect("inline math import should expose mod");
        let list_len_import = crate::eval::eval_value(&Value::expr(core_global_access(
            &context,
            vec![
                CoreKeyExpr::Key(Key::atom_from_text("list")),
                CoreKeyExpr::Key(Key::atom_from_text("len")),
            ],
        )))
        .expect("list.len import should resolve");
        let list_spec = crate::eval::eval_value(&Value::expr(core_global_access(
            &context,
            vec![
                CoreKeyExpr::Key(Key::atom_from_text("list")),
                CoreKeyExpr::Key(Key::atom_from_text("spec")),
            ],
        )))
        .expect("list.spec import should resolve");
        let list_head_import = crate::eval::eval_value(&Value::expr(core_global_access(
            &context,
            vec![
                CoreKeyExpr::Key(Key::atom_from_text("list")),
                CoreKeyExpr::Key(Key::atom_from_text("head")),
            ],
        )))
        .expect("list.head import should resolve");
        let list_tail_import = crate::eval::eval_value(&Value::expr(core_global_access(
            &context,
            vec![
                CoreKeyExpr::Key(Key::atom_from_text("list")),
                CoreKeyExpr::Key(Key::atom_from_text("tail")),
            ],
        )))
        .expect("list.tail import should resolve");
        let list_pure_import = crate::eval::eval_value(&Value::expr(core_global_access(
            &context,
            vec![
                CoreKeyExpr::Key(Key::atom_from_text("list")),
                CoreKeyExpr::Key(Key::atom_from_text("pure")),
            ],
        )))
        .expect("list.pure import should resolve");
        let (
            anno,
            std_not,
            std_could,
            std_list_len,
            std_list_split,
            std_list_split_end,
            std_list_head,
            std_list_tail,
            std_list_pure,
        ) = match &std {
            Value::Dict(std) => {
                let anno = std
                    .get(&Key::atom_from_text("anno"))
                    .expect("std import should expose anno");
                let not = std
                    .get(&Key::atom_from_text("not"))
                    .expect("std import should expose not");
                let could = std
                    .get(&Key::atom_from_text("could"))
                    .expect("std import should expose could");
                let std_list = std
                    .get(&Key::atom_from_text("list"))
                    .expect("std import should expose list");
                let Value::Dict(std_list) =
                    crate::eval::eval_value(std_list).expect("std.list should evaluate")
                else {
                    panic!("std.list should evaluate to a dictionary");
                };
                let len = std_list
                    .get(&Key::atom_from_text("len"))
                    .expect("std.list should expose len");
                let split = std_list
                    .get(&Key::atom_from_text("split"))
                    .expect("std.list should expose split");
                let split_end = std_list
                    .get(&Key::atom_from_text("split_end"))
                    .expect("std.list should expose split_end");
                let head = std_list
                    .get(&Key::atom_from_text("head"))
                    .expect("std.list should expose head");
                let tail = std_list
                    .get(&Key::atom_from_text("tail"))
                    .expect("std.list should expose tail");
                let pure = std_list
                    .get(&Key::atom_from_text("pure"))
                    .expect("std.list should expose pure");
                (
                    anno,
                    not.clone(),
                    could.clone(),
                    len.clone(),
                    split.clone(),
                    split_end.clone(),
                    head.clone(),
                    tail.clone(),
                    pure.clone(),
                )
            }
            _ => unreachable!(),
        };
        let list_module = builtin_list_module(&context);
        let list_len = list_module
            .get(&Key::atom_from_text("len"))
            .expect("list module should expose len");
        let list_split = list_module
            .get(&Key::atom_from_text("split"))
            .expect("list module should expose split");
        let list_split_end = list_module
            .get(&Key::atom_from_text("split_end"))
            .expect("list module should expose split_end");
        let list_head = list_module
            .get(&Key::atom_from_text("head"))
            .expect("list module should expose head");
        let list_tail = list_module
            .get(&Key::atom_from_text("tail"))
            .expect("list module should expose tail");
        let list_pure = list_module
            .get(&Key::atom_from_text("pure"))
            .expect("list module should expose pure");

        let Value::Dict(_) = std else {
            panic!("std import should evaluate to a dictionary");
        };
        assert!(matches!(std, Value::Dict(_)));
        assert!(matches!(anno, Value::Builtin(crate::core::Builtin::Anno)));
        assert!(context.value_lambda_body(&std_not).is_some());
        assert!(context.value_lambda_body(&std_could).is_some());
        assert!(matches!(floor, Value::Builtin(crate::core::Builtin::Floor)));
        assert!(matches!(mod_fn, Value::Builtin(crate::core::Builtin::Mod)));
        assert!(matches!(
            std_list_len,
            Value::Builtin(crate::core::Builtin::ListLen)
        ));
        assert!(matches!(
            std_list_split,
            Value::Builtin(crate::core::Builtin::ListSplit)
        ));
        assert!(matches!(
            std_list_split_end,
            Value::Builtin(crate::core::Builtin::ListSplitEnd)
        ));
        assert!(matches!(
            std_list_head,
            Value::Builtin(crate::core::Builtin::ListHead)
        ));
        assert!(matches!(
            std_list_tail,
            Value::Builtin(crate::core::Builtin::ListTail)
        ));
        assert!(matches!(
            std_list_pure,
            Value::Builtin(crate::core::Builtin::ListEffect)
        ));
        assert!(matches!(
            list_len,
            Value::Builtin(crate::core::Builtin::ListLen)
        ));
        assert!(matches!(
            list_split,
            Value::Builtin(crate::core::Builtin::ListSplit)
        ));
        assert!(matches!(
            list_split_end,
            Value::Builtin(crate::core::Builtin::ListSplitEnd)
        ));
        assert!(matches!(
            list_head,
            Value::Builtin(crate::core::Builtin::ListHead)
        ));
        assert!(matches!(
            list_tail,
            Value::Builtin(crate::core::Builtin::ListTail)
        ));
        assert!(matches!(
            list_pure,
            Value::Builtin(crate::core::Builtin::ListEffect)
        ));
        assert!(matches!(
            list_len_import,
            Value::Builtin(crate::core::Builtin::ListLen)
        ));
        assert!(matches!(
            list_head_import,
            Value::Builtin(crate::core::Builtin::ListHead)
        ));
        assert!(matches!(
            list_tail_import,
            Value::Builtin(crate::core::Builtin::ListTail)
        ));
        assert!(matches!(
            list_pure_import,
            Value::Builtin(crate::core::Builtin::ListEffect)
        ));
        assert!(!matches!(
            list_spec,
            Value::Dict(dict) if dict.is_empty()
        ));
    }

    #[test]
    fn lowers_unique_declarations_via_abstract_global_paths() {
        let context = CompileContext::default();
        let lowered = lower_with_module_path(
            "language g0\nunique Foo, palette.Blue\n",
            &["pkg", "module"],
        );
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            value.get_atom_path(&[Atom::from_key(&Key::binary_from_text("Foo"))]),
            Some(&abstract_path_atom(&["pkg", "module", "Foo"]))
        );
        assert_eq!(
            value_at_atom_path(&value, &["palette", "Blue"]).as_ref(),
            Some(&abstract_path_atom(&["pkg", "module", "palette", "Blue"]))
        );
    }

    #[test]
    fn source_paths_remain_separate_from_module_paths() {
        let context = CompileContext::from_source_path("samples/assembly/hello_text.g");

        assert_eq!(context.source_path(), Some("samples/assembly/hello_text.g"));
        assert!(context.module_path.is_empty());
    }

    #[test]
    fn parse_can_read_source_binary_from_compile_context() {
        let context = CompileContext::from_module_path(["pkg"])
            .with_source_binary(&b"language g0\nanswer = 42\n"[..]);
        let parsed = parse_with_context("language g0\nbroken =", &context);

        assert_eq!(parsed.diagnostics, []);
        assert_eq!(parsed.declarations.len(), 2);
        assert!(matches!(
            &parsed.declarations[1].kind,
            DeclarationKind::Definition(DefinitionDecl {
                target,
                kind: DefinitionKind::Introduce,
                body,
                ..
            }) if target == "answer" && body == "42"
        ));
    }

    #[test]
    fn inline_builtin_imports_follow_ordered_module_updates() {
        let context = CompileContext::default();
        let parsed = parse("language g0\nmath.answer = 42\nimport 'std\n");
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        let math = value
            .get_atom_path(&[Atom::from_key(&Key::binary_from_text("math"))])
            .expect("std import should merge into existing math");
        let math = crate::eval::eval_value(math).expect("merged math binding should evaluate");

        let Value::Dict(math) = math else {
            panic!("math should evaluate to a dictionary");
        };

        assert_eq!(
            math.get(&Key::atom_from_text("answer"))
                .map(crate::eval::eval_value)
                .transpose()
                .expect("math.answer should resolve"),
            Some(Value::Number(42.into()))
        );
        assert!(matches!(
            math.get(&Key::atom_from_text("floor")),
            Some(Value::Builtin(crate::core::Builtin::Floor))
        ));
        assert!(matches!(
            math.get(&Key::atom_from_text("mod")),
            Some(Value::Builtin(crate::core::Builtin::Mod))
        ));
    }

    #[test]
    fn introduce_and_override_checks_are_deferred_until_observed() {
        let context = CompileContext::default();
        let parsed = parse("language g0\nfoo := 1\nok = \"ok\"\n");
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            resolved_value_at_path(&value, &["ok"]),
            Value::binary_from_text("ok")
        );

        let foo = value
            .get_atom_path(&[Atom::from_key(&Key::binary_from_text("foo"))])
            .expect("foo binding should exist lazily");
        let foo =
            crate::eval::eval_value(foo).expect("foo binding should resolve to a stuck expression");
        let Value::Expr(foo) = foo else {
            panic!("foo binding should resolve to a stuck expression");
        };
        let err = crate::eval::eval_value(&Value::Expr(foo))
            .expect_err("override check should fail on demand");
        assert_eq!(
            err.to_string(),
            "cannot override `foo` because it is not defined"
        );
    }

    #[test]
    fn duplicate_introductions_fail_lazily_against_prior_module_updates() {
        let context = CompileContext::default();
        let parsed = parse("language g0\nfoo = 1\nfoo = 2\nok = \"ok\"\n");
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            resolved_value_at_path(&value, &["ok"]),
            Value::binary_from_text("ok")
        );

        let foo = value
            .get_atom_path(&[Atom::from_key(&Key::binary_from_text("foo"))])
            .expect("duplicate foo binding should exist lazily");
        let foo = crate::eval::eval_value(foo)
            .expect("duplicate foo binding should resolve to a stuck expression");
        let Value::Expr(foo) = foo else {
            panic!("duplicate foo binding should resolve to a stuck expression");
        };
        let err = crate::eval::eval_value(&Value::Expr(foo))
            .expect_err("duplicate introductions should fail on demand");
        assert_eq!(
            err.to_string(),
            "cannot introduce `foo` because it is already defined"
        );
    }

    #[test]
    fn update_definitions_observe_prior_module_state() {
        let context = CompileContext::default();
        let parsed = parse("language g0\nfoo = 1\nfoo ::= \\prior -> prior + 1\n");
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
            Value::Number(2.into())
        );
    }

    #[test]
    fn update_definitions_can_use_named_updater_functions() {
        let context = CompileContext::default();
        let parsed = parse("language g0\ninc prior = prior + 1\nfoo = 1\nfoo ::= inc\n");
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            fully_evaluated_value(resolved_value_at_path(&value, &["foo"])),
            Value::Number(2.into())
        );
    }

    #[test]
    fn overrides_replace_prior_definitions_without_union_ambiguity() {
        let context = CompileContext::default().with_prior_defs(Value::Dict(
            Dict::new_sync().insert(Key::atom_from_text("foo"), Value::Number(1.into())),
        ));
        let parsed = parse("language g0\nfoo := 2\n");
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);

        assert_eq!(
            resolved_value_at_path(&value, &["foo"]),
            Value::Number(2.into())
        );
    }
}

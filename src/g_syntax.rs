use chumsky::prelude::*;

use crate::compiler::CompileContext;
use crate::core::Builtin;
use crate::core::{Atom, Dict, Key, KeyExpr as CoreKeyExpr, Value};
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
    pub definition: DefinitionDecl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectExtendDecl {
    pub target: String,
    pub alias: Option<String>,
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
    Number(Number),
    Text(String),
    Name(String),
    PriorName(String),
    EscapedName(String),
    Access(Box<SyntaxExpr>, Vec<SyntaxKeyExpr>),
    SingletonDict(SyntaxKeyExpr, Box<SyntaxExpr>),
    DictUnion(Vec<SyntaxExpr>),
    List(Vec<SyntaxExpr>),
    Lambda(Vec<String>, Box<SyntaxExpr>),
    Apply(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Multiply(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Divide(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Add(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Subtract(Box<SyntaxExpr>, Box<SyntaxExpr>),
    Append(Box<SyntaxExpr>, Box<SyntaxExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntaxKeyExpr {
    Atom(String),
    Index(Box<SyntaxExpr>),
    PathIndex(Box<SyntaxExpr>),
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
    escaped_final_defs: Value,
    object_alias: Option<String>,
    object_final_defs: Option<Value>,
    object_prior_defs: Option<Value>,
}

impl NameScope {
    fn module(context: &CompileContext, visible_definitions: Value) -> Self {
        Self {
            final_defs: context.final_defs.clone(),
            prior_defs: visible_definitions,
            escaped_final_defs: context.final_defs.clone(),
            object_alias: None,
            object_final_defs: None,
            object_prior_defs: None,
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

            text.push('\n');
            text.push_str(next_trimmed);
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
        ImportPlacement::Inline => update_module_value(definitions.clone(), module, context),
        ImportPlacement::As(target) => update_module_value(
            definitions.clone(),
            path_to_dict_value(target, module, context)?,
            context,
        ),
        ImportPlacement::At(_) => {
            return Err(Diagnostic::error(
                line,
                "built-in `import ... at ...` is not supported by the current spike",
            ));
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
            // TODO: `import ... as m` should desugar through object/env extension
            // once object syntax exists. For this spike, load into an empty scoped
            // prior and install the resulting module at the target path.
            let loaded =
                scoped_local_import_value(reference, target, context.empty_dict_value(), context)?;
            *definitions = update_module_value(
                definitions.clone(),
                path_to_dict_value(target, loaded, context)?,
                context,
            );
        }
        ImportPlacement::At(target) => {
            let scoped_prior = path_value_in_definitions(target, definitions.clone(), context)?;
            let loaded = scoped_local_import_value(reference, target, scoped_prior, context)?;
            *definitions = update_module_value(
                definitions.clone(),
                path_to_dict_value(target, loaded, context)?,
                context,
            );
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
    *definitions = update_module_value(
        definitions.clone(),
        path_to_dict_value(target, loaded, context)?,
        context,
    );
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

fn lower_unique(
    names: &[String],
    _line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    for name in names {
        let path = context.abstract_global_path(name);
        let value = context.abstract_global_path_value(path.as_ref());
        *definitions = update_module_value(
            definitions.clone(),
            path_to_dict_value(name, value, context)?,
            context,
        );
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
        .insert(name_as_key("map"), context.value_builtin(Builtin::Map))
}

fn builtin_std_module(context: &CompileContext) -> Dict {
    Dict::new_sync()
        .insert(name_as_key("anno"), context.value_builtin(Builtin::Anno))
        .insert(
            name_as_key("math"),
            context.value_dict(builtin_math_module(context)),
        )
        .insert(
            name_as_key("list"),
            context.value_dict(builtin_list_module(context)),
        )
}

fn lower_definition(
    definition: &DefinitionDecl,
    declaration_text: &str,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
    scope: &NameScope,
) -> Result<(), Diagnostic> {
    let Some(expr) = &definition.expr else {
        return Ok(());
    };

    let value = syntax_expr_to_value(expr, line, context, scope)?;
    let value = match definition.kind {
        DefinitionKind::Introduce => annotate_definition_value(
            BuiltinAssertion::Undefined,
            &definition.target,
            definitions.clone(),
            value,
            context,
        )?,
        DefinitionKind::Override => annotate_definition_value(
            BuiltinAssertion::Defined,
            &definition.target,
            definitions.clone(),
            value,
            context,
        )?,
        DefinitionKind::Update => lower_update_definition(
            &definition.target,
            definitions.clone(),
            value,
            definition_param_count(definition, declaration_text, line)?,
            line,
            context,
        )?,
    };
    *definitions = update_module_value(
        definitions.clone(),
        path_to_dict_value(&definition.target, value, context)?,
        context,
    );

    Ok(())
}

fn lower_object(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let object_value = object_decl_value(object, line, context, definitions.clone())?;
    let object_value = annotate_definition_value(
        BuiltinAssertion::Undefined,
        &object.target,
        definitions.clone(),
        object_value,
        context,
    )?;

    *definitions = update_module_value(
        definitions.clone(),
        path_to_dict_value(&object.target, object_value, context)?,
        context,
    );
    Ok(())
}

fn object_decl_value(
    object: &ObjectDecl,
    line: usize,
    context: &CompileContext,
    visible_definitions: Value,
) -> Result<Value, Diagnostic> {
    let defs = object_body_defs_value(
        &object.body,
        object.alias.as_deref(),
        line,
        context,
        visible_definitions,
    )?;
    let deps = object
        .deps
        .iter()
        .map(|dep| {
            let dep_object = path_value_in_definitions(dep, context.final_defs.clone(), context)?;
            Ok(context.value_access(dep_object, vec![context.key_expr_key(name_as_key("spec"))]))
        })
        .collect::<Result<Vec<_>, Diagnostic>>()?;
    let spec = Dict::new_sync()
        .insert(
            name_as_key("name"),
            context
                .abstract_global_path_value(context.abstract_global_path(&object.target).as_ref()),
        )
        .insert(name_as_key("deps"), context.value_list(deps))
        .insert(name_as_key("defs"), defs);

    Ok(context.value_apply(
        context.value_builtin(Builtin::ObjectInstance),
        context.value_dict(spec),
    ))
}

fn object_body_defs_value(
    body: &[ObjectBodyDefinition],
    alias: Option<&str>,
    _line: usize,
    context: &CompileContext,
    module_prior_defs: Value,
) -> Result<Value, Diagnostic> {
    let mut definitions = remove_object_spec_value(context.value_local(1), context);
    let object_final_defs = context.value_local(0);
    let module_final_defs = context.final_defs.clone();

    for body_definition in body {
        let scope = object_body_scope(
            alias,
            object_final_defs.clone(),
            definitions.clone(),
            module_final_defs.clone(),
            module_prior_defs.clone(),
        );
        lower_definition(
            &body_definition.definition,
            body_definition.text.as_str(),
            body_definition.line,
            context,
            &mut definitions,
            &scope,
        )?;
        definitions = remove_object_spec_value(definitions, context);
    }

    Ok(context.value_lambda(context.value_lambda(definitions)))
}

fn remove_object_spec_value(value: Value, context: &CompileContext) -> Value {
    context.builtin_apply2_value(
        Builtin::DictRemove,
        value,
        context.value_atom(atom_from_str("spec")),
    )
}

fn object_body_scope(
    alias: Option<&str>,
    object_final_defs: Value,
    object_prior_defs: Value,
    module_final_defs: Value,
    module_prior_defs: Value,
) -> NameScope {
    let object_alias = alias.map(ToOwned::to_owned);
    let (final_defs, prior_defs) = if object_alias.is_some() {
        (module_final_defs.clone(), module_prior_defs)
    } else {
        (object_final_defs.clone(), object_prior_defs.clone())
    };

    NameScope {
        final_defs,
        prior_defs,
        escaped_final_defs: module_final_defs,
        object_alias,
        object_final_defs: Some(object_final_defs),
        object_prior_defs: Some(object_prior_defs),
    }
}

fn lower_extend(
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
) -> Result<(), Diagnostic> {
    let object_value = extend_object_value(extend, line, context, definitions.clone())?;
    let object_value = annotate_definition_value(
        BuiltinAssertion::Defined,
        &extend.target,
        definitions.clone(),
        object_value,
        context,
    )?;

    *definitions = update_module_value(
        definitions.clone(),
        path_to_dict_value(&extend.target, object_value, context)?,
        context,
    );
    Ok(())
}

fn extend_object_value(
    extend: &ObjectExtendDecl,
    line: usize,
    context: &CompileContext,
    visible_definitions: Value,
) -> Result<Value, Diagnostic> {
    let prior_object =
        path_value_in_definitions(&extend.target, visible_definitions.clone(), context)?;
    let prior_spec = context.value_access(
        prior_object,
        vec![context.key_expr_key(name_as_key("spec"))],
    );
    let extension_defs = object_body_defs_value(
        &extend.body,
        extend.alias.as_deref(),
        line,
        context,
        visible_definitions.clone(),
    )?;
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
) -> Result<Value, Diagnostic> {
    let prior = path_value_in_definitions(target, visible_definitions, context)?;
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
    context: &CompileContext,
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
            path_value_in_definitions(target, visible_definitions, context)?,
        ),
    );
    let annotation = context.builtin_apply2_value(
        Builtin::DictSingleton,
        context.value_atom(atom_from_str(tag)),
        payload,
    );

    Ok(context.builtin_apply2_value(Builtin::Anno, annotation, value))
}

fn update_module_value(definitions: Value, item: Value, context: &CompileContext) -> Value {
    // Module definitions are ordered updates over the incoming namespace.
    // Ordinary dictionary literals still lower through DictUnion.
    context.builtin_apply2_value(Builtin::DictUpdate, definitions, item)
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

fn path_to_dict_value(
    target: &str,
    value: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let parts = target.split('.').collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(Diagnostic::error(0, "definition target cannot be empty"));
    }

    let mut value = value;
    for part in parts.into_iter().rev() {
        value = context.builtin_apply2_value(
            Builtin::DictSingleton,
            context.value_atom(atom_from_str(part)),
            value,
        );
    }
    Ok(value)
}

fn syntax_expr_to_value(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
) -> Result<Value, Diagnostic> {
    syntax_expr_to_value_in_scope(expr, line, context, scope, &mut Vec::new())
}

fn syntax_expr_to_value_in_scope(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Result<Value, Diagnostic> {
    Ok(match expr {
        SyntaxExpr::Number(number) => context.value_number(number.clone()),
        SyntaxExpr::Text(text) => context.value_binary(text),
        SyntaxExpr::SingletonDict(key, value) => context.builtin_apply2_value(
            Builtin::DictSingleton,
            syntax_key_expr_to_value(key, line, context, scope, locals)?,
            syntax_expr_to_value_in_scope(value, line, context, scope, locals)?,
        ),
        SyntaxExpr::DictUnion(items) => lower_dict_union(items, line, context, scope, locals)?,
        SyntaxExpr::Name(name) => lower_name_expr(name, context, scope, locals),
        SyntaxExpr::PriorName(name) => lower_prior_name_expr(name, line, context, scope)?,
        SyntaxExpr::EscapedName(name) => lower_escaped_name_expr(name, context, scope),
        SyntaxExpr::Access(base, parts) => context.value_access(
            syntax_expr_to_value_in_scope(base, line, context, scope, locals)?,
            parts
                .iter()
                .map(|part| syntax_key_expr_to_core(part, line, context, scope, locals))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SyntaxExpr::List(items) => context.value_list(
            items
                .iter()
                .map(|expr| syntax_expr_to_value_in_scope(expr, line, context, scope, locals))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SyntaxExpr::Lambda(params, body) => {
            lower_lambda_expr(params, body, line, context, scope, locals)?
        }
        SyntaxExpr::Apply(function, argument) => context.value_apply(
            syntax_expr_to_value_in_scope(function, line, context, scope, locals)?,
            syntax_expr_to_value_in_scope(argument, line, context, scope, locals)?,
        ),
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

fn lower_name_expr(
    name: &str,
    context: &CompileContext,
    scope: &NameScope,
    locals: &mut Vec<LocalName>,
) -> Value {
    // TODO: special keyword atoms like 'self' and 'module'

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

fn lower_escaped_name_expr(name: &str, context: &CompileContext, scope: &NameScope) -> Value {
    context.value_access(
        scope.escaped_final_defs.clone(),
        vec![context.key_expr_key(Key::atom_from_text(name))],
    )
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

    let mut body = Vec::new();
    for (offset, body_line) in body_lines.iter().enumerate() {
        if body_line.trim().is_empty() {
            continue;
        }
        let body_line_number = line + offset + 1;
        let Some(definition) =
            parse_object_body_definition(body_line.trim(), body_line_number, diagnostics)
        else {
            continue;
        };
        body.push(definition);
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
    if !glam_name().parse(alias).into_result().is_ok() {
        diagnostics.push(Diagnostic::error(
            line,
            format!("object alias `{alias}` is not a valid name"),
        ));
        return None;
    }
    Some((Some(alias.to_owned()), tail.trim()))
}

fn parse_object_body_definition(
    text: &str,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ObjectBodyDefinition> {
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
        definition: finalize_definition_expr(definition, line, diagnostics),
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

    let mut body = Vec::new();
    for (offset, body_line) in body_lines.iter().enumerate() {
        if body_line.trim().is_empty() {
            continue;
        }
        let body_line_number = line + offset + 1;
        let Some(definition) =
            parse_object_body_definition(body_line.trim(), body_line_number, diagnostics)
        else {
            continue;
        };
        body.push(definition);
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
    path()
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

fn parse_expr_result(text: &str) -> Result<SyntaxExpr, String> {
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

#[cfg(test)]
fn parse_expr(text: &str) -> Option<SyntaxExpr> {
    parse_expr_result(text).ok()
}

fn finalize_definition_expr(
    mut definition: DefinitionDecl,
    line: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> DefinitionDecl {
    match parse_expr_result(definition.body.as_str()) {
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
        SyntaxExpr::Number(_) | SyntaxExpr::Text(_) => {}
        SyntaxExpr::Name(_) | SyntaxExpr::PriorName(_) | SyntaxExpr::EscapedName(_) => {}
        SyntaxExpr::Access(base, parts) => {
            analyze_expr_locals(base, line, diagnostics);
            for part in parts {
                analyze_key_expr_locals(part, line, diagnostics);
            }
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
        SyntaxExpr::Apply(function, argument)
        | SyntaxExpr::Multiply(function, argument)
        | SyntaxExpr::Divide(function, argument)
        | SyntaxExpr::Add(function, argument)
        | SyntaxExpr::Subtract(function, argument)
        | SyntaxExpr::Append(function, argument) => {
            analyze_expr_locals(function, line, diagnostics);
            analyze_expr_locals(argument, line, diagnostics);
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

fn mark_used_locals(expr: &SyntaxExpr, locals: &[LocalName], used: &mut [bool]) {
    match expr {
        SyntaxExpr::Number(_) | SyntaxExpr::Text(_) => {}
        SyntaxExpr::Name(name) => {
            if let Some(index) = locals
                .iter()
                .rposition(|local| local.canonical.as_deref() == Some(name.as_str()))
            {
                used[index] = true;
            }
        }
        SyntaxExpr::PriorName(_) | SyntaxExpr::EscapedName(_) => {}
        SyntaxExpr::Access(base, parts) => {
            mark_used_locals(base, locals, used);
            for part in parts {
                mark_used_key_expr(part, locals, used);
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
        SyntaxExpr::Apply(function, argument)
        | SyntaxExpr::Multiply(function, argument)
        | SyntaxExpr::Divide(function, argument)
        | SyntaxExpr::Add(function, argument)
        | SyntaxExpr::Subtract(function, argument)
        | SyntaxExpr::Append(function, argument) => {
            mark_used_locals(function, locals, used);
            mark_used_locals(argument, locals, used);
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

    #[derive(Debug, Clone)]
    enum PathSuffix {
        Single(SyntaxKeyExpr),
        Expand(Vec<SyntaxKeyExpr>),
    }

    fn resolve_infix_chain(
        first: SyntaxExpr,
        rest: Vec<(crate::core::Builtin, SyntaxExpr)>,
    ) -> Result<SyntaxExpr, String> {
        let mut exprs = vec![first];
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
            exprs.push(next_expr);
        }

        while !ops.is_empty() {
            reduce_top_operator(&mut exprs, &mut ops)?;
        }

        exprs
            .pop()
            .ok_or_else(|| "operator chain did not produce an expression".to_owned())
    }

    fn reduce_top_operator(
        exprs: &mut Vec<SyntaxExpr>,
        ops: &mut Vec<crate::core::Builtin>,
    ) -> Result<(), String> {
        let right = exprs
            .pop()
            .ok_or_else(|| "missing right operand in operator chain".to_owned())?;
        let left = exprs
            .pop()
            .ok_or_else(|| "missing left operand in operator chain".to_owned())?;
        let op = ops
            .pop()
            .ok_or_else(|| "missing operator in operator chain".to_owned())?;
        exprs.push(syntax_binary_expr(op, left, right));
        Ok(())
    }

    fn infix_operator_relation(
        left: crate::core::Builtin,
        right: crate::core::Builtin,
    ) -> OperatorRelation {
        use crate::core::Builtin::{Add, Append, Divide, Multiply, Subtract};

        match (left, right) {
            (Append, Append) => OperatorRelation::Same(Associativity::Left),
            (Append, Add | Subtract | Multiply | Divide) => OperatorRelation::Weaker,
            (Add | Subtract | Multiply | Divide, Append) => OperatorRelation::Stronger,
            (Add, Add) => OperatorRelation::Same(Associativity::Left),
            (Add, Subtract) => OperatorRelation::Unrelated,
            (Subtract, Add) => OperatorRelation::Unrelated,
            (Subtract, Subtract) => OperatorRelation::Same(Associativity::None),
            (Add | Subtract, Multiply | Divide) => OperatorRelation::Weaker,
            (Multiply | Divide, Add | Subtract) => OperatorRelation::Stronger,
            (Multiply, Multiply) => OperatorRelation::Same(Associativity::Left),
            (Multiply, Divide) => OperatorRelation::Same(Associativity::Left),
            (Divide, Multiply) => OperatorRelation::Same(Associativity::Left),
            (Divide, Divide) => OperatorRelation::Same(Associativity::None),
            _ => OperatorRelation::Unrelated,
        }
    }

    fn infix_operator_symbol(builtin: crate::core::Builtin) -> &'static str {
        match builtin {
            crate::core::Builtin::Append => "++",
            crate::core::Builtin::Add => "+",
            crate::core::Builtin::Subtract => "-",
            crate::core::Builtin::Multiply => "*",
            crate::core::Builtin::Divide => "/",
            crate::core::Builtin::Fixpoint => "fixpoint",
            crate::core::Builtin::Anno => "anno",
            crate::core::Builtin::MergeDuplicate => "merge_duplicate",
            crate::core::Builtin::UpdateDuplicate => "update_duplicate",
            crate::core::Builtin::Floor => "floor",
            crate::core::Builtin::Mod => "mod",
            crate::core::Builtin::Slice => "slice",
            crate::core::Builtin::Map => "map",
            crate::core::Builtin::DictSingleton => ":",
            crate::core::Builtin::DictUnion => "{,}",
            crate::core::Builtin::DictUpdate => "dict_update",
            crate::core::Builtin::DictRemove => "dict_remove",
            crate::core::Builtin::ObjectInstance => "object_instance",
        }
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

    fn access_if_path(base: SyntaxExpr, suffixes: Vec<PathSuffix>) -> SyntaxExpr {
        match flatten_path_suffixes(suffixes) {
            parts if parts.is_empty() => base,
            parts => SyntaxExpr::Access(Box::new(base), parts),
        }
    }

    let parser = recursive(|expr| {
        let name = glam_name().boxed();
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
                name.clone()
                    .map(SyntaxKeyExpr::Atom)
                    .map(PathSuffix::Single),
            )))
            .repeated()
            .collect::<Vec<_>>();

        let prior_name = just('_')
            .ignore_then(name.clone())
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(SyntaxExpr::PriorName(name), suffixes))
            .boxed();
        let escaped_name = just('^')
            .repeated()
            .at_least(1)
            .ignore_then(name.clone())
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(SyntaxExpr::EscapedName(name), suffixes))
            .boxed();
        let name_expr = name
            .clone()
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(SyntaxExpr::Name(name), suffixes))
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

        let literal_atom = choice((text, list, dict, number, parenthesized)).boxed();
        let literal_expr = literal_atom
            .then(path_suffix.clone())
            .map(|(base, suffixes)| access_if_path(base, suffixes))
            .boxed();
        let atom = choice((literal_expr, escaped_name, prior_name, name_expr)).boxed();
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
        let infix_operator = choice((
            just("++").to(crate::core::Builtin::Append),
            just('*').to(crate::core::Builtin::Multiply),
            just('/').to(crate::core::Builtin::Divide),
            just('+')
                .then_ignore(just('+').not())
                .to(crate::core::Builtin::Add),
            just('-').to(crate::core::Builtin::Subtract),
        ));

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

fn syntax_binary_expr(
    builtin: crate::core::Builtin,
    left: SyntaxExpr,
    right: SyntaxExpr,
) -> SyntaxExpr {
    match builtin {
        crate::core::Builtin::Append => SyntaxExpr::Append(Box::new(left), Box::new(right)),
        crate::core::Builtin::Add => SyntaxExpr::Add(Box::new(left), Box::new(right)),
        crate::core::Builtin::Subtract => SyntaxExpr::Subtract(Box::new(left), Box::new(right)),
        crate::core::Builtin::Multiply => SyntaxExpr::Multiply(Box::new(left), Box::new(right)),
        crate::core::Builtin::Divide => SyntaxExpr::Divide(Box::new(left), Box::new(right)),
        other => panic!("unexpected infix builtin in syntax parser: {other:?}"),
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

    fn core_append(left: CoreExpr, right: CoreExpr) -> CoreExpr {
        core_builtin2(Builtin::Append, left, right)
    }

    fn core_singleton(key: CoreExpr, value: CoreExpr) -> CoreExpr {
        core_builtin2(Builtin::DictSingleton, key, value)
    }

    fn core_dict_union(left: CoreExpr, right: CoreExpr) -> CoreExpr {
        core_builtin2(Builtin::DictUnion, left, right)
    }

    fn core_global_access(context: &CompileContext, path: Vec<CoreKeyExpr>) -> CoreExpr {
        let Value::Expr(thunk) = &context.final_defs else {
            panic!("final module binding should be a lazy expression");
        };
        CoreExpr::Access(thunk.expr.clone(), Arc::from(path))
    }

    fn core_visible_access(base: CoreExpr, path: Vec<CoreKeyExpr>) -> CoreExpr {
        CoreExpr::Access(Arc::new(base), Arc::from(path))
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
                let bytes = std::cell::RefCell::new(Vec::new());
                list.for_each_segment(
                    &mut |segment| {
                        bytes.borrow_mut().extend_from_slice(segment);
                        Ok::<_, String>(())
                    },
                    &mut |segment| {
                        for item in segment.iter() {
                            let item = fully_evaluated_value(item.clone());
                            let Value::Number(number) = item else {
                                return Err(
                                    "output list must contain only integers and binary segments"
                                        .to_owned(),
                                );
                            };
                            let byte = number.to_u8_if_integer().ok_or_else(|| {
                                format!(
                                    "output list contains number `{number}` that is not an in-range byte integer"
                                )
                            })?;
                            bytes.borrow_mut().push(byte);
                        }
                        Ok(())
                    },
                )
                .expect("output list should render as bytes");
                bytes.into_inner()
            }
            other => panic!("expected binary output value, got {other:?}"),
        }
    }

    fn resolved_expr_at_path(definitions: &Value, path: &[&str]) -> CoreExpr {
        let value = resolved_value_at_path(definitions, path);
        let Value::Expr(thunk) = value else {
            panic!("binding should resolve to a lazy expression");
        };
        thunk.expr.as_ref().clone()
    }

    fn core_builtin2(builtin: Builtin, left: CoreExpr, right: CoreExpr) -> CoreExpr {
        CoreExpr::Apply(
            Arc::new(CoreExpr::Apply(
                Arc::new(CoreExpr::Value(Value::Builtin(builtin))),
                Arc::new(left),
            )),
            Arc::new(right),
        )
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
        let parsed = parse("language g0\nimport 'std as std\nimport 'math\n");

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
                        definition: DefinitionDecl {
                            target: "text".to_owned(),
                            kind: DefinitionKind::Introduce,
                            body: "\"Hello\"".to_owned(),
                            expr: Some(SyntaxExpr::Text("Hello".to_owned())),
                        },
                    },
                    ObjectBodyDefinition {
                        line: 4,
                        text: "target := \"World\"".to_owned(),
                        definition: DefinitionDecl {
                            target: "target".to_owned(),
                            kind: DefinitionKind::Override,
                            body: "\"World\"".to_owned(),
                            expr: Some(SyntaxExpr::Text("World".to_owned())),
                        },
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
                        definition: DefinitionDecl {
                            target: "text".to_owned(),
                            kind: DefinitionKind::Override,
                            body: "_text ++ \"!\"".to_owned(),
                            expr: Some(SyntaxExpr::Append(
                                Box::new(SyntaxExpr::PriorName("text".to_owned())),
                                Box::new(SyntaxExpr::Text("!".to_owned())),
                            )),
                        },
                    },
                    ObjectBodyDefinition {
                        line: 4,
                        text: "tail = \"done\"".to_owned(),
                        definition: DefinitionDecl {
                            target: "tail".to_owned(),
                            kind: DefinitionKind::Introduce,
                            body: "\"done\"".to_owned(),
                            expr: Some(SyntaxExpr::Text("done".to_owned())),
                        },
                    },
                ],
            })
        );
    }

    #[test]
    fn parses_object_and_extend_aliases() {
        let parsed = parse(
            "language g0\nobject child as c extends base with\n  text = c.base\nextend child as c with\n  text := _c.text ++ \"!\"\n",
        );

        assert_eq!(parsed.diagnostics, []);
        match &parsed.declarations[1].kind {
            DeclarationKind::Object(object) => {
                assert_eq!(object.target, "child");
                assert_eq!(object.alias.as_deref(), Some("c"));
                assert_eq!(object.deps, ["base".to_owned()]);
            }
            other => panic!("expected object declaration, got {other:?}"),
        }
        match &parsed.declarations[2].kind {
            DeclarationKind::Extend(extend) => {
                assert_eq!(extend.target, "child");
                assert_eq!(extend.alias.as_deref(), Some("c"));
            }
            other => panic!("expected extend declaration, got {other:?}"),
        }
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
            Some(SyntaxExpr::Access(
                Box::new(SyntaxExpr::EscapedName("prefix".to_owned())),
                vec![SyntaxKeyExpr::Atom("value".to_owned())],
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
            None,
            "whitespace before `.` should prevent dotted-path parsing"
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
            resolved_expr_at_path(&value, &["asm", "result"]),
            core_append(
                CoreExpr::List(Arc::from([
                    Arc::new(CoreExpr::Value(Value::Number(72.into()))),
                    Arc::new(CoreExpr::Value(Value::Number(101.into()))),
                ])),
                CoreExpr::List(Arc::from([
                    Arc::new(CoreExpr::Value(Value::Number(108.into()))),
                    Arc::new(CoreExpr::Value(Value::Number(108.into()))),
                    Arc::new(CoreExpr::Value(Value::Number(111.into()))),
                ])),
            )
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
            resolved_expr_at_path(&value, &["asm", "result"]),
            core_append(
                core_append(
                    core_append(
                        core_global_access(
                            &context,
                            vec![CoreKeyExpr::Key(Key::atom_from_text("hello"))]
                        ),
                        CoreExpr::Value(Value::binary_from_text(", ")),
                    ),
                    core_global_access(
                        &context,
                        vec![CoreKeyExpr::Key(Key::atom_from_text("world"))]
                    ),
                ),
                CoreExpr::Value(Value::binary_from_text("!")),
            )
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
            resolved_expr_at_path(&value, &["asm", "result"]),
            core_append(
                core_visible_access(
                    CoreExpr::Value(context.prior_defs.clone()),
                    vec![CoreKeyExpr::Key(Key::atom_from_text("hello"))],
                ),
                CoreExpr::Value(Value::binary_from_text("!")),
            )
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
            "language g0\nprefix = \"Hello\"\nobject hello with\n  target = \"World\"\n  text = ^prefix ++ \", \" ++ target ++ \"!\"\nasm.result = hello.text\n",
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
    fn lowers_lambda_and_application_expressions_to_core_terms() {
        let parsed = parse("language g0\nasm.result = (\\x -> x.tail) d\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            resolved_expr_at_path(&value, &["asm", "result"]),
            CoreExpr::Apply(
                Arc::new(CoreExpr::Lambda(Arc::new(CoreExpr::Access(
                    Arc::new(CoreExpr::Local(0)),
                    Arc::from([CoreKeyExpr::Key(Key::atom_from_text("tail"))]),
                )))),
                Arc::new(core_global_access(
                    &context,
                    vec![CoreKeyExpr::Key(Key::atom_from_text("d"))]
                )),
            )
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
            resolved_expr_at_path(&value, &["id"]),
            CoreExpr::Lambda(Arc::new(CoreExpr::Local(0)))
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
        let parsed = parse("language g0\nkeep _value = value\n");
        let context = CompileContext::default();
        let lowered = lower_to_core_with_context(&parsed, &context);
        assert_eq!(lowered.diagnostics, []);

        let value = evaluated_module_value(&context, &lowered);
        assert_eq!(
            resolved_expr_at_path(&value, &["keep"]),
            CoreExpr::Lambda(Arc::new(CoreExpr::Local(0)))
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
        assert_eq!(
            resolved_expr_at_path(&value, &["d"]),
            core_dict_union(
                core_singleton(
                    CoreExpr::Value(Value::Atom(Atom::from_key(&Key::binary_from_text("hello")))),
                    CoreExpr::Value(Value::binary_from_text("Hello")),
                ),
                core_singleton(
                    CoreExpr::Value(Value::Atom(Atom::from_key(&Key::binary_from_text("world")))),
                    core_append(
                        core_global_access(
                            &context,
                            vec![CoreKeyExpr::Key(Key::atom_from_text("other"))]
                        ),
                        CoreExpr::Value(Value::binary_from_text("!")),
                    ),
                ),
            )
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
        let parsed = parse("language g0\nimport 'std as std\nimport 'math\n");
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
        let anno = match &std {
            Value::Dict(std) => std
                .get(&Key::atom_from_text("anno"))
                .expect("std import should expose anno"),
            _ => unreachable!(),
        };

        let Value::Dict(_) = std else {
            panic!("std import should evaluate to a dictionary");
        };
        assert!(matches!(std, Value::Dict(_)));
        assert!(matches!(anno, Value::Builtin(crate::core::Builtin::Anno)));
        assert!(matches!(floor, Value::Builtin(crate::core::Builtin::Floor)));
        assert!(matches!(mod_fn, Value::Builtin(crate::core::Builtin::Mod)));
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

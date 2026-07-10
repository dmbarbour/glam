use chumsky::prelude::*;

use std::sync::Arc;

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
    Object,
    Extend,
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
    pub placement: ImportPlacement,
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
    Name(Vec<SyntaxKeyExpr>),
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
    let mut definitions = context.expr_value(context.prior_defs.clone());
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
                if let Err(diagnostic) =
                    lower_definition(definition, declaration.line, context, &mut definitions)
                {
                    diagnostics.push(diagnostic);
                }
            }
            _ => {}
        }
    }

    LoweredSource {
        definitions: context.value_expr(definitions),
        diagnostics,
    }
}

fn lower_import(
    import: &ImportDecl,
    line: usize,
    context: &CompileContext,
    definitions: &mut CoreExpr,
) -> Result<(), Diagnostic> {
    let ImportReference::Builtin(name) = &import.reference else {
        return Err(Diagnostic::error(
            line,
            "local `import ...` is not supported by the current spike",
        ));
    };
    let module = builtin_module_value(context, name)
        .ok_or_else(|| Diagnostic::error(line, format!("unknown built-in module `'{name}`")))?;

    *definitions = match &import.placement {
        ImportPlacement::Inline => union_module_expr(
            definitions.clone(),
            value_to_core_expr(&module, context),
            context,
        ),
        ImportPlacement::As(target) => union_module_expr(
            definitions.clone(),
            path_to_dict_expr(target, value_to_core_expr(&module, context), context)?,
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

fn lower_unique(
    names: &[String],
    _line: usize,
    context: &CompileContext,
    definitions: &mut CoreExpr,
) -> Result<(), Diagnostic> {
    for name in names {
        let path = context.abstract_global_path(name);
        let value = context.abstract_global_path_value(path.as_ref());
        *definitions = union_module_expr(
            definitions.clone(),
            path_to_dict_expr(name, context.expr_value(value), context)?,
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
    line: usize,
    context: &CompileContext,
    definitions: &mut CoreExpr,
) -> Result<(), Diagnostic> {
    let Some(expr) = &definition.expr else {
        return Ok(());
    };

    let value = syntax_expr_to_value(expr, line, context)?;
    let value = match definition.kind {
        DefinitionKind::Introduce => annotate_definition_value(
            BuiltinAssertion::Undefined,
            &definition.target,
            value,
            context,
        )?,
        DefinitionKind::Override => annotate_definition_value(
            BuiltinAssertion::Defined,
            &definition.target,
            value,
            context,
        )?,
        DefinitionKind::Update => Err(Diagnostic::error(
            line,
            "update definitions are not supported by the .g spike lowering",
        ))?,
    };
    *definitions = union_module_expr(
        definitions.clone(),
        path_to_dict_expr(
            &definition.target,
            value_to_core_expr(&value, context),
            context,
        )?,
        context,
    );

    Ok(())
}

#[derive(Clone, Copy)]
enum BuiltinAssertion {
    Defined,
    Undefined,
}

fn annotate_definition_value(
    assertion: BuiltinAssertion,
    target: &str,
    value: Value,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    let tag = match assertion {
        BuiltinAssertion::Defined => "assert_defined",
        BuiltinAssertion::Undefined => "assert_undefined",
    };
    let payload = context.builtin_apply2_expr(
        Builtin::DictUnion,
        context.builtin_apply2_expr(
            Builtin::DictSingleton,
            context.expr_value(context.value_atom(atom_from_str("name"))),
            context.expr_value(context.value_binary(target)),
        ),
        context.builtin_apply2_expr(
            Builtin::DictSingleton,
            context.expr_value(context.value_atom(atom_from_str("value"))),
            prior_path_expr(target, context)?,
        ),
    );
    let annotation = context.builtin_apply2_expr(
        Builtin::DictSingleton,
        context.expr_value(context.value_atom(atom_from_str(tag))),
        payload,
    );

    Ok(context.value_expr(context.builtin_apply2_expr(
        Builtin::Anno,
        annotation,
        value_to_core_expr(&value, context),
    )))
}

fn union_module_expr(definitions: CoreExpr, item: CoreExpr, context: &CompileContext) -> CoreExpr {
    context.builtin_apply2_expr(Builtin::DictUnion, definitions, item)
}

fn value_to_core_expr(value: &Value, context: &CompileContext) -> CoreExpr {
    match value {
        Value::Expr(thunk) if thunk.env.is_empty() => thunk.expr.as_ref().clone(),
        _ => context.expr_value(value.clone()),
    }
}

fn prior_path_expr(target: &str, context: &CompileContext) -> Result<CoreExpr, Diagnostic> {
    let path = target
        .split('.')
        .map(|part| context.key_expr_key(name_as_key(part)))
        .collect::<Vec<_>>();
    Ok(context.expr_access(context.expr_value(context.prior_defs.clone()), path))
}

fn path_to_dict_expr(
    target: &str,
    value: CoreExpr,
    context: &CompileContext,
) -> Result<CoreExpr, Diagnostic> {
    let parts = target.split('.').collect::<Vec<_>>();
    if parts.is_empty() {
        return Err(Diagnostic::error(0, "definition target cannot be empty"));
    }

    let mut expr = value;
    for part in parts.into_iter().rev() {
        expr = context.builtin_apply2_expr(
            Builtin::DictSingleton,
            context.expr_value(context.value_atom(atom_from_str(part))),
            expr,
        );
    }
    Ok(expr)
}

fn syntax_expr_to_value(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
) -> Result<Value, Diagnostic> {
    match expr {
        SyntaxExpr::Number(number) => Ok(context.value_number(number.clone())),
        SyntaxExpr::Text(text) => Ok(context.value_binary(text)),
        SyntaxExpr::Name(_)
        | SyntaxExpr::SingletonDict(_, _)
        | SyntaxExpr::DictUnion(_)
        | SyntaxExpr::List(_)
        | SyntaxExpr::Lambda(_, _)
        | SyntaxExpr::Apply(_, _)
        | SyntaxExpr::Multiply(_, _)
        | SyntaxExpr::Divide(_, _)
        | SyntaxExpr::Add(_, _)
        | SyntaxExpr::Subtract(_, _)
        | SyntaxExpr::Append(_, _) => {
            Ok(context.value_expr(syntax_expr_to_core_expr(expr, line, context)?))
        }
    }
}

fn syntax_expr_to_core_expr(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
) -> Result<CoreExpr, Diagnostic> {
    syntax_expr_to_core_expr_in_scope(expr, line, context, &mut Vec::new())
}

fn syntax_expr_to_core_expr_in_scope(
    expr: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    locals: &mut Vec<LocalName>,
) -> Result<CoreExpr, Diagnostic> {
    Ok(match expr {
        SyntaxExpr::Number(number) => context.expr_value(context.value_number(number.clone())),
        SyntaxExpr::Text(text) => context.expr_value(context.value_binary(text)),
        SyntaxExpr::SingletonDict(key, value) => context.builtin_apply2_expr(
            Builtin::DictSingleton,
            syntax_key_expr_to_core_expr(key, line, context, locals)?,
            syntax_expr_to_core_expr_in_scope(value, line, context, locals)?,
        ),
        SyntaxExpr::DictUnion(items) => lower_dict_union(items, line, context, locals)?,
        SyntaxExpr::Name(parts) => lower_name_expr(parts, line, context, locals)?,
        SyntaxExpr::List(items) => context.expr_list(
            items
                .iter()
                .map(|expr| {
                    syntax_expr_to_core_expr_in_scope(expr, line, context, locals).map(Arc::new)
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SyntaxExpr::Lambda(params, body) => lower_lambda_expr(params, body, line, context, locals)?,
        SyntaxExpr::Apply(function, argument) => context.expr_apply(
            syntax_expr_to_core_expr_in_scope(function, line, context, locals)?,
            syntax_expr_to_core_expr_in_scope(argument, line, context, locals)?,
        ),
        SyntaxExpr::Multiply(left, right) => {
            lower_builtin_expr(Builtin::Multiply, left, right, line, context, locals)?
        }
        SyntaxExpr::Divide(left, right) => {
            lower_builtin_expr(Builtin::Divide, left, right, line, context, locals)?
        }
        SyntaxExpr::Add(left, right) => {
            lower_builtin_expr(Builtin::Add, left, right, line, context, locals)?
        }
        SyntaxExpr::Subtract(left, right) => {
            lower_builtin_expr(Builtin::Subtract, left, right, line, context, locals)?
        }
        SyntaxExpr::Append(left, right) => {
            lower_builtin_expr(Builtin::Append, left, right, line, context, locals)?
        }
    })
}

fn lower_builtin_expr(
    builtin: Builtin,
    left: &SyntaxExpr,
    right: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    locals: &mut Vec<LocalName>,
) -> Result<CoreExpr, Diagnostic> {
    Ok(context.builtin_apply2_expr(
        builtin,
        syntax_expr_to_core_expr_in_scope(left, line, context, locals)?,
        syntax_expr_to_core_expr_in_scope(right, line, context, locals)?,
    ))
}

fn syntax_key_expr_to_core_expr(
    key: &SyntaxKeyExpr,
    line: usize,
    context: &CompileContext,
    locals: &mut Vec<LocalName>,
) -> Result<CoreExpr, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => context.expr_value(context.value_atom(atom_from_str(name))),
        SyntaxKeyExpr::Index(expr) => {
            syntax_expr_to_core_expr_in_scope(expr, line, context, locals)?
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
    locals: &mut Vec<LocalName>,
) -> Result<CoreExpr, Diagnostic> {
    let mut items = items.iter();
    let Some(first) = items.next() else {
        return Ok(context.expr_value(context.empty_dict_value()));
    };

    let mut expr = syntax_expr_to_core_expr_in_scope(first, line, context, locals)?;
    for item in items {
        expr = context.builtin_apply2_expr(
            Builtin::DictUnion,
            expr,
            syntax_expr_to_core_expr_in_scope(item, line, context, locals)?,
        );
    }
    Ok(expr)
}

fn lower_lambda_expr(
    params: &[String],
    body: &SyntaxExpr,
    line: usize,
    context: &CompileContext,
    locals: &mut Vec<LocalName>,
) -> Result<CoreExpr, Diagnostic> {
    let base_len = locals.len();
    locals.extend(params.iter().map(|param| local_name_metadata(param)));
    let mut lowered = syntax_expr_to_core_expr_in_scope(body, line, context, locals)?;
    locals.truncate(base_len);

    for _ in params.iter().rev() {
        lowered = context.expr_lambda(lowered);
    }

    Ok(lowered)
}

fn lower_name_expr(
    parts: &[SyntaxKeyExpr],
    line: usize,
    context: &CompileContext,
    locals: &mut Vec<LocalName>,
) -> Result<CoreExpr, Diagnostic> {
    let Some(SyntaxKeyExpr::Atom(first)) = parts.first() else {
        return Err(Diagnostic::error(
            line,
            "first part of name expression must be an atom",
        ));
    };

    // TODO: special keyword atoms like 'self' and 'module'
    // TODO: binding to prior names (part of atom or as flag on lower)

    let Some(local_index) = local_binding_index(first, locals) else {
        return Ok(context.expr_access(
            context.expr_value(context.final_defs.clone()),
            parts
                .iter()
                .map(|part| syntax_key_expr_to_core(part, line, context, locals))
                .collect::<Result<Vec<_>, _>>()?,
        ));
    };

    if parts.len() == 1 {
        return Ok(context.expr_local(local_index));
    }

    Ok(context.expr_access(
        context.expr_local(local_index),
        parts[1..]
            .iter()
            .map(|part| syntax_key_expr_to_core(part, line, context, locals))
            .collect::<Result<Vec<_>, _>>()?,
    ))
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
    locals: &mut Vec<LocalName>,
) -> Result<CoreKeyExpr, Diagnostic> {
    Ok(match key {
        SyntaxKeyExpr::Atom(name) => context.key_expr_key(name_as_key(name)),
        SyntaxKeyExpr::Index(expr) => context.key_expr_index(syntax_expr_to_core_expr_in_scope(
            expr, line, context, locals,
        )?),
        SyntaxKeyExpr::PathIndex(expr) => context.key_expr_path_index(
            syntax_expr_to_core_expr_in_scope(expr, line, context, locals)?,
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
        Some("object") => return DeclarationKind::Object,
        Some("extend") | Some("extends") => return DeclarationKind::Extend,
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

    just("import")
        .padded()
        .ignore_then(reference)
        .then(placement)
        .map(|(reference, placement)| ImportDecl {
            reference,
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
                    body: desugar_definition_body(&params, body),
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

fn desugar_definition_body(params: &[String], body: String) -> String {
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
        SyntaxExpr::Name(parts) => {
            for part in parts.iter().skip(1) {
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
        SyntaxExpr::Name(parts) => {
            if let Some(SyntaxKeyExpr::Atom(name)) = parts.first() {
                if let Some(index) = locals
                    .iter()
                    .rposition(|local| local.canonical.as_deref() == Some(name.as_str()))
                {
                    used[index] = true;
                }
            }
            for part in parts.iter().skip(1) {
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

    #[derive(Debug)]
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
            crate::core::Builtin::Floor => "floor",
            crate::core::Builtin::Mod => "mod",
            crate::core::Builtin::Slice => "slice",
            crate::core::Builtin::Map => "map",
            crate::core::Builtin::DictSingleton => ":",
            crate::core::Builtin::DictUnion => "{,}",
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
        let name_expr = name
            .clone()
            .map(SyntaxKeyExpr::Atom)
            .then(
                just('.')
                    .ignore_then(choice((
                        path_list_shorthand,
                        path_list_expr,
                        name.clone()
                            .map(SyntaxKeyExpr::Atom)
                            .map(PathSuffix::Single),
                    )))
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(|(first, suffixes)| {
                let mut parts = vec![first];
                for suffix in suffixes {
                    match suffix {
                        PathSuffix::Single(part) => parts.push(part),
                        PathSuffix::Expand(items) => parts.extend(items),
                    }
                }
                SyntaxExpr::Name(parts)
            });

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

        let atom = choice((text, list, dict, number, name_expr, parenthesized)).boxed();
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
    use crate::core::{Builtin, Expr as CoreExpr, Key, KeyExpr as CoreKeyExpr, Value};
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

    fn core_global_access(path: Vec<CoreKeyExpr>) -> CoreExpr {
        CoreExpr::Access(Arc::new(CoreExpr::Local(0)), Arc::from(path))
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
                placement: ImportPlacement::As("conf".to_owned()),
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
                placement: ImportPlacement::As("std".to_owned()),
            })
        );
        assert_eq!(
            parsed.declarations[2].kind,
            DeclarationKind::Import(ImportDecl {
                reference: ImportReference::Builtin("math".to_owned()),
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
                            Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom(
                                "hello".to_owned(),
                            )])),
                            Box::new(SyntaxExpr::Text(", ".to_owned())),
                        )),
                        Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom(
                            "world".to_owned(),
                        )])),
                    )),
                    Box::new(SyntaxExpr::Text("!".to_owned())),
                )),
            })
        );
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
                        Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("x".to_owned())])),
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
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("x".to_owned()),
                        SyntaxKeyExpr::Atom("tail".to_owned()),
                    ])),
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
                            Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom(
                                "value".to_owned(),
                            )])),
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
                    Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("x".to_owned())])),
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
                    Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom(
                        "value".to_owned()
                    )])),
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
                    Box::new(SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("y".to_owned())])),
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
                    SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("left".to_owned())]),
                    SyntaxExpr::Name(vec![SyntaxKeyExpr::Atom("right".to_owned())]),
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
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("d".to_owned()),
                        SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(42)))),
                    ])),
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("d".to_owned()),
                        SyntaxKeyExpr::Atom("tail".to_owned()),
                    ])),
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
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("foo".to_owned()),
                        SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(1)))),
                        SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(2)))),
                        SyntaxKeyExpr::Index(Box::new(SyntaxExpr::Number(n(3)))),
                    ])),
                    Box::new(SyntaxExpr::Name(vec![
                        SyntaxKeyExpr::Atom("foo".to_owned()),
                        SyntaxKeyExpr::PathIndex(Box::new(SyntaxExpr::Append(
                            Box::new(SyntaxExpr::List(vec![
                                SyntaxExpr::Number(n(1)),
                                SyntaxExpr::Number(n(2)),
                            ])),
                            Box::new(SyntaxExpr::List(vec![SyntaxExpr::Number(n(3))])),
                        ))),
                    ])),
                )),
            })
        );
    }

    #[test]
    fn dotted_paths_require_tight_dots() {
        assert!(matches!(
            parse_expr("foo.[  42  ].bar"),
            Some(SyntaxExpr::Name(_))
        ));
        assert!(matches!(
            parse_expr("foo.([1,2] ++ [3]).bar"),
            Some(SyntaxExpr::Name(_))
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
                        core_global_access(vec![CoreKeyExpr::Key(Key::atom_from_text("hello"))]),
                        CoreExpr::Value(Value::binary_from_text(", ")),
                    ),
                    core_global_access(vec![CoreKeyExpr::Key(Key::atom_from_text("world"))]),
                ),
                CoreExpr::Value(Value::binary_from_text("!")),
            )
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
                Arc::new(core_global_access(vec![CoreKeyExpr::Key(
                    Key::atom_from_text("d")
                )])),
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
                        core_global_access(vec![CoreKeyExpr::Key(Key::atom_from_text("other"))]),
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
    fn inline_builtin_imports_use_dict_union_semantics() {
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
    fn duplicate_introductions_stay_lazy_until_observed() {
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
            "dictionary union is ambiguous at key `foo`"
        );
    }
}

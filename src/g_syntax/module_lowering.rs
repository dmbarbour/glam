use super::*;

mod definitions;
mod imports;
mod objects;

pub(in crate::g_syntax) use definitions::*;
pub(in crate::g_syntax) use imports::*;
pub(in crate::g_syntax) use objects::*;

pub fn lower_to_core_with_context(parsed: ParsedSource, context: &CompileContext) -> LoweredSource {
    // note: we'll extend 'prior' within the 'body' of an implicit lambda
    let mut definitions = context.prior_defs().clone();
    let ParsedSource {
        declarations,
        mut diagnostics,
    } = parsed;

    for declaration in &declarations {
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

pub(super) fn lower_definition(
    definition: &DefinitionDecl,
    declaration_text: &str,
    line: usize,
    context: &CompileContext,
    definitions: &mut Value,
    scope: &NameScope,
) -> Result<(), Diagnostic> {
    let mut locals = ResolverContext::default();
    let definitions_root = ResolvedRoot::Provided(definitions.clone());
    let resolved = lower_definition_resolved(
        definition,
        declaration_text,
        line,
        context,
        &definitions_root,
        &scope.resolved(),
        &mut locals,
    )?;
    *definitions = lower_resolved_expr(resolved);
    Ok(())
}

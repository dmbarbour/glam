//! Token-native top-level declarations that do not contain expressions.

use chumsky::error::Rich;
use chumsky::prelude::*;

use super::super::super::keywords::{g0_keyword, reserved_keyword_message};
use super::super::super::{
    DeclarationKind, ImportDecl, ImportPlacement, ImportReference, LanguageDecl,
};
use super::super::input::{
    ParseSession, TokenExtra, TokenInput, TokenView, joint, keyword, line_start, name, symbol,
    text_id,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::g_syntax::parser) enum SimpleDeclaration {
    Language,
    Import,
    Abstract,
    Unique,
}

impl SimpleDeclaration {
    pub(in crate::g_syntax::parser) fn from_head(head: &str) -> Option<Self> {
        match head {
            "language" => Some(Self::Language),
            "import" => Some(Self::Import),
            "abstract" => Some(Self::Abstract),
            "unique" => Some(Self::Unique),
            _ => None,
        }
    }
}

pub(in crate::g_syntax::parser) fn parse_simple_declaration<'lex, 'source: 'lex>(
    view: TokenView<'lex, 'source>,
    line: usize,
    declaration: SimpleDeclaration,
    session: &mut ParseSession,
) -> DeclarationKind {
    match declaration {
        SimpleDeclaration::Language => parse_with(
            view,
            line,
            session,
            language_decl().map(DeclarationKind::Language),
        ),
        SimpleDeclaration::Import => parse_with(
            view,
            line,
            session,
            import_decl(view).map(DeclarationKind::Import),
        ),
        SimpleDeclaration::Abstract => parse_with(
            view,
            line,
            session,
            keyword_name_list("abstract").map(DeclarationKind::Abstract),
        ),
        SimpleDeclaration::Unique => parse_with(
            view,
            line,
            session,
            keyword_name_list("unique").map(DeclarationKind::Unique),
        ),
    }
}

fn parse_with<'lex, 'source: 'lex, P>(
    view: TokenView<'lex, 'source>,
    line: usize,
    session: &mut ParseSession,
    parser: P,
) -> DeclarationKind
where
    P: Parser<'lex, TokenInput<'lex, 'source>, DeclarationKind, TokenExtra<'lex, 'source>>,
{
    let (output, errors) = line_start()
        .ignore_then(parser)
        .then_ignore(end())
        .parse(view.chumsky_input())
        .into_output_errors();
    session.record_token_errors_at_line(view, line, errors);
    output.unwrap_or(DeclarationKind::Unknown)
}

fn language_decl<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, LanguageDecl, TokenExtra<'lex, 'source>> {
    keyword("language")
        .then_ignore(layout_padding())
        .ignore_then(name().map(str::to_owned))
        .then(
            padded(keyword("with"))
                .ignore_then(
                    name()
                        .map(str::to_owned)
                        .separated_by(padded(symbol(",")))
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

fn import_decl<'lex, 'source: 'lex>(
    view: TokenView<'lex, 'source>,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, ImportDecl, TokenExtra<'lex, 'source>> {
    let reference = choice((
        text_id().map(move |id| {
            ImportReference::Local(
                view.text(id)
                    .expect("text tokens must refer to lexer-owned values")
                    .value()
                    .to_owned(),
            )
        }),
        symbol("'")
            .ignore_then(joint(glam_name()))
            .map(|name| ImportReference::Builtin(name.to_owned())),
    ));
    let binary = padded(keyword("binary"))
        .to(true)
        .or_not()
        .map(|binary| binary.unwrap_or(false));
    let placement = choice((
        padded(keyword("as"))
            .ignore_then(path())
            .map(ImportPlacement::As),
        padded(keyword("at"))
            .ignore_then(path())
            .map(ImportPlacement::At),
    ))
    .or_not()
    .map(|placement| placement.unwrap_or(ImportPlacement::Inline));

    keyword("import")
        .then_ignore(layout_padding())
        .ignore_then(reference)
        .then(binary)
        .then(placement)
        .map(|((reference, binary), placement)| ImportDecl {
            reference,
            binary,
            placement,
        })
}

fn keyword_name_list<'lex, 'source: 'lex>(
    expected: &'static str,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, Vec<String>, TokenExtra<'lex, 'source>> {
    keyword(expected).then_ignore(layout_padding()).ignore_then(
        path()
            .separated_by(padded(symbol(",")))
            .at_least(1)
            .collect::<Vec<_>>(),
    )
}

fn path<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, String, TokenExtra<'lex, 'source>> {
    source_name()
        .then(
            joint(symbol("."))
                .ignore_then(joint(glam_name().map(str::to_owned)))
                .repeated()
                .collect::<Vec<_>>(),
        )
        .map(|(first, rest)| {
            rest.into_iter().fold(first, |mut path, part| {
                path.push('.');
                path.push_str(&part);
                path
            })
        })
}

fn source_name<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, String, TokenExtra<'lex, 'source>> {
    name().try_map(|name, span| {
        if !name.starts_with(char::is_alphabetic) {
            return Err(Rich::custom(
                span,
                "expected a name beginning with a letter",
            ));
        }
        if let Some(keyword) = g0_keyword(name) {
            return Err(Rich::custom(span, reserved_keyword_message(keyword)));
        }
        Ok(name.to_owned())
    })
}

fn glam_name<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, &'source str, TokenExtra<'lex, 'source>> {
    name().try_map(|name, span| {
        name.starts_with(char::is_alphabetic)
            .then_some(name)
            .ok_or_else(|| Rich::custom(span, "expected a name beginning with a letter"))
    })
}

fn layout_padding<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, (), TokenExtra<'lex, 'source>> {
    line_start().repeated().ignored()
}

fn padded<'lex, 'source: 'lex, O, P>(
    parser: P,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>
where
    P: Parser<'lex, TokenInput<'lex, 'source>, O, TokenExtra<'lex, 'source>>,
{
    layout_padding()
        .ignore_then(parser)
        .then_ignore(layout_padding())
}

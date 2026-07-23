//! Token-native ordinary expression grammar.
//!
//! Phase 4 keeps this beside the complete character/compound parser. It is
//! exercised through differential tests until the contextual forms migrate.

use chumsky::error::Rich;
use chumsky::prelude::*;

use crate::number::Number;

use super::super::super::{
    Diagnostic, PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator, flatten_path_suffixes,
};
use super::super::input::{
    ParseSession, TokenExtra, TokenInput, TokenView, close, joint, keyword, line_start, name,
    number, open, space_before, symbol, text_id,
};
use super::super::lexical::Delimiter;
use super::infix::resolve_infix_chain;
use super::{access_if_path, quoted_path};

pub(in crate::g_syntax::parser) fn syntax_expr_parser<'lex, 'source: 'lex>(
    view: TokenView<'lex, 'source>,
) -> impl Parser<'lex, TokenInput<'lex, 'source>, SyntaxExpr, TokenExtra<'lex, 'source>> {
    recursive(move |expr| {
        let glam_name = glam_name().boxed();
        let expr_name = expr_name().boxed();
        let local_name = local_name().boxed();

        let single_key_expr = || {
            choice((
                symbol("'")
                    .ignore_then(joint(glam_name.clone()))
                    .map(SyntaxKeyExpr::Atom),
                expr.clone()
                    .map(|expr| SyntaxKeyExpr::Index(Box::new(expr))),
            ))
        };

        let path_list_shorthand = open(Delimiter::Bracket)
            .ignore_then(
                padded(single_key_expr())
                    .separated_by(padded(symbol(",")))
                    .allow_leading()
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(close(Delimiter::Bracket))
            .map(PathSuffix::Expand)
            .boxed();
        let path_list_expr = open(Delimiter::Parenthesis)
            .ignore_then(padded(expr.clone()))
            .then_ignore(close(Delimiter::Parenthesis))
            .map(|expr| PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(expr))))
            .boxed();

        let path_suffix_item = joint(symbol("."))
            .ignore_then(choice((
                joint(path_list_shorthand.clone()),
                joint(path_list_expr.clone()),
                joint(expr_name.clone())
                    .map(SyntaxKeyExpr::Atom)
                    .map(PathSuffix::Single),
            )))
            .boxed();
        let path_suffix = path_suffix_item.clone().repeated().collect::<Vec<_>>();

        let rooted_name = name()
            .try_map(|name, span| {
                if let Some(prior) = name.strip_prefix('_') {
                    if prior.starts_with(|character: char| character.is_ascii_alphabetic()) {
                        return Ok(SyntaxExpr::PriorName(prior.to_owned()));
                    }
                    return Err(Rich::custom(span, "expected name after `_`"));
                }
                validate_expr_name(name)
                    .map(SyntaxExpr::Name)
                    .map_err(|message| Rich::custom(span, message))
            })
            .then(path_suffix.clone())
            .map(|(name, suffixes)| access_if_path(name, suffixes))
            .boxed();

        let escaped_target = choice((
            joint(open(Delimiter::Parenthesis))
                .ignore_then(padded(expr.clone()))
                .then_ignore(close(Delimiter::Parenthesis)),
            joint(expr_name.clone())
                .map(SyntaxExpr::Name)
                .then(path_suffix.clone())
                .map(|(name, suffixes)| access_if_path(name, suffixes)),
        ));
        let escaped_expr = symbol("^")
            .ignore_then(
                joint(symbol("^"))
                    .repeated()
                    .collect::<Vec<_>>()
                    .map(|carets| carets.len()),
            )
            .then(escaped_target)
            .then(path_suffix.clone())
            .map(|((more_carets, escaped), suffixes)| {
                access_if_path(
                    SyntaxExpr::Escape(more_carets + 1, Box::new(escaped)),
                    suffixes,
                )
            })
            .boxed();

        let effect_expr = symbol(".")
            .ignore_then(joint(glam_name.clone()))
            .then(
                joint(symbol("."))
                    .ignore_then(joint(glam_name.clone()))
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(|(first, rest)| {
                let mut path = Vec::with_capacity(rest.len() + 1);
                path.push(first);
                path.extend(rest);
                SyntaxExpr::Effect(path)
            })
            .boxed();

        let number = number().try_map(|text, span| {
            Number::parse(text)
                .map(SyntaxExpr::Number)
                .map_err(|error| {
                    Rich::custom(span, format!("invalid number literal `{text}`: {error}"))
                })
        });
        let text = text_id().map(move |id| {
            SyntaxExpr::Text(
                view.text(id)
                    .expect("text tokens must refer to lexer-owned values")
                    .value()
                    .to_owned(),
            )
        });

        let quoted_literal = symbol("'").ignore_then(choice((
            path_suffix_item
                .clone()
                .repeated()
                .at_least(1)
                .collect::<Vec<_>>()
                .map(quoted_path),
            joint(glam_name.clone()).map(SyntaxExpr::Atom),
        )));
        let unit = open(Delimiter::Parenthesis)
            .then_ignore(joint(close(Delimiter::Parenthesis)))
            .map(|_| SyntaxExpr::Unit);

        let list = open(Delimiter::Bracket)
            .ignore_then(
                padded(expr.clone())
                    .separated_by(padded(symbol(",")))
                    .allow_leading()
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(close(Delimiter::Bracket))
            .map(SyntaxExpr::List);

        let named_path = glam_name
            .clone()
            .map(SyntaxKeyExpr::Atom)
            .then(path_suffix.clone())
            .map(|(head, suffixes)| {
                let mut path = vec![head];
                path.extend(flatten_path_suffixes(suffixes));
                path
            });
        let computed_path = choice((path_list_shorthand.clone(), path_list_expr.clone()))
            .map(|suffix| flatten_path_suffixes(vec![suffix]))
            .try_map(|path, span| {
                if path.is_empty() {
                    Err(Rich::custom(span, "dictionary paths cannot be empty"))
                } else {
                    Ok(path)
                }
            });
        let data_path = choice((named_path, computed_path)).boxed();
        let dict_item = choice((
            data_path
                .clone()
                .then_ignore(padded(symbol(":")))
                .then(expr.clone())
                .map(|(path, value)| SyntaxExpr::PathDict(path, Box::new(value))),
            expr.clone(),
        ));
        let dict = open(Delimiter::Brace)
            .ignore_then(
                padded(dict_item)
                    .separated_by(padded(symbol(",")))
                    .allow_leading()
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(close(Delimiter::Brace))
            .map(SyntaxExpr::DictUnion);

        let infix_operator = infix_operator().boxed();
        let prefix_operator_section = open(Delimiter::Parenthesis)
            .ignore_then(padded(infix_operator.clone()))
            .then(expr.clone())
            .then_ignore(close(Delimiter::Parenthesis))
            .map(|(operator, right)| SyntaxExpr::OperatorSection {
                operator,
                left: None,
                right: Some(Box::new(right)),
            });
        let postfix_operator_section = open(Delimiter::Parenthesis)
            .ignore_then(padded(expr.clone()))
            .then(infix_operator.clone())
            .then_ignore(layout_padding())
            .then_ignore(close(Delimiter::Parenthesis))
            .map(|(left, operator)| SyntaxExpr::OperatorSection {
                operator,
                left: Some(Box::new(left)),
                right: None,
            });
        let bare_operator_section = open(Delimiter::Parenthesis)
            .ignore_then(padded(infix_operator.clone()))
            .then_ignore(close(Delimiter::Parenthesis))
            .map(|operator| SyntaxExpr::OperatorSection {
                operator,
                left: None,
                right: None,
            });

        let tuple_separator = || padded(symbol(","));
        let tuple_items_after_comma = || {
            padded(expr.clone())
                .separated_by(tuple_separator())
                .allow_trailing()
                .collect::<Vec<_>>()
        };
        let leading_tuple = open(Delimiter::Parenthesis)
            .ignore_then(tuple_separator())
            .ignore_then(tuple_items_after_comma())
            .then_ignore(close(Delimiter::Parenthesis))
            .map(SyntaxExpr::Tuple);
        let grouped_or_trailing_tuple = open(Delimiter::Parenthesis)
            .ignore_then(padded(expr.clone()))
            .then(
                tuple_separator()
                    .ignore_then(tuple_items_after_comma())
                    .or_not(),
            )
            .then_ignore(close(Delimiter::Parenthesis))
            .map(|(first, tail)| match tail {
                Some(tail) => {
                    let mut items = Vec::with_capacity(tail.len() + 1);
                    items.push(first);
                    items.extend(tail);
                    SyntaxExpr::Tuple(items)
                }
                None => first,
            });
        let parenthesized = choice((leading_tuple, grouped_or_trailing_tuple));

        let lambda = symbol("\\")
            .then_ignore(layout_padding())
            .ignore_then(
                padded(local_name.clone())
                    .repeated()
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .then_ignore(padded(symbol("->")))
            .then(expr.clone())
            .map(|(params, body)| SyntaxExpr::Lambda(params, Box::new(body)));

        let literal_atom = choice((
            unit,
            text,
            quoted_literal,
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
        let base_atom = choice((literal_expr, escaped_expr, effect_expr, rooted_name)).boxed();

        let atom = recursive(|atom| {
            let constructor = symbol(":")
                .ignore_then(joint(data_path.clone()))
                .map(SyntaxExpr::TaggedConstructor);
            let tagged = data_path
                .clone()
                .then_ignore(joint(symbol(":")))
                .then(joint(atom))
                .map(|(path, payload)| SyntaxExpr::PathDict(path, Box::new(payload)));

            choice((constructor, tagged, base_atom.clone())).boxed()
        })
        .boxed();
        let application_argument = choice((
            space_before(atom.clone()),
            line_start()
                .repeated()
                .at_least(1)
                .ignored()
                .ignore_then(atom.clone()),
        ));
        let application = atom
            .clone()
            .then(application_argument.repeated().collect::<Vec<_>>())
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
                    padded(infix_operator)
                        .then(application)
                        .repeated()
                        .collect::<Vec<_>>(),
                )
                .try_map(|(first, rest), span| {
                    resolve_infix_chain(first, rest).map_err(|message| Rich::custom(span, message))
                }),
        ))
        .boxed()
    })
}

fn glam_name<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, String, TokenExtra<'lex, 'source>> {
    name().try_map(|name, span| {
        name.starts_with(|character: char| character.is_ascii_alphabetic())
            .then(|| name.to_owned())
            .ok_or_else(|| Rich::custom(span, "expected name"))
    })
}

fn expr_name<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, String, TokenExtra<'lex, 'source>> {
    name().try_map(|name, span| {
        validate_expr_name(name).map_err(|message| Rich::custom(span, message))
    })
}

fn validate_expr_name(name: &str) -> Result<String, String> {
    if !name.starts_with(|character: char| character.is_ascii_alphabetic()) {
        return Err("expected name".to_owned());
    }
    match name {
        "abstract" | "and" | "do" | "or" => Err(format!("`{name}` is a keyword")),
        _ => Ok(name.to_owned()),
    }
}

fn local_name<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, String, TokenExtra<'lex, 'source>> {
    name().try_map(|name, span| {
        let is_local = name == "_"
            || name.starts_with(|character: char| character.is_ascii_alphabetic())
            || name.strip_prefix('_').is_some_and(|rest| {
                rest.starts_with(|character: char| character.is_ascii_alphabetic())
            });
        is_local
            .then(|| name.to_owned())
            .ok_or_else(|| Rich::custom(span, "expected local name"))
    })
}

fn infix_operator<'lex, 'source: 'lex>()
-> impl Parser<'lex, TokenInput<'lex, 'source>, SyntaxOperator, TokenExtra<'lex, 'source>> {
    choice((
        keyword("and").to(SyntaxOperator::BoolAnd),
        keyword("or").to(SyntaxOperator::BoolOr),
        symbol(">>=").to(SyntaxOperator::EffectBind),
        symbol(">=>").to(SyntaxOperator::KleisliCompose),
        symbol("=>>").to(SyntaxOperator::EffectThen),
        symbol("!>").to(SyntaxOperator::ApplicativeForward),
        symbol("<!").to(SyntaxOperator::ApplicativeBackward),
        symbol(">=").to(SyntaxOperator::Builtin(crate::core::Builtin::GreaterEqual)),
        symbol("==").to(SyntaxOperator::Builtin(crate::core::Builtin::Equal)),
        symbol("<>").to(SyntaxOperator::Builtin(crate::core::Builtin::NotEqual)),
        symbol("=<").to(SyntaxOperator::Builtin(crate::core::Builtin::LessEqual)),
        symbol(">>").to(SyntaxOperator::ComposeForward),
        symbol("<<").to(SyntaxOperator::ComposeBackward),
        symbol("|>").to(SyntaxOperator::PipeForward),
        symbol("<|").to(SyntaxOperator::PipeBackward),
        symbol(">").to(SyntaxOperator::Builtin(crate::core::Builtin::Greater)),
        symbol("<").to(SyntaxOperator::Builtin(crate::core::Builtin::Less)),
        symbol("++").to(SyntaxOperator::Builtin(crate::core::Builtin::Append)),
        symbol("*").to(SyntaxOperator::Builtin(crate::core::Builtin::Multiply)),
        symbol("/").to(SyntaxOperator::Builtin(crate::core::Builtin::Divide)),
        symbol("+").to(SyntaxOperator::Builtin(crate::core::Builtin::Add)),
        symbol("-").to(SyntaxOperator::Builtin(crate::core::Builtin::Subtract)),
    ))
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

pub(super) fn parse_expression_fragment(source: &[u8]) -> Result<SyntaxExpr, Vec<Diagnostic>> {
    super::super::input::parse_expression_fragment(source, parse_expression_view)
}

pub(in crate::g_syntax::parser) fn parse_expression_view(
    view: TokenView<'_, '_>,
) -> Result<SyntaxExpr, Vec<Diagnostic>> {
    let mut session = ParseSession::new(view.source());
    let (output, errors) = syntax_expr_parser(view)
        .then_ignore(layout_padding())
        .then_ignore(end())
        .parse(view.chumsky_input())
        .into_output_errors();
    session.record_token_errors(view, errors);
    let diagnostics = session.into_diagnostics();
    if diagnostics.is_empty() {
        output.ok_or_else(Vec::new)
    } else {
        Err(diagnostics)
    }
}

#[cfg(test)]
mod tests;

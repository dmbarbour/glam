//! G-expression grammar and the small layout-aware scanners used by compound
//! expression forms.

use chumsky::prelude::*;

use crate::number::Number;

use super::super::{PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator, flatten_path_suffixes};
#[cfg(test)]
use super::compound::parse_expr_result;
use super::declaration::quoted_text;
use super::layout::{legacy_glam_name, legacy_local_name, legacy_whitespace1};

mod infix;
#[cfg(test)]
pub(super) mod token;

use infix::resolve_infix_chain;

#[cfg(test)]
pub(in crate::g_syntax) fn parse_expr(text: &str) -> Option<SyntaxExpr> {
    parse_expr_result(text).ok()
}

fn access_if_path(base: SyntaxExpr, suffixes: Vec<PathSuffix>) -> SyntaxExpr {
    match flatten_path_suffixes(suffixes) {
        parts if parts.is_empty() => base,
        parts => SyntaxExpr::Access(Box::new(base), parts),
    }
}

fn quoted_path(suffixes: Vec<PathSuffix>) -> SyntaxExpr {
    let mut chunks = Vec::new();
    let mut literal = Vec::new();
    let flush_literal = |chunks: &mut Vec<SyntaxExpr>, literal: &mut Vec<SyntaxExpr>| {
        if !literal.is_empty() {
            chunks.push(SyntaxExpr::List(std::mem::take(literal)));
        }
    };

    for part in flatten_path_suffixes(suffixes) {
        match part {
            SyntaxKeyExpr::Atom(name) => literal.push(SyntaxExpr::Atom(name)),
            SyntaxKeyExpr::Index(expr) => literal.push(*expr),
            SyntaxKeyExpr::PathIndex(expr) => {
                flush_literal(&mut chunks, &mut literal);
                chunks.push(*expr);
            }
        }
    }
    flush_literal(&mut chunks, &mut literal);

    chunks
        .into_iter()
        .reduce(|left, right| SyntaxExpr::Append(Box::new(left), Box::new(right)))
        .unwrap_or_else(|| SyntaxExpr::List(Vec::new()))
}

pub(in crate::g_syntax) fn syntax_expr_parser<'src>()
-> impl Parser<'src, &'src str, SyntaxExpr, extra::Err<Rich<'src, char>>> {
    recursive(|expr| {
        let name = legacy_glam_name().boxed();
        let expr_name = legacy_glam_name()
            .try_map(|name, span| match name.as_str() {
                "abstract" | "and" | "do" | "or" => {
                    Err(Rich::custom(span, format!("`{name}` is a keyword")))
                }
                _ => Ok(name),
            })
            .boxed();
        let local = legacy_local_name().boxed();

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
            .map(PathSuffix::Expand)
            .boxed();
        let path_list_expr = expr
            .clone()
            .padded()
            .delimited_by(just('('), just(')'))
            .map(|expr| PathSuffix::Single(SyntaxKeyExpr::PathIndex(Box::new(expr))))
            .boxed();

        // Dotted paths stay lexically tight because `.` has other roles in the
        // language surface, such as future effect sugar like `.bar`.
        let path_suffix_item = just('.')
            .ignore_then(choice((
                path_list_shorthand.clone(),
                path_list_expr.clone(),
                expr_name
                    .clone()
                    .map(SyntaxKeyExpr::Atom)
                    .map(PathSuffix::Single),
            )))
            .boxed();
        let path_suffix = path_suffix_item.clone().repeated().collect::<Vec<_>>();

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
            .ignore_then(
                name.clone()
                    .separated_by(just('.'))
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
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
        let multiline_indent = just(' ').repeated().ignored();
        let multiline_content = multiline_indent.ignore_then(just('"')).ignore_then(choice((
            just('\n').to(String::new()),
            just(' ')
                .ignore_then(none_of('\n').repeated().to_slice())
                .then_ignore(just('\n'))
                .map(ToOwned::to_owned),
        )));
        let multiline_text = just("\"\"\"")
            .then_ignore(just('\n'))
            .ignore_then(multiline_content.repeated().collect::<Vec<_>>())
            .then_ignore(multiline_indent.ignore_then(just("\"\"\"")))
            .map(|lines| SyntaxExpr::Text(lines.join("\n")));
        let text = choice((multiline_text, quoted_text().map(SyntaxExpr::Text)));
        let quoted_literal = just('\'').ignore_then(choice((
            path_suffix_item
                .repeated()
                .at_least(1)
                .collect::<Vec<_>>()
                .map(quoted_path),
            name.clone().map(SyntaxExpr::Atom),
        )));
        let unit = just("()").map(|_| SyntaxExpr::Unit);

        let list = expr
            .clone()
            .padded()
            .separated_by(just(',').padded())
            .allow_leading()
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just('['), just(']'))
            .map(SyntaxExpr::List);

        let named_path = name
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
                .then_ignore(just(':').padded())
                .then(expr.clone())
                .map(|(path, value)| SyntaxExpr::PathDict(path, Box::new(value))),
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
            just("!>").to(SyntaxOperator::ApplicativeForward),
            just("<!").to(SyntaxOperator::ApplicativeBackward),
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
        let tuple_separator = just(',').padded();
        let tuple_items_after_comma = || {
            expr.clone()
                .padded()
                .separated_by(tuple_separator)
                .allow_trailing()
                .collect::<Vec<_>>()
        };
        let leading_tuple = tuple_separator
            .ignore_then(tuple_items_after_comma())
            .map(SyntaxExpr::Tuple);
        let grouped_or_trailing_tuple = expr
            .clone()
            .padded()
            .then(
                tuple_separator
                    .ignore_then(tuple_items_after_comma())
                    .or_not(),
            )
            .map(|(first, tail)| match tail {
                Some(tail) => {
                    let mut items = Vec::with_capacity(tail.len() + 1);
                    items.push(first);
                    items.extend(tail);
                    SyntaxExpr::Tuple(items)
                }
                None => first,
            });
        let parenthesized =
            choice((leading_tuple, grouped_or_trailing_tuple)).delimited_by(just('('), just(')'));
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
        let base_atom = choice((
            literal_expr,
            escaped_expr,
            effect_expr,
            prior_name,
            name_expr,
        ))
        .boxed();
        let atom = recursive(|atom| {
            let constructor = just(':')
                .ignore_then(data_path.clone())
                .map(SyntaxExpr::TaggedConstructor);
            let tagged = data_path
                .clone()
                .then_ignore(just(':'))
                .then(atom)
                .map(|(path, payload)| SyntaxExpr::PathDict(path, Box::new(payload)));

            choice((constructor, tagged, base_atom.clone()))
        })
        .boxed();
        let application = atom
            .clone()
            .then(
                legacy_whitespace1()
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
    })
}

#[cfg(test)]
mod tests;

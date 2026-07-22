//! G-expression grammar and the small layout-aware scanners used by compound
//! expression forms.

use chumsky::prelude::*;

use crate::number::Number;

use super::super::{
    PathSuffix, SyntaxExpr, SyntaxKeyExpr, SyntaxOperator, flatten_path_suffixes,
    is_comparison_operator,
};
use super::compound::*;
use super::declaration::quoted_text;
use super::layout::{glam_name, local_name, whitespace1};

#[cfg(test)]
pub(in crate::g_syntax) fn parse_expr(text: &str) -> Option<SyntaxExpr> {
    parse_expr_result(text).ok()
}

pub(in crate::g_syntax) fn syntax_expr_parser<'src>()
-> impl Parser<'src, &'src str, SyntaxExpr, extra::Err<Rich<'src, char>>> {
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
            ApplicativeBackward, ApplicativeForward, BoolAnd, BoolOr, Builtin, ComposeBackward,
            ComposeForward, EffectBind, EffectThen, KleisliCompose, PipeBackward, PipeForward,
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
            (ApplicativeForward, ApplicativeForward) => {
                OperatorRelation::Same(Associativity::Right)
            }
            (ApplicativeBackward, ApplicativeBackward) => {
                OperatorRelation::Same(Associativity::Left)
            }
            (ApplicativeForward, ApplicativeBackward)
            | (ApplicativeBackward, ApplicativeForward) => OperatorRelation::Unrelated,
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
            ApplicativeBackward, ApplicativeForward, BoolAnd, BoolOr, Builtin, ComposeBackward,
            ComposeForward, EffectBind, EffectThen, KleisliCompose, PipeBackward, PipeForward,
        };

        match operator {
            BoolOr => 0,
            BoolAnd => 1,
            EffectBind | EffectThen => 2,
            PipeForward | PipeBackward | ApplicativeForward | ApplicativeBackward => 3,
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
            SyntaxOperator::ApplicativeForward => "!>",
            SyntaxOperator::ApplicativeBackward => "<!",
            SyntaxOperator::ComposeForward => ">>",
            SyntaxOperator::ComposeBackward => "<<",
            SyntaxOperator::EffectBind => ">>=",
            SyntaxOperator::KleisliCompose => ">=>",
            SyntaxOperator::EffectThen => "=>>",
            SyntaxOperator::Builtin(crate::core::Builtin::Fixpoint) => "fixpoint",
            SyntaxOperator::Builtin(crate::core::Builtin::Anno) => "anno",
            SyntaxOperator::Builtin(crate::core::Builtin::Seq) => "seq",
            SyntaxOperator::Builtin(crate::core::Builtin::Spark) => "spark",
            SyntaxOperator::Builtin(crate::core::Builtin::InteractionNet) => "interaction_net",
            SyntaxOperator::Builtin(crate::core::Builtin::NetArity) => "net_arity",
            SyntaxOperator::Builtin(crate::core::Builtin::MergeDuplicate) => "merge_duplicate",
            SyntaxOperator::Builtin(crate::core::Builtin::Floor) => "floor",
            SyntaxOperator::Builtin(crate::core::Builtin::Mod) => "mod",
            SyntaxOperator::Builtin(crate::core::Builtin::Slice) => "slice",
            SyntaxOperator::Builtin(crate::core::Builtin::Map) => "map",
            SyntaxOperator::Builtin(crate::core::Builtin::ListConcat) => "list.concat",
            SyntaxOperator::Builtin(crate::core::Builtin::ListLen) => "list.len",
            SyntaxOperator::Builtin(crate::core::Builtin::ListSplit) => "list.split",
            SyntaxOperator::Builtin(crate::core::Builtin::ListSplitEnd) => "list.split_end",
            SyntaxOperator::Builtin(crate::core::Builtin::ListHead) => "list.head",
            SyntaxOperator::Builtin(crate::core::Builtin::ListTail) => "list.tail",
            SyntaxOperator::Builtin(crate::core::Builtin::TextLines) => "text.lines",
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
            SyntaxOperator::Builtin(crate::core::Builtin::EffectApply) => "effect_apply",
            SyntaxOperator::Builtin(crate::core::Builtin::EffectCall) => "effect_call",
            SyntaxOperator::Builtin(crate::core::Builtin::EffectMap) => "eff.map",
            SyntaxOperator::Builtin(crate::core::Builtin::EffectMapRun) => "effect_map_run",
            SyntaxOperator::Builtin(crate::core::Builtin::EffectMapContinue) => {
                "effect_map_continue"
            }
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectDefaultDefs) => {
                "object_default_defs"
            }
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectDictDefs) => "object_dict_defs",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectWithDefs) => "object_with_defs",
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectComposedDefs) => {
                "object_composed_defs"
            }
            SyntaxOperator::Builtin(crate::core::Builtin::ObjectOverrideDefs) => {
                "object_override_defs"
            }
        }
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

    recursive(|expr| {
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
    })
}

#[cfg(test)]
mod tests;

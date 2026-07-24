use super::super::super::{SyntaxExpr, SyntaxOperator, is_comparison_operator};
use super::syntax_binary_expr;

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
            Self::Expr(expr) => expr,
            Self::ComparisonChain { first, mut rest } if rest.len() == 1 => {
                let (operator, right) = rest
                    .pop()
                    .expect("single-item comparison chain should contain one comparison");
                syntax_binary_expr(operator, *first, right)
            }
            Self::ComparisonChain { first, rest } => SyntaxExpr::ComparisonChain { first, rest },
        }
    }
}

pub(in crate::g_syntax::parser) fn resolve_infix_chain(
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
                OperatorRelation::Weaker | OperatorRelation::Same(Associativity::Right) => break,
                OperatorRelation::Same(Associativity::None)
                    if is_comparison_operator(previous_op) && is_comparison_operator(next_op) =>
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
        (BoolOr, BoolAnd) | (BoolAnd, BoolOr) => OperatorRelation::Unrelated,
        (EffectBind, EffectBind)
        | (EffectBind, EffectThen)
        | (EffectThen, EffectBind)
        | (EffectThen, EffectThen) => OperatorRelation::Same(Associativity::Left),
        (KleisliCompose, KleisliCompose) => OperatorRelation::Same(Associativity::Right),
        (PipeForward, PipeForward) => OperatorRelation::Same(Associativity::Left),
        (PipeBackward, PipeBackward) => OperatorRelation::Same(Associativity::Right),
        (PipeForward, PipeBackward) | (PipeBackward, PipeForward) => OperatorRelation::Unrelated,
        (ApplicativeForward, ApplicativeForward) => OperatorRelation::Same(Associativity::Right),
        (ApplicativeBackward, ApplicativeBackward) => OperatorRelation::Same(Associativity::Left),
        (ApplicativeForward, ApplicativeBackward) | (ApplicativeBackward, ApplicativeForward) => {
            OperatorRelation::Unrelated
        }
        (ComposeForward, ComposeForward) => OperatorRelation::Same(Associativity::Left),
        (ComposeBackward, ComposeBackward) => OperatorRelation::Same(Associativity::Right),
        (ComposeForward, ComposeBackward) | (ComposeBackward, ComposeForward) => {
            OperatorRelation::Unrelated
        }
        (Builtin(Append), Builtin(Append)) => OperatorRelation::Same(Associativity::Left),
        (Builtin(Add), Builtin(Add)) => OperatorRelation::Same(Associativity::Left),
        (Builtin(Subtract), Builtin(Subtract)) => OperatorRelation::Same(Associativity::None),
        (Builtin(Multiply), Builtin(Multiply)) => OperatorRelation::Same(Associativity::Left),
        (Builtin(Divide), Builtin(Divide)) => OperatorRelation::Same(Associativity::None),
        (
            Builtin(Add | Subtract | Multiply | Divide),
            Builtin(Add | Subtract | Multiply | Divide),
        ) => OperatorRelation::Unrelated,
        (
            Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less),
            Builtin(Greater | GreaterEqual | Equal | NotEqual | LessEqual | Less),
        ) => OperatorRelation::Same(Associativity::None),
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
        SyntaxOperator::Builtin(crate::core::Builtin::ListAt) => "list.at",
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
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectFromDict) => "object_from_dict",
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectLocalName) => "object_local_name",
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectAbstractFromParts) => {
            "object_abstract_from_parts"
        }
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectInstanceFromParts) => {
            "object_instance_from_parts"
        }
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectInstance) => "object_instance",
        SyntaxOperator::Builtin(crate::core::Builtin::EffectApply) => "effect_apply",
        SyntaxOperator::Builtin(crate::core::Builtin::EffectCall) => "effect_call",
        SyntaxOperator::Builtin(crate::core::Builtin::EffectMap) => "eff.map",
        SyntaxOperator::Builtin(crate::core::Builtin::EffectMapRun) => "effect_map_run",
        SyntaxOperator::Builtin(crate::core::Builtin::EffectMapContinue) => "effect_map_continue",
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectDefaultDefs) => "object_default_defs",
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectDictDefs) => "object_dict_defs",
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectWithDefs) => "object_with_defs",
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectComposedDefs) => "object_composed_defs",
        SyntaxOperator::Builtin(crate::core::Builtin::ObjectOverrideDefs) => "object_override_defs",
    }
}

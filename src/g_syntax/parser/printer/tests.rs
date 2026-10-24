//! The round trip: generated trees print and parse back to themselves.

use super::print_expression;
use crate::core::Builtin;
use crate::g_syntax::parser::structural::parse_compound_expression_fragment;
use crate::g_syntax::{
    Alias, ConditionalMode, ConditionalResultMode, DefinitionDecl, DefinitionKind, DoExpr, DoStep,
    DoStepKind, IfExpr, MatchArm, MatchCommitment, MatchExpr, MatchOutcome, ObjectBodyDefinition,
    ObjectBodyDefinitionKind, SyntaxExpr, SyntaxGuardClause, SyntaxKeyExpr, SyntaxOperator,
    SyntaxPattern, SyntaxPatternKind,
};
use crate::number::Number;

/// A seeded xorshift generator, so generated trees are reproducible.
struct Generator(u64);

const NAMES: &[&str] = &["a", "b", "c", "f", "g", "x", "y", "item", "next"];
const KEYS: &[&str] = &["k", "len", "value", "tag"];

impl Generator {
    fn next(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.0 = state;
        state
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn name(&mut self) -> String {
        NAMES[self.below(NAMES.len())].to_owned()
    }

    fn key(&mut self) -> SyntaxKeyExpr {
        SyntaxKeyExpr::Atom(KEYS[self.below(KEYS.len())].to_owned())
    }

    fn keys(&mut self) -> Vec<SyntaxKeyExpr> {
        (0..1 + self.below(2)).map(|_| self.key()).collect()
    }

    fn leaf(&mut self) -> SyntaxExpr {
        match self.below(9) {
            0 => SyntaxExpr::Number(Number::from(self.below(100) as i64)),
            1 => SyntaxExpr::Text(self.name()),
            2 => SyntaxExpr::Atom(self.name()),
            3 => SyntaxExpr::Unit,
            4 => SyntaxExpr::Effect((0..1 + self.below(2)).map(|_| self.name()).collect()),
            5 => SyntaxExpr::PriorName(self.name()),
            6 => SyntaxExpr::Escape(1, Box::new(SyntaxExpr::Name(self.name()))),
            7 => SyntaxExpr::TaggedConstructor(self.keys()),
            _ => SyntaxExpr::Name(self.name()),
        }
    }

    fn operator(&mut self) -> SyntaxOperator {
        const OPERATORS: &[SyntaxOperator] = &[
            SyntaxOperator::Builtin(Builtin::Add),
            SyntaxOperator::Builtin(Builtin::Subtract),
            SyntaxOperator::Builtin(Builtin::Multiply),
            SyntaxOperator::Builtin(Builtin::Divide),
            SyntaxOperator::Builtin(Builtin::Append),
            SyntaxOperator::Builtin(Builtin::Less),
            SyntaxOperator::Builtin(Builtin::Equal),
            SyntaxOperator::BoolAnd,
            SyntaxOperator::BoolOr,
            SyntaxOperator::PipeForward,
            SyntaxOperator::PipeBackward,
            SyntaxOperator::ComposeForward,
            SyntaxOperator::EffectBind,
            SyntaxOperator::KleisliCompose,
        ];
        OPERATORS[self.below(OPERATORS.len())]
    }

    fn binary(&mut self, depth: usize) -> SyntaxExpr {
        let operator = self.operator();
        let left = Box::new(self.expression(depth));
        let right = Box::new(self.expression(depth));
        match operator {
            SyntaxOperator::Builtin(Builtin::Add) => SyntaxExpr::Add(left, right),
            SyntaxOperator::Builtin(Builtin::Subtract) => SyntaxExpr::Subtract(left, right),
            SyntaxOperator::Builtin(Builtin::Multiply) => SyntaxExpr::Multiply(left, right),
            SyntaxOperator::Builtin(Builtin::Divide) => SyntaxExpr::Divide(left, right),
            SyntaxOperator::Builtin(Builtin::Append) => SyntaxExpr::Append(left, right),
            // Two comparisons or more make a chain.
            SyntaxOperator::Builtin(Builtin::Less | Builtin::Equal) if self.chance(3) => {
                SyntaxExpr::ComparisonChain {
                    first: left,
                    rest: vec![
                        (operator, *right),
                        (operator, SyntaxExpr::Name(self.name())),
                    ],
                }
            }
            _ => SyntaxExpr::OperatorApply {
                operator,
                left,
                right,
            },
        }
    }

    fn pattern(&mut self) -> SyntaxPattern {
        SyntaxPattern {
            kind: if self.chance(3) {
                SyntaxPatternKind::Wildcard
            } else {
                SyntaxPatternKind::Capture(self.name())
            },
        }
    }

    /// A simple guard: guards hold patterns, which these trees keep plain.
    fn guard(&mut self) -> SyntaxGuardClause {
        SyntaxGuardClause::Effect(match self.below(3) {
            0 => SyntaxExpr::OperatorApply {
                operator: SyntaxOperator::Builtin(Builtin::Less),
                left: Box::new(SyntaxExpr::Name(self.name())),
                right: Box::new(SyntaxExpr::Number(Number::from(self.below(10) as i64))),
            },
            _ => SyntaxExpr::Name(self.name()),
        })
    }

    fn expression(&mut self, depth: usize) -> SyntaxExpr {
        if depth == 0 {
            return self.leaf();
        }
        let depth = depth - 1;
        match self.below(24) {
            0..=3 => self.leaf(),
            4 | 5 => SyntaxExpr::Apply(
                Box::new(match self.below(3) {
                    0 => self.expression(depth),
                    _ => SyntaxExpr::Name(self.name()),
                }),
                Box::new(self.expression(depth)),
            ),
            6..=8 => self.binary(depth),
            9 => SyntaxExpr::Access(Box::new(SyntaxExpr::Name(self.name())), self.keys()),
            10 => {
                let payload = match self.expression(depth) {
                    // A tag's payload that is a tag nests, as written.
                    SyntaxExpr::PathDict(..) => SyntaxExpr::Name(self.name()),
                    payload => payload,
                };
                SyntaxExpr::PathDict(self.keys(), Box::new(payload))
            }
            11 => SyntaxExpr::List((0..self.below(3)).map(|_| self.expression(depth)).collect()),
            12 => SyntaxExpr::Tuple(
                (0..2 + self.below(2))
                    .map(|_| self.expression(depth))
                    .collect(),
            ),
            13 => SyntaxExpr::dict_literal(
                (0..self.below(3))
                    .map(|_| {
                        if self.chance(3) {
                            SyntaxExpr::Name(self.name())
                        } else {
                            SyntaxExpr::PathDict(vec![self.key()], Box::new(self.expression(depth)))
                        }
                    })
                    .collect(),
            ),
            14 => SyntaxExpr::Lambda(
                (0..1 + self.below(2)).map(|_| self.name()).collect(),
                Box::new(self.expression(depth)),
            ),
            15 => SyntaxExpr::If(IfExpr {
                mode: if self.chance(4) {
                    ConditionalMode::Host
                } else {
                    ConditionalMode::Pure
                },
                guards: (0..1 + self.below(2)).map(|_| self.guard()).collect(),
                then_mode: ConditionalResultMode::Ordinary,
                then_result: Box::new(self.expression(depth)),
                else_result: Box::new(self.expression(depth)),
            }),
            16 => SyntaxExpr::Let {
                bindings: (0..1 + self.below(2))
                    .map(|_| (self.name(), self.expression(depth)))
                    .collect(),
                body: Box::new(self.expression(depth)),
            },
            17 => SyntaxExpr::Using {
                namespace: Box::new(self.expression(depth)),
                body: Box::new(self.expression(depth)),
            },
            18 => SyntaxExpr::Do(DoExpr {
                steps: (0..self.below(3))
                    .map(|_| DoStep {
                        line: 1,
                        kind: if self.chance(2) {
                            DoStepKind::Bind {
                                pattern: self.pattern(),
                                operation: self.expression(depth),
                            }
                        } else {
                            DoStepKind::Then(self.expression(depth))
                        },
                    })
                    .collect(),
                result_line: 1,
                result: Box::new(self.expression(depth)),
            }),
            19 => SyntaxExpr::Match(MatchExpr {
                line: 1,
                mode: ConditionalMode::Pure,
                commitment: MatchCommitment::Cut,
                subject: Box::new(self.expression(depth)),
                arms: (0..1 + self.below(2))
                    .map(|_| MatchArm {
                        line: 1,
                        pattern: self.pattern(),
                        guards: if self.chance(4) {
                            vec![self.guard()]
                        } else {
                            Vec::new()
                        },
                        outcome: MatchOutcome::Result {
                            line: 1,
                            mode: ConditionalResultMode::Ordinary,
                            expression: self.expression(depth),
                        },
                    })
                    .collect(),
            }),
            20 => SyntaxExpr::With {
                base: Box::new(self.expression(depth)),
                alias: match self.below(6) {
                    0 => Some(self.name()),
                    1 => Some("_".to_owned()),
                    2 => Some("self".to_owned()),
                    _ => None,
                }
                .map(|name| Alias { name, line: 1 }),
                body: (0..self.below(3))
                    .map(|_| ObjectBodyDefinition {
                        line: 1,
                        kind: if self.chance(4) {
                            ObjectBodyDefinitionKind::Let(
                                (0..1 + self.below(2))
                                    .map(|_| (self.name(), self.expression(depth)))
                                    .collect(),
                            )
                        } else {
                            ObjectBodyDefinitionKind::Definition(DefinitionDecl {
                                target: vec![self.key()],
                                parameters: Vec::new(),
                                kind: if self.chance(2) {
                                    DefinitionKind::Override
                                } else {
                                    DefinitionKind::Introduce
                                },
                                expr: Some(self.expression(depth)),
                            })
                        },
                    })
                    .collect(),
            },
            21 => {
                let operator = self.operator();
                let operand = || Box::new(SyntaxExpr::Name(NAMES[0].to_owned()));
                match self.below(3) {
                    0 => SyntaxExpr::OperatorSection {
                        operator,
                        left: None,
                        right: None,
                    },
                    1 => SyntaxExpr::OperatorSection {
                        operator,
                        left: None,
                        right: Some(operand()),
                    },
                    _ => SyntaxExpr::OperatorSection {
                        operator,
                        left: Some(operand()),
                        right: None,
                    },
                }
            }
            _ => SyntaxExpr::Apply(
                Box::new(SyntaxExpr::Name(self.name())),
                Box::new(self.expression(depth)),
            ),
        }
    }
}

fn round_trip_count() -> usize {
    2_000
        * std::env::var("GLAM_ROUND_TRIP")
            .ok()
            .and_then(|factor| factor.parse().ok())
            .unwrap_or(1)
}

/// Generated trees print and parse back to themselves: the parser reads
/// what the printer writes with the fewest parentheses.
#[test]
fn generated_trees_round_trip() {
    let mut generator = Generator(0x9e37_79b9_7f4a_7c15);
    let mut failures = Vec::new();
    for _ in 0..round_trip_count() {
        let depth = 1 + generator.below(4);
        let tree = generator.expression(depth);
        let printed = print_expression(&tree);
        match parse_compound_expression_fragment(printed.as_bytes()) {
            Ok(parsed) if parsed == tree => {}
            Ok(parsed) => failures.push(format!(
                "printed: {printed}\n  tree: {tree:?}\n  read: {parsed:?}"
            )),
            Err(diagnostics) => failures.push(format!(
                "printed: {printed}\n  tree: {tree:?}\n  error: {:?}",
                diagnostics
                    .iter()
                    .map(|diagnostic| &diagnostic.message)
                    .collect::<Vec<_>>()
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} trees did not round-trip; the first:\n{}",
        failures.len(),
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

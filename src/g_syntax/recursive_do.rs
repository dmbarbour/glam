use std::collections::HashMap;

use super::*;

pub(in crate::g_syntax) type ForwardNameId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) struct ForwardName {
    pub(in crate::g_syntax) canonical: String,
    pub(in crate::g_syntax) written: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) struct ForwardNamePlan {
    pub(in crate::g_syntax) canonical: String,
    pub(in crate::g_syntax) written: String,
    pub(in crate::g_syntax) declaration_step: usize,
    pub(in crate::g_syntax) fulfillment_step: usize,
    pub(in crate::g_syntax) semantic_start: usize,
    pub(in crate::g_syntax) children: Vec<ForwardNameId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) struct DeclarationPlan {
    pub(in crate::g_syntax) step: usize,
    pub(in crate::g_syntax) names: Vec<ForwardNameId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) struct RecursiveDoPlan {
    pub(in crate::g_syntax) forwards: Vec<ForwardNamePlan>,
    pub(in crate::g_syntax) declarations: Vec<DeclarationPlan>,
    pub(in crate::g_syntax) roots: Vec<ForwardNameId>,
    step_lines: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) enum RecursiveDoEvent {
    None,
    Declare(Vec<ForwardNameId>),
    Fulfill(ForwardNameId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::g_syntax) struct RecursiveDoStep {
    pub(in crate::g_syntax) line: usize,
    pub(in crate::g_syntax) event: RecursiveDoEvent,
}

/// Assigns forward identities while source names are resolved.
///
/// This registry establishes name availability only. It deliberately does not
/// calculate recursive regions; `RecursiveDoPlan` derives those later from the
/// completed primitive step stream.
#[derive(Default)]
pub(in crate::g_syntax) struct ForwardNameRegistry {
    forwards: Vec<ForwardName>,
    active: HashMap<String, ForwardNameId>,
}

impl ForwardNameRegistry {
    pub(in crate::g_syntax) fn declare(
        &mut self,
        names: &[String],
        line: usize,
    ) -> Result<Vec<ForwardNameId>, Diagnostic> {
        if names.is_empty() {
            return Err(Diagnostic::error(
                line,
                "recursive do abstract declaration requires at least one name",
            ));
        }

        let mut declaration_names = Vec::with_capacity(names.len());
        let mut canonical_names = Vec::with_capacity(names.len());
        for written in names {
            let Some(canonical) = local_name_metadata(written).canonical else {
                return Err(Diagnostic::error(
                    line,
                    "recursive do abstract declarations require accessible local names",
                ));
            };
            if canonical_names.contains(&canonical) || self.active.contains_key(&canonical) {
                return Err(Diagnostic::error(
                    line,
                    format!("duplicate recursive do abstract declaration for `{canonical}`"),
                ));
            }
            canonical_names.push(canonical.clone());

            let id = self.forwards.len();
            self.forwards.push(ForwardName {
                canonical: canonical.clone(),
                written: written.clone(),
            });
            self.active.insert(canonical, id);
            declaration_names.push(id);
        }
        Ok(declaration_names)
    }

    pub(in crate::g_syntax) fn fulfill(&mut self, written: &str) -> Option<ForwardNameId> {
        let canonical = local_name_metadata(written).canonical?;
        self.active.remove(&canonical)
    }

    pub(in crate::g_syntax) fn into_forwards(self) -> Vec<ForwardName> {
        self.forwards
    }
}

impl RecursiveDoPlan {
    pub(in crate::g_syntax) fn build<'a>(
        steps: impl IntoIterator<Item = &'a RecursiveDoStep>,
        names: Vec<ForwardName>,
    ) -> Result<Self, Diagnostic> {
        let steps = steps.into_iter().collect::<Vec<_>>();
        let mut forwards = names
            .into_iter()
            .map(|name| ForwardNamePlan {
                canonical: name.canonical,
                written: name.written,
                declaration_step: usize::MAX,
                fulfillment_step: usize::MAX,
                semantic_start: usize::MAX,
                children: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut declarations = Vec::new();

        for (step_index, step) in steps.iter().enumerate() {
            match &step.event {
                RecursiveDoEvent::None => {}
                RecursiveDoEvent::Declare(ids) => {
                    for id in ids.iter().copied() {
                        let forward = &mut forwards[id];
                        debug_assert_eq!(forward.declaration_step, usize::MAX);
                        forward.declaration_step = step_index;
                        forward.semantic_start = step_index;
                    }
                    declarations.push(DeclarationPlan {
                        step: step_index,
                        names: ids.clone(),
                    });
                }
                RecursiveDoEvent::Fulfill(id) => {
                    let forward = &mut forwards[*id];
                    debug_assert_eq!(forward.fulfillment_step, usize::MAX);
                    forward.fulfillment_step = step_index;
                }
            }
        }

        if let Some(declaration_step) = forwards
            .iter()
            .filter(|forward| forward.fulfillment_step == usize::MAX)
            .map(|forward| forward.declaration_step)
            .min()
        {
            let unresolved = forwards
                .iter()
                .filter(|forward| {
                    forward.declaration_step == declaration_step
                        && forward.fulfillment_step == usize::MAX
                })
                .map(|forward| format!("`{}`", forward.canonical))
                .collect::<Vec<_>>();
            return Err(Diagnostic::error(
                steps[declaration_step].line,
                format!(
                    "recursive do abstract declaration has no later fulfillment for {}",
                    unresolved.join(", ")
                ),
            ));
        }

        debug_assert!(
            forwards
                .iter()
                .all(|forward| forward.declaration_step != usize::MAX),
            "every registered forward appears in a primitive declaration step"
        );
        align_crossing_starts(&mut forwards);
        let roots = build_scope_tree(&mut forwards);
        Ok(Self {
            forwards,
            declarations,
            roots,
            step_lines: steps.iter().map(|step| step.line).collect(),
        })
    }

    pub(in crate::g_syntax) fn promotion_warnings(&self) -> Vec<Diagnostic> {
        self.declarations
            .iter()
            .filter_map(|declaration| {
                let promoted = declaration
                    .names
                    .iter()
                    .copied()
                    .filter(|id| self.forwards[*id].semantic_start < declaration.step)
                    .collect::<Vec<_>>();
                let earliest = promoted
                    .iter()
                    .map(|id| self.forwards[*id].semantic_start)
                    .min()?;
                let names = promoted
                    .iter()
                    .map(|id| format!("`{}`", self.forwards[*id].written))
                    .collect::<Vec<_>>()
                    .join(", ");
                Some(Diagnostic::warn(
                    self.step_lines[declaration.step],
                    format!(
                        "crossing recursive regions move {names} into a `.fix` begun on line {}; align the declarations to make the wider fixpoint scope explicit",
                        self.step_lines[earliest]
                    ),
                ))
            })
            .collect()
    }
}

fn align_crossing_starts(forwards: &mut [ForwardNamePlan]) {
    loop {
        let mut changed = false;
        for left in 0..forwards.len() {
            for right in 0..forwards.len() {
                if left == right {
                    continue;
                }
                let left_start = forwards[left].semantic_start;
                let right_start = forwards[right].semantic_start;
                let left_end = forwards[left].fulfillment_step;
                let right_end = forwards[right].fulfillment_step;
                if left_start < right_start && right_start < left_end && left_end < right_end {
                    forwards[right].semantic_start = left_start;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn build_scope_tree(forwards: &mut [ForwardNamePlan]) -> Vec<ForwardNameId> {
    let mut ordered = (0..forwards.len()).collect::<Vec<_>>();
    ordered.sort_by_key(|id| {
        (
            forwards[*id].semantic_start,
            std::cmp::Reverse(forwards[*id].fulfillment_step),
            *id,
        )
    });

    let mut roots = Vec::new();
    let mut stack = Vec::<ForwardNameId>::new();
    for id in ordered {
        while stack
            .last()
            .is_some_and(|parent| forwards[*parent].fulfillment_step < forwards[id].semantic_start)
        {
            stack.pop();
        }

        if let Some(parent) = stack.last().copied() {
            debug_assert!(forwards[parent].semantic_start <= forwards[id].semantic_start);
            debug_assert!(
                forwards[id].fulfillment_step <= forwards[parent].fulfillment_step,
                "aligned recursive-do intervals must be laminar"
            );
            forwards[parent].children.push(id);
        } else {
            roots.push(id);
        }
        stack.push(id);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_bind(name: &str, line: usize) -> DoStep {
        DoStep {
            line,
            kind: DoStepKind::ValueBind {
                pattern: SyntaxPattern::capture(name),
                value: SyntaxExpr::Unit,
            },
        }
    }

    fn abstract_names(names: &[&str], line: usize) -> DoStep {
        DoStep {
            line,
            kind: DoStepKind::Abstract(names.iter().map(|name| (*name).to_owned()).collect()),
        }
    }

    fn plan_expr(do_expr: &DoExpr) -> RecursiveDoPlan {
        let mut registry = ForwardNameRegistry::default();
        let mut steps = Vec::new();
        for step in &do_expr.steps {
            match &step.kind {
                DoStepKind::Abstract(names) => {
                    let ids = registry.declare(names, step.line).unwrap();
                    steps.push(RecursiveDoStep {
                        line: step.line,
                        event: RecursiveDoEvent::Declare(ids),
                    });
                }
                DoStepKind::Bind { pattern, .. } | DoStepKind::ValueBind { pattern, .. } => {
                    let mut captures = Vec::new();
                    pattern.visit_captures(&mut |name| captures.push(name));
                    let no_captures = captures.is_empty();
                    if captures.len() > 1 {
                        steps.push(RecursiveDoStep {
                            line: step.line,
                            event: RecursiveDoEvent::None,
                        });
                    }
                    for name in captures {
                        let event = registry
                            .fulfill(name)
                            .map_or(RecursiveDoEvent::None, RecursiveDoEvent::Fulfill);
                        steps.push(RecursiveDoStep {
                            line: step.line,
                            event,
                        });
                    }
                    if no_captures {
                        steps.push(RecursiveDoStep {
                            line: step.line,
                            event: RecursiveDoEvent::None,
                        });
                    }
                }
                DoStepKind::Then(_) => steps.push(RecursiveDoStep {
                    line: step.line,
                    event: RecursiveDoEvent::None,
                }),
            }
        }
        RecursiveDoPlan::build(steps.iter(), registry.into_forwards())
            .expect("recursive-do plan should be valid")
    }

    fn plan(steps: Vec<DoStep>) -> RecursiveDoPlan {
        plan_expr(&DoExpr {
            steps,
            result: Box::new(SyntaxExpr::Unit),
            result_line: 99,
        })
    }

    #[test]
    fn same_declaration_names_are_nested_by_independent_fulfillment() {
        let original = plan(vec![
            abstract_names(&["x", "y", "z"], 1),
            value_bind("y", 2),
            value_bind("x", 3),
            value_bind("z", 4),
        ]);

        assert_eq!(original.roots, [2]);
        assert_eq!(original.forwards[2].children, [0]);
        assert_eq!(original.forwards[0].children, [1]);
        assert!(original.promotion_warnings().is_empty());

        let reordered = plan(vec![
            abstract_names(&["z", "x", "y"], 1),
            value_bind("y", 2),
            value_bind("x", 3),
            value_bind("z", 4),
        ]);
        let root = reordered.roots[0];
        let middle = reordered.forwards[root].children[0];
        let inner = reordered.forwards[middle].children[0];
        assert_eq!(reordered.forwards[root].canonical, "z");
        assert_eq!(reordered.forwards[middle].canonical, "x");
        assert_eq!(reordered.forwards[inner].canonical, "y");
    }

    #[test]
    fn crossing_and_transitive_intervals_promote_individual_starts() {
        let do_expr = DoExpr {
            steps: vec![
                abstract_names(&["a"], 10),
                abstract_names(&["b"], 11),
                abstract_names(&["c"], 12),
                value_bind("a", 13),
                value_bind("b", 14),
                value_bind("c", 15),
            ],
            result: Box::new(SyntaxExpr::Unit),
            result_line: 16,
        };
        let plan = plan_expr(&do_expr);

        assert_eq!(plan.forwards[0].semantic_start, 0);
        assert_eq!(plan.forwards[1].semantic_start, 0);
        assert_eq!(plan.forwards[2].semantic_start, 0);
        assert_eq!(plan.roots, [2]);
        assert_eq!(plan.forwards[2].children, [1]);
        assert_eq!(plan.forwards[1].children, [0]);
        let warnings = plan.promotion_warnings();
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].line, 11);
        assert_eq!(warnings[1].line, 12);
    }

    #[test]
    fn contained_and_disjoint_intervals_form_a_laminar_forest() {
        let plan = plan(vec![
            abstract_names(&["outer"], 1),
            abstract_names(&["inner"], 2),
            value_bind("inner", 3),
            value_bind("outer", 4),
            abstract_names(&["later"], 5),
            value_bind("later", 6),
        ]);

        assert_eq!(plan.roots, [0, 2]);
        assert_eq!(plan.forwards[0].children, [1]);
        assert!(plan.forwards[1].children.is_empty());
        assert!(plan.forwards[2].children.is_empty());
    }

    #[test]
    fn promotion_warns_once_for_only_the_moved_names_in_a_declaration() {
        let do_expr = DoExpr {
            steps: vec![
                abstract_names(&["a"], 20),
                abstract_names(&["b", "c"], 21),
                value_bind("c", 22),
                value_bind("a", 23),
                value_bind("b", 24),
            ],
            result: Box::new(SyntaxExpr::Unit),
            result_line: 25,
        };
        let plan = plan_expr(&do_expr);
        let warnings = plan.promotion_warnings();

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].line, 21);
        assert!(warnings[0].message.contains("`b`"));
        assert!(!warnings[0].message.contains("`c`"));
        assert!(warnings[0].message.contains("line 20"));
    }

    #[test]
    fn planner_counts_internal_steps_without_treating_them_as_fulfillments() {
        let mut registry = ForwardNameRegistry::default();
        let ids = registry
            .declare(&["left".to_owned(), "right".to_owned()], 10)
            .unwrap();
        assert_eq!(ids, [0, 1]);
        let left = registry.fulfill("left").unwrap();
        let right = registry.fulfill("right").unwrap();
        let steps = [
            RecursiveDoStep {
                line: 10,
                event: RecursiveDoEvent::Declare(ids),
            },
            RecursiveDoStep {
                line: 11,
                event: RecursiveDoEvent::None,
            },
            RecursiveDoStep {
                line: 12,
                event: RecursiveDoEvent::Fulfill(left),
            },
            RecursiveDoStep {
                line: 13,
                event: RecursiveDoEvent::None,
            },
            RecursiveDoStep {
                line: 14,
                event: RecursiveDoEvent::Fulfill(right),
            },
        ];

        let plan = RecursiveDoPlan::build(steps.iter(), registry.into_forwards()).unwrap();
        assert_eq!(plan.forwards[0].fulfillment_step, 2);
        assert_eq!(plan.forwards[1].fulfillment_step, 4);
        assert_eq!(plan.roots, [1]);
        assert_eq!(plan.forwards[1].children, [0]);
    }
}

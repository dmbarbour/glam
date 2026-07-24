use std::collections::HashMap;

use super::*;

pub(in crate::g_syntax) type ForwardNameId = usize;

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

/// Incremental recursive-do planner driven by the primitive step stream.
///
/// Pattern expansion can insert internal steps and multiple source captures,
/// so planning must observe emitted primitive bindings instead of recovering
/// them from the surface `DoExpr`.
#[derive(Default)]
pub(in crate::g_syntax) struct RecursiveDoTracker {
    forwards: Vec<ForwardNamePlan>,
    declarations: Vec<DeclarationPlan>,
    active: HashMap<String, ForwardNameId>,
    step_lines: Vec<usize>,
}

impl RecursiveDoTracker {
    pub(in crate::g_syntax) fn push_abstract(
        &mut self,
        names: &[String],
        line: usize,
    ) -> Result<Vec<ForwardNameId>, Diagnostic> {
        let step = self.step_lines.len();
        self.step_lines.push(line);
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
            self.forwards.push(ForwardNamePlan {
                canonical: canonical.clone(),
                written: written.clone(),
                declaration_step: step,
                fulfillment_step: usize::MAX,
                semantic_start: step,
                children: Vec::new(),
            });
            self.active.insert(canonical, id);
            declaration_names.push(id);
        }
        self.declarations.push(DeclarationPlan {
            step,
            names: declaration_names.clone(),
        });
        Ok(declaration_names)
    }

    pub(in crate::g_syntax) fn push_capture(
        &mut self,
        written: &str,
        line: usize,
    ) -> Option<ForwardNameId> {
        let step = self.step_lines.len();
        self.step_lines.push(line);
        let canonical = local_name_metadata(written).canonical?;
        let id = self.active.remove(&canonical)?;
        self.forwards[id].fulfillment_step = step;
        Some(id)
    }

    pub(in crate::g_syntax) fn push_internal(&mut self, line: usize) {
        self.step_lines.push(line);
    }

    pub(in crate::g_syntax) fn step_count(&self) -> usize {
        self.step_lines.len()
    }

    pub(in crate::g_syntax) fn finish(mut self) -> Result<RecursiveDoPlan, Diagnostic> {
        if !self.active.is_empty() {
            let declaration_step = self
                .active
                .values()
                .map(|id| self.forwards[*id].declaration_step)
                .min()
                .expect("nonempty active map has a declaration");
            let unresolved = self
                .forwards
                .iter()
                .filter(|forward| {
                    forward.declaration_step == declaration_step
                        && forward.fulfillment_step == usize::MAX
                })
                .map(|forward| format!("`{}`", forward.canonical))
                .collect::<Vec<_>>();
            return Err(Diagnostic::error(
                self.step_lines[declaration_step],
                format!(
                    "recursive do abstract declaration has no later fulfillment for {}",
                    unresolved.join(", ")
                ),
            ));
        }

        align_crossing_starts(&mut self.forwards);
        let roots = build_scope_tree(&mut self.forwards);
        Ok(RecursiveDoPlan {
            forwards: self.forwards,
            declarations: self.declarations,
            roots,
            step_lines: self.step_lines,
        })
    }
}

impl RecursiveDoPlan {
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
        let mut tracker = RecursiveDoTracker::default();
        for step in &do_expr.steps {
            match &step.kind {
                DoStepKind::Abstract(names) => {
                    let _ = tracker.push_abstract(names, step.line).unwrap();
                }
                DoStepKind::Bind { pattern, .. } | DoStepKind::ValueBind { pattern, .. } => {
                    let mut emitted = false;
                    pattern.visit_events(&mut |event| match event {
                        SyntaxPatternEvent::Capture(name) => {
                            tracker.push_capture(name, step.line);
                            emitted = true;
                        }
                    });
                    if !emitted {
                        tracker.push_internal(step.line);
                    }
                }
                DoStepKind::Then(_) => tracker.push_internal(step.line),
            }
        }
        tracker.finish().expect("recursive-do plan should be valid")
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
    fn primitive_tracker_counts_internal_steps_without_treating_them_as_fulfillments() {
        let mut tracker = RecursiveDoTracker::default();
        assert_eq!(
            tracker
                .push_abstract(&["left".to_owned(), "right".to_owned()], 10)
                .unwrap(),
            [0, 1]
        );
        tracker.push_internal(11);
        assert_eq!(tracker.push_capture("left", 12), Some(0));
        tracker.push_internal(13);
        assert_eq!(tracker.push_capture("right", 14), Some(1));

        let plan = tracker.finish().unwrap();
        assert_eq!(plan.forwards[0].fulfillment_step, 2);
        assert_eq!(plan.forwards[1].fulfillment_step, 4);
        assert_eq!(plan.roots, [1]);
        assert_eq!(plan.forwards[1].children, [0]);
    }
}

use std::collections::BTreeSet;
use std::num::NonZeroU64;

/// Stable identity for one lexical binding in the g-syntax front end.
///
/// Unlike the temporary de Bruijn indices used by the Core compatibility
/// backend, this identity does not change when another scope is introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct BindingId(NonZeroU64);

impl BindingId {
    pub(super) fn from_local_index(index: u64) -> Self {
        let encoded = index
            .checked_add(1)
            .expect("g-syntax binding ID space exhausted");
        Self(NonZeroU64::new(encoded).expect("encoded binding ID is always nonzero"))
    }
}

/// Affine semantic expressions resolved and owned by the g-syntax front end.
///
/// Direct net lowering consumes these structural variants by value without
/// constructing a core expression or cloning expression subtrees.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ResolvedExpr<V> {
    /// Closed ordinary data, including literal and builtin values.
    Embedded(V),
    /// An opaque value supplied by an assembler capability, such as a module
    /// environment or import result.
    Provided(V),
    Local(BindingId),
    List(Vec<Self>),
    Access {
        base: Box<Self>,
        path: Vec<ResolvedPathPart<V>>,
    },
    Lambda {
        parameters: Vec<BindingId>,
        body: Box<Self>,
    },
    Apply {
        function: Box<Self>,
        arguments: Vec<Self>,
    },
    /// A literal lambda and its immediately available arguments.
    ///
    /// Keeping this distinct lets direct net lowering wire the lambda body and
    /// arguments into one net instead of first materializing a function net and
    /// then loading it through a cursor. This applies to every `(lambda) arg`
    /// expression, not only lambdas introduced by `let` or `where` sugar.
    ApplyLambda {
        parameters: Vec<BindingId>,
        body: Box<Self>,
        arguments: Vec<Self>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ResolvedPathPart<V> {
    Key(crate::core::Key),
    Index(Box<ResolvedExpr<V>>),
    PathIndex(Box<ResolvedExpr<V>>),
}

impl<V> ResolvedExpr<V> {
    pub(super) fn lambda(parameters: Vec<BindingId>, body: Self) -> Self {
        Self::Lambda {
            parameters,
            body: Box::new(body),
        }
    }

    /// Builds one maximal application spine and exposes literal beta-redexes
    /// to the future interaction-net emitter.
    pub(super) fn apply(function: Self, arguments: impl IntoIterator<Item = Self>) -> Self {
        let mut new_arguments = arguments.into_iter().collect::<Vec<_>>();
        if new_arguments.is_empty() {
            return function;
        }

        match function {
            Self::Apply {
                function,
                mut arguments,
            } => {
                arguments.append(&mut new_arguments);
                Self::Apply {
                    function,
                    arguments,
                }
            }
            Self::Lambda { parameters, body } => Self::ApplyLambda {
                parameters,
                body,
                arguments: new_arguments,
            },
            Self::ApplyLambda {
                parameters,
                body,
                mut arguments,
            } => {
                arguments.append(&mut new_arguments);
                Self::ApplyLambda {
                    parameters,
                    body,
                    arguments,
                }
            }
            function => Self::Apply {
                function: Box::new(function),
                arguments: new_arguments,
            },
        }
    }

    pub(super) fn free_bindings(&self) -> BTreeSet<BindingId> {
        let mut free = BTreeSet::new();
        self.collect_free_bindings(&mut free, &mut BTreeSet::new());
        free
    }

    fn collect_free_bindings(
        &self,
        free: &mut BTreeSet<BindingId>,
        bound: &mut BTreeSet<BindingId>,
    ) {
        match self {
            Self::Embedded(_) | Self::Provided(_) => {}
            Self::Local(binding) => {
                if !bound.contains(binding) {
                    free.insert(*binding);
                }
            }
            Self::List(items) => {
                for item in items.iter() {
                    item.collect_free_bindings(free, bound);
                }
            }
            Self::Access { base, path } => {
                base.collect_free_bindings(free, bound);
                for part in path.iter() {
                    match part {
                        ResolvedPathPart::Key(_) => {}
                        ResolvedPathPart::Index(expr) | ResolvedPathPart::PathIndex(expr) => {
                            expr.collect_free_bindings(free, bound);
                        }
                    }
                }
            }
            Self::Lambda { parameters, body } => {
                bound.extend(parameters.iter().copied());
                body.collect_free_bindings(free, bound);
                for parameter in parameters {
                    bound.remove(parameter);
                }
            }
            Self::Apply {
                function,
                arguments,
            } => {
                function.collect_free_bindings(free, bound);
                for argument in arguments.iter() {
                    argument.collect_free_bindings(free, bound);
                }
            }
            Self::ApplyLambda {
                parameters,
                body,
                arguments,
            } => {
                bound.extend(parameters.iter().copied());
                body.collect_free_bindings(free, bound);
                for parameter in parameters {
                    bound.remove(parameter);
                }
                for argument in arguments.iter() {
                    argument.collect_free_bindings(free, bound);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestResolver {
        next_binding_id: u64,
    }

    impl TestResolver {
        fn fresh_binding(&mut self) -> BindingId {
            let binding = BindingId::from_local_index(self.next_binding_id);
            self.next_binding_id += 1;
            binding
        }
    }

    #[test]
    fn application_spines_are_grouped() {
        let function = ResolvedExpr::Embedded("f");
        let first = ResolvedExpr::apply(function, [ResolvedExpr::Embedded("x")]);
        let grouped = ResolvedExpr::apply(
            first,
            [ResolvedExpr::Embedded("y"), ResolvedExpr::Embedded("z")],
        );

        assert!(matches!(
            grouped,
            ResolvedExpr::Apply { arguments, .. } if arguments.len() == 3
        ));
    }

    #[test]
    fn application_spines_move_non_clone_expressions() {
        #[derive(Debug, PartialEq, Eq)]
        struct NonClone(&'static str);

        let first = ResolvedExpr::apply(
            ResolvedExpr::Embedded(NonClone("f")),
            [ResolvedExpr::Embedded(NonClone("x"))],
        );
        let grouped = ResolvedExpr::apply(first, [ResolvedExpr::Embedded(NonClone("y"))]);

        assert!(matches!(
            grouped,
            ResolvedExpr::Apply { arguments, .. } if arguments.len() == 2
        ));
    }

    #[test]
    fn every_literal_lambda_application_is_marked_for_local_fusion() {
        let parameter = TestResolver::default().fresh_binding();
        let lambda = ResolvedExpr::lambda(vec![parameter], ResolvedExpr::Local(parameter));
        let applied = ResolvedExpr::apply(lambda, [ResolvedExpr::Embedded("argument")]);

        assert!(matches!(applied, ResolvedExpr::ApplyLambda { .. }));
    }

    #[test]
    fn free_bindings_use_stable_identity_across_nested_lambdas() {
        let mut resolver = TestResolver::default();
        let outer = resolver.fresh_binding();
        let inner = resolver.fresh_binding();
        let expression = ResolvedExpr::<()>::lambda(
            vec![inner],
            ResolvedExpr::apply(ResolvedExpr::Local(outer), [ResolvedExpr::Local(inner)]),
        );

        assert_eq!(expression.free_bindings(), BTreeSet::from([outer]));
    }
}

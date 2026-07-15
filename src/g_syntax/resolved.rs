use std::collections::BTreeSet;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_BINDING_ID: AtomicU64 = AtomicU64::new(1);

/// Stable identity for one lexical binding in the g-syntax front end.
///
/// Unlike the temporary de Bruijn indices used by the Core compatibility
/// backend, this identity does not change when another scope is introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct BindingId(NonZeroU64);

impl BindingId {
    pub(super) fn fresh() -> Self {
        let raw = NEXT_BINDING_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("g-syntax binding ID space exhausted");
        Self(NonZeroU64::new(raw).expect("binding IDs start at one"))
    }
}

/// Syntax-independent expressions resolved by the g-syntax front end.
///
/// `Opaque` is the compatibility seam used while expression forms migrate off
/// Core. Direct net lowering will ultimately consume the remaining variants
/// without constructing a Core expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResolvedExpr<V> {
    Opaque(V),
    #[allow(dead_code)] // Constructed as lexical lowering migrates off CoreExpr.
    Local(BindingId),
    Lambda {
        parameters: Arc<[BindingId]>,
        body: Arc<Self>,
    },
    Apply {
        function: Arc<Self>,
        arguments: Arc<[Self]>,
    },
    /// A literal lambda and its immediately available arguments.
    ///
    /// Keeping this distinct lets direct net lowering wire the lambda body and
    /// arguments into one net instead of first materializing a function net and
    /// then loading it through a cursor. This applies to every `(lambda) arg`
    /// expression, not only lambdas introduced by `let` or `where` sugar.
    ApplyLambda {
        parameters: Arc<[BindingId]>,
        body: Arc<Self>,
        arguments: Arc<[Self]>,
    },
}

impl<V: Clone> ResolvedExpr<V> {
    pub(super) fn lambda(parameters: Arc<[BindingId]>, body: Self) -> Self {
        Self::Lambda {
            parameters,
            body: Arc::new(body),
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
                arguments,
            } => {
                let mut combined = arguments.to_vec();
                combined.append(&mut new_arguments);
                Self::Apply {
                    function,
                    arguments: Arc::from(combined),
                }
            }
            Self::Lambda { parameters, body } => Self::ApplyLambda {
                parameters,
                body,
                arguments: Arc::from(new_arguments),
            },
            Self::ApplyLambda {
                parameters,
                body,
                arguments,
            } => {
                let mut combined = arguments.to_vec();
                combined.append(&mut new_arguments);
                Self::ApplyLambda {
                    parameters,
                    body,
                    arguments: Arc::from(combined),
                }
            }
            function => Self::Apply {
                function: Arc::new(function),
                arguments: Arc::from(new_arguments),
            },
        }
    }

    #[allow(dead_code)] // Becomes the closure-conversion input for direct net emission.
    pub(super) fn free_bindings(&self) -> BTreeSet<BindingId> {
        let mut free = BTreeSet::new();
        self.collect_free_bindings(&mut free);
        free
    }

    #[allow(dead_code)]
    fn collect_free_bindings(&self, free: &mut BTreeSet<BindingId>) {
        match self {
            Self::Opaque(_) => {}
            Self::Local(binding) => {
                free.insert(*binding);
            }
            Self::Lambda { parameters, body } => {
                let mut body_free = body.free_bindings();
                body_free.retain(|binding| !parameters.contains(binding));
                free.extend(body_free);
            }
            Self::Apply {
                function,
                arguments,
            } => {
                function.collect_free_bindings(free);
                for argument in arguments.iter() {
                    argument.collect_free_bindings(free);
                }
            }
            Self::ApplyLambda {
                parameters,
                body,
                arguments,
            } => {
                let mut body_free = body.free_bindings();
                body_free.retain(|binding| !parameters.contains(binding));
                free.extend(body_free);
                for argument in arguments.iter() {
                    argument.collect_free_bindings(free);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_spines_are_grouped() {
        let function = ResolvedExpr::Opaque("f");
        let first = ResolvedExpr::apply(function, [ResolvedExpr::Opaque("x")]);
        let grouped = ResolvedExpr::apply(
            first,
            [ResolvedExpr::Opaque("y"), ResolvedExpr::Opaque("z")],
        );

        assert!(matches!(
            grouped,
            ResolvedExpr::Apply { arguments, .. } if arguments.len() == 3
        ));
    }

    #[test]
    fn every_literal_lambda_application_is_marked_for_local_fusion() {
        let parameter = BindingId::fresh();
        let lambda = ResolvedExpr::lambda(Arc::from([parameter]), ResolvedExpr::Local(parameter));
        let applied = ResolvedExpr::apply(lambda, [ResolvedExpr::Opaque("argument")]);

        assert!(matches!(applied, ResolvedExpr::ApplyLambda { .. }));
    }

    #[test]
    fn free_bindings_use_stable_identity_across_nested_lambdas() {
        let outer = BindingId::fresh();
        let inner = BindingId::fresh();
        let expression = ResolvedExpr::<()>::lambda(
            Arc::from([inner]),
            ResolvedExpr::apply(ResolvedExpr::Local(outer), [ResolvedExpr::Local(inner)]),
        );

        assert_eq!(expression.free_bindings(), BTreeSet::from([outer]));
    }
}

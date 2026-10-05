//! Audit: coordinator publication passes through its classified boundaries.
//!
//! Waiters and settlement observe the coordinator's work generation, and idle
//! executors wake on its shared condition variable. Each production change to
//! either goes through one function that classifies it by
//! `CoordinatorMutationKind`, so new coordinator work cannot silently escape
//! route-effect review and profiling.

use std::fs;
use std::path::Path;

use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ExprAssign, ExprBinary, ExprMethodCall, ImplItemFn, ItemFn, ItemImpl};

/// The only item that may advance the work generation.
const GENERATION_BOUNDARY: &str =
    "crate::evaluation::coordinator::WorkCoordinatorState::advance_work_generation";

/// The only item that may wake executors waiting for shared work.
const NOTIFICATION_BOUNDARY: &str =
    "crate::evaluation::coordinator::EvaluationWorkCoordinator::notify_all";

#[derive(Default)]
struct PublicationVisitor {
    scope: Vec<String>,
    generation_writes: Vec<String>,
    work_notifications: Vec<String>,
}

impl PublicationVisitor {
    fn within(&mut self, name: String, attributes: &[Attribute], visit: impl FnOnce(&mut Self)) {
        if is_test_only(attributes) {
            return;
        }
        self.scope.push(name);
        visit(self);
        self.scope.pop();
    }

    fn record_generation_write(&mut self, target: &Expr) {
        if is_field(target, "work_generation") {
            self.generation_writes.push(self.scope.join("::"));
        }
    }
}

fn is_field(expression: &Expr, name: &str) -> bool {
    matches!(expression, Expr::Field(field)
        if matches!(&field.member, syn::Member::Named(member) if member == name))
}

impl<'ast> Visit<'ast> for PublicationVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.within(node.ident.to_string(), &node.attrs, |visitor| {
            visit::visit_item_mod(visitor, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let name = match node.self_ty.as_ref() {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .map_or_else(|| "impl".to_owned(), |part| part.ident.to_string()),
            _ => "impl".to_owned(),
        };
        self.within(name, &node.attrs, |visitor| {
            visit::visit_item_impl(visitor, node);
        });
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.within(node.sig.ident.to_string(), &node.attrs, |visitor| {
            visit::visit_item_fn(visitor, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.within(node.sig.ident.to_string(), &node.attrs, |visitor| {
            visit::visit_impl_item_fn(visitor, node);
        });
    }

    fn visit_expr_assign(&mut self, node: &'ast ExprAssign) {
        self.record_generation_write(&node.left);
        visit::visit_expr_assign(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast ExprBinary) {
        if matches!(
            node.op,
            syn::BinOp::AddAssign(_) | syn::BinOp::SubAssign(_) | syn::BinOp::MulAssign(_)
        ) {
            self.record_generation_write(&node.left);
        }
        visit::visit_expr_binary(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if (node.method == "notify_one" || node.method == "notify_all")
            && is_field(&node.receiver, "work_available")
        {
            self.work_notifications.push(self.scope.join("::"));
        }
        visit::visit_expr_method_call(self, node);
    }
}

fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute
                    .parse_args::<syn::Path>()
                    .is_ok_and(|path| path.is_ident("test")))
    })
}

/// Visits the coordinator module and its production submodules.
fn coordinator_publications() -> PublicationVisitor {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/evaluation");
    let parse = |path: &Path| {
        let source = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
        syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} should parse: {error}", path.display()))
    };
    let module = "crate::evaluation::coordinator";
    let coordinator = parse(&directory.join("coordinator.rs"));
    let mut visitor = PublicationVisitor {
        scope: vec![module.to_owned()],
        ..PublicationVisitor::default()
    };
    visitor.visit_file(&coordinator);
    for item in &coordinator.items {
        if let syn::Item::Mod(child) = item
            && child.content.is_none()
            && !is_test_only(&child.attrs)
        {
            let path = directory
                .join("coordinator")
                .join(format!("{}.rs", child.ident));
            visitor.scope = vec![format!("{module}::{}", child.ident)];
            visitor.visit_file(&parse(&path));
        }
    }
    visitor
}

#[test]
fn work_generation_advances_only_at_its_classified_boundary() {
    let writes = coordinator_publications().generation_writes;
    assert!(
        writes.iter().any(|item| item == GENERATION_BOUNDARY),
        "the audit should find the generation boundary"
    );
    let bypasses = writes
        .iter()
        .filter(|item| *item != GENERATION_BOUNDARY)
        .collect::<Vec<_>>();
    assert!(
        bypasses.is_empty(),
        "advance the work generation through {GENERATION_BOUNDARY}: {bypasses:?}"
    );
}

#[test]
fn shared_work_wakeups_pass_through_the_profiled_boundary() {
    let notifications = coordinator_publications().work_notifications;
    assert!(
        notifications
            .iter()
            .any(|item| item == NOTIFICATION_BOUNDARY),
        "the audit should find the notification boundary"
    );
    let bypasses = notifications
        .iter()
        .filter(|item| *item != NOTIFICATION_BOUNDARY)
        .collect::<Vec<_>>();
    assert!(
        bypasses.is_empty(),
        "wake shared-work waiters through {NOTIFICATION_BOUNDARY}: {bypasses:?}"
    );
}

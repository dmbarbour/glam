//! Audit: specialization request work never evaluates synchronously.
//!
//! A specialization's request work runs inside a reflection machine poll. It
//! hands semantic demand back as `SpecializationRequestPoll::Demand` and
//! resumes with the result. Evaluating synchronously instead would nest
//! evaluation on the Rust stack and could block a worker on its own work.
//!
//! The audit covers every production module that implements
//! `SpecializationRequestWork`, in the library and the `glam` binary. Within
//! such a module it checks every item except the task host's implementations,
//! which run outside request polling.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprCall, ExprMethodCall, ImplItemFn, ItemFn, ItemImpl, ItemMod, Meta, Token,
};

/// Calls that obtain or use synchronous evaluation.
const SYNCHRONOUS_EVALUATION: &[&str] = &["eval", "evaluate", "evaluate_root_whnf", "evaluator"];

struct SynchronousEvaluationVisitor {
    scope: Vec<String>,
    violations: Vec<String>,
}

impl SynchronousEvaluationVisitor {
    fn within(&mut self, name: String, attributes: &[Attribute], visit: impl FnOnce(&mut Self)) {
        if is_test_only(attributes) {
            return;
        }
        self.scope.push(name);
        visit(self);
        self.scope.pop();
    }

    fn check(&mut self, name: &syn::Ident) {
        if SYNCHRONOUS_EVALUATION
            .iter()
            .any(|forbidden| name == forbidden)
        {
            self.violations
                .push(format!("{}: `{name}`", self.scope.join("::")));
        }
    }
}

impl<'ast> Visit<'ast> for SynchronousEvaluationVisitor {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        self.within(node.ident.to_string(), &node.attrs, |visitor| {
            visit::visit_item_mod(visitor, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        self.within(type_name(&node.self_ty), &node.attrs, |visitor| {
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

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        self.check(&node.method);
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(path) = node.func.as_ref()
            && let Some(last) = path.path.segments.last()
        {
            self.check(&last.ident);
        }
        visit::visit_expr_call(self, node);
    }
}

fn implements(item: &ItemImpl, trait_name: &str) -> bool {
    !is_test_only(&item.attrs)
        && item.trait_.as_ref().is_some_and(|(_, path, _)| {
            path.segments
                .last()
                .is_some_and(|segment| segment.ident == trait_name)
        })
}

/// Synchronous evaluation inside a request-work module, or `None` when the
/// module implements no request work.
fn request_work_violations(module: &str, syntax: &syn::File) -> Option<Vec<String>> {
    let impls = syntax.items.iter().filter_map(|item| match item {
        syn::Item::Impl(item) => Some(item),
        _ => None,
    });
    if !impls
        .clone()
        .any(|item| implements(item, "SpecializationRequestWork"))
    {
        return None;
    }
    let hosts = impls
        .filter(|item| implements(item, "TaskHost"))
        .map(|item| type_name(&item.self_ty))
        .collect::<BTreeSet<_>>();
    let mut visitor = SynchronousEvaluationVisitor {
        scope: vec![module.to_owned()],
        violations: Vec::new(),
    };
    for item in &syntax.items {
        match item {
            syn::Item::Impl(item) if hosts.contains(&type_name(&item.self_ty)) => {}
            item => visitor.visit_item(item),
        }
    }
    Some(visitor.violations)
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map_or_else(|| "impl".to_owned(), |part| part.ident.to_string()),
        _ => "impl".to_owned(),
    }
}

/// Whether an item exists only in test builds: `#[test]`, or a `cfg`
/// predicate that is false whenever `test` is off.
fn is_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("test")
            || (attribute.path().is_ident("cfg")
                && attribute
                    .parse_args::<Meta>()
                    .is_ok_and(|predicate| cfg_without_test(&predicate) == Some(false)))
    })
}

/// Evaluates a `cfg` predicate with `test` off and every other option
/// unknown (`None`).
fn cfg_without_test(predicate: &Meta) -> Option<bool> {
    match predicate {
        Meta::Path(path) if path.is_ident("test") => Some(false),
        Meta::List(list) => {
            let operands = list
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .ok()?
                .iter()
                .map(cfg_without_test)
                .collect::<Vec<_>>();
            if list.path.is_ident("not") {
                operands.first().copied().flatten().map(|value| !value)
            } else if list.path.is_ident("all") {
                if operands.contains(&Some(false)) {
                    Some(false)
                } else {
                    operands
                        .iter()
                        .all(|value| *value == Some(true))
                        .then_some(true)
                }
            } else if list.path.is_ident("any") {
                if operands.contains(&Some(true)) {
                    Some(true)
                } else {
                    operands
                        .iter()
                        .all(|value| *value == Some(false))
                        .then_some(false)
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Rust sources under `directory`, except test directories and test files.
fn non_test_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the source tree should be readable") {
        let path = entry.expect("a source entry should be readable").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name != "tests" {
                non_test_sources(&path, sources);
            }
        } else if let Some(stem) = name.strip_suffix(".rs")
            && !matches!(stem, "tests" | "test_support")
            && !stem.ends_with("_inventory")
        {
            sources.push(path);
        }
    }
}

#[test]
fn specialization_request_work_never_evaluates_synchronously() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    non_test_sources(&manifest.join("src"), &mut sources);
    sources.sort();

    let mut request_modules = Vec::new();
    let mut violations = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a source should belong to this package")
            .display()
            .to_string();
        let source = fs::read_to_string(&path).expect("a source should be readable");
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{relative} should parse: {error}"));
        if let Some(found) = request_work_violations(&relative, &syntax) {
            request_modules.push(relative);
            violations.extend(found);
        }
    }

    assert!(
        !request_modules.is_empty(),
        "the audit must find the modules that implement SpecializationRequestWork"
    );
    assert!(
        violations.is_empty(),
        "specialization request work must return SpecializationRequestPoll::Demand \
         instead of evaluating synchronously: {violations:#?}"
    );
}

#[test]
fn request_work_audit_exempts_only_the_task_host() {
    let syntax = syn::parse_file(
        r"
        impl SpecializationRequestWork<Effects> for Work {
            fn poll(&mut self) { self.assembler.evaluator().eval(&self.value); }
        }
        fn helper(context: &EvalContext) { context.evaluate_root_whnf(root); }
        impl TaskHost<Effects> for Host {}
        impl Host {
            fn new(assembler: Assembler) { assembler.evaluator().eval(&value); }
        }
        #[cfg(test)]
        fn fixture(assembler: Assembler) { assembler.evaluator().eval(&value); }
        ",
    )
    .expect("the request-work fixture should parse");

    assert_eq!(
        request_work_violations("fixture", &syntax),
        Some(vec![
            "fixture::Work::poll: `eval`".to_owned(),
            "fixture::Work::poll: `evaluator`".to_owned(),
            "fixture::helper: `evaluate_root_whnf`".to_owned(),
        ])
    );
    let no_request_work =
        syn::parse_file("fn helper(context: &EvalContext) { context.evaluate(); }")
            .expect("the fixture should parse");
    assert_eq!(request_work_violations("fixture", &no_request_work), None);
}

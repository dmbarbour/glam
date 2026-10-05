//! Audit: no unapproved recursion in evaluator and core-value code.
//!
//! User data can be arbitrarily deep, so semantic work must not recurse on the
//! Rust stack. The permitted forms are bounded plumbing, log-depth balanced
//! containers, and owned worklists. This audit builds a call graph over the
//! production code of the evaluator, the core value domain, and the
//! evaluator-facing orchestration and reflection modules. It rejects every
//! call cycle that is neither approved on [`BOUNDED_RECURSION`] nor recorded
//! as an open defect on [`KNOWN_UNBOUNDED_RECURSION`].
//!
//! The graph sees only calls it can resolve syntactically within one module:
//! a free function, `Self::f` or `Type::f`, and `self.f()`. It does not see
//! recursion through another receiver (`child.f()`), another module, a trait
//! object, a closure value, or implicit `Drop` glue. Those shapes need
//! small-stack tests instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{Attribute, ExprCall, ExprMethodCall, ImplItemFn, ItemFn, Meta, Token};

/// Recursive call families whose depth is bounded independently of user
/// data. Each entry names one function on a cycle, keyed by module and item,
/// with the reason the cycle cannot grow with user input.
const BOUNDED_RECURSION: &[(&str, &str)] = &[];

/// Open defects, not approvals: cycles whose depth follows user data. Each
/// awaits an iterative rewrite. They are listed so that a new cycle still
/// fails the audit; remove an entry once its function stops recursing.
const KNOWN_UNBOUNDED_RECURSION: &[(&str, &str)] = &[
    (
        "crate::core::RuntimeValueAccess::key_from_value",
        "recurses once per list or dictionary nesting level of the value it converts",
    ),
    (
        "crate::core::RuntimeValueAccess::same_representation",
        "recurses once per strict list or dictionary nesting level of the compared values",
    ),
    (
        "crate::core::RuntimeValueAccess::value_from_key",
        "recurses once per list or dictionary nesting level of the key it reifies",
    ),
    (
        "crate::core::managed::payload_edges::managed::visit_value_with",
        "the collector's edge walk recurses once per strict aggregate level above a managed identity",
    ),
];

/// Modules whose production code must not recurse, with their submodules.
const AUDITED_MODULE_TREES: &[&str] = &["crate::eval", "crate::core"];

/// Single orchestration and reflection modules that drive evaluator work.
const AUDITED_MODULES: &[&str] = &[
    "crate::evaluation::pump",
    "crate::evaluation::session",
    "crate::evaluation::whnf",
    "crate::reflection::machine",
    "crate::reflection::protocol",
    "crate::reflection::requests",
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FunctionKey {
    module: String,
    impl_name: Option<String>,
    name: String,
}

impl FunctionKey {
    fn item_path(&self) -> String {
        match &self.impl_name {
            Some(owner) => format!("{}::{owner}::{}", self.module, self.name),
            None => format!("{}::{}", self.module, self.name),
        }
    }
}

#[derive(Default)]
struct CallGraph {
    definitions: BTreeSet<FunctionKey>,
    calls: BTreeMap<FunctionKey, BTreeSet<FunctionKey>>,
}

impl CallGraph {
    fn add_module(&mut self, module: &str, syntax: &syn::File) {
        let mut visitor = CallGraphVisitor {
            module: vec![module.to_owned()],
            impl_name: None,
            function: None,
            definitions: BTreeSet::new(),
            calls: Vec::new(),
        };
        visitor.visit_file(syntax);
        self.definitions.extend(visitor.definitions);
        for (caller, callee) in visitor.calls {
            self.calls.entry(caller).or_default().insert(callee);
        }
    }

    /// Every defined function from which a resolved call path returns to it.
    fn cyclic_functions(&self) -> BTreeSet<String> {
        self.definitions
            .iter()
            .filter(|start| {
                let resolved = |key: &FunctionKey| self.definitions.contains(key);
                let mut pending = self
                    .calls
                    .get(*start)
                    .into_iter()
                    .flatten()
                    .filter(|key| resolved(key))
                    .collect::<Vec<_>>();
                let mut visited = BTreeSet::new();
                while let Some(next) = pending.pop() {
                    if next == *start {
                        return true;
                    }
                    if visited.insert(next)
                        && let Some(following) = self.calls.get(next)
                    {
                        pending.extend(following.iter().filter(|key| resolved(key)));
                    }
                }
                false
            })
            .map(FunctionKey::item_path)
            .collect()
    }
}

struct CallGraphVisitor {
    module: Vec<String>,
    impl_name: Option<String>,
    function: Option<FunctionKey>,
    definitions: BTreeSet<FunctionKey>,
    calls: Vec<(FunctionKey, FunctionKey)>,
}

impl CallGraphVisitor {
    fn key(&self, impl_name: Option<String>, name: String) -> FunctionKey {
        FunctionKey {
            module: self.module.join("::"),
            impl_name,
            name,
        }
    }

    fn visit_function(&mut self, name: String, attributes: &[Attribute], body: &syn::Block) {
        if is_test_only(attributes) {
            return;
        }
        let function = self.key(self.impl_name.clone(), name);
        self.definitions.insert(function.clone());
        let prior = self.function.replace(function);
        self.visit_block(body);
        self.function = prior;
    }

    fn record(&mut self, impl_name: Option<String>, name: String) {
        if let Some(caller) = self.function.clone() {
            let callee = self.key(impl_name, name);
            self.calls.push((caller, callee));
        }
    }
}

impl<'ast> Visit<'ast> for CallGraphVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if is_test_only(&node.attrs) {
            return;
        }
        if let Some((_, items)) = &node.content {
            self.module.push(node.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.module.pop();
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if is_test_only(&node.attrs) {
            return;
        }
        let prior = self.impl_name.replace(type_name(&node.self_ty));
        visit::visit_item_impl(self, node);
        self.impl_name = prior;
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.visit_function(node.sig.ident.to_string(), &node.attrs, &node.block);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.visit_function(node.sig.ident.to_string(), &node.attrs, &node.block);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(path) = node.func.as_ref() {
            let segments = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            match segments.as_slice() {
                [name] => self.record(None, name.clone()),
                [owner, name] if owner == "Self" => {
                    self.record(self.impl_name.clone(), name.clone());
                }
                [owner, name] => self.record(Some(owner.clone()), name.clone()),
                _ => {}
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if matches!(
            node.receiver.as_ref(),
            syn::Expr::Path(path) if path.path.is_ident("self")
        ) {
            self.record(self.impl_name.clone(), node.method.to_string());
        }
        visit::visit_expr_method_call(self, node);
    }
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

/// Every production module reachable from `src/lib.rs` through `mod`
/// declarations that are not test-only, with its parsed file.
fn production_modules(manifest: &Path) -> Vec<(String, syn::File)> {
    let mut pending = vec![(PathBuf::from("src/lib.rs"), "crate".to_owned())];
    let mut modules = Vec::new();
    while let Some((path, module)) = pending.pop() {
        let source = fs::read_to_string(manifest.join(&path))
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", path.display()));
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{} should parse: {error}", path.display()));
        let directory = if path.ends_with("lib.rs") || path.ends_with("mod.rs") {
            path.parent()
                .expect("a module file has a directory")
                .to_owned()
        } else {
            path.with_extension("")
        };
        child_module_files(manifest, &syntax.items, &directory, &module, &mut pending);
        modules.push((module, syntax));
    }
    modules
}

fn child_module_files(
    manifest: &Path,
    items: &[syn::Item],
    directory: &Path,
    module: &str,
    pending: &mut Vec<(PathBuf, String)>,
) {
    for item in items {
        let syn::Item::Mod(child) = item else {
            continue;
        };
        if is_test_only(&child.attrs) {
            continue;
        }
        let name = child.ident.to_string();
        let child_module = format!("{module}::{name}");
        if let Some((_, items)) = &child.content {
            child_module_files(
                manifest,
                items,
                &directory.join(&name),
                &child_module,
                pending,
            );
        } else {
            assert!(
                !child
                    .attrs
                    .iter()
                    .any(|attribute| attribute.path().is_ident("path")),
                "{child_module}: a production `#[path]` module needs audit support"
            );
            let file = directory.join(format!("{name}.rs"));
            let file = if manifest.join(&file).exists() {
                file
            } else {
                directory.join(&name).join("mod.rs")
            };
            pending.push((file, child_module));
        }
    }
}

fn is_audited(module: &str) -> bool {
    AUDITED_MODULES.contains(&module)
        || AUDITED_MODULE_TREES.iter().any(|tree| {
            module == *tree
                || module
                    .strip_prefix(tree)
                    .is_some_and(|rest| rest.starts_with("::"))
        })
}

#[test]
fn evaluator_and_core_code_has_no_unapproved_recursion() {
    let mut graph = CallGraph::default();
    for (module, syntax) in production_modules(Path::new(env!("CARGO_MANIFEST_DIR"))) {
        if is_audited(&module) {
            graph.add_module(&module, &syntax);
        }
    }
    let cyclic = graph.cyclic_functions();
    let listed = BOUNDED_RECURSION
        .iter()
        .chain(KNOWN_UNBOUNDED_RECURSION)
        .map(|(function, reason)| {
            assert!(!reason.is_empty(), "{function} needs a reason");
            *function
        })
        .collect::<BTreeSet<_>>();

    let unapproved = cyclic
        .iter()
        .filter(|function| !listed.contains(function.as_str()))
        .collect::<Vec<_>>();
    assert!(
        unapproved.is_empty(),
        "recursive call cycles must become explicit iteration, or be added to \
         BOUNDED_RECURSION with a reason when their depth is bounded: {unapproved:#?}"
    );
    let stale = listed
        .iter()
        .filter(|function| !cyclic.contains(**function))
        .collect::<Vec<_>>();
    assert!(
        stale.is_empty(),
        "remove listed functions that no longer recurse: {stale:#?}"
    );
}

#[test]
fn recursion_audit_detects_direct_mutual_and_method_cycles() {
    let syntax = syn::parse_file(
        r#"
        fn direct(depth: usize) { if depth > 0 { direct(depth - 1) } }
        fn even(n: usize) -> bool { n == 0 || odd(n - 1) }
        fn odd(n: usize) -> bool { n != 0 && even(n - 1) }
        fn leaf() {}
        fn caller() { leaf(); leaf() }
        struct Tree;
        impl Tree {
            fn walk(&self) { self.walk() }
            fn visit(&self) { Self::visit_children(self) }
            fn visit_children(&self) { Tree::visit(self) }
        }
        #[cfg(test)]
        fn test_only_recursion() { test_only_recursion() }
        #[cfg(any(test, feature = "profiling"))]
        fn profiling_recursion() { profiling_recursion() }
        "#,
    )
    .expect("the recursion fixture should parse");
    let mut graph = CallGraph::default();
    graph.add_module("crate::fixture", &syntax);

    assert_eq!(
        graph.cyclic_functions(),
        [
            "crate::fixture::Tree::visit",
            "crate::fixture::Tree::visit_children",
            "crate::fixture::Tree::walk",
            "crate::fixture::direct",
            "crate::fixture::even",
            "crate::fixture::odd",
            "crate::fixture::profiling_recursion",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
    );
}

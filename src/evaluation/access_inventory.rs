//! Audit: managed value-access regions are bounded and never nested.
//!
//! The core value domain owns the collector's raw mutator. Production code
//! reaches it only through one higher-ranked gateway, so access authority
//! cannot outlive the callback. A region is never opened while another is
//! open: neither lexically inside a gateway callback, nor in a function that
//! already receives access authority.

use std::fs;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprMethodCall, FnArg, GenericArgument, ImplItemFn, ItemFn, ItemImpl, ItemMod, Meta,
    PathArguments, Signature, Token, Type, TypeParamBound,
};

/// The value domain's raw collector entry.
const RAW_MUTATOR_ENTRY: &str = "with_mutator";

/// The only production item that may call the raw collector entry.
const RAW_MUTATOR_GATEWAY: &str =
    "crate::core::managed::CoreValueFactory::with_runtime_value_access";

/// Calls that open a managed value-access region for their callback.
const REGION_OPENERS: &[&str] = &[
    "construct_runtime_value_root",
    "try_construct_runtime_value_root",
    "with_managed_values",
    "with_mutator",
    "with_runtime_value_access",
];

/// Parameter and receiver types that carry access authority already.
const ACCESS_CARRIERS: &[&str] = &["EvaluationValueAccess", "RuntimeValueAccess"];

#[derive(Debug, Eq, PartialEq)]
struct RegionOpening {
    item: String,
    opener: String,
    nested: bool,
    carries_access: bool,
}

struct RegionVisitor {
    scope: Vec<String>,
    carrier_scopes: Vec<bool>,
    nesting: usize,
    openings: Vec<RegionOpening>,
}

impl RegionVisitor {
    fn new(module: &str) -> Self {
        Self {
            scope: vec![module.to_owned()],
            carrier_scopes: vec![false],
            nesting: 0,
            openings: Vec::new(),
        }
    }

    fn within(
        &mut self,
        name: String,
        attributes: &[Attribute],
        carries_access: bool,
        visit: impl FnOnce(&mut Self),
    ) {
        if is_test_only(attributes) {
            return;
        }
        let inherited = self.carrier_scopes.last().copied().unwrap_or(false);
        self.scope.push(name);
        self.carrier_scopes.push(inherited || carries_access);
        visit(self);
        self.carrier_scopes.pop();
        self.scope.pop();
    }
}

impl<'ast> Visit<'ast> for RegionVisitor {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        self.within(node.ident.to_string(), &node.attrs, false, |visitor| {
            visit::visit_item_mod(visitor, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let name = match node.self_ty.as_ref() {
            Type::Path(path) => path
                .path
                .segments
                .last()
                .map_or_else(|| "impl".to_owned(), |part| part.ident.to_string()),
            other => other.to_token_stream().to_string(),
        };
        self.within(
            name,
            &node.attrs,
            carries_access(&node.self_ty),
            |visitor| visit::visit_item_impl(visitor, node),
        );
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.within(
            node.sig.ident.to_string(),
            &node.attrs,
            signature_carries_access(&node.sig),
            |visitor| visit::visit_item_fn(visitor, node),
        );
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.within(
            node.sig.ident.to_string(),
            &node.attrs,
            signature_carries_access(&node.sig),
            |visitor| visit::visit_impl_item_fn(visitor, node),
        );
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        if !REGION_OPENERS.contains(&method.as_str()) {
            visit::visit_expr_method_call(self, node);
            return;
        }
        self.openings.push(RegionOpening {
            item: self.scope.join("::"),
            opener: method,
            nested: self.nesting != 0,
            carries_access: self.carrier_scopes.last().copied().unwrap_or(false),
        });
        // The receiver and ordinary arguments run before the region opens;
        // only a closure argument runs inside it.
        self.visit_expr(&node.receiver);
        for argument in &node.args {
            let opens = matches!(argument, syn::Expr::Closure(_));
            self.nesting += usize::from(opens);
            self.visit_expr(argument);
            self.nesting -= usize::from(opens);
        }
    }
}

/// Whether a value of this type already carries access authority. An
/// `impl FnOnce(RuntimeValueAccess)` parameter is a callback that receives a
/// new region, not authority, so trait bounds are not inspected.
fn carries_access(value_type: &Type) -> bool {
    match value_type {
        Type::Path(path) => path.path.segments.iter().any(|segment| {
            ACCESS_CARRIERS
                .iter()
                .any(|carrier| segment.ident == carrier)
                || matches!(&segment.arguments, PathArguments::AngleBracketed(arguments)
                if arguments.args.iter().any(|argument| {
                    matches!(argument, GenericArgument::Type(inner) if carries_access(inner))
                }))
        }),
        Type::Reference(reference) => carries_access(&reference.elem),
        Type::Group(group) => carries_access(&group.elem),
        Type::Paren(paren) => carries_access(&paren.elem),
        Type::Tuple(tuple) => tuple.elems.iter().any(carries_access),
        _ => false,
    }
}

fn signature_carries_access(signature: &Signature) -> bool {
    signature.inputs.iter().any(|input| match input {
        FnArg::Receiver(_) => false,
        FnArg::Typed(input) => carries_access(&input.ty),
    })
}

/// Whether some `impl Trait` parameter is higher-ranked, as in
/// `impl for<'scope> FnOnce(RuntimeValueAccess<'scope>) -> R`.
fn has_higher_ranked_callback(signature: &Signature) -> bool {
    signature.inputs.iter().any(|input| match input {
        FnArg::Typed(input) => matches!(input.ty.as_ref(), Type::ImplTrait(bounds)
        if bounds.bounds.iter().any(|bound| {
            matches!(bound, TypeParamBound::Trait(bound) if bound.lifetimes.is_some())
        })),
        FnArg::Receiver(_) => false,
    })
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

fn production_region_openings(modules: &[(String, syn::File)]) -> Vec<RegionOpening> {
    let mut openings = Vec::new();
    for (module, syntax) in modules {
        let mut visitor = RegionVisitor::new(module);
        visitor.visit_file(syntax);
        openings.extend(visitor.openings);
    }
    openings
}

#[test]
fn raw_mutator_entry_is_private_to_the_higher_ranked_gateway() {
    let modules = production_modules(Path::new(env!("CARGO_MANIFEST_DIR")));
    let entries = production_region_openings(&modules)
        .into_iter()
        .filter(|opening| opening.opener == RAW_MUTATOR_ENTRY)
        .map(|opening| opening.item)
        .collect::<Vec<_>>();
    assert!(
        entries.iter().all(|item| item == RAW_MUTATOR_GATEWAY),
        "only {RAW_MUTATOR_GATEWAY} may enter the collector's raw mutator: {entries:#?}"
    );

    let (_, managed) = modules
        .iter()
        .find(|(module, _)| module == "crate::core::managed")
        .expect("the managed value domain module should exist");
    let name = RAW_MUTATOR_GATEWAY
        .rsplit("::")
        .next()
        .expect("the gateway has a name");
    let signature = managed
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(item) if carries_name(&item.self_ty, "CoreValueFactory") => Some(item),
            _ => None,
        })
        .flat_map(|item| &item.items)
        .find_map(|item| match item {
            syn::ImplItem::Fn(function) if function.sig.ident == name => Some(&function.sig),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{RAW_MUTATOR_GATEWAY} should exist"));
    assert!(
        has_higher_ranked_callback(signature),
        "{RAW_MUTATOR_GATEWAY} must keep a higher-ranked callback so access cannot escape it"
    );
}

fn carries_name(value_type: &Type, name: &str) -> bool {
    matches!(value_type, Type::Path(path)
        if path.path.segments.last().is_some_and(|segment| segment.ident == name))
}

#[test]
fn production_never_opens_a_value_access_region_inside_another() {
    let modules = production_modules(Path::new(env!("CARGO_MANIFEST_DIR")));
    let reentries = production_region_openings(&modules)
        .into_iter()
        .filter(|opening| opening.nested || opening.carries_access)
        .collect::<Vec<_>>();
    assert!(
        reentries.is_empty(),
        "reuse the caller's access instead of opening another region: {reentries:#?}"
    );
}

#[test]
fn region_audit_distinguishes_access_authority_from_region_callbacks() {
    let syntax = syn::parse_file(
        r"
        fn runtime_input(access: &RuntimeValueAccess<'_>) {
            values.with_runtime_value_access(|_| ());
        }
        fn evaluation_input(access: Option<&EvaluationValueAccess<'_>>) {
            values.construct_runtime_value_root(|_| unit);
        }
        fn region_callback(operation: impl for<'scope> FnOnce(RuntimeValueAccess<'scope>)) {
            values.with_runtime_value_access(|_| ());
        }
        fn nested() {
            values.with_runtime_value_access(|_| {
                values.with_managed_values(|_| ());
            });
        }
        impl RuntimeValueAccess<'_> {
            fn reopen(&self) { self.values().with_runtime_value_access(|_| ()); }
        }
        #[cfg(test)]
        fn test_only(access: &RuntimeValueAccess<'_>) {
            values.with_runtime_value_access(|_| ());
        }
        ",
    )
    .expect("the region fixture should parse");
    let mut visitor = RegionVisitor::new("fixture");
    visitor.visit_file(&syntax);
    let reentries = visitor
        .openings
        .iter()
        .filter(|opening| opening.nested || opening.carries_access)
        .map(|opening| format!("{}: {}", opening.item, opening.opener))
        .collect::<Vec<_>>();

    assert_eq!(
        reentries,
        [
            "fixture::runtime_input: with_runtime_value_access",
            "fixture::evaluation_input: construct_runtime_value_root",
            "fixture::nested: with_managed_values",
            "fixture::RuntimeValueAccess::reopen: with_runtime_value_access",
        ]
    );
}

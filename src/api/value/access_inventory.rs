//! Audit: no authority-free escape between public, rooted, and core values.
//!
//! A bare core value is valid only inside a managed access region. Converting
//! a public value or runtime root to or from one without access authority
//! would let a core value outlive its region, or publish one the collector
//! does not see. The audit forbids the escape shapes everywhere under `src/`,
//! in production and test code alike.

use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{ExprCall, ExprMethodCall, ImplItemFn, ItemFn, ItemImpl};

/// Methods that would project a bare core value out of a public value.
const CORE_PROJECTIONS: &[&str] = &["as_core", "into_core"];

/// Value types that must not gain an authority-free constructor.
const ESCAPE_TYPES: &[&str] = &["RuntimeValueRoot", "Value"];

/// Constructors that would wrap a bare core value without access.
const BARE_CONSTRUCTORS: &[&str] = &["from_core", "from_runtime"];

struct EscapeVisitor<'path> {
    path: &'path str,
    impl_name: Option<String>,
    escapes: Vec<String>,
}

impl EscapeVisitor<'_> {
    fn record(&mut self, shape: String) {
        self.escapes.push(format!("{}: {shape}", self.path));
    }

    fn is_escape_constructor(&self, owner: &str, name: &str) -> bool {
        let owner = if owner == "Self" {
            self.impl_name.as_deref().unwrap_or_default()
        } else {
            owner
        };
        ESCAPE_TYPES.contains(&owner) && BARE_CONSTRUCTORS.contains(&name)
    }

    fn check_definition(&mut self, name: &syn::Ident) {
        let name = name.to_string();
        if CORE_PROJECTIONS.contains(&name.as_str())
            || self.is_escape_constructor("Self", name.as_str())
        {
            self.record(format!("defines `{name}`"));
        }
    }
}

impl<'ast> Visit<'ast> for EscapeVisitor<'_> {
    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let name = match node.self_ty.as_ref() {
            syn::Type::Path(path) => path.path.segments.last().map(|part| part.ident.to_string()),
            _ => None,
        };
        let prior = std::mem::replace(&mut self.impl_name, name);
        visit::visit_item_impl(self, node);
        self.impl_name = prior;
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if CORE_PROJECTIONS.iter().any(|name| node.sig.ident == name) {
            self.record(format!("defines `{}`", node.sig.ident));
        }
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.check_definition(&node.sig.ident);
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if CORE_PROJECTIONS.iter().any(|name| node.method == name) {
            self.record(format!("calls `.{}()`", node.method));
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(path) = node.func.as_ref()
            && let [.., owner, name] = path.path.segments.iter().collect::<Vec<_>>().as_slice()
            && self.is_escape_constructor(&owner.ident.to_string(), &name.ident.to_string())
        {
            self.record(format!("calls `{}::{}`", owner.ident, name.ident));
        }
        visit::visit_expr_call(self, node);
    }
}

fn rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("the source tree should be readable") {
        let path = entry.expect("a source entry should be readable").path();
        if path.is_dir() {
            rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

fn escapes_in(path: &str, syntax: &syn::File) -> Vec<String> {
    let mut visitor = EscapeVisitor {
        path,
        impl_name: None,
        escapes: Vec::new(),
    };
    visitor.visit_file(syntax);
    visitor.escapes
}

#[test]
fn no_code_escapes_between_public_and_bare_core_values() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&manifest.join("src"), &mut sources);
    sources.sort();

    let mut escapes = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a source should belong to this package")
            .display()
            .to_string();
        let source = fs::read_to_string(&path).expect("a source should be readable");
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{relative} should parse: {error}"));
        escapes.extend(escapes_in(&relative, &syntax));
    }
    assert!(
        escapes.is_empty(),
        "convert between public, rooted, and core values only through access: {escapes:#?}"
    );
}

#[test]
fn escape_audit_rejects_each_shape() {
    let syntax = syn::parse_file(
        r"
        impl Value {
            pub fn into_core(self) -> CoreValue { todo!() }
            fn from_runtime(value: CoreValue) -> Self { todo!() }
            fn wrap(value: CoreValue) -> Self { Self::from_runtime(value) }
        }
        impl ValueKind {
            fn from_core(access: &RuntimeValueAccess<'_>, value: &CoreValue) -> Self { todo!() }
        }
        fn project(value: &Value) { value.as_core(); RuntimeValueRoot::from_runtime(core); }
        ",
    )
    .expect("the escape fixture should parse");

    assert_eq!(
        escapes_in("fixture", &syntax),
        [
            "fixture: defines `into_core`",
            "fixture: defines `from_runtime`",
            "fixture: calls `Self::from_runtime`",
            "fixture: calls `.as_core()`",
            "fixture: calls `RuntimeValueRoot::from_runtime`",
        ]
    );
}

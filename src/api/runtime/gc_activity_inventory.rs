//! Audit: no heap in this crate collects automatically.
//!
//! A `CollectionPolicy::Automatic` heap may collect when a mutator region
//! opens. Glam instead collects only as explicit runtime maintenance, at a
//! boundary where no evaluation holds unrooted values. Every heap the crate
//! constructs, in production and test code alike, therefore selects
//! `CollectionPolicy::NoAuto`; `Heap::new()` and `Heap::default()` select
//! `Automatic`.

use std::fs;
use std::path::{Path, PathBuf};

use syn::ExprCall;
use syn::visit::{self, Visit};

#[derive(Default)]
struct HeapConstructionVisitor {
    automatic: Vec<String>,
}

fn ends_with(path: &syn::Path, suffix: [&str; 2]) -> bool {
    let segments = path.segments.iter().collect::<Vec<_>>();
    matches!(segments.as_slice(), [.., owner, name]
        if owner.ident == suffix[0] && name.ident == suffix[1])
}

impl<'ast> Visit<'ast> for HeapConstructionVisitor {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(function) = node.func.as_ref() {
            let function = &function.path;
            let no_auto = matches!(node.args.first(), Some(syn::Expr::Path(policy))
                if ends_with(&policy.path, ["CollectionPolicy", "NoAuto"]));
            for constructor in ["new", "default", "new_with_policy"] {
                if ends_with(function, ["Heap", constructor])
                    && !(constructor == "new_with_policy" && no_auto)
                {
                    self.automatic.push(format!("Heap::{constructor}"));
                }
            }
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

#[test]
fn every_heap_selects_no_automatic_collection() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    rust_sources(&manifest.join("src"), &mut sources);
    sources.sort();

    let mut automatic = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(manifest)
            .expect("a source should belong to this package")
            .display()
            .to_string();
        let source = fs::read_to_string(&path).expect("a source should be readable");
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("{relative} should parse: {error}"));
        let mut visitor = HeapConstructionVisitor::default();
        visitor.visit_file(&syntax);
        automatic.extend(
            visitor
                .automatic
                .into_iter()
                .map(|call| format!("{relative}: {call}")),
        );
    }
    assert!(
        automatic.is_empty(),
        "construct heaps with Heap::new_with_policy(CollectionPolicy::NoAuto): {automatic:#?}"
    );
}

#[test]
fn heap_audit_rejects_default_and_automatic_policies() {
    let syntax = syn::parse_file(
        r"
        fn heaps() {
            glam_gc::Heap::new_with_policy(glam_gc::CollectionPolicy::NoAuto);
            Heap::new_with_policy(CollectionPolicy::Automatic);
            Heap::new_with_policy(policy);
            Heap::new();
            Heap::default();
        }
        ",
    )
    .expect("the heap fixture should parse");
    let mut visitor = HeapConstructionVisitor::default();
    visitor.visit_file(&syntax);

    assert_eq!(
        visitor.automatic,
        [
            "Heap::new_with_policy",
            "Heap::new_with_policy",
            "Heap::new",
            "Heap::default",
        ]
    );
}

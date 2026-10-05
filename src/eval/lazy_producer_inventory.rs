//! Audit: a lazy task's route marker carries no producer state.
//!
//! `LazyTaskWork` only names which managed checkpoint a lazy task polls, or
//! grants the one-shot host-call invocation permit. Producer progress lives in
//! the checkpoint beneath the managed lazy, where the collector traces it and
//! a later route resumes it without replay. A field on a marker would hold
//! state outside that checkpoint.

use std::fs;
use std::path::Path;

#[test]
fn lazy_task_work_variants_carry_no_state() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/eval/value.rs");
    let source = fs::read_to_string(&path).expect("the lazy task source should be readable");
    let syntax = syn::parse_file(&source).expect("the lazy task source should parse");
    let work = syntax
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "LazyTaskWork" => Some(item),
            _ => None,
        })
        .expect("crate::eval::value::LazyTaskWork should exist");

    let stateful = work
        .variants
        .iter()
        .filter(|variant| !matches!(variant.fields, syn::Fields::Unit))
        .map(|variant| variant.ident.to_string())
        .collect::<Vec<_>>();
    assert!(
        stateful.is_empty(),
        "LazyTaskWork variants must stay state-free markers; keep producer state in the \
         managed lazy checkpoint: {stateful:?}"
    );
}

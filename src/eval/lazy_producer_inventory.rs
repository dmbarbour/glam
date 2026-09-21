//! W6G.1f.0 source-backed inventory of lazy producer checkpoint families.
//!
//! The policy table lives in the baseline review. This test makes additions
//! to `LazyTaskWork` fail closed until their ownership, tracing, and replay
//! behavior have been reviewed.

use std::fs;
use std::path::Path;

const EXPECTED_VARIANTS: &[&str] = &[
    // W6G.1f.3d carries no access progress: it marks the typed checkpoint
    // retained directly beneath the managed lazy.
    "AccessCheckpoint",
    // Migrated builtin families retain raw progress beneath the typed managed
    // checkpoint; this marker carries no duplicate producer state.
    "BuiltinCheckpoint",
    // Only the installer receives one transient invocation permit. Durable
    // before/after state belongs to the managed checkpoint.
    "HostCallCheckpoint",
    "HostCallInvoke",
    "ListEffectCheckpoint",
    // W6G.1f.3c retains the complete driver beneath the managed lazy.
    "NetWhnfCheckpoint",
    // W6G.1f.3e retains complete C3 and mix progress beneath the lazy.
    "ObjectFixpointCheckpoint",
    "Produce",
    "Whnf",
    // W6G.1f.2a carries no producer state: it marks that the canonical WHNF
    // state has moved into the owning managed lazy.
    "WhnfCheckpoint",
];

#[test]
fn lazy_task_work_families_are_exact() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest.join("src/eval/value.rs");
    let source = fs::read_to_string(&path).expect("lazy producer source should be readable");
    let syntax = syn::parse_file(&source).expect("lazy producer source should parse");
    let item = syntax
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "LazyTaskWork" => Some(item),
            _ => None,
        })
        .expect("LazyTaskWork must remain an explicit checkpoint-family enum");
    let mut variants = item
        .variants
        .iter()
        .map(|variant| variant.ident.to_string())
        .collect::<Vec<_>>();
    variants.sort();
    assert_eq!(variants, EXPECTED_VARIANTS);

    for variant in &item.variants {
        match variant.ident.to_string().as_str() {
            "Whnf" => {
                let syn::Fields::Unnamed(fields) = &variant.fields else {
                    panic!("the transitional WHNF route must retain one explicit payload")
                };
                assert_eq!(fields.unnamed.len(), 1);
                let syn::Type::Path(path) = &fields.unnamed[0].ty else {
                    panic!("the WHNF route payload must name its owner type")
                };
                assert_eq!(
                    path.path
                        .segments
                        .last()
                        .map(|segment| segment.ident.to_string()),
                    Some("WhnfComputation".to_owned()),
                );
            }
            _ => assert!(
                matches!(&variant.fields, syn::Fields::Unit),
                "{name} must remain a state-free route marker or transient permit",
                name = variant.ident,
            ),
        }
    }
}

#[test]
fn lazy_task_work_external_boundaries_remain_visible() {
    let mut source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/eval/value.rs"))
            .expect("lazy producer source should be readable");
    source.push_str(
        &fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/eval/lazy_checkpoint.rs"),
        )
        .expect("lazy checkpoint source should be readable"),
    );

    for boundary in [
        "ManagedHostCallCheckpointState::Invoking",
        "reserve_reflection_completion_activation",
        "ManagedPromiseRoot",
    ] {
        assert!(
            source.contains(boundary),
            "W6G.1f.0 boundary `{boundary}` disappeared; update the producer replay and tracing review"
        );
    }
}

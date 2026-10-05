//! Audit: foreground client demand is not background work.
//!
//! A client-demand record lives in the coordinator's foreground registry and
//! is driven by its client. Background executors select only `WorkKind`
//! records, so `WorkKind` must not gain a variant that carries client demand.

#[test]
fn background_work_kinds_exclude_client_demand() {
    let coordinator =
        syn::parse_file(include_str!("../coordinator.rs")).expect("coordinator source must parse");
    let work_kind = coordinator
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "WorkKind" => Some(item),
            _ => None,
        })
        .expect("crate::evaluation::coordinator::WorkKind should exist");

    let client_variants = work_kind
        .variants
        .iter()
        .filter(|variant| {
            let fields = variant
                .fields
                .iter()
                .map(|field| quote::ToTokens::to_token_stream(&field.ty).to_string());
            std::iter::once(variant.ident.to_string())
                .chain(fields)
                .any(|name| name.contains("ClientDemand"))
        })
        .map(|variant| variant.ident.to_string())
        .collect::<Vec<_>>();
    assert!(
        client_variants.is_empty(),
        "client demand stays in the foreground registry, never a background work kind: \
         {client_variants:?}"
    );
}

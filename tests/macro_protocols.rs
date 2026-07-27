use glam::{Assembler, Value};

fn compile(source: &str, module: &str) -> (Assembler, Value) {
    let assembler = Assembler::default();
    let built = assembler
        .module(["macro_protocol", module])
        .script("g", source)
        .build()
        .unwrap_or_else(|error| panic!("{module} macro protocol should compile: {error:#?}"));
    (assembler, built.into_value())
}

fn assert_protocol_failure(source: &str, module: &str, expected: &str) {
    let assembler = Assembler::default();
    let result = assembler
        .module(["invalid_macro_protocol", module])
        .script("g", source)
        .build();
    let error = match result {
        Ok(_) => panic!("{module} unexpectedly accepted malformed macro protocol input"),
        Err(error) => error,
    };
    assert!(
        error
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.message().contains(expected)),
        "{module} diagnostics did not contain `{expected}`: {:#?}",
        error.diagnostics(),
    );
}

#[test]
fn recursive_logic_macro_compiles_nested_rules_and_runs_the_query() {
    let (assembler, module) = compile(include_str!("../samples/contracts/macros/logic.g"), "logic");

    assert_eq!(
        assembler
            .binary_at(&module, "asm.result")
            .expect("logic query should produce text solutions"),
        b"bob,carol".as_slice(),
    );
}

#[test]
fn rewrite_rule_macro_replays_a_balanced_source_fragment() {
    let (assembler, module) = compile(
        include_str!("../samples/contracts/macros/rewrite_rules.g"),
        "rewrite_rules",
    );

    assert_eq!(
        assembler
            .binary_at(&module, "asm.result")
            .expect("selected rewrite should produce text"),
        b"rewrite-ok".as_slice(),
    );
}

#[test]
fn packet_macro_compiles_nested_layout_into_a_codec_object() {
    let (assembler, module) = compile(
        include_str!("../samples/contracts/macros/packet.g"),
        "packet",
    );

    assert_eq!(
        assembler
            .binary_at(&module, "asm.result")
            .expect("packet encoder should produce a binary"),
        [1, 0, 2, b'H', b'i', 2, b'O', b'K'].as_slice(),
    );
    assert_eq!(
        assembler
            .binary_at(&module, "decoded_payload")
            .expect("packet decoder should recover the payload"),
        b"Hi".as_slice(),
    );
    assert_eq!(
        assembler
            .binary_at(&module, "decoded_variant")
            .expect("packet decoder should recover the selected variant"),
        b"OK".as_slice(),
    );
}

#[test]
fn protocol_macros_retain_their_active_case_explanations() {
    let logic = include_str!("../samples/contracts/macros/logic.g").replace(
        "fact parent \"alice\" \"bob\"",
        "unknown parent \"alice\" \"bob\"",
    );
    assert_protocol_failure(&logic, "logic", "a logic fact");

    let rules = include_str!("../samples/contracts/macros/rewrite_rules.g").replace(
        "  (true,$yes:group,$no:group)=>$yes\n  (false,$yes:group,$no:group)=>$no",
        "  (maybe,$yes:group,$no:group)=>$yes\n  (false,$yes:group,$no:group)=>$no",
    );
    assert_protocol_failure(&rules, "rewrite_rules", "the true rewrite rule");

    let packet = include_str!("../samples/contracts/macros/packet.g")
        .replace("field version u8", "field version word");
    assert_protocol_failure(&packet, "packet", "a packet field type");
}

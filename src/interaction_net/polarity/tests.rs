use std::sync::Arc;

use super::*;
use crate::interaction_net::model::Wire;
use crate::interaction_net::{NetBuildError, NetBuilder};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Signed;

impl NetSpecialization for Signed {
    type Data = ();
    type Operator = ();
    type RuntimeSource = ();
    type WaitToken = ();
    type StuckReason = ();
    type CallableCheckpoint = ();
}

fn finish_unchecked(builder: NetBuilder<Signed>, exposed: Port) -> InteractionNet<Signed> {
    builder
        .unpolarized_for_test()
        .try_finish(exposed)
        .expect("the fixture is a well-formed template")
}

#[test]
fn data_and_identity_application_are_polarized() {
    let mut builder = NetBuilder::new();
    let data = builder.data(());
    assert_eq!(check_template(&finish_unchecked(builder, data)), Ok(()));

    // (\x -> x) ()
    let mut builder = NetBuilder::new();
    let [function, argument, result] = builder.function_bind();
    builder.wire(argument, result);
    let [application, applied, applied_result] = builder.bind();
    builder.wire(application, function);
    let data = builder.data(());
    builder.wire(applied, data);
    assert_eq!(
        check_template(&finish_unchecked(builder, applied_result)),
        Ok(())
    );
}

#[test]
fn copies_and_user_merges_are_polarized() {
    // \x -> x x
    let mut builder = NetBuilder::new();
    let [function, argument, result] = builder.function_bind();
    let copy = builder.copy(2);
    builder.wire(argument, copy.input);
    let [application, applied, applied_result] = builder.bind();
    builder.wire(application, copy.outputs[0]);
    builder.wire(applied, copy.outputs[1]);
    builder.wire(applied_result, result);
    assert_eq!(check_template(&finish_unchecked(builder, function)), Ok(()));

    // A merge fan superposes two values for one consumer.
    let mut builder = NetBuilder::new();
    let merge = builder.push_fan();
    let [input, output] = builder.operator(());
    builder.wire(Port::principal(merge), input);
    let left = builder.data(());
    let right = builder.data(());
    builder.wire(Port::auxiliary(merge, 1), left);
    builder.wire(Port::auxiliary(merge, 2), right);
    assert_eq!(check_template(&finish_unchecked(builder, output)), Ok(()));
}

#[test]
fn an_exposed_eraser_is_an_error_value_not_a_conflict() {
    let mut builder = NetBuilder::new();
    let erase = builder.copy(0).input;
    assert_eq!(check_template(&finish_unchecked(builder, erase)), Ok(()));
}

#[test]
fn data_wired_to_data_conflicts_at_its_wire() {
    let mut builder = NetBuilder::new();
    let left = builder.data(());
    let right = builder.data(());
    builder.wire(left, right);
    let exposed = builder.data(());
    assert_eq!(
        check_template(&finish_unchecked(builder, exposed)),
        Err(PolarityViolation::Conflict {
            closing: PolarityRule::Wire(left, right),
            forced_by: vec![
                PolarityRule::Node(left, NodeRule::DataProvides),
                PolarityRule::Node(right, NodeRule::DataProvides),
            ],
        })
    );
}

#[test]
fn a_function_built_in_application_order_conflicts() {
    // \x -> () built with an application-role bind: its body result is
    // wired to the auxiliary that provides, so data meets data.
    let mut builder = NetBuilder::new();
    let [function, argument, result] = builder.bind();
    let data = builder.data(());
    builder.wire(result, data);
    let erase = builder.copy(0).input;
    builder.wire(argument, erase);
    assert!(matches!(
        check_template(&finish_unchecked(builder, function)),
        Err(PolarityViolation::Conflict {
            closing: PolarityRule::Wire(..),
            ..
        })
    ));
}

#[test]
fn a_consuming_exposed_port_conflicts() {
    let mut builder = NetBuilder::new();
    let [input, output] = builder.operator(());
    let erase = builder.copy(0).input;
    builder.wire(output, erase);
    assert_eq!(
        check_template(&finish_unchecked(builder, input)),
        Err(PolarityViolation::Conflict {
            closing: PolarityRule::Exposed(input),
            forced_by: vec![PolarityRule::Node(input, NodeRule::OperatorConsumesInput)],
        })
    );
}

#[test]
fn a_component_unreachable_from_the_exposed_port_is_disconnected() {
    let mut builder = NetBuilder::new();
    let exposed = builder.data(());
    let garbage = builder.data(());
    let erase = builder.copy(0).input;
    builder.wire(garbage, erase);
    assert_eq!(
        check_template(&finish_unchecked(builder, exposed)),
        Err(PolarityViolation::Disconnected(garbage.node()))
    );
}

#[test]
fn finishing_rejects_an_unpolarized_template_and_names_the_forcing_rules() {
    let mut builder = NetBuilder::<Signed>::new();
    let left = builder.data(());
    let right = builder.data(());
    builder.wire(left, right);
    let exposed = builder.data(());
    let Err(NetBuildError::Polarity(violation)) = builder.try_finish(exposed) else {
        panic!("an unpolarized template must not finish")
    };
    let message = violation.to_string();
    assert!(message.starts_with("interaction net is not polarized: the wire between"));
    assert!(message.contains("provides its data"), "{message}");

    let mut builder = NetBuilder::<Signed>::new();
    let exposed = builder.data(());
    let garbage = builder.data(());
    let erase = builder.copy(0).input;
    builder.wire(garbage, erase);
    assert_eq!(
        builder.try_finish(exposed).err(),
        Some(NetBuildError::Polarity(PolarityViolation::Disconnected(
            garbage.node()
        )))
    );
}

#[test]
fn a_copy_tunnel_passes_its_sign_through() {
    // .data -> .copy 1 -> exposed: the tunnel's output provides.
    let mut builder = NetBuilder::<Signed>::new();
    let data = builder.data(());
    let tunnel = builder.copy(1);
    builder.wire(data, tunnel.input);
    assert!(builder.try_finish(tunnel.outputs[0]).is_ok());

    // An operator's input exposed through a tunnel still consumes.
    let mut builder = NetBuilder::<Signed>::new();
    let [input, output] = builder.operator(());
    let erase = builder.copy(0).input;
    builder.wire(output, erase);
    let tunnel = builder.copy(1);
    builder.wire(input, tunnel.input);
    assert!(matches!(
        builder.try_finish(tunnel.outputs[0]),
        Err(NetBuildError::Polarity(PolarityViolation::Conflict {
            closing: PolarityRule::Exposed(_),
            ..
        }))
    ));
}

#[test]
fn a_long_fan_chain_checks_without_deep_recursion() {
    // Each fan's left branch feeds the next fan's principal, so the sign
    // classes join into one chain as long as the net. It is built directly
    // because the builder's per-wire duplicate check is quadratic.
    const FANS: usize = 100_000;
    let mut nodes = Vec::with_capacity(2 * FANS + 1);
    let mut wires = Vec::with_capacity(2 * FANS);
    for index in 0..FANS {
        nodes.push(Node::Fan {
            site: crate::interaction_net::model::FanSite(index as u64),
        });
    }
    for index in 0..FANS {
        let fan = NodeId::from_index(index);
        let erase = NodeId::from_index(nodes.len());
        nodes.push(Node::Erase);
        wires.push(Wire {
            left: Port::auxiliary(fan, 2),
            right: Port::principal(erase),
        });
        let next = if index + 1 < FANS {
            Port::principal(NodeId::from_index(index + 1))
        } else {
            let erase = NodeId::from_index(nodes.len());
            nodes.push(Node::Erase);
            Port::principal(erase)
        };
        wires.push(Wire {
            left: Port::auxiliary(fan, 1),
            right: next,
        });
    }
    let net = InteractionNet::<Signed> {
        nodes: Arc::from(nodes),
        wires: Arc::from(wires),
        exposed: Port::principal(NodeId::from_index(0)),
        polarized: true,
    };
    assert_eq!(check_template(&net), Ok(()));
}

#[test]
fn sample_nets_are_polarized() {
    use crate::api::{Assembler, TestValueFacade};

    // Compiling a sample lowers its functions into net templates, and
    // assembling it builds the evaluator's own templates on demand. The
    // test-build hook in `NetBuilder::try_finish` checks every one.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut programs = std::fs::read_dir(root.join("samples/hello"))
        .expect("the hello samples should be readable")
        .map(|entry| vec![entry.expect("a sample entry should be readable").path()])
        .filter(|files| {
            files[0]
                .extension()
                .is_some_and(|extension| extension == "g")
        })
        .collect::<Vec<_>>();
    programs.sort();
    assert!(!programs.is_empty(), "the hello samples should exist");
    // The assembly samples are one program: an override layered on a base.
    programs.push(vec![
        root.join("samples/assembly/mixin_override.g"),
        root.join("samples/assembly/mixin_base.g"),
    ]);
    for files in programs {
        let assembler = Assembler::default();
        let module = files
            .iter()
            .fold(assembler.module(["polarity_sample"]), |module, file| {
                module.file(file)
            })
            .build()
            .unwrap_or_else(|error| panic!("{files:?} should compile: {error}"));
        let result = assembler
            .get(module.value(), "asm.result")
            .unwrap_or_else(|error| panic!("{files:?} should assemble: {error}"));
        assembler
            .to_binary(&result)
            .unwrap_or_else(|error| panic!("{files:?} should produce bytes: {error}"));
    }
}

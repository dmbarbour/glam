//! The runtime's own diagnostics reach the root's diagnostics input
//! (`machines-communicate-through-wired-ports`).

use super::*;
use crate::diagnostic::Severity;

#[test]
fn the_runtime_s_diagnostics_join_the_root_s_diagnostics_input() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let bus = rt.assembler().diagnostic_bus();
    for message in ["first", "second"] {
        bus.publish(Diagnostic::new(&values, Severity::Warning, message))
            .expect("the diagnostic should publish");
    }
    let decoded = rt
        .run((), |root| {
            root.read(DIAGNOSTICS.clone(), 2)?;
            let second = root.pop()?;
            let first = root.pop()?;
            Ok([first, second].map(|value| {
                Diagnostic::from_transport_value(&values, &value)
                    .expect("the port holds transport values")
            }))
        })
        .expect("the root reads its diagnostics input");
    assert_eq!(
        decoded.each_ref().map(|diagnostic| diagnostic.message()),
        ["first", "second"],
        "in the bus's order"
    );
    assert_eq!(decoded[0].severity(), Severity::Warning);
    assert_eq!(bus.counts().warnings(), 2, "the bus still counts them");
}

#[test]
fn a_runtime_without_the_default_wiring_keeps_its_diagnostics_on_the_bus() {
    let mut rt = Runtime::open(RuntimeConfig::new().diagnostics_wiring(DiagnosticsWiring::None))
        .expect("a runtime should open");
    let values = rt.assembler().values();
    rt.assembler()
        .diagnostic_bus()
        .publish(Diagnostic::new(&values, Severity::Warning, "kept"))
        .expect("the diagnostic should publish");
    let held = rt
        .run((), |root| Ok(root.available(DIAGNOSTICS.clone())))
        .expect("a turn");
    assert_eq!(held, 0);
}

#[test]
fn a_read_of_the_root_s_diagnostics_waits_for_one_to_arrive() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let bus = rt.assembler().diagnostic_bus().clone();
    let message = rt
        .run((), |root| {
            let publisher = {
                let values = values.clone();
                std::thread::spawn(move || {
                    bus.publish(Diagnostic::new(&values, Severity::Info, "late"))
                        .expect("the diagnostic should publish");
                })
            };
            root.read(DIAGNOSTICS.clone(), 1)?;
            publisher.join().expect("the publisher should finish");
            let value = root.pop()?;
            Ok(Diagnostic::from_transport_value(&values, &value)?
                .message()
                .to_owned())
        })
        .expect("the read waits for the diagnostic");
    assert_eq!(message, "late");
}

#[test]
fn the_runtime_s_diagnostics_follow_the_root_s_wiring() {
    let mut rt = Runtime::open(RuntimeConfig::new()).expect("a runtime should open");
    let values = rt.assembler().values();
    let bus = rt.assembler().diagnostic_bus();
    let diagnostics = || DIAGNOSTICS.clone();
    rt.run((), |root| {
        root.wire(
            Endpoint::input(diagnostics()),
            Endpoint::child("sink", diagnostics()),
        )
    })
    .expect("the root wires its input on");
    bus.publish(Diagnostic::new(&values, Severity::Warning, "looped"))
        .expect("the diagnostic should publish");
    let held = |rt: &mut Runtime, path: MachinePath| {
        rt.run(path, |m| Ok(m.available(diagnostics())))
            .expect("a turn")
    };
    assert_eq!(
        (held(&mut rt, ().into()), held(&mut rt, "sink".into())),
        (1, 0),
        "the sink's default pass-through loops back, so it waits at the root"
    );

    rt.run("sink", |sink| {
        sink.unwire(
            Endpoint::input(diagnostics()),
            Endpoint::output(diagnostics()),
        )
    })
    .expect("the sink keeps what reaches it");
    bus.publish(Diagnostic::new(&values, Severity::Warning, "routed"))
        .expect("the diagnostic should publish");
    assert_eq!(
        (held(&mut rt, ().into()), held(&mut rt, "sink".into())),
        (1, 1),
        "it passes through the root to the sink"
    );
}

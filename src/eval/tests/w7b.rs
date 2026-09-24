//! Deterministic small-stack closure fixtures for Phase W7B.
//!
//! These tests force semantic depth and suspension order. Repeating a test
//! under an uncontrolled scheduler is deliberately not used as evidence.

use std::hint::black_box;
use std::process::{Command, Stdio};
use std::thread;

use crate::core::{CoreValueFactory, Value};
use crate::eval::whnf::{
    RegionalWhnfDrive, RegionalWhnfStep, RegionalWhnfWork, WhnfStepBudget, drive_regional,
};
use crate::evaluation::{EvalContext, EvaluationPollContext};
use crate::number::Number;
use crate::runtime::{RuntimeIds, allocate_evaluation_runtime_id};

const SMALL_STACK_BYTES: usize = 512 * 1024;
const SEMANTIC_DEPTH: usize = 4_096;
const RECURSIVE_CONTROL_ENV: &str = "GLAM_W7B_RECURSIVE_CONTROL";

fn on_small_stack<T: Send + 'static>(
    name: &'static str,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T {
    thread::Builder::new()
        .name(name.into())
        .stack_size(SMALL_STACK_BYTES)
        .spawn(operation)
        .expect("the W7B small-stack witness thread should spawn")
        .join()
        .expect("the W7B small-stack witness must not panic")
}

#[inline(never)]
fn recursive_control(depth: usize) -> usize {
    // Keep the frame observably source-shaped and large enough that the
    // selected depth cannot accidentally fit the explicit W7B stack.
    let frame = [depth as u8; 1_024];
    black_box(&frame);
    if depth == 0 {
        black_box(frame[0] as usize)
    } else {
        let child = recursive_control(depth - 1);
        black_box(&frame);
        child.wrapping_add(black_box(frame[0] as usize))
    }
}

#[test]
fn recursive_depth_control_child() {
    if std::env::var_os(RECURSIVE_CONTROL_ENV).is_none() {
        return;
    }
    on_small_stack("w7b-recursive-control", || {
        black_box(recursive_control(SEMANTIC_DEPTH));
    });
}

#[test]
fn selected_depth_overflows_an_equivalent_recursive_fixture() {
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let test_name = format!("{module}::recursive_depth_control_child");
    let status = Command::new(std::env::current_exe().expect("the test executable should exist"))
        .args(["--exact", test_name.as_str(), "--nocapture"])
        .env(RECURSIVE_CONTROL_ENV, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the isolated recursive control should launch");
    assert!(
        !status.success(),
        "the W7B semantic depth must exceed the equivalent recursive small-stack fixture"
    );
}

#[test]
fn explicit_whnf_worklist_completes_at_the_recursive_control_depth() {
    on_small_stack("w7b-explicit-whnf-worklist", || {
        let values = CoreValueFactory::new(allocate_evaluation_runtime_id(), RuntimeIds::new());
        let context = EvalContext::isolated(values);
        let poll = EvaluationPollContext::for_context(&context);
        poll.with_value_access(&context, |access| {
            let expected = Value::Number(Number::from_usize(SEMANTIC_DEPTH));
            let work =
                RegionalWhnfWork::from_focus(&access, access.values().duplicate_value(&expected));
            let mut transitions = 0;
            let mut budget = WhnfStepBudget::new(SEMANTIC_DEPTH + 1);
            let outcome = drive_regional(&access, work, &mut budget, |_access, _work| {
                if transitions == SEMANTIC_DEPTH {
                    RegionalWhnfStep::Ready(Value::Number(Number::from_usize(SEMANTIC_DEPTH)))
                } else {
                    transitions += 1;
                    RegionalWhnfStep::Delegate(Value::Number(Number::from_usize(SEMANTIC_DEPTH)))
                }
            });
            let RegionalWhnfDrive::Ready(actual) = outcome else {
                panic!("the bounded explicit worklist should complete")
            };
            assert_eq!(actual, expected);
            assert_eq!(transitions, SEMANTIC_DEPTH);
            assert_eq!(budget.remaining(), 0);
        });
    });
}

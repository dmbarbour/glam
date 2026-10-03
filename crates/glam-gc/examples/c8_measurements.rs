//! Reproducible, threshold-free C8 collector measurements.
//!
//! Invoke this through `scripts/capture-c8-measurements.sh` so the revision,
//! release profile, and host context are recorded with the observations.

use std::process::Command;
use std::time::{Duration, Instant};

use glam_gc::{CollectionReport, Heap, HeapMetrics};

const SCHEMA: &str = "glam-gc-c8-v1";

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                write!(escaped, "\\u{:04x}", character as u32)
                    .expect("writing to a String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    match Command::new(program).args(arguments).output() {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        Ok(output) => format!("{program} exited with {}", output.status),
        Err(error) => format!("{program} unavailable: {error}"),
    }
}

fn emit_context() {
    let revision =
        std::env::var("GLAM_GC_MEASUREMENT_REVISION").unwrap_or_else(|_| "unknown".to_owned());
    let rustc = command_output("rustc", &["-Vv"]);
    let host = command_output("uname", &["-a"]);
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    println!(
        "{{\"schema\":{schema},\"kind\":\"context\",\"revision\":{revision},\"rustc\":{rustc},\"host\":{host},\"workers\":{workers},\"debug_assertions\":{debug_assertions}}}",
        schema = json_string(SCHEMA),
        revision = json_string(&revision),
        rustc = json_string(&rustc),
        host = json_string(&host),
        debug_assertions = cfg!(debug_assertions),
    );
}

fn nanos(duration: Duration) -> u128 {
    duration.as_nanos()
}

fn emit_workload(
    name: &str,
    operations: usize,
    elapsed: Duration,
    report: Option<CollectionReport>,
    metrics: HeapMetrics,
) {
    let (
        epoch,
        traced,
        marked,
        reclaimed,
        finalized,
        reclaimed_runs,
        worklist_len,
        worklist_capacity,
        pause_ns,
        trace_ns,
        sweep_ns,
        finalization_ns,
    ) = report.map_or((0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0), |report| {
        (
            report.epoch(),
            report.traced_objects(),
            report.marked_slots(),
            report.reclaimed_slots(),
            report.finalized_slots(),
            report.reclaimed_runs(),
            report.peak_object_worklist_len(),
            report.peak_object_worklist_capacity(),
            nanos(report.pause_duration()),
            nanos(report.trace_duration()),
            nanos(report.sweep_duration()),
            nanos(report.finalization_duration()),
        )
    });
    println!(
        "{{\"schema\":{schema},\"kind\":\"workload\",\"name\":{name},\"operations\":{operations},\"elapsed_ns\":{elapsed_ns},\"epoch\":{epoch},\"traced_objects\":{traced},\"marked_slots\":{marked},\"reclaimed_slots\":{reclaimed},\"finalized_slots\":{finalized},\"reclaimed_runs\":{reclaimed_runs},\"peak_worklist_len\":{worklist_len},\"peak_worklist_capacity\":{worklist_capacity},\"pause_ns\":{pause_ns},\"trace_ns\":{trace_ns},\"sweep_ns\":{sweep_ns},\"finalization_ns\":{finalization_ns},\"arena_chunks\":{arena_chunks},\"assigned_runs\":{assigned_runs},\"free_runs\":{free_runs},\"assigned_slot_capacity\":{slot_capacity},\"allocated_slots\":{allocated_slots},\"partial_run_free_slots\":{partial_free},\"cache_hits\":{cache_hits},\"cache_misses\":{cache_misses}}}",
        schema = json_string(SCHEMA),
        name = json_string(name),
        elapsed_ns = nanos(elapsed),
        arena_chunks = metrics.arena_chunks(),
        assigned_runs = metrics.assigned_runs(),
        free_runs = metrics.free_runs(),
        slot_capacity = metrics.assigned_slot_capacity(),
        allocated_slots = metrics.allocated_slots(),
        partial_free = metrics.partial_run_free_slots(),
        cache_hits = metrics.class_cache_hits(),
        cache_misses = metrics.class_cache_misses(),
    );
}

fn main() {
    let selections = std::env::args().skip(1).collect::<Vec<_>>();
    if selections.iter().any(|argument| argument == "--help") {
        println!("usage: c8_measurements [all]");
        return;
    }
    if !selections.is_empty() && selections != ["all"] {
        eprintln!("unknown measurement selection: {}", selections.join(" "));
        std::process::exit(2);
    }

    emit_context();

    // C8B.1a establishes the schema and runner. C8B.1b adds the workloads;
    // retaining one empty observation makes an incomplete harness explicit.
    let heap = Heap::new();
    let started = Instant::now();
    let report = heap
        .collect_full()
        .expect("empty measurement collection failed");
    emit_workload(
        "empty_collection",
        0,
        started.elapsed(),
        Some(report),
        heap.metrics(),
    );
}

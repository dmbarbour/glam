//! Reproducible, threshold-free C8 collector measurements.
//!
//! Invoke this through `scripts/capture-c8-measurements.sh` so the revision,
//! release profile, and host context are recorded with the observations.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use glam_gc::{CollectionPolicy, CollectionReport, Gc, Heap, HeapMetrics, Trace, Visitor};

const SCHEMA: &str = "glam-gc-c8-v1";
const ALLOCATION_COUNT: usize = 500_000;
const GRAPH_COUNT: usize = 100_000;
const FINALIZATION_COUNT: usize = 100_000;
const RECLAMATION_COUNT: usize = 100_000;

struct SmallValue {
    _payload: u64,
}

// SAFETY: `SmallValue` contains no managed edge.
unsafe impl Trace for SmallValue {
    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

struct GraphNode {
    edges: Vec<Gc<GraphNode>>,
}

// SAFETY: every managed edge is stored in `edges` and reported once.
unsafe impl Trace for GraphNode {
    fn trace(&self, visitor: &mut Visitor<'_>) {
        for edge in &self.edges {
            visitor.visit(edge);
        }
    }
}

struct DropCounter {
    drops: Arc<AtomicUsize>,
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: `DropCounter` contains host-owned counting state and no managed edge.
unsafe impl Trace for DropCounter {
    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

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

fn allocator_workload() {
    let heap = Heap::new_with_policy(CollectionPolicy::NoAuto);
    let started = Instant::now();
    heap.with_mutator(|mutator| {
        let allocator = mutator
            .allocator::<SmallValue>()
            .expect("small-value layout is supported");
        for index in 0..ALLOCATION_COUNT {
            let _ = allocator.alloc(SmallValue {
                _payload: std::hint::black_box(index as u64),
            });
        }
    });
    let elapsed = started.elapsed();
    let metrics = heap.metrics();
    assert_eq!(metrics.allocated_slots(), ALLOCATION_COUNT);
    emit_workload("allocator", ALLOCATION_COUNT, elapsed, None, metrics);
}

fn deep_trace_workload() {
    let heap = Heap::new_with_policy(CollectionPolicy::NoAuto);
    let root = heap.with_mutator(|mutator| {
        let allocator = mutator
            .allocator::<GraphNode>()
            .expect("graph-node layout is supported");
        let mut head = allocator.alloc(GraphNode { edges: Vec::new() });
        for _ in 1..GRAPH_COUNT {
            head = allocator.alloc(GraphNode { edges: vec![head] });
        }
        mutator.root(head)
    });
    let started = Instant::now();
    let report = heap.collect_full().expect("deep trace collection failed");
    let elapsed = started.elapsed();
    assert_eq!(report.traced_objects(), GRAPH_COUNT);
    assert_eq!(report.marked_slots(), GRAPH_COUNT);
    emit_workload(
        "trace_deep",
        GRAPH_COUNT,
        elapsed,
        Some(report),
        heap.metrics(),
    );
    drop(root);
}

fn wide_trace_workload() {
    let heap = Heap::new_with_policy(CollectionPolicy::NoAuto);
    let root = heap.with_mutator(|mutator| {
        let allocator = mutator
            .allocator::<GraphNode>()
            .expect("graph-node layout is supported");
        let edges = (0..GRAPH_COUNT)
            .map(|_| allocator.alloc(GraphNode { edges: Vec::new() }))
            .collect::<Vec<_>>();
        mutator.root(allocator.alloc(GraphNode { edges }))
    });
    let started = Instant::now();
    let report = heap.collect_full().expect("wide trace collection failed");
    let elapsed = started.elapsed();
    assert_eq!(report.traced_objects(), GRAPH_COUNT + 1);
    assert_eq!(report.marked_slots(), GRAPH_COUNT + 1);
    assert_eq!(report.peak_object_worklist_len(), GRAPH_COUNT);
    emit_workload(
        "trace_wide",
        GRAPH_COUNT,
        elapsed,
        Some(report),
        heap.metrics(),
    );
    drop(root);
}

fn finalization_workload() {
    let heap = Heap::new_with_policy(CollectionPolicy::NoAuto);
    let drops = Arc::new(AtomicUsize::new(0));
    heap.with_mutator(|mutator| {
        let allocator = mutator
            .allocator::<DropCounter>()
            .expect("drop-counter layout is supported");
        for _ in 0..FINALIZATION_COUNT {
            let _ = allocator.alloc(DropCounter {
                drops: Arc::clone(&drops),
            });
        }
    });
    let started = Instant::now();
    let report = heap
        .collect_full()
        .expect("finalization measurement collection failed");
    let elapsed = started.elapsed();
    assert_eq!(report.finalized_slots(), FINALIZATION_COUNT);
    assert_eq!(drops.load(Ordering::Relaxed), FINALIZATION_COUNT);
    emit_workload(
        "finalization",
        FINALIZATION_COUNT,
        elapsed,
        Some(report),
        heap.metrics(),
    );
}

fn reclamation_workload() {
    let heap = Heap::new_with_policy(CollectionPolicy::NoAuto);
    heap.with_mutator(|mutator| {
        let allocator = mutator
            .allocator::<SmallValue>()
            .expect("small-value layout is supported");
        for index in 0..RECLAMATION_COUNT {
            let _ = allocator.alloc(SmallValue {
                _payload: index as u64,
            });
        }
    });
    let started = Instant::now();
    let report = heap
        .collect_full()
        .expect("reclamation measurement collection failed");
    let elapsed = started.elapsed();
    assert_eq!(report.reclaimed_slots(), RECLAMATION_COUNT);
    assert_eq!(heap.metrics().allocated_slots(), 0);
    emit_workload(
        "reclamation",
        RECLAMATION_COUNT,
        elapsed,
        Some(report),
        heap.metrics(),
    );
}

fn selected(selections: &[String], name: &str) -> bool {
    selections.is_empty()
        || selections.iter().any(|selection| selection == "all")
        || selections.iter().any(|selection| selection == name)
}

fn main() {
    let selections = std::env::args().skip(1).collect::<Vec<_>>();
    if selections.iter().any(|argument| argument == "--help") {
        println!(
            "usage: c8_measurements [all|allocator|trace_deep|trace_wide|finalization|reclamation]..."
        );
        return;
    }
    const WORKLOADS: &[&str] = &[
        "all",
        "allocator",
        "trace_deep",
        "trace_wide",
        "finalization",
        "reclamation",
    ];
    if selections
        .iter()
        .any(|selection| !WORKLOADS.contains(&selection.as_str()))
    {
        eprintln!("unknown measurement selection: {}", selections.join(" "));
        std::process::exit(2);
    }

    emit_context();
    if selected(&selections, "allocator") {
        allocator_workload();
    }
    if selected(&selections, "trace_deep") {
        deep_trace_workload();
    }
    if selected(&selections, "trace_wide") {
        wide_trace_workload();
    }
    if selected(&selections, "finalization") {
        finalization_workload();
    }
    if selected(&selections, "reclamation") {
        reclamation_workload();
    }
}

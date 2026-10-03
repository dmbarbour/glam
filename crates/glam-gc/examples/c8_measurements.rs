//! Reproducible, threshold-free C8 collector measurements.
//!
//! Invoke this through `scripts/capture-c8-measurements.sh` so the revision,
//! release profile, and host context are recorded with the observations.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use glam_gc::{CollectionPolicy, CollectionReport, Gc, Heap, HeapMetrics, Trace, Visitor};
#[cfg(feature = "deterministic-test-hooks")]
use glam_gc::{
    GeometryMeasurement, geometry_measurement, is_rootable_for_measurement, trace_work_item_bytes,
};

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

struct RequestedStride<const STRIDE: usize> {
    _payload: u64,
}

struct SparseFinalizer {
    id: usize,
    countdown: Arc<AtomicUsize>,
    dropped: Arc<[AtomicBool]>,
}

impl Drop for SparseFinalizer {
    fn drop(&mut self) {
        self.dropped[self.id].store(true, Ordering::Relaxed);
        if self.countdown.fetch_sub(1, Ordering::Relaxed) == 1 {
            panic!("injected C8 sparse-finalizer panic");
        }
    }
}

// SAFETY: `SparseFinalizer` contains host-owned measurement state and no
// managed edge. The large requested stride creates many sparse finalizer runs.
unsafe impl Trace for SparseFinalizer {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(1024);

    fn trace(&self, _visitor: &mut Visitor<'_>) {}
}

// SAFETY: `RequestedStride` contains no managed edge. Each instantiation uses
// its const parameter as a total slot-extent request for measurement only.
unsafe impl<const STRIDE: usize> Trace for RequestedStride<STRIDE> {
    const REQUESTED_SLOT_SIZE: Option<usize> = Some(STRIDE);

    fn trace(&self, _visitor: &mut Visitor<'_>) {}
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

#[cfg(feature = "deterministic-test-hooks")]
fn peak_worklist_bytes(capacity: usize) -> usize {
    capacity
        .checked_mul(trace_work_item_bytes())
        .expect("worklist byte measurement overflowed")
}

#[cfg(not(feature = "deterministic-test-hooks"))]
fn peak_worklist_bytes(_capacity: usize) -> usize {
    0
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
    let worklist_bytes = peak_worklist_bytes(worklist_capacity);
    println!(
        "{{\"schema\":{schema},\"kind\":\"workload\",\"name\":{name},\"operations\":{operations},\"elapsed_ns\":{elapsed_ns},\"epoch\":{epoch},\"traced_objects\":{traced},\"marked_slots\":{marked},\"reclaimed_slots\":{reclaimed},\"finalized_slots\":{finalized},\"reclaimed_runs\":{reclaimed_runs},\"peak_worklist_len\":{worklist_len},\"peak_worklist_capacity\":{worklist_capacity},\"peak_worklist_bytes\":{worklist_bytes},\"pause_ns\":{pause_ns},\"trace_ns\":{trace_ns},\"sweep_ns\":{sweep_ns},\"finalization_ns\":{finalization_ns},\"arena_chunks\":{arena_chunks},\"assigned_runs\":{assigned_runs},\"free_runs\":{free_runs},\"assigned_slot_capacity\":{slot_capacity},\"allocated_slots\":{allocated_slots},\"partial_run_free_slots\":{partial_free},\"cache_hits\":{cache_hits},\"cache_misses\":{cache_misses}}}",
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

#[cfg(feature = "deterministic-test-hooks")]
fn emit_geometry(requested_stride: usize, geometry: GeometryMeasurement) {
    println!(
        "{{\"schema\":{schema},\"kind\":\"geometry\",\"requested_stride\":{requested_stride},\"run_bytes\":{run_bytes},\"header_bytes\":{header_bytes},\"slot_stride\":{slot_stride},\"slot_count\":{slot_count},\"allocation_bitmap_bytes\":{allocation_bitmap_bytes},\"lease_bitmap_bytes\":{lease_bitmap_bytes},\"mark_bitmap_bytes\":{mark_bitmap_bytes},\"bitmap_padding_bytes\":{bitmap_padding_bytes},\"payload_bytes\":{payload_bytes},\"tail_slack_bytes\":{tail_slack_bytes}}}",
        schema = json_string(SCHEMA),
        run_bytes = geometry.run_bytes(),
        header_bytes = geometry.header_bytes(),
        slot_stride = geometry.slot_stride(),
        slot_count = geometry.slot_count(),
        allocation_bitmap_bytes = geometry.allocation_bitmap_bytes(),
        lease_bitmap_bytes = geometry.lease_bitmap_bytes(),
        mark_bitmap_bytes = geometry.mark_bitmap_bytes(),
        bitmap_padding_bytes = geometry.bitmap_padding_bytes(),
        payload_bytes = geometry.payload_bytes(),
        tail_slack_bytes = geometry.tail_slack_bytes(),
    );
}

#[cfg(feature = "deterministic-test-hooks")]
fn geometry_workload() {
    macro_rules! measure {
        ($stride:literal) => {
            emit_geometry(
                $stride,
                geometry_measurement::<RequestedStride<$stride>>()
                    .expect("requested-stride fixture must fit one run"),
            );
        };
    }
    measure!(8);
    measure!(16);
    measure!(24);
    measure!(32);
    measure!(64);
    measure!(128);
    measure!(256);
    measure!(1024);
    measure!(4096);
}

fn assigned_run_scan_workload() {
    const SCANS: usize = 10_000;
    let heap = Heap::new_with_policy(CollectionPolicy::NoAuto);
    heap.with_mutator(|mutator| {
        let allocator = mutator
            .allocator::<SmallValue>()
            .expect("small-value layout is supported");
        for index in 0..ALLOCATION_COUNT {
            let _ = allocator.alloc(SmallValue {
                _payload: index as u64,
            });
        }
    });
    let started = Instant::now();
    let mut observed_slots = 0_usize;
    for _ in 0..SCANS {
        observed_slots = observed_slots
            .checked_add(std::hint::black_box(heap.metrics().allocated_slots()))
            .expect("measurement accumulator overflowed");
    }
    let elapsed = started.elapsed();
    assert_eq!(observed_slots, ALLOCATION_COUNT * SCANS);
    emit_workload("assigned_run_scan", SCANS, elapsed, None, heap.metrics());
}

#[cfg(feature = "deterministic-test-hooks")]
fn emit_finalization_state(name: &str, operations: usize, elapsed: Duration, heap: &Heap) {
    let statistics = heap.statistics();
    let metrics = heap.metrics();
    println!(
        "{{\"schema\":{schema},\"kind\":\"finalization_state\",\"name\":{name},\"operations\":{operations},\"elapsed_ns\":{elapsed_ns},\"pending_finalizers\":{pending_finalizers},\"finalization_batch_runs\":{batch_runs},\"failed_collections\":{failed_collections},\"finalizer_panics\":{finalizer_panics},\"assigned_runs\":{assigned_runs},\"allocated_slots\":{allocated_slots}}}",
        schema = json_string(SCHEMA),
        name = json_string(name),
        elapsed_ns = nanos(elapsed),
        pending_finalizers = statistics.pending_finalizers(),
        batch_runs = statistics.finalization_batch_runs(),
        failed_collections = metrics.failed_collections(),
        finalizer_panics = metrics.finalizer_panics(),
        assigned_runs = metrics.assigned_runs(),
        allocated_slots = metrics.allocated_slots(),
    );
}

#[cfg(feature = "deterministic-test-hooks")]
fn sparse_finalization_workload() {
    const VALUES: usize = 4_096;
    const ROOTABILITY_CHECKS: usize = 100_000;

    let heap = Heap::new_with_policy(CollectionPolicy::NoAuto);
    let countdown = Arc::new(AtomicUsize::new(VALUES - 1));
    let dropped = (0..VALUES)
        .map(|_| AtomicBool::new(false))
        .collect::<Arc<[_]>>();
    let mut values = heap.with_mutator(|mutator| {
        let allocator = mutator
            .allocator::<SparseFinalizer>()
            .expect("sparse-finalizer layout is supported");
        (0..VALUES)
            .map(|id| {
                Some(allocator.alloc(SparseFinalizer {
                    id,
                    countdown: Arc::clone(&countdown),
                    dropped: Arc::clone(&dropped),
                }))
            })
            .collect::<Vec<_>>()
    });

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let started = Instant::now();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| heap.collect_full()));
    let failed_elapsed = started.elapsed();
    std::panic::set_hook(previous_hook);
    assert!(failed.is_err(), "sparse finalizer did not inject its panic");
    assert_eq!(
        dropped
            .iter()
            .filter(|dropped| dropped.load(Ordering::Relaxed))
            .count(),
        VALUES - 1
    );
    assert_eq!(heap.statistics().pending_finalizers(), 1);
    assert_eq!(heap.statistics().finalization_batch_runs(), 1);
    emit_finalization_state(
        "sparse_finalization_failure",
        VALUES - 1,
        failed_elapsed,
        &heap,
    );

    let pending_index = dropped
        .iter()
        .position(|dropped| !dropped.load(Ordering::Relaxed))
        .expect("sparse finalization lost its pending allocation");
    let pending = values[pending_index]
        .take()
        .expect("pending finalizer handle disappeared");
    let started = Instant::now();
    heap.with_mutator(|mutator| {
        for _ in 0..ROOTABILITY_CHECKS {
            assert!(!is_rootable_for_measurement(mutator, &pending));
        }
    });
    emit_finalization_state(
        "sparse_rootability_checks",
        ROOTABILITY_CHECKS,
        started.elapsed(),
        &heap,
    );

    let started = Instant::now();
    let report = heap
        .collect_full()
        .expect("sparse finalization retry failed");
    let elapsed = started.elapsed();
    assert_eq!(report.conservatively_retained_slots(), 1);
    assert_eq!(report.finalized_slots(), 1);
    assert_eq!(report.reclaimed_slots(), 1);
    assert_eq!(heap.statistics().pending_finalizers(), 0);
    emit_workload(
        "sparse_finalization_retry",
        1,
        elapsed,
        Some(report),
        heap.metrics(),
    );
}

fn main() {
    let selections = std::env::args().skip(1).collect::<Vec<_>>();
    if selections.iter().any(|argument| argument == "--help") {
        println!(
            "usage: c8_measurements [all|geometry|assigned_run_scan|sparse_finalization|allocator|trace_deep|trace_wide|finalization|reclamation]..."
        );
        return;
    }
    const WORKLOADS: &[&str] = &[
        "all",
        "geometry",
        "assigned_run_scan",
        "sparse_finalization",
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
    #[cfg(feature = "deterministic-test-hooks")]
    if selected(&selections, "geometry") {
        geometry_workload();
    }
    if selected(&selections, "assigned_run_scan") {
        assigned_run_scan_workload();
    }
    #[cfg(feature = "deterministic-test-hooks")]
    if selected(&selections, "sparse_finalization") {
        sparse_finalization_workload();
    }
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

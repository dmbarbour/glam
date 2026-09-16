# Interaction-Net Callable WHNF Spill Review — 2026-09-16

Baseline: NC0 revision `08f7c09`; NC6 verification boundary `be7f2bb`.

Status: review complete. NC0-NC6 implement and verify the callable-WHNF spill
protocol required by W6B.4b.2. No open correctness defect or unresolved design
question was found. One test-fixture ownership defect exposed by aggressive
collection was repaired before this review closed.

## Scope

This is the focused NC6C.2 review required by
[`InteractionNetCallableWhnfSpill_2026-09-16.md`](../plans/InteractionNetCallableWhnfSpill_2026-09-16.md).
It audits:

- canonical state identity across regional and net-owned roles;
- direct versus spilled callable results;
- exact-pair claims, dependency ownership, and stale admission;
- cursor deferral and managed-edge mutation at checkpoint pairs;
- trait, copy, node-layout, and persistent-edge interlocks;
- profiling attribution and determinism;
- remaining synchronous evaluator compatibility; and
- drift in W6C-W8 and the parent raw-value migration.

## Outcome

A `Bind >< Data(callable)` reduction first drives the canonical `WhnfState`
under the caller's existing bounded value-access region and shared evaluator
budget. Immediate and cached callables retain the ordinary direct lowering.
Only a budget yield or an actual dependency boundary installs
`Bind >< CallableCheckpoint(Box<NetWhnfState>)`.

The regional and net-owned representations are consuming wrappers around the
same `WhnfState`; conversion moves the state without walking continuations,
duplicating values, or registering roots. While installed, the enclosing
managed core net traces every value-bearing focus, frame, retained member, and
promise breadcrumb. A claimed checkpoint moves that state back into regional
work for one bounded quantum and then finishes, replaces, republishes, blocks,
or terminalizes the exact generation.

There is no durable checkpoint claim. Claims remain stack-bound beneath one
matching access region. Dependency publication restores a complete checkpoint
before access closes, and exact subscription happens afterward. Completion
and stale-admission paths compare the checkpoint generation before changing
topology, so a losing worker cannot publish state over a newer generation.

## Semantic Accounting

### Topology and copying

`CallableCheckpoint` has one interaction rule: `Bind >< CallableCheckpoint`.
Every other principal partner becomes structurally stuck before ordinary
fan/erase rules run. Generic net copying returns no copyable checkpoint node.
A remote cursor reaching the pair therefore waits for the source pair's
semantic reduction rather than copying partial evaluator state.

The payload is boxed. On the x86-64 baseline, `WhnfState` and `NetWhnfState`
are 128 bytes, the box is one pointer, and `RuntimeNode<CoreSpecialization>`
remains 96 bytes. The unboxed prototype would enlarge the node to 128 bytes.
These measurements are compile-time-test policy rather than a stable public
ABI.

### Ownership and managed edges

The net is the only durable liveness owner of installed checkpoint state.
`NetWhnfState` contains no runtime roots, and its one canonical edge visitor
delegates to the compile-exhaustive compatibility walk for every raw `Value`
position. This compatibility traversal remains intentionally temporary until
the persistent-edge migration replaces raw recursive values; NC did not add a
second visitor or another ownership vocabulary.

Aggressive verification found that one task-terminal test constructed a
managed net in one access region and retained its bare handle across later
regions. Ordinary scheduling happened to preserve the allocation; collection
correctly reclaimed it. The fixture now constructs, claims, and roots the net
in one region and retains that root for the complete scenario. Exact access
and regional-constructor inventories classify this as a test-only ownership
boundary. No production path needed repair.

### Dependencies and failure

An unresolved lazy or promise publishes the complete state, then subscribes
the exact checkpoint generation to the dependency. Completion before block,
completion after block, stale generation, task cancellation, abandonment,
task failure, permanent WHNF failure, and unsupported external boundaries all
have deterministic fixtures. A dependency retry reclaims the same state; a
terminal outcome removes or marks the exact pair once.

`source_owner`, followed-value identity, the promise breadcrumb, and every
continuation frame remain in canonical state. NC5's usage inventory found no
safe field deletion worth a second representation or conversion walk.

### Profiling

Static interaction-net profiling records inline callable transitions and each
checkpoint install, resume, replace, dependency block/retry, stale admission,
and terminalization. Events are recorded at authoritative runtime-net
mutations rather than speculative evaluator attempts. Fresh-runtime fixtures
make the counters deterministic and keep concurrent tests from sharing a
profile. The focused script runs only the named profiling contracts.

## Verification

NC6C.1 passed:

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
scripts/check-interaction-net-profiling.sh
```

The ordinary library partition reported 1,659 passed and 2 ignored tests; all
integration and executable partitions passed. Focused aggressive-GC runs pass
the 14 callable-checkpoint tests and the regional/net role-handoff fixture.
Both forced-order retirement/publication schedules pass individually. Exact
raw-value, mutator-introduction, regional-constructor, and persistent-edge
inventories pass after recording the intentional ownership changes.

NC6B.3 compared both direct-assembly executable fixtures against `08f7c09`.
The duplicate-symbol case was 13.11s before and 13.16s after; the successful
repeated-split ELF case was 53.01s before and 52.12s after. The minimized
71-work-item signature is unchanged. These timings corroborate the
deterministic semantic/profile checks; they are not scheduling proofs.

## Drift Assessment

### Intentional and justified

1. **The checkpoint is a specialization payload, not ordinary net data.** It
   cannot be copied, fanned, erased, compared, or formatted through generic
   payload traits. This preserves linear evaluator progress rather than
   inventing semantics for partial evaluation state.
2. **Profiling is mutation-level.** Counters moved into core runtime-net
   transitions where necessary so abandoned or stale evaluator attempts do
   not become committed history.
3. **A test fixture gained one durable root.** The root is required because
   the fixture intentionally observes the same net across multiple mutator
   regions. It is not evidence that ordinary checkpoint transitions need
   per-state roots.
4. **No field-minimized checkpoint was introduced.** The rare boxed state
   retains the canonical representation; avoiding a full conversion walk is
   more valuable and auditable than saving a few transitory fields.

### Future phases

- W6B.4b.2 is complete. W6B.4b remains open only for the independent
  `resolve_core_access_in` migration in W6B.4b.1.
- W6C-W6F operate on builtin, collection, annotation, object, and
  net-construction evaluator families. They require no checkpoint-topology
  revision; they should reuse the canonical regional driver and borrowed
  budget established here.
- W6G.1 retains the pre-existing approximately 1.5x machine overhead against
  `7fed99e`; NC6 did not enlarge it.
- W6G.3 may replace fine-grained durable WHNF roots with one managed state
  cell. It must reuse this canonical `WhnfState` and edge visitor rather than
  introducing another net/regional conversion.
- W7 owns the global quantum, fairness, and small-stack proof. NC's budget is
  already borrowed and exact, but it does not establish those whole-runtime
  properties.
- W8 may remove remaining direct-evaluator and retryable-halt compatibility.
  NC6 removed the synchronous deferred-callable helper and leaves no
  checkpoint-specific compatibility seam to preserve.

No accidental future-phase drift was found.

## Findings

### NCWHNFR-001 — Resolved: task-terminal fixture retained an unrooted net

**Severity:** test ownership defect

**Status:** resolved before review closure

The aggressive callable-checkpoint partition deterministically rejected a
managed pointer after collection between fixture construction and later task
terminalization. The fixture now publishes an exact managed-net root before
leaving its construction region and retains it through cancellation,
abandonment, and failure observation. Aggressive collection and both exact
ownership inventories latch the correction.

No further findings remain open.

# Glam GC Worker and Finalizer Schedule Review — 2026-09-11

Baseline: `03aec8c`, the completed I11B controlled-production-collection
checkpoint. This review covers I11C.1-I11C.4.

Status: complete. Deterministically forced production-runtime schedules cover
worker managed access, passive finalization beside host work, a collection
request issued during Finalizing, and value-domain retirement before and after
controlled collection. No unresolved accidental drift was found. Production
heap policy remains `CollectionPolicy::NoAuto`; I11D still owns the post-I11
review and Gate G3 certification.

## Scope and Method

The review compared the I11C implementation with the integration plan, I11B's
serial boundary, the collector admission/finalization protocol, runtime worker
poll boundaries, diagnostic ingress and event delivery, public-root lifetime,
and the project rule that repetition is not evidence for a concurrency
contract.

Every relevant ordering is latched constructively:

- a worker announces while it holds its real bounded managed-access region;
- another worker announces while paused in host code outside managed access;
- a collector announces only after it has established Finalizing and its
  finalizer mutator; and
- channels release the selected participant only after the contrary outcome
  has been checked.

The tests use the actual `EvaluationRuntime`, coordinator, worker executor,
diagnostic ingress, event journal, output delivery, value domain, and
production collector. The deterministic collector APIs remain under the
existing private `deterministic-test-hooks` feature and are hidden from the
supported embedding API.

## Checkpoint Accounting

| Checkpoint | Implementation and evidence |
| --- | --- |
| I11C.1 | `collection_interleaves_with_worker_quantum_without_lost_work` pauses a real scheduled machine inside `EvaluationPollContext::with_value_access`, starts collection afterward, proves collection cannot complete first, releases the quantum, and observes the normal task result. A later explicit collection is exactly one epoch later. `aggressive_debug_collection_runs_before_outer_runtime_entry` and the collector-local companion prove that private aggressive mode collects once before each outer entry, never before recursive same-heap entry, and does not mutate `NoAuto`. |
| I11C.2 | `passive_finalization_produces_no_runtime_work` pauses a real worker outside managed access while an unrooted managed opaque shell finalizes. Diagnostic counts, observation epoch, coordinator generation/session/work inventory, and `Busy` disposition remain fixed. Assigned-run pressure does not grow, finalizer activity retires, and the logger FIFO retains exactly its one original event. The active opaque payload drops only during the later explicit external-owner drain. |
| I11C.3 | `FinalizingPhaseProbe` is one-shot and pauses only after finalizer-mutator installation. It removes itself from the heap under its probe-slot mutex, releases that mutex, and waits only on probe-local state. `external_request_during_finalization_is_coalesced` invokes a mutator-free output callback during that pause. Its nonblocking request becomes visible, successful completion clears it, and collection epochs prove neither recursion nor an intervening pass. |
| I11C.4 | `runtime_retirement_before_collection_leaves_public_value_inert` proves a public root does not retain an otherwise uncollected value domain. The worker/collector fixture repeats the same proof after real work and controlled collection. In both cases the value retains runtime provenance but cannot be observed after domain retirement. `dropping_finalizing_phase_probe_releases_one_collection_only` independently proves RAII release and one-shot consumption. |

## Locking, Ownership, and Failure Review

The managed-worker fixture blocks collection for the intended reason: its
bounded poll region owns an ordinary mutator admission. The host-worker fixture
does not hold managed access, so collection may proceed while scheduler work
is still progress-owned. Neither fixture treats a delay or repeated success as
proof of ordering.

The Finalizing probe introduces no production semantic lock order. The heap's
probe-slot mutex is released before the collector announces or waits. The
collector retains only its normal finalizer mutator admission plus the
probe-local condition-variable mutex while parked. Consequently managed
entrants remain excluded, while a host callback that neither inspects nor
constructs managed values can issue `request_collection` through its atomic
path. Dropping the probe handle releases a parked collector, including during
test unwind.

The request callback captures the value-domain factory rather than
`EvaluationRuntime`. That factory can reach the heap request bit but has no
strong route back to runtime state, so the fixture does not create a runtime
ownership cycle. Successful collection clears requests raised before its
final pressure publication; a later request would survive through the
existing data-lock/request ordering.

Managed opaque destruction remains passive. Collection drops only the scalar
`ExternalOwnerHandle` in the managed shell. It neither invokes the arbitrary
opaque payload nor routes a diagnostic, event, task, observation, or managed
allocation. Active payload retirement remains an explicit external-registry
operation after collection.

No unsafe block, public collection operation, automatic policy transition, or
runtime-readiness authority was added by I11C.

## Drift Classification

### Intentional and justified

- I11C was split into four checkpoints because the original phase crossed
  worker scheduling, collector admission, finalization, host callbacks, and
  runtime retirement.
- Aggressive pre-entry collection lives in `glam-gc`, where outer versus
  recursive entry is authoritative. Keeping that choice out of the runtime
  wrapper avoids a stale request when recursive entry occurs.
- The Finalizing pause is a dedicated one-shot phase probe rather than a
  destructor callback. Destructors remain unable to schedule worker entry.
- Passive shell finalization and active opaque-payload retirement are tested
  as separate operations, preserving I10's external-owner boundary.
- The existing cache-builder scheduler snapshot was renamed to a general
  test-only scheduler inventory because I11C needs the same authoritative
  tuple and no new production introspection surface.

### Corrective documentation

- The plan's generic reference to repeated worker stress was replaced with
  deterministically latched orderings. Ordinary stress remains useful as a
  smoke test but is not evidence for either schedule.
- I11D, not I11C, retains the focused Miri, sanitizer, aggressive-mode, and
  final Gate G3 certification obligation.

### Accidental drift

No unresolved accidental implementation drift was found.

## Verification

Verification passed on 2026-09-11:

- `cargo fmt --check`;
- `cargo clippy --all-targets --all-features -- -D warnings`;
- `cargo test -q -p glam-gc --lib --features deterministic-test-hooks`
  (196 passed, 2 ignored); and
- `cargo test -q` (1,459 main library tests plus every auxiliary target).

The focused production module contains nine passing managed-collection tests.
Gate G3 remains closed until I11D performs the post-I11 audit and its focused
dynamic-tool certification.

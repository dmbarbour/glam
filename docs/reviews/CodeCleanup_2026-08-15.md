# Code Cleanup Review — 2026-08-15

This is a review-only simplification pass over revision `205697a`. It uses the
current architecture and agent-context documents as the semantic baseline. No
Rust implementation was changed as part of the review.

The audit deliberately distinguishes redundant representation from legitimate
boundary projection. Similar-looking types are retained when they separate an
authoritative scheduler state, a transactional observation, and a public host
view. Findings below identify a canonical owner and an expected deletion or
removed action; they do not recommend adding a generic layer merely to reduce
line count.

## Summary

The strongest immediate cleanup opportunities are:

1. stop structurally comparing the complete reflection heap during settlement;
2. remove an unused vector that retains and clones every transactionally read
   input value;
3. collapse the deferred-producer reservation/activation handshake now that the
   complete machine is installed atomically;
4. remove the exact duplicate terminal-task enum and its bidirectional
   conversions; and
5. move test-only readiness inspection and the old byte-host adapter out of the
   production public surface.

The event-input conflict journal is the largest promising simplification, but
it is also the only finding that changes an explicit policy boundary. The
current FIFO protocol can validate both successful and empty reads directly,
making the general conflict-address history unnecessary. That conclusion
should be confirmed before implementation because it deliberately specializes
runtime event inputs rather than preserving a generic virtual-resource model.

Two apparent duplicates are specifically **not** findings:

- local and coordinator-owned completion publication still encode distinct
  promise ownership policies; and
- committed delivery-ID history prevents a cloned journal from replaying an
  output after the live delivery record has been retired.

## Representation matrix

| Concept | Authoritative representation | Necessary projections | Review conclusion |
| --- | --- | --- | --- |
| Work lifecycle | `WorkRecord` / `WorkState` in the coordinator | task status, readiness, host lifecycle reports | Preserve the projections. They expose different observation contracts. |
| Terminal task outcome | `EvaluationWaitTerminal` | `EvaluationTaskStatus` and public value/status rendering | `EvaluationTaskState` duplicates the authoritative terminal payload exactly and can be removed. |
| Evaluation blockage | coordinator block and wait subscriptions | retryable task block and client-demand result | Preserve. A block is mutable scheduling state, not a terminal result. |
| Reflection store | persistent `StoreSnapshot` plus transactional journal | runtime snapshot and protected-volume capabilities | Preserve snapshot types; remove only redundant whole-root equality during settlement. |
| Runtime event input | immutable FIFO buffer plus transaction-local cursor | `RuntimeEventSnapshot` / `RuntimeEventJournal` | Cursor validation is canonical. The parallel conflict-address history appears redundant. |
| Runtime output | output intent, accepted-ID set, delivery record | claimed delivery and terminal delivery outcome | Preserve accepted IDs and delivery records; they serve replay prevention and host ownership respectively. |
| Lazy/promise/deferred identity | runtime-scoped IDs and producer cells | wait token and work ID | Preserve the distinct identities. Remove only the now-unneeded deferred activation state. |
| Runtime/session/context/profile | runtime-owned shared resources; demand-session lease; evaluation context; immutable task profile | capabilities appropriate to each boundary | Preserve. Similar `Arc` wrappers encode lifetime and authority, not duplicate data. |
| Isolated effect execution | `IsolatedEffectSearch` plus a non-committing host | CLI, token, macro, and net-construction policies | The host mechanism is repeated; specialization snapshots and effect policies remain distinct. |

## Findings

### CCR-001 — Settlement re-traverses the reflection heap after an authoritative epoch check

**Classification:** unnecessary work  
**Priority:** high  
**Confidence:** high

`EvaluationRuntime::validate_quiescence_guarded` locks transactional state and
compares the complete persistent reflection root with the root retained by the
readiness snapshot. It then compares the authoritative runtime observation
epoch and validates the coordinator generation
([`api.rs`](../../src/api.rs#L4228)).

The observation epoch already advances for every semantically changed
reflection commit, while no-op commits intentionally leave it unchanged. The
settlement admission guard prevents another mutation from entering between
state validation and terminalization. Consequently, structural `Value`
equality is a redundant second proof. Depending on dictionary shape, it can
walk a large shared heap at the precise point where the runtime is attempting
to settle.

**Canonical owner:** `RuntimeObservationEpoch` for cross-component mutation,
plus coordinator generation and explicit output-activity validation.

**Recommended change:** remove
`state.reflection.root() == snapshot.reflection.root()`. If an additional
development invariant is desired, expose a constant-time store revision or
identity comparison and use a `debug_assert!`; do not retain structural value
equality in the release path.

**Risks:** an existing reflection mutation path might fail to advance the
observation epoch. That would be an epoch-publication bug and should be tested
directly rather than masked by a second, expensive protocol.

**Preservation tests:**

- a changed heap between readiness and settlement yields `RuntimeChanged`;
- a semantic no-op commit does not invalidate readiness;
- query retirement and event activity invalidate readiness; and
- settlement cannot pass while a shared mutation guard is held.

**Expected simplification:** remove a potentially whole-heap traversal from
every settlement validation and one redundant condition.

### CCR-002 — Runtime input cursors retain values that no code reads

**Classification:** redundant representation and unnecessary work  
**Priority:** high  
**Confidence:** high

`RuntimeInputCursor` contains `claimed: Vec<RuntimeValueRoot>`
([`api.rs`](../../src/api.rs#L2714)). Each successful read clones the payload
into that vector before returning another runtime-rooted view
([`api.rs`](../../src/api.rs#L2747)). There is no reader of `claimed`.

The frozen input snapshot already retains every admitted record for the life of
the journal, and the returned `Value` independently retains its runtime root.
Cloning a journal also clones this unused vector, extending the lifetime of all
values read so far.

**Canonical owner:** the immutable `RuntimeInputBuffer` in the event snapshot;
the cursor needs only `start` and `next`.

**Recommended change:** remove the field, its initialization, and the per-read
push.

**Risks:** very low. The main check is that an input payload remains alive after
the returned value is retained and the journal is dropped; that ownership
belongs to the returned value rather than a hidden cursor history.

**Preservation tests:** multiple reads followed by commit, journal clone and
abandonment, retained returned values after journal drop, and runtime
provenance rejection.

**Expected simplification:** remove three pieces of state manipulation and one
root clone per successful input read, plus all clones of that history.

### CCR-003 — Deferred work performs a reservation/activation handshake after its machine already exists

**Classification:** transitional handshake  
**Priority:** high  
**Confidence:** high

`EvalContext::deferred_task` constructs the complete task machine before
calling `reserve_deferred`, passes that machine into the coordinator, and then
calls `activate_deferred` in a second mutation cycle
([`evaluation.rs`](../../src/evaluation.rs#L1816)). The coordinator nevertheless
installs the record as `WorkState::Reserved`, tracks
`demanded_while_reserved`, and later moves it to `Dormant` or `Queued`
([`coordinator.rs`](../../src/evaluation/coordinator.rs#L3500)). Promotion has a
special branch that only sets the temporary demand bit
([`coordinator.rs`](../../src/evaluation/coordinator.rs#L5095)).

This handshake was appropriate when a coordinator-visible reservation could
precede machine construction. That is no longer the call path. The fully built
machine and every index become visible together while the coordinator mutex is
held. A promoter cannot observe a partially installed record: after insertion
it can immediately promote `Dormant` to `Queued`.

`WorkState::Reserved` should remain for reflection-task launch, whose machine
is installed after reservation. The finding is specific to deferred producers.

**Canonical owner:** the atomic `reserve_deferred` insertion. New deferred work
starts `Dormant`; exact demand promotion owns the only transition to `Queued`.

**Recommended change:** initialize new deferred records as `Dormant`; remove
`demanded_while_reserved`, `activate_deferred`, the reserved-promotion branch,
and the second generation increment/wakeup.

**Risks:** first-demand races and closure of the demand session immediately
after insertion. Both are observable with barriers and should be pinned before
deleting the handshake.

**Preservation tests:** force promotion immediately after insertion, race two
candidate producers for the same lazy value, close the owner immediately after
insertion, and retain the existing loser-machine destruction test.

**Expected simplification:** one state field, one coordinator operation, one
work-state branch, and one mutation/generation/wakeup cycle per new deferred
producer.

### CCR-004 — `EvaluationTaskState` exactly duplicates `EvaluationWaitTerminal`

**Classification:** redundant representation  
**Priority:** high  
**Confidence:** high

`EvaluationWaitTerminal` and `EvaluationTaskState` each contain the same six
variants with the same payloads: complete, failed, cancelled, abandoned,
exited, and killed ([`evaluation.rs`](../../src/evaluation.rs#L120),
[`evaluation.rs`](../../src/evaluation.rs#L1065)). Two exhaustive conversion
functions translate between them, and `settle_task_work` converts to the wait
form for the coordinator and immediately converts the result back.

This is not the same as the useful distinction between `WorkState`, live
`EvaluationTaskStatus`, and public `EffectLifecycleStatus`. Those types include
pending/blocking states or deliberately project to a public value contract.
The two terminal enums do not.

**Canonical owner:** `EvaluationWaitTerminal`, renamed if necessary to reflect
that it is the runtime's immutable terminal task outcome rather than merely a
wait-cell detail.

**Recommended change:** use the terminal outcome directly in settlement and
release paths; delete `EvaluationTaskState` and both conversion functions.

**Risks:** accidental extra clones when matching borrowed terminal results, and
changes to which failure object is entered into the unacknowledged-failure
ledger. Preserve the same `Arc` and `RuntimeValueRoot` ownership.

**Preservation tests:** status publication for all six terminal variants,
failure-ledger insertion, cancellation/abandonment/exit/kill, and promise
failure messages produced by terminal settlement.

**Expected simplification:** delete one enum and two exhaustive conversions;
simplify every terminal settlement call site.

### CCR-005 — FIFO input conflicts maintain a general address history in parallel with cursor validation

**Classification:** duplicated mechanism with a broader-than-needed policy  
**Priority:** medium  
**Confidence:** medium; confirm the intended event-resource policy first

Runtime event state carries a revision, a `latest_changes` map keyed by generic
`ConflictAddress`, and a configurable conflict strategy
([`api.rs`](../../src/api.rs#L2514)). Every input read records an exact slot
observation; every append or consumption adds another slot to change history;
and validation scans the complete history for changes newer than the snapshot
before separately validating each FIFO cursor
([`api.rs`](../../src/api.rs#L2549), [`api.rs`](../../src/api.rs#L2562),
[`api.rs`](../../src/api.rs#L2750)). Since input sequence numbers are never
reused, this change map can grow for the lifetime of the runtime.

The FIFO protocol can validate the observations directly:

- for a cursor that claimed values, the current head must still equal `start`
  and the claimed count must remain available; and
- for an empty read, both the current head and next sequence must still equal
  the observed empty boundary.

Any append, competing consumption, or fallback drain changes one of those
boundaries. Unrelated endpoints remain independent without a probabilistic or
hierarchical conflict index.

**Canonical owner:** endpoint-local FIFO sequence boundaries and transaction
cursors. The configurable conflict strategy remains appropriate for the
hierarchical reflection store, where observations are not a single canonical
queue protocol.

**Recommended change:** give every attempted read a cursor, including empty
reads; validate the cursor directly; remove event revisions,
`latest_changes`, event conflict observations, and `InputSlot` from the generic
conflict-address vocabulary if it then has no other caller.

**Risks:** this intentionally abandons the idea that event endpoints are
arbitrary virtual transactional resources. It is correct for the currently
documented FIFO protocol but would need extension before introducing a
different endpoint policy. Replace the test named
`runtime_input_uses_the_configured_conflict_strategy` with protocol tests that
do not imply heap strategy controls queues.

**Preservation tests:** empty read then append; append and consume before a
stale commit; two competing consumers; mutations to unrelated endpoints;
fallback drain; cloned journals; and atomic rollback when a combined heap/event
commit conflicts.

**Expected simplification:** remove an unbounded change-history map, a full-map
validation scan, and the event side of the generic conflict-analysis
abstraction.

### CCR-006 — Every event snapshot rebuilds the endpoint map

**Classification:** unnecessary persistent-state copying  
**Priority:** medium  
**Confidence:** high

`RuntimeEventState` stores its input endpoint table as a plain `BTreeMap`, even
though every input buffer is already shared by `Arc`. `snapshot` iterates the
entire map, clones each entry, collects a new map, and wraps it in another
`Arc` ([`api.rs`](../../src/api.rs#L2514), [`api.rs`](../../src/api.rs#L2535)).
Transaction snapshots are taken on effect attempts and retries, while endpoint
registration and input admission are comparatively rare.

**Canonical owner:** a persistent root for the endpoint map itself.

**Recommended change:** store `Arc<BTreeMap<...>>` and mutate it with
`Arc::make_mut`, or use the project's persistent map convention if profiling
shows enough endpoints to justify it. Snapshot creation then clones one root.

**Risks:** mutations must still copy before modifying a buffer pointer, and a
snapshot must remain immutable across endpoint registration and admission.
This change should follow or accompany CCR-005 so the event representation is
only rewritten once.

**Preservation tests:** snapshot immutability across registration, admission,
consumption, and mutations on two endpoints; concurrent snapshot/commit; and
combined heap/event validation.

**Expected simplification:** remove the per-snapshot collection traversal and
shift map copying to actual map mutations.

### CCR-007 — A transitional readiness probe is public production API used only by tests

**Classification:** test-only production surface  
**Priority:** medium  
**Confidence:** high

`EvaluationRuntime::exclusive_admission_available` is `pub`, doc-hidden, and
explicitly described as transitional ([`api.rs`](../../src/api.rs#L4474)). All
in-tree calls are unit-test synchronization assertions or a reflection test
helper; production code does not use it.

**Canonical owner:** runtime-internal test support beside the forced-ordering
fixtures that inspect settlement admission.

**Recommended change:** make it `#[cfg(test)] pub(crate)` or replace it with a
test helper that attempts the internal settlement guard. Do not preserve an
external probe whose answer is stale immediately after it is returned.

**Risks:** external pre-release users may have discovered the doc-hidden method.
Its observational semantics are unsuitable as a supported API regardless.

**Preservation tests:** the existing barrier-based delivery, conversion,
logger, and settlement tests should continue to prove that callbacks run
outside exclusive admission.

**Expected simplification:** remove one public/transitional API and narrow the
runtime surface; little line-count reduction.

### CCR-008 — The byte-oriented `Host` adapter duplicates the artifact-oriented source system

**Classification:** transitional compatibility  
**Priority:** medium  
**Confidence:** high

`source.rs` still defines the old byte `Host`, `SystemHost`,
`HostSourceSystem`, and a dedicated relative import resolver
([`source.rs`](../../src/source.rs#L376)). `AssemblerBuilder::host` explicitly
adapts the “previous byte-host API” ([`api.rs`](../../src/api.rs#L5193)), and
the types are re-exported publicly. Production construction uses
`SourceSystem`; the remaining in-tree clients are compatibility tests with a
`MemoryHost`. Even `FileSourceSystem::read_untracked` reaches the filesystem
through `SystemHost` rather than reading directly.

**Canonical owner:** `SourceSystem` returning a `SourceArtifact` with identity,
digest, provenance, and an `ImportResolver`.

**Recommended change:** migrate the memory fixtures to a small
`MemorySourceSystem`, remove `AssemblerBuilder::host` and the compatibility
exports/types, and have `FileSourceSystem` call `fs::read` directly.

**Risks:** public API break and accidental loss of relative-import behavior in
tests. The project is pre-release and the API labels itself transitional, but
the migration should preserve artifact identity and content-digest assertions.

**Preservation tests:** in-memory top-level load, relative imports, source
identity, digest/manifest behavior, mutated-local-file detection, and import
diagnostic provenance.

**Expected simplification:** remove roughly seventy lines of adapter code plus
the old public builder path and duplicate test fixture vocabulary.

### CCR-009 — A phase-zero macro oracle duplicates production behavior and preserves stale chronology

**Classification:** vestigial test oracle and stale documentation  
**Priority:** medium  
**Confidence:** high

`parser/macro_contract.rs` is a 158-line test-only implementation whose header
says later phases will replace and consume it
([`macro_contract.rs`](../../src/g_syntax/parser/macro_contract.rs#L1)). Those
phases are complete. It reimplements static macro-head scanning and the
`@`/`#` output restriction. Production macro-expansion tests already cover
right-to-left expansion and output restrictions.

The malformed/dynamic-head table remains useful and should be moved to tests
that exercise the production staged parser before deleting the oracle. Related
module comments still describe the staged lexer, logical tokens, isolated CLI
search, and coordinator task terminalization as future phases rather than
current invariants.

**Canonical owner:** staged parser and macro-expansion contract tests; current
architecture documents for lifecycle explanation.

**Recommended change:** first latch every unique malformed-head case against
the production parser, then delete `macro_contract` and rewrite chronological
comments as timeless ownership/invariant statements.

**Risks:** deleting a unique invalid-syntax case without migrating it. Compare
the oracle table case by case rather than assuming broader integration tests
cover it.

**Preservation tests:** every malformed/dynamic macro head, joint static paths,
macros hidden in text/comments, right-to-left expansion, and forbidden textual
output markers.

**Expected simplification:** delete 158 lines of duplicate test parser and
remove misleading phase language from production modules.

### CCR-010 — Four isolated effect hosts repeat the same non-committing execution mechanism

**Classification:** duplicated mechanism with distinct specialization policy  
**Priority:** low/medium  
**Confidence:** medium

CLI, token parsing, macro expansion, and interaction-net construction each
define a host containing an environment, an exact empty reflection-store
snapshot, and a specialization snapshot. Each returns generation `1`, rejects
commit with `CommitResult::Closed`, and never waits for change
([`cli/host.rs`](../../src/cli/host.rs#L87),
[`cli/token.rs`](../../src/cli/token.rs#L51),
[`macro_expansion/host.rs`](../../src/g_syntax/macro_expansion/host.rs#L132),
[`construction.rs`](../../src/eval/builtins/net/construction.rs#L185)).

The specializations and their journals are intentionally different. The host
lifecycle is not: `IsolatedEffectSearch` owns all branch journals and nothing
may commit through the host ([`search.rs`](../../src/reflection/search.rs#L165)).

**Canonical owner:** a small `IsolatedTaskHost<S>` or equivalent mechanism next
to `IsolatedEffectSearch`, parameterized only by the task environment and
specialization snapshot. Specialized effect interpreters continue to own
their journal and diagnostics policy.

**Recommended change:** prototype the generic shape in one consumer. Keep it
only if it removes all four concrete `TaskHost` implementations without
introducing more associated-type plumbing than it deletes. An immutable empty
store snapshot may also be shared within the owning runtime/factory after
confirming that these effects cannot access the shared reflection heap.

**Risks:** macro and CLI `.log` operations are journaled rather than emitted;
over-generalizing `ReflectionServices` could silently change that policy.
Invocation-local specialization snapshots must also remain isolated.

**Preservation tests:** all-results branch isolation, abandoned journals,
specialized `.log` journaling, forbidden commit, no shared-heap access, and net
construction in an existing evaluation context.

**Expected simplification:** centralize four repeated host lifecycles and empty
store construction. Reject the refactor if the generic API merely moves equal
amounts of complexity into type constraints.

### CCR-011 — Presence-oriented `Assembler::{get,get_optional}` remains a compatibility interpreter

**Classification:** transitional compatibility  
**Priority:** low  
**Confidence:** medium

The methods explicitly call themselves compatibility helpers and direct new
code to `Values::access` ([`api.rs`](../../src/api.rs#L5647)). They manually
split an atom path, demand each intermediate dictionary, and deliberately leave
the final member lazy. They remain widely used by `main`, configured CLI code,
and tests, so immediate deletion would be churn rather than cleanup.

They are also not quite equivalent to evaluating a composed accessor: the
current API can report absence without demanding the final value. That
difference must be either preserved as a real host-inspection capability or
declared unnecessary before migration.

**Canonical owner:** semantic path composition using `Values::access` for
ordinary evaluation; a reflection/public inspection API only if host-side
presence without final demand remains a genuine requirement.

**Recommended change:** stage retirement:

1. introduce a private atom-path composition helper;
2. migrate call sites that immediately demand the result (`conf.env`,
   `conf.cli`, `conf.log`, and viewer lookups where applicable);
3. migrate tests from the compatibility surface; and
4. decide whether the remaining no-final-demand behavior merits an explicitly
   named inspection API before removing `get_optional`.

**Risks:** changing when the final member is forced, changing `{}`/missing
semantics, and losing the existing path-lookup error context.

**Preservation tests:** missing intermediate and final members, a divergent
final member, non-dictionary intermediates, foreign-runtime values, and path
lookup context on evaluation failure.

**Expected simplification:** eventually remove a second small path interpreter
and shrink `Assembler`; defer until semantic composition has replaced real
callers.

## Explicit non-findings

### Completion publication has two policies, not two accidental mechanisms

`CompletionSubscriptions::notify_published` supports promises completed by a
directly driven/local effect task, while coordinator-owned tasks publish their
terminal obligations under coordinator mutation admission. The split should be
documented with current ownership language—the “transitional” wording is
stale—but merging it now would make local promises depend on a coordinator
record they do not have. Revisit only if every direct runner becomes
coordinator-owned.

### Accepted delivery IDs are not duplicate delivery records

The live delivery record owns queued/running output and is removed on terminal
delivery. The accepted-ID set outlives that record so a cloned event journal
cannot recommit the same reserved ID. The regression test
`cloned_output_intent_cannot_republish_a_terminal_delivery_id` pins this
distinction. Monotonic ID allocation alone does not distinguish committed IDs
from merely reserved or burned IDs.

### Public task and lifecycle enums are boundary projections

`WorkState`, `EvaluationTaskStatus`, `EffectLifecycleStatus`, and readiness
snapshots should not be collapsed merely because terminal variant names
overlap. They separately encode scheduler mutation, transactionally visible
task status, host reporting, and stable readiness. CCR-004 is narrower because
its two enums are exact terminal payload copies with immediate two-way
conversion.

### Exact completion subscriptions and the observation epoch serve different scopes

An exact subscription wakes work waiting on a known lazy/promise/task
dependency. `RuntimeObservationEpoch` invalidates a stable whole-runtime
observation when any relevant component changes. One cannot replace the other
without either broad wakeups or incomplete settlement validation.

## Recommended implementation order

1. **Low-risk removal:** CCR-001, CCR-002, CCR-007, and the stale comments from
   CCR-009. Each should reduce work or public surface without changing a
   protocol.
2. **Latch and retire compatibility:** migrate the macro oracle cases, then
   finish CCR-009; migrate source tests, then perform CCR-008.
3. **Coordinator protocol cleanup:** barrier-test and implement CCR-003, then
   collapse the terminal representation in CCR-004.
4. **Event-state rewrite:** decide CCR-005 explicitly, then combine it with
   CCR-006 so the event maps and validation protocol are changed once.
5. **Repeated mechanism:** prototype CCR-010 and retain it only if the result is
   materially smaller and clearer.
6. **Semantic facade cleanup:** stage CCR-011 after the main/configuration call
   sites have composition helpers.
7. **Module splitting:** only after the representations above settle. Split
   large files along the remaining ownership boundaries, not around mechanisms
   scheduled for deletion.

## Verification baseline

At review time, `cargo clippy --all-targets --all-features -- -D warnings`
passes. The parallel default `cargo test -q` run is **not green**: 1,095 tests
passed and these two tests failed while forcing the cached macro-environment
helper:

- `macro_environment_extends_a_dictionary_with_ordinary_introduction_rules`;
- `macro_environment_reinstantiates_an_adapting_object`.

Both reported `lazy net computation reached a non-data normal form`. The pair
passes when filtered and run alone, and the entire suite passes with
`--test-threads=1`. This is therefore an unresolved parallel-suite
ordering/concurrency defect, not a deterministic failure introduced by this
review. It should receive a barrier-based reproduction and a separate fix; a
later passing parallel run would not by itself close it.

Implementation of any finding should run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Concurrency-sensitive findings should first add a forced-order regression test
that exposes the old transition or race; repetition alone is not adequate.

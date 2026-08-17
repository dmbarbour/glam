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

### CCR-001 — Resolved: settlement re-traversed the reflection heap after an authoritative epoch check

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

Resolution on 2026-08-17:

- `validate_quiescence_guarded` now validates empty output activity and the
  authoritative observation epoch without comparing reflection roots.
- Existing forced-order coverage still proves that validation waits for shared
  mutation admission, and a heap commit after capture still produces
  `RuntimeChanged`.
- The same test now establishes that a semantic no-op reflection commit keeps
  the retained readiness snapshot valid.
- `ReflectionStore::root`, made production-dead by this removal, is now
  test-only for store implementation and compatibility assertions.

### CCR-002 — Resolved: runtime input cursors retained values that no code reads

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

Resolution on 2026-08-17:

`RuntimeInputCursor` now contains only `start` and `next`. Reads return the
payload's independently rooted `Value` without cloning that root into a hidden
history. The abandoned-claim regression retains and observes the returned
value after its journal is dropped, then confirms that a later transaction can
still consume the same buffered input.

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

### CCR-007 — Resolved: a transitional readiness probe was public production API used only by tests

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

Resolution on 2026-08-17:

`exclusive_admission_available` is now `#[cfg(test)] pub(crate)`. The existing
forced-order callback and settlement tests retain the probe, while production
and external builds no longer expose or compile it as an `EvaluationRuntime`
operation.

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

#### CCR-009 update plan

Treat this as two independently verifiable checkpoints. The first changes the
test ownership boundary; the second documents the architecture left by that
change.

##### Phase 1: Latch production contracts and retire the oracle — complete

1. Inventory every behavior in `macro_contract` before editing it:
   - one-component and joint multi-component static heads;
   - a spaced dot ending the head and beginning macro input;
   - heads inside delimiter groups and attached layout;
   - macro-like text hidden in source texts and comments;
   - missing, spaced, dynamic, computed, empty, and non-joint path components;
   - right-to-left expansion order; and
   - rejection of `@` and `#` from textual macro output.
2. Map each case to a test that exercises the production staged parser or
   macro-expansion path. A lexer-only assertion is insufficient when the
   contract concerns expansion, diagnostics, or source rewriting.
3. Add every missing production regression before deleting its oracle case.
   Invalid heads must assert the relevant production diagnostic, while valid
   heads must demonstrate the selected path and expansion order where those
   are observable.
4. Delete `parser/macro_contract.rs` and its test-module registration only
   after the coverage map has no unmatched behavior.
5. Run the focused lexer/parser, macro-expansion, macro contract-sample, and
   invalid-syntax suites.

Phase 1 is complete when no source reference to `macro_contract` remains and
every unique table entry is owned by production-path coverage. Broader tests
which happen to pass are not a substitute for the explicit case inventory.

Resolution on 2026-08-17:

- `production_macro_heads_are_static_joint_paths_in_expansion_order` now owns
  the accepted-head table through `DeclarationMacroWork::from_original`. It
  covers single and joint paths, the spaced-dot boundary, delimiter and layout
  nesting, source-order reversal into expansion order, and exclusion of text
  and comments.
- `production_macro_heads_reject_missing_dynamic_or_nonjoint_paths` sends all
  eight missing, dynamic, computed, empty, or non-joint cases through the same
  production collector and asserts its public diagnostic.
- `declaration_macros_expand_right_to_left_and_share_the_evolving_view`
  continues to prove actual execution order, one execution per original
  invocation, and evolving-source visibility. The macro protocol sample suite
  independently covers single-component and object-path macro lookup.
- `source_macro_rejects_reserved_or_unbalanced_generated_text` now covers both
  `@` and `#`, including markers embedded within otherwise ordinary output;
  `GeneratedText::classify` retains its lower-level structural assertion.
- `parser/macro_contract.rs` and its module registration have been deleted.
  No production or test source references the oracle.

The focused logical-parser, right-to-left expansion, generated-text, and five
macro protocol tests pass after deletion, as does the complete 1,276-test
repository suite. Phase 2 remains intentionally separate so comment edits
describe this final ownership boundary.

##### Phase 2: Replace stale chronology with current invariants

1. Audit production comments which still describe completed parser,
   macro-expansion, isolated-search, conditional-syntax, or coordinator phases
   as future work. Start with the module headers and transition comments in
   `g_syntax/parser`, `reflection/search.rs`, `evaluation.rs`, and
   `evaluation/coordinator.rs`.
2. Classify each match before editing it:
   - rewrite completed-phase chronology as a timeless ownership or behavior
     statement;
   - retain genuine deferred design work, labeling it by the missing behavior
     rather than an obsolete phase number; and
   - leave ordinary uses of words such as “later” or “temporary” untouched
     when they describe runtime ordering or temporary files rather than project
     chronology.
3. Update any architecture or agent-context statement which still points at
   the deleted oracle or contradicts the production parser after Phase 1.
4. Re-run a targeted text audit and review the resulting comments beside their
   implementations; comment cleanup has no useful automated semantic oracle.

Phase 2 is complete when production comments describe present owners and
contracts, while actual future work remains explicit without stale transition
numbering.

##### Final verification

Run `cargo fmt --check`, all-target/all-feature Clippy with warnings denied,
and the complete parallel test suite. Record the migrated cases, deleted lines,
and any chronology deliberately retained in this finding before marking
CCR-009 resolved.

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

1. **Low-risk removal:** CCR-001, CCR-002, and CCR-007 are complete. Keep the
   stale-comment and oracle work in CCR-009 as its own compatibility-retirement
   checkpoint.
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

### Verification follow-up: parallel net claim defect resolved

The defect above was subsequently reproduced deterministically. A remote
cursor claimed directly at a net's exposed interface had no active-pair record,
so the runtime neither made the claim exclusive nor counted it as in-flight
work. A concurrent evaluator could therefore observe an empty scheduler and
declare a non-data normal form while source inspection was still publishing
the interface result.

The repair records pairless cursor claims explicitly, includes them in
quiescence, and rechecks the exposed interface under the runtime lock before
returning normal form. The regression suite now forces the conflicting claim
order directly. Separate barrier tests also exercise concurrent construction
and use of the runtime-cached compiler helpers, ruling out the cache's
deliberately redundant construction as the source of this failure.

Verification after the repair includes five consecutive parallel library-suite
runs (5,500 tests total), 50 repetitions of the scheduler terminal-policy
regression, and the complete default suite. All passed, together with
`cargo fmt --check` and clippy with warnings denied. The original baseline is
retained above as the review-time observation; the concurrency defect is no
longer open.

### Follow-up resolved: replace pairless cursor scaffolding through Cursor WHNF

The pairless-claim repair above is necessary for the current synchronous
interface-demand model, but it should not become permanent architecture. The
approved [Cursor WHNF transition](../.tmp/CursorWHNF.md) replaces direct
`demand_interface`/`claim_dependent_cursor` driving with owning-net cursor
obligations and request-relative normalization.

This is a semantic transition before it is a cleanup. Do not independently
remove the repaired exclusivity or quiescence accounting. Instead, latch its
concurrency behavior, then use the transition phases to retire:

- `claimed_cursors` as a parallel work ledger;
- `CursorClaim::pair: Option<ActivePairKey>` and pairless claim special cases;
- durable `CursorDependency::SourcePair` and transitional `SourceCursor`;
- recursive `progress_cursor_dependency` calls into exact active-pair work;
- the defensive cursor-dependency depth limit; and
- whole-net terminal inference where a cursor-WHNF request needs only its
  demanded cone.

The replacement must keep one authoritative owner for every cursor
transition: pair-owned cursor state remains in `ActivePairState`, while
pairless demand belongs to an obligation in the cursor's own `RuntimeNet`.
Evaluators may aggressively follow claimable work across nets, but they park
and subscribe when another evaluator owns the required claim. Spine addresses
are recomputed rather than cached during the initial transition.

Close this follow-up only after the original forced-order pairless-claim test,
the new demand-spine and nested-failure tests, and the parallel suite pass
without compatibility use of the structures above.

Resolution on 2026-08-17:

- `claimed_cursors` and optional pair ownership are gone. Every transition is
  owned either by `ActivePairState` or by one cursor-local
  `PairlessCursorObligation`, selected through `CursorClaimOwner`.
- `CursorDependency::SourcePair` and its direct mutation interface are gone.
  The `SourceCursor` name remains only as the endpoint classification of a
  versioned `FrontierObservation`; it no longer grants or records a direct
  source claim and is therefore not the transitional representation named by
  this follow-up.
- `NormalizationRequest` now drives an iterative worklist. Recursive
  `progress_cursor_dependency`, the defensive depth limit, and evaluator
  fallback to unrelated whole-net work have been removed.
- The obligation transition methods used by `step_cursor` are private owning
  state-machine operations. The one evaluator test that deliberately creates
  contention uses a `#[cfg(test)]` `SharedRuntimeNet` hook rather than retaining
  a production transition surface.
- Forced-order concurrent root demand, source mutation before wait,
  transitive-demand, nested structured-failure, mixed-owner depth, and
  request-relative terminal tests all pass. The complete parallel repository
  suite passes with 1,278 tests.

Pairless *obligations* remain intentionally: they are the authoritative owner
for a demanded cursor which is not in an active pair. What was retired was the
parallel claim ledger and compatibility driving API, not that ownership case.
The detailed transition accounting and its independent findings are recorded
in the [Cursor-WHNF review](CursorWHNF_2026-08-17.md).

Implementation of any finding should run:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Concurrency-sensitive findings should first add a forced-order regression test
that exposes the old transition or race; repetition alone is not adequate.

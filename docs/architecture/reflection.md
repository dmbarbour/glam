# Reflection Architecture

Reflection tasks interpret freer-monad effects outside pure value and
interaction-net semantics. This note describes the current implementation;
[`../agent_context/reflection.md`](../agent_context/reflection.md) contains the
regression-sensitive rules.

## Specialization Layers

`reflection.rs` implements the generic task machine and standard effects.
`TaskSpecialization` contributes an additional request enum, private request
tags, host behavior, and transactional host data. Request families remain
reusable by mapping their request type into a specialization.

The reusable `ReflectionEffects` family adds environment lookup, diagnostic
emission, dictionary iteration, lazy-shell value observation, and child-task
operations. `main` defines a broader logger specialization with diagnostic
stream reads and stderr output. Children launched through `.task.new` inherit
their parent's complete profile, so logger children retain those additional
operations, its role environment, and its diagnostic destination.

`refl` and `meta_refl` annotations instead use the runtime's immutable default
reflection profile. Their behavior therefore does not depend on whether an
assembler, macro, logger, or other demand session happened to claim the lazy
annotation first. Children launched by an annotation inherit that default
profile from the annotation task.

Core operators merely construct tagged request values. Host operations occur
when the effect task dispatches those requests.

## Persistent Effect Machine

An `EffectTask` retains its continuation, application, alternative, and nested
transaction frames across bounded polls:

```text
effect value
  -> drive until request or result
  -> dispatch request through TaskHost
  -> deliver success, failure, or suspension
  -> preserve frames and return task state to EvaluationSession
```

Standard effects include `r`, `seq`, `alt`, `fail`, `cut`, `fix`, indexed
task-local `get`/`set`, shared `heap.get`/`heap.set`/`heap.rewrite`, and indexed
`reset`/`shift`.
Local user state, including the reset stack, is ordinary task state. Shared
store state is staged separately in the runtime transaction; it is never
projected into local state. The ordinary heap is one runtime-owned store
volume shared by every attached reasoning host. Choice frames and journals
remain machine or host bookkeeping.

An outer `cut` provides an optimistic transaction boundary. Alternatives start
from snapshots; losing branches discard changes; a winning outer branch
validates and commits. A host observation can turn later failure into a retry
point. `cut` alone does not: unobservant failure is terminal.

`reflection/search.rs` supplies a second outer policy for clients that need all
effect results, such as configured CLI parsing. It installs one uncommitted
outer transaction, explores successful alternatives in deterministic
left-to-right depth-first order, and returns each value with its isolated
specialization journal. Nested `cut` still chooses at most its first success.
No search branch commits to the host. A retryable observation or lazy
dependency suspends the pollable search without discarding pending branches;
an observed-state change conservatively restarts the complete isolated search.
Ordinary `EffectRun` retains its explicit-cut requirement and single-result
behavior.

A specialization request may return an ordered collection of alternatives.
The machine resumes the current continuation once for each value using the
same ordinary choice machinery; an empty collection is effect failure. This is
used by nested configured-CLI token parsing so distinct structured token
results remain distinct outer parse branches rather than being collapsed in
host code.

Both outer policies use the same standard-effect machine. The machine separates
deterministic effect failure, current dependency waits,
and evaluation errors. A dependency becoming terminal reruns the unchanged
operation that observed it. A non-blocking evaluation error remains retryably
blocked only when an existing state observation can rewind its checkpoint; it
does not advance `.alt`. The scheduler receives only the dependency token,
coarse retry generation, and retained diagnostic text.

`reflection/store.rs` owns a persistent map of shared volumes independently of
host wake state. Transactions record volume-qualified hierarchical read paths
and one ordered edit overlay; commits rebase edits onto the current persistent
roots. The store retains exact changed addresses. Blind sets and rewrites,
including overlapping parent and child paths, serialize in commit order while
their target volume exists. A session-selected
`Arc<dyn ConflictAnalysisStrategy>` controls only how reads are summarized:
the bootstrap supplies exact, conservative fingerprint, and fully coarse
strategies. `EvaluationRuntime` fixes the strategy at construction; an
assembler which attaches that runtime cannot replace it.

Heap paths are ordinary lazy value operations rather than a store schema.
`.heap.set` stages a replacement without inspecting the old heap.
`.heap.rewrite Path Updater` lazily applies `Updater` to the commit-time value
at `Path`, allowing concurrent rewrites to serialize without retrying. A later
local read remains snapshot-dependent through a rewrite; an ancestor rewrite
widens a descendant read to the updater's complete input path. An earlier
covering set can still make that widened dependency entirely local.
`.heap.get` returns an unforced access value; malformed roots, updates, and
updaters therefore remain latent evaluator errors, which `.eval` can observe
as data.

The runtime transaction lock makes reflection-store and generic event changes
atomic. The logger reads its ordered diagnostic input endpoint and buffers
diagnostic or stderr output in the same event journal as its heap edits. A
failed validation therefore cannot partially edit the heap, consume a
diagnostic, or leak output from an abandoned choice. The outer runtime mutation
gate spans authoritative publication through the broad observation-epoch
advance; diagnostic subscribers, condition-variable notification, decoding,
and stderr delivery run only after it is released.

## Protected Client Volumes

`AssemblerBuilder` selects the runtime and creates an unsealed host before
constructing the reflection environment. Its environment closure may therefore
create runtime-owned protected volumes and embed their capabilities. `build()`
then seals the environment. The first assembler attached to a dormant runtime
also seals its default annotation-task profile; later assemblers reuse that
profile and cannot replace it. Each assembler still owns its complete task
profile for `.task.new`. Runtime resources and the default profile are held as
siblings: the retained host may keep the resources alive, but has only a weak
link back to the profile. This avoids a runtime/profile ownership cycle while
allowing cached evaluation contexts to keep their resources usable. Selecting
another runtime or conflict strategy after runtime-bound builder state exists
makes construction fail.

`Assembler::create_volume` installs an explicitly initialized volume and
returns a Rust owner handle. The handle exposes one closed Glam
`{get,set,rewrite}` capability value. Possession is authority: the functions
are not members of the ordinary reflection API, while `.heap.*` remains rooted
to the session's private heap volume.

Each capability request embeds its globally unique `ReasoningSessionId`, its
session-local `VolumeId`, and its operation. Ordinary child tasks share the
host identity and may use capabilities passed to them. Logger, IDE, and other
foreign reasoning sessions reject the request before it enters a
store journal.

The owner explicitly revokes the complete volume and recovers its final
unforced value. Volume IDs are never reused. A missing `get` returns a latent
error value; blind sets and rewrites still enter the journal but fail
permanently at commit, so they cannot recreate a revoked volume. Revocation is
serialized with commits under the host lock and records a root change, causing
transactions that read the old volume to retry. Dropping the Rust owner does
not revoke it.

## Reusable Reflection Requests

- `.env Path` reads the active task host's immutable reasoning environment.
  The authoritative `glam.origin.inspect` capability projects assembler-created
  opaque compilation-origin values into ordinary provenance. It rejects other
  opaque values; callers choose which context value to inspect.
- `.log Severity Message` stages a diagnostic in the current transaction and
  publishes it through the session's diagnostic bus only after commit.
- `.dict_items Dict` returns ordered `{key,value}` records.
- `.eval Value` demands weak-head normal form and returns `ok:WHNF` or
  `err:Text`. A raw opaque `Value::Net` is already WHNF and is returned
  unchanged; only an explicit net-arity bridge observes its interface.
- `.task.new Effect` reserves an opaque child handle plus a private status
  query; launch is commit-ordered inside a transaction. The status query is
  updated only when the projected state changes between atoms `'launched` and
  `'blocked`, terminal tagged values `ok:Value` and `err:Error`, and the atom
  `'canceled`.
- `.task.join` waits directly and propagates non-success terminal states,
  adding `{task:{operation:'join, id:TaskId}}` when it forwards a child
  failure.
  `.task.status` returns that stored status value unchanged, while
  `.task.value` and `.task.error` project and transactionally wait for their
  matching terminal payload. `.task.ack_error` transactionally acknowledges
  present or future failure reporting without changing any of those
  observations; it is valid before launch, while running, and after
  termination. A terminal failure otherwise remains in the session's
  reporting ledger even though its active scheduler record has retired.
  `.task.cancel` journals a best-effort cancellation request.
  Same-transaction modifiers are folded into the reserved task before launch,
  so cancellation can bypass machine construction and acknowledgement can
  precede an arbitrarily fast failure. Task inspection creates no secondary
  scheduler work.

The immutable environment conventionally contains assembler-owned `glam`
identity plus client context. `glam.reasoning.role` distinguishes assembler,
logger, and future service sessions. `main` adds process arguments,
reflection-only arguments, and binary-preserving OS environment data. This is
data installed by the client, not command-line policy embedded in the
reflection API.

## Session Scheduling

`EvaluationSession` stores type-erased machines and a FIFO ready queue. A pump
claims a machine under the session mutex, polls it without the mutex, then
records its queued, blocked, or terminal state. Exact lazy wait tokens allow it
to prioritize a known producer chain. Coarse host generations currently wake
state observations; path journals decide whether an optimistic heap commit
must retry.

Foreground evaluation pumps only tasks needed by the lazy value it is trying
to observe. Shared workers may opportunistically poll any ready task. Explicit
reasoning drain continues without a time or step limit, includes newly spawned
tasks, and returns either terminal results or a structured stable-deadlock
report. Unfinished-task reports preserve retryably blocked evaluation errors
for library clients and CLI diagnostics without treating them as wake sources.

## Sources of Tasks

`anno refl:Effect Target` creates a lazy gate. Demand launches `Effect`; unit
success reveals `Target` without forcing it.

The built-in g front end also decorates ordinary module definitions and named
declared-object members with one-shot boundaries. Demand launches one scanner,
which waits for final `refl.*`, then launches each named task in deterministic
order. Guards are stored with explicit shared-heap effects under identities
derived from module paths or final object `spec.name`.

## CLI Logger Session

Configured `conf.log` is a reflection task in a separate session sharing the
same executor. Main-only effects expose the incoming diagnostic stream and its
open/closed state. Committed `.log` from the logger or its children goes to a
separate logger-session bus with a default-formatting subscriber rather than
back into that stream.

After assembly reasoning drains, `main` seals the input and lets the logger
finish its own task tree. A logger may use `.log_status` to stop once the queue
is empty and closed. Stable child deadlock, task failure, or a non-unit logger
result fails configured logging and activates the fallback path.

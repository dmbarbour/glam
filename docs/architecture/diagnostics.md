# Diagnostic Architecture

This document follows structured failures and diagnostic events from their
producer through runtime transport and the executable's configured logger. It
describes the current Rust bootstrap. Shape and publication invariants live in
[`../agent_context/diagnostics.md`](../agent_context/diagnostics.md).

## Structured Failures

Evaluation does not reduce an error to renderer-owned text at the point where
it occurs. A permanent `EvaluationFailure` retains a Glam diagnostic emission
and an ordered context stack. `EvaluationHalt` distinguishes that terminal
failure from retryable waits and unassigned promises, which remain scheduler
control states rather than diagnostic values.

The structured failure survives lazy-result caching, task terminal
publication, `.task.status`, public `Error`, and runtime readiness reports.
Only an explicit client projection chooses a human-readable summary. A public
`Error` keeps one primary structured diagnostic separately from additional
diagnostics emitted while attempting the operation; its Rust `Display` is an
emergency summary, not the semantic message.

Evaluator-owned context describes why a value was demanded rather than which
Rust function happened to fail. Conventional frames use tagged dictionaries
such as `eval`, `task`, `conf`, `import`, `asm`, and `g`. The built-in front end
also places its opaque source-origin token in shallow definition-initialization
frames. User `anno context:Frame Expr` values join the same ordered context
without becoming diagnostic bus events themselves.

## Diagnostic Bus

The embedding implementation for this section lives in
`api/diagnostics.rs`; runtime FIFO routing and outbox delivery live in
`api/runtime/events.rs`. This direction keeps publication policy independent
of runtime storage while preserving one runtime-owned transaction boundary.

Each reasoning role owns a non-buffering `DiagnosticBus`. A committed
publication:

1. assigns the next sequence number;
2. increments the authoritative severity counters; and
3. sends one immutable event to the subscribers present at that moment.

The bus never owns a queue of rendered or pending messages. Subscribers choose
buffering, dropping, indexing, forwarding, and display policy independently.
Counts therefore remain coherent even when no subscriber retains an event or a
bounded consumer drops it.

Assembler, macro-compilation, and logger roles use distinct buses. They may
share one evaluation runtime and executor, but their sequence domains and
severity counts do not alias. Batch exit reads the assembler and logger counts
independently.

The embedding library installs no default renderer. `Assembler` drops events
unless a client subscribes and never prints diagnostics itself. The `glam`
binary privately owns `conf.log` selection, supervision, fallback rendering,
terminal styling, and the process exit decision; it implements those policies
with the library's generic buses, ingresses, runtime event endpoints, and
effect-host facilities.

## Provenance and Enrichment

Compiler emission attaches hidden assembler provenance to the envelope only
after the surrounding transaction commits. Source provenance records the
artifact identity, digest of the exact compiled bytes, import chain, and
namespace extension without retaining module values or environments.

Front-end context origins are opaque capabilities because compiler-authored
context must not observe assembler paths or provenance. A privileged client or
reflection task explicitly calls the runtime-bound origin-inspection
capability. Neither evaluation nor rendering recursively searches arbitrary
diagnostics for opaque values.

An observer may enrich an event with authoritative `msg.severity` and
`msg.origin`, producing an independent object view. Enrichment does not mutate
the original event or publish a second message.

## Runtime Diagnostic Ingress

Before configuration compilation, the binary's batch/configuration layer binds
the assembler bus to its runtime and installs one long-lived
`DiagnosticIngress`. The ingress converts
each structured diagnostic to a runtime-rooted transport value before entering
runtime mutation admission. Valid bus publication remains counted even if
transport preparation fails; the ingress retains that terminal transport
failure and ordinary bus subscribers still receive the event.
Encoding and decoding both require the matching runtime's `Values` service;
the transport envelope is projected only inside its bounded value-access
region and cannot be decoded as a freestanding opaque `Value`.

The active ingress route initially points to a generic runtime input endpoint.
Its authoritative FIFO contains runtime roots, not a typed Rust diagnostic
queue. `.read_log` observes and consumes this endpoint through the logger's
ordinary `RuntimeEventJournal`, so a successful transaction commits its heap
edits, input claim, and output intents atomically.

Installing `conf.log` prepares its evaluation root first. The binary then switches
the ingress route and activates that root under one exclusive settlement guard.
No diagnostic can enter an intermediate state in which the logger route is
visible without its consumer root. Notifications happen after the guard is
released.

## Transactional Logger Output

The configured logger runs as a reflection task in its own demand session. It
shares the runtime coordinator, reflection heap, protected volumes, and
executor, while retaining its role environment and diagnostic bus.

Logger `.log` and `.write_stderr` calls are output intents in the same event
journal as `.read_log` input claims and reflection-store edits. Commit installs
identified outbox records in per-endpoint order. Delivery later claims one
record, releases every runtime lock, decodes the retained root, and invokes the
host callback. Independent endpoints may deliver concurrently; one endpoint
preserves commit order.

Input converters run and finish rooting before mutation admission. Output
decoders and adapters run after a delivery ticket has detached the retained
root from guarded state, and delivery terminalization reacquires runtime state
only after both callbacks return. None of these host callbacks inherits a
managed mutator.

The event layer transports unrestricted values and neither forces them nor
uses outer WHNF as an admission policy. `.log` places its possibly lazy message
inside an immediate transport envelope; `.write_stderr` performs its semantic
binary assertion before committing the output intent. A decoder remains free
to evaluate its delivered value explicitly. This choice is host policy rather
than a primitive delivery guarantee, and nested diagnostic fields may remain
lazy until formatting demands them.

Decode errors, callback errors, and caught panics become persistent Rust-layer
delivery failures. They remain reportable until explicitly acknowledged and
make batch execution fail independently of whether rendering succeeds.
Abandoned alternatives cannot leak diagnostics or bytes because their event
journals never commit.

Logger children created with `.task.new` inherit the complete logger profile,
including `.read_log`, `.write_stderr`, its environment, diagnostic
destination, and coordinated exit effects. Annotation-created reflection tasks
instead use the runtime's immutable default reflection profile and shared
reflection bus policy.

## Formatting Policy

The `glam` binary is the default terminal presentation client. It uses
`Assembler::reflection` to inspect context structure, resolve opaque origins,
and build `viewer` data. The runtime-cached Glam `Diagnostic -> Bytes`
formatter arranges that client-provided view; it does not choose diagnostic
semantics or perform privileged inspection.

The terminal viewer supplies the complete `viewer.header`, including location,
severity wording, punctuation, spacing, color, and terminal policy. Text
continuations use a deeper anchor than the `context:` header. Conventional
context tags receive compact summaries. A frame with a `msg` interface is
recursively enriched and formatted as a nested diagnostic-style view, but is
not republished and does not affect bus counts.

Formatting failure uses a minimal Rust renderer. That fallback is a last-mile
presentation path; it does not replace or acknowledge the authoritative task,
delivery, exit, or killed-work failure which caused the message.
Both Glam formatting and fallback rendering finish into owned bytes before the
terminal writer is invoked, so a writer is another mutator-free host callback
rather than an extension of evaluation.

## Logger Completion and Fallback

The diagnostic stream has no semantic closed state. A conventional logger loop
uses `.read_log` as the first branch and `.exit.success` as its terminal vote.
New committed input disturbs the vote and retries the read. Runtime readiness
and settlement, not a bus-close bit, determine whether the logger and every
other live machine agree that batch work may finish.

After settlement, `DiagnosticIngress::fallback` atomically transfers buffered
logger input to a configured fallback output and routes later publications
there. The settled report contains one-shot reporting obligations; rendering
does not maintain an ever-growing set of previously seen failure identities.
Fallback output is delivered through the ordinary outbox protocol, and work it
admits causes another runtime pump before final exit.

A stable deadlock may be explicitly killed and settled. Task failures, output
delivery failures, exit errors, and killed work make the batch unsuccessful
directly, even if their diagnostic presentation also fails. Valid assembly
bytes may therefore be written to stdout before reasoning produces a nonzero
exit status.

## Adjacent Owners

- [`assembly.md`](assembly.md): batch ordering, settlement, and process exit.
- [`evaluation.md`](evaluation.md): permanent versus retryable demand results,
  task publication, and readiness.
- [`reflection.md`](reflection.md): transactional `.log`, logger host
  specialization, and inherited task capabilities.
- [`../agent_context/diagnostics.md`](../agent_context/diagnostics.md): stable
  diagnostic shapes and publication boundaries.

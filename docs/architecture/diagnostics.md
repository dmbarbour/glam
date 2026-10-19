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

Projecting a failure to a diagnostic composes it without evaluating
(`senders-never-evaluate-diagnostics`): the emission, normalized lazily by
`DiagnosticObject`, extended by a mixin that prepends the failure's context
frames to `msg.context` (`DiagnosticContextDefs`), under a
`diagnostic:{from:Frames}` frame that carries those frames, so an error
raised while rendering it says where it came from. A `RuntimeFailureRoot`
shares its one
composition with every publication. Only a renderer evaluates it, usually on
the logger's thread; a message that fails to evaluate fails its renderer.
The Rust summaries `Diagnostic::message` and `line` never read a
diagnostic's value: they keep the Rust text a diagnostic was built from, or
summarize one built from a value statically.

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
`msg.origin`, producing an independent object view. Enrichment composes a
mixin without evaluating or observing the event, and does not mutate the
original event or publish a second message.

## Runtime Diagnostic Ingress

A runtime binds its assembler bus and installs one long-lived
`DiagnosticIngress`, which routes every diagnostic into the `diagnostics`
input of the `refl` machine under the root, as if its children had sent it,
so by default it passes through to the root's input
(`machines-communicate-through-wired-ports`,
`machines-inherit-an-environment`). The ingress
converts
each structured diagnostic to a runtime-rooted transport value before entering
runtime mutation admission. Valid bus publication remains counted even if
transport preparation fails; the ingress retains that terminal transport
failure and ordinary bus subscribers still receive the event.
Encoding and decoding both require the matching runtime's `Values` service;
the transport envelope is projected only inside its bounded value-access
region and cannot be decoded as a freestanding opaque `Value`.

Each transport value arrives at the root's input as a child's write would,
following the root's wiring. Before configuration compilation, the binary
wires that input on to its `logger` machine, a direct child of the root, whose
input buffer holds runtime roots, not a typed Rust diagnostic queue. The
logger, and its own child `fallback`, keep what reaches them in place of the
default pass-through to their parents. `.read_log` claims from the logger's
input through the logger task's ordinary `RuntimeEventJournal`, so a
successful transaction commits its heap edits, input claim, and output intents
atomically.

Since diagnostics wait at the logger from the start, installing `conf.log`
switches no route: its root is scheduled and reads what has gathered. Its
lifecycle closes the root's demand session when the root ends, abandoning
the tasks the root started and left unjoined.

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

### The `viewer` record

The terminal client (`src/bin/glam/rendering.rs`) enriches each diagnostic
with one `viewer` record before calling the formatter
(`src/g_syntax/diagnostic_formatter.rs`). That record is the whole contract
between them:

| Field | Value | Read by the default formatter |
| --- | --- | --- |
| `header` | Text placed before the first message line: location plus severity wording for a top-level diagnostic; `msg: ` or a severity header for a nested frame. | yes |
| `indent` | Spaces placed before each continuation line of `msg.text`. | yes |
| `context_lines` | The pre-rendered `context:` block, one text per line. The client fills it after enrichment, recursing into nested `msg` frames. | yes |
| `anchor_indent` | Spaces before the `context:` header. | no (the client uses it) |
| `auto_indent` | The continuation depth as an integer, currently 4. | no |
| `kind` | `"terminal"`. | no |
| `columns` | Terminal width from `COLUMNS`, default 80. | no |
| `color` | `none`, `ansi16`, `ansi256`, or `truecolor`; color is already applied inside `header`. | no |
| `location` | The location text alone. | no |
| `term`, `lang` | `TERM` and the first non-empty `LC_ALL`, `LC_MESSAGES`, or `LANG`; present only when set. | no |
| `source.file` | The display path of an absolute source; present only then. | no |

The default formatter returns, as binary,
`header ++ first line ++ ("\n" ++ indent ++ line)* ++ ("\n" ++ context line)* ++ "\n"`,
where the lines come from `msg.text`. The unread fields exist so a
configured formatter can make its own layout and color decisions without
privileged inspection.

Formatting failure uses a minimal Rust renderer. That fallback is a last-mile
presentation path; it does not replace or acknowledge the authoritative task,
delivery, exit, or killed-work failure which caused the message.
Both Glam formatting and fallback rendering finish into owned bytes before the
terminal writer is invoked, so a writer is another mutator-free host callback
rather than an extension of evaluation.

## Logger Completion and Fallback

This section describes the CLI's logger as it is during the runtime API's
transition: its exit vote and the settled reports the CLI renders are bridge
behavior, which `quiescence-reports-unresolved-state` replaces with machines
that outlive quiescence and a report that a machine emits.

The diagnostic stream has no semantic closed state. A conventional logger loop
uses `.read_log` as the first branch and `.exit.success` as its terminal vote.
New committed input disturbs the vote and retries the read. Runtime readiness
and settlement, not a bus-close bit, determine whether the logger and every
other live machine agree that batch work may finish.

When the configured logger ends, at settlement or before, the logger hands
over in one cut: it wires its input on to `fallback` and sends what the input
holds there, so the fallback receives every diagnostic in order and nothing
arrives between. Without a configured logger, the logger hands over at once.
The CLI renders what waits at the fallback with its default renderer: between
settlement pumps while no configured logger's fork holds the logger, and at the
end. Settled reports render directly. They are one-shot reporting obligations;
rendering does not maintain an ever-growing set of previously seen failure
identities, and work it admits causes another runtime pump before final exit.

Delivery and rendering run only on client threads, never on an evaluation
worker. The logger task only journals and commits its outputs, wherever it
runs; the CLI's logger thread delivers them between pumps
(`ScheduledEffectRun::run_with`), and its subscribers render there. The
hand-over runs on that thread too, as the configured logger ends, and the
fallback renders on the thread of the turn that reads it.

A stable deadlock may be explicitly killed and settled. Task failures, output
delivery failures, exit errors, killed work, and collector maintenance
failures make the batch unsuccessful directly, even if their diagnostic
presentation also fails. Valid assembly bytes may therefore be written to
stdout before reasoning produces a nonzero exit status.

A maintenance failure from a usable heap renders as an error diagnostic with
context `runtime:{op:'maintenance_failure, args:{failure:Id, kind:Kind}}`.
`Kind` is `'collector_panic` for a recovered trace panic or `'finalizer_panic`
when a panic left finalizer work pending; both leave a retry obligation. A
`'poisoned` failure never reaches a settled report, because a poisoned heap
reports `MaintenanceFailed` and stops settlement with host-text fallback (see
[`assembly.md`](assembly.md) "Batch Lifecycle").

## Adjacent Owners

- [`assembly.md`](assembly.md): batch ordering, settlement, and process exit.
- [`evaluation.md`](evaluation.md): permanent versus retryable demand results,
  task publication, and readiness.
- [`reflection.md`](reflection.md): transactional `.log`, logger host
  specialization, and inherited task capabilities.
- [`../agent_context/diagnostics.md`](../agent_context/diagnostics.md): stable
  diagnostic shapes and publication boundaries.

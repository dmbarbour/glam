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
publication, broken replies and machine error flags, public `Error`, and
runtime readiness reports.
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
such as `eval`, `runtime`, `conf`, `import`, `asm`, and `g`. The built-in
front end also places its opaque source-origin token in shallow
definition-initialization frames. User `anno context:Frame Expr` values join
the same ordered context without becoming diagnostic bus events themselves.

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
binary privately owns `conf.log` selection, supervision, default rendering,
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
each structured diagnostic to its enriched message (`Diagnostic::enrich`), a
runtime-rooted value composed without evaluation, before entering runtime
mutation admission: a diagnostics port carries the message glam reads and
writes, with `msg.severity` and `msg.origin` folded in. Valid bus
publication remains counted even if that preparation fails; the ingress
retains that terminal failure and ordinary bus subscribers still receive the
event. A Rust reader takes a port's message as a renderer does: the CLI's
default logger evaluates its severity, source and line (`port_diagnostic`).

Each message arrives at the root's input as a child's write would,
following the root's wiring. Before configuration compilation, the binary
wires the CLI's loggers, the root's children, and owns their wiring, out of a
configured logger's reach (`configuration/logger/mod.rs`):

- `logger` receives the root's diagnostics input and keeps it. A
  configured logger runs on it as a turn, `conf.log` an effect of that
  turn (`Machine::effect_run`), every family passing through: it reads
  with `.port.read 'diagnostics N` and writes with `.port.write 'stderr
  [Bin]`, and its `.log` goes to its diagnostics output, which the root
  wires to `default_logger` rather than back to its own input.
- `default_logger` runs continuously on a fork of its own: it renders each
  message that reaches it to bytes with the default renderer, counts it on
  a bus of its own, whose errors fail the batch, and writes the bytes to
  its `stderr` output. A configured logger may leave rendering to it and
  focus on enrichment, or render itself.
- `stderr` writes what reaches it, binaries, to the process's standard
  error, on a fork of its own, so both loggers' output keeps one order.

When the configured logger ends, at the batch's end or before, the logger
hands over in one cut: it wires its input on to its `unread` output, which
the root wires to `default_logger`, and sends what the input holds there,
so the default logger receives every diagnostic in order and nothing
arrives between. Without a configured logger, the logger hands over at
once. A configured logger's failure, and a result other than unit, are
published on the assembler's bus, and so reach the default logger.

## Transactional Logger Output

A configured logger's reads, writes and `.log` are port actions of its
turn's transaction: a cut commits them together, and an abandoned
alternative leaves none, since its journal never commits. Output endpoints,
whose delivery decodes a committed root on a client thread, remain for
hosts that wire a machine's output to the host
(`RuntimeOutputWriter`): their decode errors, callback errors and caught
panics become persistent delivery failures, reportable until acknowledged,
which fail a batch independently of rendering.

The event layer transports unrestricted values and neither forces them nor
uses outer WHNF as an admission policy. A turn's `.log` writes its possibly
lazy message, enriched, to its machine's diagnostics output; `stderr`
evaluates what reaches it to binary data as it writes, and reports a value
that is not, on the assembler's bus. Nested diagnostic fields may remain lazy
until formatting demands them.

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
presentation path; it does not replace or acknowledge the authoritative
failure, or the issue an analysis found, which caused the message.
Both Glam formatting and fallback rendering finish into owned bytes before the
terminal writer is invoked, so a writer is another mutator-free host callback
rather than an extension of evaluation.

## Logger Completion

The diagnostic stream has no semantic closed state. A conventional logger loop
reads with `.port.read 'diagnostics 1` in a cut, which waits, idle, while its
input is empty; new committed input wakes it and retries the read. The
batch, not a bus-close bit, decides when diagnostics stop: it ends at a
quiescence whose analysis has nothing new (`quiescence-is-analyzed`), which
counts the loggers' turns: a turn waiting where what it observed holds is
idle, and one woken by input, or rendering, is in progress, so the batch
waits on it (`EvaluationRuntime::wait_for_turns`). At the end, the batch
interrupts the configured logger, whose wait ends cleanly and which hands
over, then the default logger and `stderr`, each once its input holds
nothing more, and renders what is left directly. A logger whose input holds
unread diagnostics at quiescence has stopped reading: the batch halts it,
with an error diagnostic saying so, and the default logger renders its
backlog.

The batch publishes each issue an analysis at quiescence finds through the
assembler's bus, and so through the logger, never around it: each issue once,
by its identity (`PublishedIssues`), for the batch's life. Work that
publishing admits causes another runtime pump before the next analysis.

Rendering runs only on client threads, never on an evaluation worker: the
loggers' turns are the CLI's forks.

Nothing is killed at the batch's end. Each issue an analysis at quiescence
finds, a wait on a promise no one resolves, an erred wait, a backlog, a
pending obligation, a machine in error, or a retained output delivery or
collector maintenance failure, makes the batch unsuccessful directly, even if
its diagnostic presentation also fails. Valid assembly bytes may therefore be written to
stdout before reasoning produces a nonzero exit status.

A failed machine, a flagged child machine or a failed anonymous root
(`RuntimeAnalysis::failed_machines`), renders with its own diagnostic under
context `runtime:{op:'machine_failure, args:{machine:Path}}`, which the
terminal viewer summarizes as
`runtime: machine failure (machine refl/<abstract>/scanner/k)`, where
`<abstract>` stands for a key with no text, such as an anonymous root's.

A maintenance failure from a usable heap renders as an error diagnostic with
context `runtime:{op:'maintenance_failure, args:{failure:Id, kind:Kind}}`.
`Kind` is `'collector_panic` for a recovered trace panic or `'finalizer_panic`
when a panic left finalizer work pending; both leave a retry obligation. A
`'poisoned` failure never reaches an analysis's issues, because a poisoned
heap reports `MaintenanceFailed` and ends the batch's loop with host-text
fallback (see
[`assembly.md`](assembly.md) "Batch Lifecycle").

## Adjacent Owners

- [`assembly.md`](assembly.md): batch ordering, the batch's end, and process
  exit.
- [`evaluation.md`](evaluation.md): permanent versus retryable demand results,
  task publication, and readiness.
- [`reflection.md`](reflection.md): transactional `.log`, logger host
  specialization, and inherited task capabilities.
- [`../agent_context/diagnostics.md`](../agent_context/diagnostics.md): stable
  diagnostic shapes and publication boundaries.

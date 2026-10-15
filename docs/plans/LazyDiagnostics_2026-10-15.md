# Lazy Diagnostics — 2026-10-15

Status: complete; every step landed on 2026-10-15, the maintainer adding
`diag-origin-frame` and `diag-static-summaries` that day. The maintainer decided on 2026-10-15 that senders
never evaluate diagnostics; only renderers do
(`senders-never-evaluate-diagnostics`, in `Decisions.md`). Commits:
`3ad91c7c` (`diag-context-mixin`, `diag-lazy-failures`), `07038531`
(`diag-lazy-enrichment`), `fef17642`, `2bc61681` and `a9c0804e`
(`diag-renderers-evaluate`), `0849f579` (`diag-origin-frame`,
`diag-static-summaries`) and `e803e31b` (`diag-static-summaries`). Later
steps revised earlier ones the same day; the step texts record each as it
landed, with notes where a later step replaced it.

Retirement (2026-10-15): the decision and its rules are in standing docs
(`agent_context/diagnostics.md` "Publication and Counts",
`architecture/diagnostics.md`). No code or test names this plan. It is
still cited by `Decisions.md` ("Recorded in"), the plans README and the
explicit heap holds plan, and `Decisions.md`'s "History docs cited" table
has no row for "lazy diagnostics plan" yet.

## Purpose

The runtime still evaluates diagnostics where it builds or enriches them:

| Where | What it evaluates |
| --- | --- |
| Settlement | a failed task's `.task.status` diagnostic, before taking the gate (`status-diagnostic-before-gate`) |
| `.eval` request | its `err` result |
| `TaskHalt::diagnostic`, readiness's `reasoning_diagnostic` | public `Diagnostic`s for task failures and readiness reports |
| `Error::from_eval`, `Error::with_context` | every public `Error` from an evaluation failure |
| `Diagnostic::enrich`, `enrich_with`, `apply_updates`, `with_context` | public enrichment |
| Macro expansion | failure causes; the front end's cause frames and macro context (`g_syntax/parser/source.rs`) |

Each runs drivers in an isolated evaluation session. The quantum boundary
lends its mutator to them (`gc-hold-boundary-release`), and an isolated
session is dropped with that mutator suspended, in case closing it settles
work the normalization launched. Rendering is the only consumer that needs
the evaluated object, and it usually runs on the logger's thread.

## Design

A diagnostic is an unevaluated composition built with value access only:

```
anno context:diagnostic:{from:Frames}
  (diagnostic_object Emission with mixins)
```

- `DiagnosticObject` already normalizes an emission to an object lazily;
  `Diagnostic::prepare_enrichment` already composes assembler metadata this
  way with `ObjectWithDefs` and `ObjectOverrideDefs`.
- Context frames prepend with the mixin
  `msg.context ::= \ prior -> frames ++ prior`, where an absent context
  (`{}`) contributes nothing and a single non-list frame counts as one. Core
  functions are nets, which only the front end lowers, so a
  compiler-private definitions builtin, `DiagnosticContextDefs`, partially
  applied to its frames as `ObjectOverrideDefs` is to its updates, stands
  for that mixin.
- A message that fails to evaluate fails its renderer, under a
  `diagnostic:{from:Frames}` frame carrying the composed frames
  (`diag-origin-frame`; first `eval:{op:'diagnostic_message}`). There is
  no fallback text.

## Steps

### Context mixin (`diag-context-mixin`)

Add `DiagnosticContextDefs` and the composition above
(`failure_diagnostic_in`, built with access), and test that it renders as
the eager normalization did, that an absent or single context normalizes to
a list, and that a failing message carries the frame.

Landed 2026-10-15. `DiagnosticContextDefs` is an object builtin machine:
it demands `prior`, its `msg` interface and the existing context, which
happens only when a renderer demands the diagnostic.
`diagnostic_with_frames_in` composes
`anno context:{eval:{op:'diagnostic_message}} (DiagnosticObject message
with DiagnosticContextDefs frames)`.

### Lazy failure diagnostics (`diag-lazy-failures`)

Every projection of a failure to a diagnostic composes it:
`RuntimeFailureRoot::diagnostic`, the `.task.status` and `.eval` values,
`TaskHalt::diagnostic`, readiness reports, `Error::from_eval` and macro
causes. Settlement stops building diagnostics, so the ordering and retry
of `status-diagnostic-before-gate` go, with the drivers that borrow a
mutator for diagnostics and the suspended drop of their isolated session.

Landed 2026-10-15, with three findings:

- **Rust summaries.** `Diagnostic::message` and `line`, which the default
  logger's header, its fallback rendering, transport and
  `ReasoningFailure::message` use, summarized the evaluated emission.
  `conventional_summary` now reads through a composition's recipe (a lazy's
  cached result, or the message argument of `Anno`, `ObjectWithDefs` and
  `DiagnosticObject`) without evaluating; the transport envelope carries
  the message. `Diagnostic::from_failure` composes a failure's diagnostic.
  `project_blocked_diagnostic`, a projection for rendering, evaluates its
  summary (`summarized_for_rendering`); readiness's separate
  non-normalizing projection went, its reason (an isolated session) gone,
  and every report of a retained failure shares its one composition
  (`RuntimeFailureRoot::diagnostic`), so a renderer's evaluation serves
  later reports' summaries too. `diag-static-summaries` replaced this the
  same day: `conventional_summary`, `summarized_for_rendering` and
  `project_blocked_diagnostic` went (`0849f579`).
- **Advisory status.** A release under a mutator deferred its nonterminal
  status publication past its gate (`gc-hold-boundary-release`), so a
  runtime observed stable could show a blocked root still `Launched`; a
  test under load caught it. A release expected to change a published
  status now runs suspended, under one gate with its transition, and a
  test forces the interleaving at the end of the release's gate.
- **Test helpers.** Tests that read a diagnostic's structure now evaluate
  each level as a renderer does; the in-region eager projection
  (`eval::failure_diagnostic_value_in`) went, its one production caller, a
  composed child failure's cause frame, now composes.

The builders that still evaluate, with the mutator-borrowing drivers and
the isolated session's suspended drop, serve enrichment until
`diag-lazy-enrichment`.

### Lazy enrichment (`diag-lazy-enrichment`)

`Diagnostic::enrich` (as `prepare_enrichment` does), `enrich_with`,
`apply_updates`, `with_context`, `Error::with_context`, and the front end's
cause frames and macro context compose mixins instead of evaluating.

Landed 2026-10-15. `enrich` composes what `prepare_enrichment` did, which
went; `enrich_with` and `apply_updates` compose `diagnostic_object message
with updates` (`apply_updates_in`), and `with_context` prepends one frame,
keeping its summary. The eager builders went with the isolated session and
its suspended drop, and the client-demand driver returned to the shape it
had before `gc-hold-boundary-release`, which lent it a mutator only for
diagnostics. Composition can no longer fail: a failing update surfaces
where the composed value is rendered, as a test of a viewer update now
shows.

### Renderers evaluate (`diag-renderers-evaluate`)

Renderers, and any Rust reader that inspects a diagnostic's fields,
evaluate it where they render it.

Landed 2026-10-15. The default logger already evaluated what it formats
(the message through the Glam formatter, context frames through its
evaluator); its header's line came from the Rust summary, which an
unevaluated message lacks, so it now evaluates `msg.location.line` when
the summary has none. `project_blocked_diagnostic` evaluates its summary
(`diag-lazy-failures`); it went with `diag-static-summaries`.

### Origin frame (`diag-origin-frame`)

Added by the maintainer on 2026-10-15. The `eval:{op:'diagnostic_message}`
frame names only what failed. A composed diagnostic instead evaluates
under `anno context:diagnostic:{from:Frames}`, the frames it prepends, so
an error raised while rendering it carries where the diagnostic came from
through the error side channel, a redundant expression of its location
and context, as the eager normalization's fallback text did.

Landed 2026-10-15: `diagnostic_with_frames_in` annotates with
`{diagnostic:{from:Frames}}`, and a test shows a failing message's render
error carrying the original failure's frame.

### Static summaries (`diag-static-summaries`)

Added by the maintainer on 2026-10-15: eliminate the Rust summaries read
from diagnostic values. A diagnostic built from Rust text, including the
front end's, keeps that text and line, passed beside its emission rather
than read back from it. A diagnostic built from a value summarizes
statically: `EvalFailure` for a failure, `EvalDiagnostic` for another
emission. `conventional_summary`, its reading through compositions,
readiness's evaluated summary and the public
`RuntimeDeadlockWork::blocked_error` go. (The coordinator's internal
snapshot field `blocked_error` stays; readiness projects it to the
public `blocked_diagnostic`.) Reflection semantics
may change, since they do not affect the assembly result's reproducibility
(maintainer); `.eval` returns its value or `err:Error`, and nothing
required that `Error` evaluated.

Landed 2026-10-15. Compile diagnostics carry their text and line beside
their emission (`CompileDiagnostic`), and the transport envelope carries
the message. `ReasoningFailure::message` is static too. With no reader of
the blocked failure left, deadlock snapshots and settlement reports no
longer keep it beside its diagnostic (maintainer): its composed diagnostic
retains what a renderer needs, which the retention tests now check by
rendering it after a collection. Production no longer builds isolated evaluation sessions
at all, so they are test-only. About forty tests that asserted the summary
of a diagnostic built from a value now evaluate its `msg.text`, as a
renderer does, through test helpers (`rendered_text_for_test`).

## Measurements

The profiling workloads build no failure diagnostics, so the plan changes
no work there; their outer heap regions are unchanged. Instructions rose
0.24% against `gc-hold-boundary-release` (`sum_800` 0.7%). Callgrind on
`sum_800` traced 0.7 M to `ManagedLazyAccess::source_snapshot`, which the
summary's new call site moved out of line in the lazy poll's hot path, and
1.1 M to `EvaluatorStepContext::finish`'s own code, about 14 instructions
per call with unchanged callees: a codegen shift, not more work. Forcing
`source_snapshot` inline recovers part: 0.19% over all workloads, at most
0.6%. `diag-static-summaries` removed that caller and the forced inline:
0.17% over all workloads, at most 0.5%, the codegen shift in `finish`.

## Open questions (2026-10-15)

- The plans README lists "Public diagnostic projection" as an open item:
  review the transitional `Diagnostic::from_parts` policy, which keeps
  shallow `message` and `line` views beside the emission. Since
  `diag-static-summaries`, keeping a Rust-built diagnostic's text and line
  beside its emission is the design, and a value-built one summarizes
  statically. Does that close the item, or do other conveniences still
  need review?

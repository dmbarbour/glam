# Lazy Diagnostics — 2026-10-15

Status: in progress; `diag-context-mixin`, `diag-lazy-failures` and
`diag-lazy-enrichment` landed on 2026-10-15. The maintainer decided on 2026-10-15 that senders
never evaluate diagnostics; only renderers do
(`senders-never-evaluate-diagnostics`, in `Decisions.md`).

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
anno context:{eval:{op:'diagnostic_message}}
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
- A message that fails to evaluate fails its renderer, with the
  `eval:{op:'diagnostic_message}` frame. There is no fallback text.

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
  later reports' summaries too.
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

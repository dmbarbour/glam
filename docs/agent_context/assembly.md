# Assembly and Client Invariants

These rules protect the public construction boundary, source authority,
configured CLI isolation, and batch lifecycle. Current control flow lives in
[`../architecture/assembly.md`](../architecture/assembly.md); user commands and
configuration live in [`../CLI.md`](../CLI.md).

## Construction and Values

- `AssemblerBuilder` fixes one `SourceSystem`, evaluation runtime, conflict
  strategy, reflection environment, diagnostic subscriptions, and reasoning
  resources before constructing one live assembler session. Do not add fluent
  `Assembler` methods which silently replace that session.
- The library assigns neither `configuration` nor `assembly` roles. `main` is
  one client which chooses those roots.
- Public `Value`s belong to exactly one `EvaluationRuntime`. Construct through
  that runtime or assembler's `Values`; public consumers reject foreign roots
  before unwrapping recursive core values.
- Ordinary accessors do not silently evaluate arbitrary values.
  `Assembler::reflection` is the privileged, session-bound inspection surface
  for executable and IDE policy.
- External task hosts receive `RuntimeTaskCapability`, not raw
  `RuntimeSharedResources` or mutation admission.
- `Assembler::net` is a scoped checked facade. Raw graph nodes, cursors,
  scheduling state, and fan histories remain internal.

## Source Authority

- An assembler has one immutable `SourceSystem`. Relative imports use the
  resolver carried by the loaded `SourceArtifact`; inline scripts without a
  resolver cannot import.
- A source digest covers the exact bytes supplied to the compiler. Diagnostic
  provenance and manifests use that retained digest, never a later rescan.
- Re-reading one local path with changed bytes during assembly is an error. A
  change discovered only by the final recheck is a warning because it did not
  affect the completed demand.
- Standalone manifest checking, source inspection, help, and version operations
  do not construct an assembler or load configuration.
- The public built-in `.g` inspection facade returns summaries and diagnostics;
  syntax AST and lowering capabilities stay private.

## CLI and Configuration

- Bootstrap parsing consumes `OsString` and produces a typed
  `TopLevelCommand`. `main` executes that command and does not reinterpret
  individual assembly flags.
- CLI worker count is `--workers`, then `GLAM_WORKERS`, then zero.
  Configuration and configured CLI rewriting run with zero workers; the
  selected runtime activates its executor once afterward.
- A bare command runs `conf.cli` as an isolated all-results search. Its API has
  standard control, read-only `.env`, CLI-local diagnostics, and CLI
  readers/writers, but no shared heap or task API. Branch journals never
  commit.
- `process.cli.args` is the concrete user-provided argument list.
  `process.args` and `process.refl_args` are builder-created promises while
  `conf.cli` chooses one canonical semantic plan. Do not build a second
  assembler or reparse projected arguments to cross that seam.
- `.read.token` runs a restricted nested all-results machine against one UTF-8
  argument and requires complete token consumption. Token effects may not
  escape into the outer CLI host.
- `.case` is scoped lazy explanation metadata. It does not change `.alt` order
  or force its value during successful command construction.
- Completion is shell-neutral. Preserve the absent-versus-empty active
  argument distinction, OS path values, and the v0 count-framed protocol. Do
  not expose lossy display text or internal candidate kinds.
- `--parse_cli` forms use the same configured expansion but execute no command
  and activate no workers.

## Diagnostics and Logger Supervision

- An assembler has no default retention or renderer. `main` binds a runtime
  diagnostic ingress and owns configured logger/fallback policy.
- `conf.log` has its own demand session and diagnostic bus while sharing the
  runtime coordinator, reflection heap, and executor.
- The logger is required to return unit. Its preferred loop reads input before
  a terminal `.exit.success` vote; arriving input disturbs the vote.
- The diagnostic stream is never semantically closed. Stable runtime readiness
  plus explicit settlement determines completion.
- Report rendering may admit more runtime work. Pump and settle again until the
  fallback path and runtime are both stable.
- Assembler and logger error counts are read separately. Retained task,
  delivery, exit, and killed-work reports independently make the batch fail.
- Valid stdout may accompany a failing exit status when later reasoning or
  diagnostics fail.

## Batch Ordering

For a selected assembly command, preserve this lifecycle:

```text
load configuration on dormant runtime
  -> optionally rewrite bare CLI and resolve environment promises
  -> activate workers once
  -> compile assembly
  -> demand and write valid asm.result
  -> finalize source tracking and optional manifest
  -> pump runtime to stable readiness or deadlock
  -> settle exit votes, or explicitly kill and settle deadlock
  -> render retained reports through fallback
  -> repump any work admitted by rendering
  -> drain fallback output
  -> combine result/report failures and both bus counts for exit status
```

Do not introduce an arbitrary scheduler timeout or step budget. Productive
reasoning may run indefinitely; stable blocked work is reported as deadlock.

## Verification Anchors

- `tests/public_api.rs`: runtime provenance and embedding surface.
- `tests/cli.rs`: bootstrap validation and command behavior.
- `tests/hello_assemblies.rs`, `tests/executable_samples.rs`, and
  `tests/sample_sources.rs`: end-to-end assembly behavior.
- `src/main.rs` tests: logger activation, settlement, fallback ordering, and
  exit accounting.
- `src/api.rs` tests: diagnostic ingress, runtime event endpoints, public
  readiness, and settlement.

When a lifecycle bug is order-dependent, add explicit barriers around the
relevant publication or settlement boundary. Repetition without forced
ordering is not sufficient evidence that a race is fixed.

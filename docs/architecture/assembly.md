# Assembly and CLI Flow

This document describes the current Rust bootstrap. It is an implementation
map, not the eventual assembler contract.

## Library Boundary

`api::Assembler` is the primary embedding facade. It owns a source host and one
internal `ReasoningSession`. That session groups the immutable reflection
environment, reflection host/heap, diagnostic bus, task scheduler, and its
attachment to the shared evaluation runtime. Clients
choose module paths and inputs; the library does not assign special meaning to
`configuration` or `assembly`.

`main` is one client. It chooses those two roots, supplies CLI-derived values,
installs the local-files host and subscribes a diagnostic queue, requests
`asm.result`, and decides process output and exit status.

## Module Construction

```text
ModuleBuilder + ordered ModuleInput values
  -> Assembler::build_module_inner
       -> Host reads source bytes
       -> CompileContext hides source/import provenance
       -> selected front end parses and lowers one source
       -> imports re-enter the same Assembler session
  -> module final-definition promise closes the module fixpoint
  -> assembled module Value
```

Inputs are applied from last to first so earlier CLI inputs override later
ones. A front end sees raw bytes, a relative import request, and compiler
capabilities. The assembler host retains absolute source identity, qualifies
names, performs loads, and builds the import chain.

Each source compilation receives a local invocation ID. Diagnostic envelopes
retain a compact root-to-parent chain of relative requests and namespace
extensions, without retaining module values or environments. Observers choose
when to enrich that provenance into `msg.origin`.

## Diagnostics and Logging

Each reasoning session owns a non-buffering `DiagnosticBus`. Publishing a
committed envelope assigns a session-local sequence number, increments a
coherent severity counter, and sends the immutable event to the subscribers
present at that point. Subscribers own all buffering, dropping, rendering,
forwarding, and indexing policy. Changing an assembler's default subscription
does not rebuild its reasoning session.

`Assembler` does not render diagnostics. Before compiling configuration, the
CLI subscribes its own unbounded queue to the assembler bus so bootstrap
messages are observable by `conf.log`. The embedding facade itself is silent
by default and owns no retention policy. Queue consumption does not change the
bus's authoritative counters.

If configured, `conf.log` runs in its own evaluation session and owns a
separate diagnostic bus, while sharing the assembler's executor. It reads the
sealed-or-open assembler-bus subscription through main-only effects. Its own
`.log` writes and synthetic logger failures publish to the logger bus, whose
default subscriber enriches them with terminal `viewer` context, applies the
cached closed Glam formatter, and writes stderr. Logger output therefore cannot
feed back into assembler input. Formatter failure falls back to a minimal Rust
renderer.

The logger is wrapped with the native equivalent of `(=>> .r ())`; returning a
non-unit result is an error. A logger failure produces a synthetic diagnostic,
then remaining messages use the default path.

## Local Files and Manifest

`local_files` wraps the system host for CLI execution. It retains the SHA-256
digest of the bytes returned by every successful local read. Reading the same
path with different contents during assembly is an error. A final recheck only
warns, because an edit after the last read did not affect the produced result.

`--manifest` writes the retained path/digest set, including configuration and
transitive imports. Paths below the invocation directory are made relative;
hashes never come from a later rescan.

## Batch Lifecycle

```text
compile configuration and assembly
  -> evaluate and write valid asm.result bytes
  -> drain assembler reflection reasoning
  -> emit task-failure or deadlock diagnostics
  -> recheck observed local files and write optional manifest
  -> seal diagnostic input
  -> finish or fail conf.log
  -> exit nonzero if any error diagnostic was committed
```

Valid stdout may therefore accompany a failing exit status when reasoning or
diagnostics report an error. Main checks the assembler and logger bus error
counts independently; both are independent of queue retention, reads, and
rendering.

The current CLI also exposes temporary `--parse` inspection. `--workers`
overrides `GLAM_WORKERS`; zero workers is the default. Raw process arguments
remain in `process.args`; repeated `--refl` values are additionally collected
in `process.refl_args` and excluded from `asm.args`, while arguments after `--`
form `asm.args`.

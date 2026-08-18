# Assembly and CLI Flow

This document describes the current Rust bootstrap. It is an implementation
map, not the eventual assembler contract. Built-in compiler internals live in
[`front_end.md`](front_end.md); structured diagnostics and configured logging
live in [`diagnostics.md`](diagnostics.md).

## Library Boundary

`api::Assembler` is the primary embedding facade. `AssemblerBuilder` selects
one immutable `SourceSystem`, an evaluation runtime, diagnostic subscriptions,
and reasoning configuration before creating exactly one internal
`ReasoningSession`. That session groups the immutable reflection environment,
role-specific host and diagnostic bus, and one evaluation demand-owner lease.
The selected `EvaluationRuntime` owns the shared reflection heap, protected
volumes, work coordinator, and executor attachment. Clients choose module
paths and inputs; the library does not assign special meaning to
`configuration` or `assembly`.

The embedding value boundary has three explicit roles. `Assembler::values`
constructs runtime-local literals and lazy semantic composition without
demand. `Assembler::evaluator().eval` performs ordinary outer-WHNF demand and
returns `EvaluatedValue`, whose scalar and strict-array extractors cannot be
called on an arbitrary unresolved root. `Assembler::reflection` owns
runtime-specific pre-demand kind, atom-key, dictionary-entry, associated
metadata, and opaque-origin inspection. Bare `Value` exposes runtime identity
but no scalar, kind, undefined, or structural observation. Host policy should
extend reflection through a constructed capability when it needs another
privileged observation; it should not add client-specific interpretation or
rendering builtins to core evaluation.

Runtime input and output FIFOs retain unrestricted `Value` roots. Admission,
journaling, commit, and delivery do not demand a payload or require WHNF. A
host output decoder which needs semantic data explicitly captures an assembler
and evaluates the delivered value before using `EvaluatedValue` extraction.
This is not an isolation boundary: nested values may remain lazy, and a future
phase barrier would require an explicit opaque envelope and opening
capability.

`main` is one client. `cli::dispatch_bootstrap` first turns raw `OsString`
arguments into a typed `TopLevelCommand`; `main` performs the requested I/O but
does not interpret individual assembly flags. A hyphen-leading command uses
the bootstrap plan directly. A bare command loads configuration and runs
`conf.cli` through the CLI effect specialization before producing the same
typed plan. For assembly main chooses the two module roots, supplies CLI-derived
values, installs a `FileSourceSystem` and subscribes a diagnostic queue,
requests `asm.result`, and decides process output and exit status.

## Module Construction

```text
ModuleBuilder + ordered ModuleInput values
  -> allocate one CompilationExecution for this top-level build
  -> Assembler::build_module_inner
       -> SourceSystem returns an immutable SourceArtifact
       -> artifact supplies identity, SHA-256 digest, and relative resolver
       -> CompileContext hides source/import provenance
       -> selected front end parses and lowers one source
       -> imports re-enter the same Assembler session and CompilationExecution
  -> module final-definition promise closes the module fixpoint
  -> drain compilation-session macro reflection reasoning
  -> assembled module Value
```

Inputs are applied from last to first so earlier CLI inputs override later
ones. A front end sees raw bytes, a relative import request, and compiler
capabilities. The assembler retains source identity and digest, qualifies
names, performs loads through artifact-carried relative resolvers, and builds
the import chain. Inline scripts have no resolver and therefore cannot import.

Each source compilation receives a local invocation ID. Diagnostic envelopes
retain compact source/import provenance without retaining module values or
environments. `CompilationExecution` shares import lookup and one macro demand
session across all inputs and recursive imports in the build. See the
[front-end architecture](front_end.md) for parsing, macro staging, lowering,
and compiler-value caching, and the [diagnostic architecture](diagnostics.md)
for provenance and macro-bus bridging.

## Diagnostics and Logging

`Assembler` is silent by default and owns neither buffering nor rendering.
Batch `main` installs a runtime diagnostic ingress, optionally runs `conf.log`
as a separate demand session, and retains independent assembler and logger bus
counts. The logger reads transactionally, buffers output in runtime outboxes,
and votes to exit only after input is quiescent. Returning non-unit is an error.

Diagnostic structure, provenance, ingress activation, transactional output,
viewer enrichment, the cached Glam formatter, and fallback delivery are owned
by [`diagnostics.md`](diagnostics.md). This assembly flow owns only their place
in batch ordering and final exit policy.

## Local Files and Manifest

`FileSourceSystem` owns CLI local-file acquisition and consistency. It retains the SHA-256
digest of the bytes returned by every successful local read. Reading the same
path with different contents during assembly is an error. A final recheck only
warns, because an edit after the last read did not affect the produced result.

`--manifest` writes the retained path/digest set, including configuration and
transitive imports. Paths below the invocation directory are made relative;
hashes never come from a later rescan. Each entry records the percent-encoded
path, digest algorithm, and hexadecimal digest in tab-separated fields, so the
algorithm remains explicit even if a manifest combines different source kinds
or digest formats in the future.

Standalone `--check_manifest PATH` re-reads every entry relative to the
invocation directory when its recorded path is relative. It prints every
changed or unreadable path and exits unsuccessfully if any differ;
`--quiet` suppresses that changed-file output. Manifest checking does not
construct an assembler or load configuration.

## Batch Lifecycle

```text
construct one dormant assembler and compile configuration
  -> for bare input, search all isolated conf.cli alternatives
  -> select one semantic command and resolve canonical environment promises
  -> activate the selected worker count exactly once
  -> compile assembly
  -> evaluate and write valid asm.result bytes
  -> recheck observed local files and write optional manifest
  -> pump all runtime work to a stable readiness or deadlock
  -> settle exit votes, or explicitly kill and settle a stable deadlock
  -> render retained task, delivery, exit, and killed-work reports via fallback
  -> repump if rendering admitted work, then drain fallback output
  -> finish conf.log and read assembler/logger bus counts independently
  -> exit nonzero for result/report failure or either bus's error count
```

Valid stdout may therefore accompany a failing exit status when reasoning or
diagnostics report an error. Retained runtime failures establish failure before
rendering, so a failed fallback adapter cannot turn an unsuccessful batch into
a successful one. Main checks the assembler and logger bus error counts
independently; both are independent of queue retention, reads, and rendering.

Standalone `--parse` inspects one built-in `.g` source through the narrow
library report without constructing an assembler or loading imports. Its
diagnostics and summaries go to stdout; `--quiet` keeps only the exit status
and `--verbose` includes declaration rows.

For assembly, `--workers` overrides `GLAM_WORKERS`; zero workers is the default.
Configuration and `conf.cli` always run before activation with zero workers.
Bootstrap parsing retains paths and unrelated arguments as OS strings instead
of requiring process-wide UTF-8. `process.cli.args` records the arguments the
user supplied, while `process.args` is their final canonical interpretation;
both exclude the executable name. The canonical value is a promised environment
slot while `conf.cli` runs, so a rewrite cannot depend on its own result.
Repeated `--refl` values are additionally collected in
`process.refl_args` and excluded from `asm.args`, while arguments after `--`
form `asm.args`.

The configured CLI exposes standard control effects, read-only `.env`,
branch-local `.log`, and CLI reader/writer operations. It exposes neither the
shared heap nor reflection-task operations, and its outer branch journals are
inspected rather than committed. `--parse_cli` prints the selected canonical
arguments one per line; `--parse_cli.0` uses NUL delimiters. Neither executes
the command nor activates workers.

`.case Explain Parse` scopes lazy, structured explanation metadata around one
configured parser branch without changing `.alt`. A failed reader captures its
active outer-to-inner case stack at the same argument/token frontier as its
expectation. Ordinary successful construction never observes `Explain`.
Completion returns those values as structured candidate/expectation metadata;
parse and ambiguity errors render plain text or the conventional `usage`,
`summary`, and `details` fields. Published error diagnostics retain the original
values at `cli.cases` alongside `msg.text`. Higher-level choice helpers remain
configuration/library code.

Configured parsing records an argument index and token-relative byte offset for
failed reader expectations. `.read.token` starts a nested restricted effect
search over one UTF-8 argument; literal, capture-free Glam text-pattern, Unicode
scalar, and end readers advance its byte cursor, and every complete nested
result resumes the outer continuation. Regex matching returns its whole match
as `{span:Text}`, is anchored at the current token cursor, and follows
the text-pattern contract's ordered, leftmost-first preference.

`cli::complete_configured` runs the same outer parser with an optional active
argument split into prefix and suffix. Readers at that frontier record
candidates and expectations, then fail so sibling alternatives remain visible.
Candidates at shallower frontiers are discarded. Complete keyword, path, and
token candidates are replayed against the unchanged suffix and later
arguments; command edits remain isolated throughout. Filesystem completion
preserves OS path values, offers folders for navigation, and filters terminal
entries by the path reader's kind.

`--completions v0` carries mode plus counted arguments before and after the
cursor as ordinary OS arguments. `active` mode additionally carries prefix and
suffix; `absent` preserves the distinction between no argument and an empty
argument. Lexical routing sends bootstrap options to the Rust basic completer
and bare commands to `conf.cli`; a complete `--parse_cli` or `--parse_cli.0`
prefix explicitly delegates its tail. Successful output is only the complete
replacement arguments separated by NUL. `--completion_script NAME` prefers
`conf.completion_script.[NAME]` and otherwise offers minimal Bash and Zsh
bindings; shell-specific quoting remains outside the completion protocol.

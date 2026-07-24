# Assembly and CLI Flow

This document describes the current Rust bootstrap. It is an implementation
map, not the eventual assembler contract.

## Library Boundary

`api::Assembler` is the primary embedding facade. `AssemblerBuilder` selects
one immutable `SourceSystem`, an evaluation runtime, diagnostic subscriptions,
and reasoning configuration before creating exactly one internal
`ReasoningSession`. That session groups the immutable reflection
environment, reflection host/heap, diagnostic bus, task scheduler, and its
attachment to the shared evaluation runtime. Clients
choose module paths and inputs; the library does not assign special meaning to
`configuration` or `assembly`.

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
  -> Assembler::build_module_inner
       -> SourceSystem returns an immutable SourceArtifact
       -> artifact supplies identity, SHA-256 digest, and relative resolver
       -> CompileContext hides source/import provenance
       -> selected front end parses and lowers one source
       -> imports re-enter the same Assembler session
  -> module final-definition promise closes the module fixpoint
  -> assembled module Value
```

Inputs are applied from last to first so earlier CLI inputs override later
ones. A front end sees raw bytes, a relative import request, and compiler
capabilities. The assembler retains source identity and digest, qualifies
names, performs loads through artifact-carried relative resolvers, and builds
the import chain. Inline scripts have no resolver and therefore cannot import.

Each source compilation receives a local invocation ID. Diagnostic envelopes
retain a compact root-to-parent chain of relative requests, namespace
extensions, tagged source identities, and the digest of every artifact's exact
bytes, without retaining module values or environments. Observers choose when
to enrich that provenance into `msg.origin`.

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
search over one UTF-8 argument; literal, capture-free `regex-lite`, Unicode
scalar, and end readers advance its byte cursor, and every complete nested
result resumes the outer continuation. Regex matching returns its whole match
as `{span:Text}`, is anchored at the current token cursor, and follows
`regex-lite`'s leftmost-first preference.

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

# Module Split Plan — 2026-08-18

Status: Phase 5A complete; Phase 5B is next.

This is a dated review and transition plan. Module shape will continue to
change as the bootstrap grows, so a later review should create a new dated
plan rather than silently treating this inventory as permanent architecture.

The immediate purpose is to close the final module-structure item from
[`CodeCleanup_2026-08-15.md`](../reviews/CodeCleanup_2026-08-15.md): inspect
the settled implementation, identify files which still combine distinct
ownership roles, and plan low-risk splits along those boundaries.

## Intent

Review every Rust source file before selecting split targets. Large size is a
signal, not a finding. A file warrants division when the resulting child
modules would own coherent state, invariants, protocols, or semantic roles and
would reduce visibility or change coupling. A large implementation of one
coherent algorithm may remain intact.

The review must distinguish:

- production code from colocated tests;
- facade aggregation from implementation ownership;
- architectural responsibilities from merely repeated mechanisms;
- stable ownership seams from representations still scheduled to change; and
- navigation improvements from splits which would only add module ceremony.

No semantic behavior, public API, scheduling rule, lock ordering, or diagnostic
policy changes merely because code moves between modules. Phase 1 separately
authorizes one deliberate API correction: executable command and configuration
policy moves out of the library, after generic host mechanisms replace its
private dependencies, and the accidental public `glam::cli` surface is removed.

## Package and Product Boundary

This remains one Cargo package containing one library crate and the primary
`glam` binary crate. There is no planned workspace or package split. Glam is
first a command-line tool and second an embeddable library; the library exists
to expose the assembler, evaluation runtime, values, sources, diagnostics, and
generic effect-host mechanisms without owning the executable's product policy.

The complete configuration model belongs to the binary. This includes
`GLAM_CONF` discovery, configuration assembly, `conf.env`, `conf.cli`,
`conf.log`, future `conf.ide`, shell completion integration, process I/O, and
exit policy. The library may expose generic operations used to implement those
features, but must not know their command names, configuration paths, command
models, rendering policy, or lifecycle ordering.

Rust library and binary targets are separate crates even when they share a
package. The binary therefore consumes the library's public API; Rust has no
package-private visibility spanning those targets. Phase 1 must close real
embedding gaps with narrow reusable APIs rather than expose `CoreValue`,
`EvalContext`, arbitrary opaque runtime values, or built-in front-end internals
merely so executable-owned code will compile.

The intended source shape is:

```text
src/
  lib.rs                         # embedding facade
  ...                            # library implementation modules
  bin/
    glam/
      main.rs                    # process entry and top-level dispatch
      batch.rs                   # assembly, output, settlement, exit policy
      rendering.rs               # default/fallback terminal rendering
      command_line/
        mod.rs                   # typed command dispatch facade
        adapters.rs              # shell adapters
        basic.rs                 # bootstrap completion and routing
        bootstrap.rs             # bootstrap option grammar
        completion.rs            # completion protocol and evidence
        model.rs                 # executable command models
        output.rs                # help and command output formatting
        configured/
          mod.rs                 # `conf.cli` expansion and completion
          effects.rs             # configured CLI effect vocabulary
          host.rs                # isolated invocation state and journal
          path.rs                # path readers and completion policy
          search.rs              # branch selection and ambiguity policy
          token.rs               # nested token parser facade
          token/                 # token effect implementation
      configuration/
        mod.rs                   # loading, assembly, and `conf.env`
        logger/
          mod.rs                 # `conf.log` integration
          effects.rs             # logger-visible host effects
          supervisor.rs          # ingress and logger lifecycle
        ide/                     # future `conf.ide`
```

Names below `command_line/configured` and `configuration/logger` may be refined
by their dependency maps, but their ownership direction is fixed. A separate
package is not an alternative under this plan. `ModuleInput`, source and
manifest facilities, `.g` inspection, and generic text-pattern/effect-search
mechanisms may remain in the library; their CLI routing belongs to the binary.

## Artifacts

1. [`ModuleSplitInventory_2026-08-18.md`](ModuleSplitInventory_2026-08-18.md)
   records every Rust file, its rough production/test size, role, cohesion, and
   any provisional seam.
2. This plan records the review method, ranked candidates, approved transition
   phases, and verification once the inventory is complete.
3. The cleanup review receives a resolution summary after approved splits land.

The dated inventory is evidence for this review. Enduring ownership
claims belong in `src/README.md` or `docs/architecture/` only after the new
module shape exists.

## Review Method

### Pass 0 — Mechanical census

For every `src/**/*.rs` file, record:

- total lines;
- approximate production and test lines where a terminal `mod tests` permits
  that distinction;
- module-tree position; and
- whether it is production, test-only, a facade/root, or an implementation
  leaf.

The counts prioritize inspection but do not establish desired file size.

### Pass 1 — File role inventory

Read the files by subsystem, including small neighboring files. For each file,
record:

- its primary responsibility;
- state and important types it owns;
- important entry points and collaborators;
- lock, lifecycle, or semantic invariants when relevant;
- a cohesion classification; and
- a provisional recommendation.

The classifications are:

- **cohesive** — one clear role, even if large;
- **facade** — intentionally gathers or re-exports several child roles;
- **mixed** — owns independently understandable responsibilities;
- **test-heavy** — raw size mostly reflects colocated verification; and
- **test-only** — verification organization rather than production ownership.

### Pass 2 — Hotspot dossiers

Deep-review every mixed file and every large production file. Map major item
clusters and ask:

1. Does each proposed child own a coherent concept rather than a grab bag?
2. Are dependencies mostly directed, or would the split create cyclic sibling
   imports and broad `pub(crate)` exposure?
3. Can implementation details become `pub(super)` or private?
4. Do tests naturally follow the extracted responsibility?
5. Can the first change be a mechanical move with public paths and behavior
   preserved?
6. Is the candidate likely to be invalidated by deferred architectural work?

### Pass 3 — Ranking and transition design

Rank candidates using, in order:

1. number of independent responsibilities;
2. strength and direction of internal seams;
3. visibility reduction;
4. improvement to change and test localization;
5. extraction risk; and
6. likelihood of near-term representation churn.

Do not introduce catch-all `common`, `shared`, or `util` modules. If a proposed
split needs one, revisit ownership first.

## Implementation Policy

Each approved hotspot gets its own low-risk checkpoints:

1. **Mechanical extraction.** Move one coherent cluster, retain existing
   public paths through the root module, and avoid behavior changes.
2. **Test relocation.** Move or partition tests according to the responsibility
   they verify while preserving useful filtered test names where practical.
3. **Boundary tightening.** Narrow `pub(crate)` to `pub(super)` or private only
   after the move is green and the new dependency direction is visible.
4. **Local cleanup.** Remove move-induced forwarding or stale comments without
   broadening the phase into an unrelated refactor.

Combining checkpoints is reasonable for a genuinely mechanical, small split.
Files with locks, unsafe code, lifecycle ownership, or large test fixtures
should remain partitioned.

## Success Criteria

- Every Rust file has been accounted for before candidates are approved.
- Root modules read as ownership maps or deliberate facades rather than
  miscellaneous containers.
- Child modules own state and operations together where practical.
- Splits reduce visibility or change coupling, not merely line count.
- Tests remain close to the responsibility they establish.
- Public paths and runtime semantics remain unchanged unless a separate,
  explicitly reviewed change says otherwise. Phase 1 explicitly removes the
  executable-owned `glam::cli` facade after its generic dependencies are
  available through the embedding boundary.
- No new dependency cycle or generic dumping-ground module is introduced.
- The finished package has one library and one primary directory-form binary;
  executable configuration policy is private to `src/bin/glam/`.

## Verification

Before implementation, latch the current required checks. For each extraction,
run the narrow affected tests first, followed by:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Use `git diff --color-moved` or an equivalent move-aware review to distinguish
mechanical relocation from edits. Public-facade moves additionally run
`cargo test -q --test public_api`; parser, macro, CLI, reflection, evaluation,
and interaction-net moves run their corresponding focused targets or module
filters before the full suite.

## Decision Discipline

The inventory ranks candidates, but does not pre-authorize every provisional
child module. Each candidate begins with an exact dependency-map checkpoint.
That checkpoint may approve the proposed shape, revise it, combine or divide
later checkpoints, defer the candidate, or conclude that the file should
remain intact.

After every completed hotspot, review the remaining phases against the new
module graph. Do not mechanically follow an obsolete split proposal merely
because it appears later in this plan. In particular:

- child names remain provisional until their dependency map is approved;
- cross-cutting tests stay at the nearest common owner, while responsibility-
  local tests move with their implementation;
- a move which requires broadening visibility or introducing sibling cycles
  is evidence that the proposed seam is wrong; and
- `core.rs` remains outside this transition unless the expected GC/value-model
  work first supplies a stable ownership boundary.

No line-count ceiling is proposed. The outcome should be a smaller number of
strong ownership decisions, not the maximum possible number of files.

## Inventory Result — 2026-08-18

The inventory accounts for all 127 Rust files. It separates dedicated and
inline tests from production size, then classifies each file by role and
cohesion. The main findings are:

1. `main.rs` has strong private seams between batch/configuration orchestration,
   logger effects and supervision, and default rendering. The adjacent public
   `cli` module is also executable policy and belongs in the same binary tree,
   although its private evaluator and reflection dependencies require a
   boundary transition before it can move.
2. `reflection.rs` has durable seams between its public specialization/host
   protocol, effect lifecycle and launchers, and the continuation-based task
   machine.
3. `api.rs` mixes the largest number of ownership domains and offers the
   greatest payoff, but its public paths and runtime/event dependencies require
   careful staging.
4. `evaluation/coordinator.rs` and `evaluation.rs` should receive one combined
   dependency review before either is partitioned; their completion, task,
   promise, and session protocols deliberately cross the current file boundary.
5. Large evaluator, lexer, parser, persistent-list, and test files are often
   cohesive. They are not split solely for size.
6. `core.rs` is mixed but should remain deferred while garbage collection and
   the value representation are expected to change its natural ownership
   boundary.

Secondary candidates are recorded in the inventory rather than promoted to
implementation phases prematurely. The next step is Phase 1A: turn the
executable boundary, including `main.rs` and `src/cli/`, into an exact
dependency map and staged move list before editing Rust.

## Follow-up Phases

### Phase 0 — Inventory and candidate selection

Status: complete (2026-08-18).

- Record the review contract and move policy.
- Account for every Rust source file exactly once.
- Separate approximate production and test size.
- Classify cohesion and produce hotspot dossiers.
- Rank candidates without treating size as an automatic split finding.

Result: all 127 files are represented in the linked inventory. The executable
boundary formed by `main.rs` and `src/cli/` is the first candidate;
`reflection.rs`, `api.rs`, and the combined evaluation/coordinator boundary
follow. Secondary candidates require fresh approval, while `core.rs` is
deferred.

### Phase 1 — Establish the binary-owned command and configuration boundary

#### Phase 1A — Exact dependency and move map

Status: complete (2026-08-18).

- Map every production item in `main.rs` to process entry/command dispatch,
  configuration loading, batch assembly/settlement, logger effect handling,
  logger supervision, or default rendering.
- Map every production item under `src/cli/` to bootstrap command parsing,
  command models, shell completion, configured `conf.cli` effects, isolated
  search, token parsing, path policy, or output formatting.
- Record call direction, owned state, callbacks, runtime/assembler handles,
  and the tests which establish each boundary.
- Record every use by `src/cli/` of private library state. The initial audit
  must cover at least private value/core conversion, direct `EvalContext`
  demand, construction of standard and specialization-owned effect requests,
  isolated-search host/context construction, restricted environment/log
  requests, text-pattern parsing, opaque path handles, and the runtime CLI
  invocation-ID allocator.
- Refine the approved `src/bin/glam/` child shape without changing its ownership
  direction. Do not retain configuration-specific code in the library merely
  to avoid adding an appropriate generic embedding operation.
- Identify cross-cutting orchestration which should remain in `main.rs` and
  reject any split requiring a generic shared-helper module.

Deliverable: an in-plan item-range/dependency table and an ordered list of
mechanical moves plus a list of exact generic library prerequisites. Review
that map before editing Rust.

Resulting production ownership map:

| Current source | Current responsibility | Target owner |
| --- | --- | --- |
| `main.rs:32-363`, `2541-2562` | entry dispatch, completion/inspection/check-manifest commands, process argument and local-file helpers | `main.rs`, `command_line`, and thin command-specific adapters |
| `main.rs:364-473` | dormant runtime, assembler, configuration, diagnostic ingress, and canonical-environment preparation | `configuration` with the final handoff owned by `batch` |
| `main.rs:474-830` | assembly execution, output, manifest finalization, runtime settlement, and no-logger completion | `batch` |
| `main.rs:831-1019`, `1868-1931` | configuration module, `conf.env`, `conf.log` selection, configuration paths, and configuration error contexts | `configuration`, with logger launch under `configuration/logger` |
| `main.rs:1020-1204` | configured logger request vocabulary, snapshot, journal, and handler | `configuration/logger/effects` |
| `main.rs:1205-1726` | ingress lifecycle, fallback outbox, settled-report selection and conversion | `configuration/logger/supervisor` |
| `main.rs:1597-1867` | logger task host, runtime endpoints, transactional commit, and diagnostic consumption | `configuration/logger` split by the Phase 1C dependency checkpoint |
| `main.rs:1932-2540` | default diagnostic evaluation, structured context layout, and terminal color | `rendering` |
| `main.rs:2563-end` | renderer, logger, settlement, configuration, and process integration tests | responsibility-local binary tests plus cross-component tests at the binary root |
| `cli/{bootstrap,basic,adapters}.rs` | bootstrap grammar and shell-neutral/basic completion routing | `command_line` |
| `cli/{model,completion,output}.rs` | executable command model, completion evidence, help and output format | `command_line` |
| `cli/path.rs` | filesystem path acceptance and completion policy | `command_line/configured/path.rs` |
| `cli/{configured,effects,host,search}.rs` | `conf.cli` selection, effect vocabulary, branch journal, and ambiguity policy | `command_line/configured` |
| `cli/token.rs`, `cli/token/` | nested token-effect search | `command_line/configured/token.rs`, `token/` |
| `cli/tests.rs` | bootstrap, configured CLI, completion, token, and path contracts | binary command-line tests, partitioned only after the move is green |

The directed executable call graph is:

```text
main
  -> command_line
  -> configuration
       -> configuration::logger
  -> batch
       -> configuration::PreparedAssembly
       -> configuration::logger
       -> rendering (fallback only)

command_line::configured
  -> public glam value/effect/search mechanisms
  -> command_line::{model,completion}
```

The library never depends on any node in this graph. Cross-cutting
`PreparedAssembly` remains configuration-owned because it owns the loaded
configuration and unresolved environment promises; batch consumes it after
canonical arguments are selected.

The private dependency audit found these required replacements:

| Existing private dependency | Required generic boundary |
| --- | --- |
| `Values::core`, `Value::from_core`, direct core construction | existing public `Values` constructors plus missing semantic effect/token constructors |
| `RequestContext::eval_context` and `eval::eval_value` | request-context outer-WHNF demand returning public `EvaluatedValue`/structured `TaskHalt` |
| `IsolatedTaskHost::new` | safe public immutable isolated-search host construction |
| `IsolatedEffectSearch::new_in_context` | nested request-context search preserving current demand ownership; top-level configured search may own an explicit client-demand session |
| `g_syntax::fail_effect_value` | generic semantic standard-fail constructor |
| hand-built hidden case-close request | specialization-owned request-effect construction without exposing abstract-global keys |
| `environment_log_request_specs` | reusable restricted environment/diagnostic request profile |
| private `TextPattern` | narrow public capture-free text-pattern facility shared with macros |
| `OpaqueValue<PathHandle>` | constrained host-token domain whose values cannot be forged or downcast generically |
| runtime `next_cli_invocation` | binary-owned invocation identity, removable once path-token ownership is explicit |

Minor decisions recorded for review:

- `PreparedAssembly` is configuration-owned rather than a generic shared type.
- The default renderer remains separate from the configured logger lifecycle;
  it is process fallback policy, not part of `conf.log` semantics.
- The existing one-file CLI test suite moves intact first; responsibility-local
  partitioning happens only after the external-crate boundary is green.

Verification: the pre-move baseline completed with all 1,300 tests passing
(`cargo test -q`: 1,165 library/unit plus 135 binary/integration tests across
the remaining targets). `git diff --check` passed for the plan update.

#### Phase 1B — Establish the directory-form binary and default rendering

Status: complete (2026-08-18).

- Move the binary crate root from `src/main.rs` to `src/bin/glam/main.rs` without
  changing the target name or process behavior.
- Move terminal color, context/header formatting, and default diagnostic
  rendering to private `rendering` ownership in that binary tree.
- Preserve all output bytes, indentation, fallback policy, and public process
  behavior.
- Move renderer-local tests; keep end-to-end logger tests at their common owner.

This validates both Cargo target discovery and the private binary module shape
before moving lifecycle code. Confirm that `cargo check --lib` remains
independent of the binary and that binary unit tests remain discoverable.

Result: Cargo now discovers `src/bin/glam/main.rs` as the package's sole
primary binary, while the library remains rooted at `src/lib.rs`.
`src/bin/glam/rendering.rs` owns the default formatter, context rendering,
terminal policy, and eight focused renderer tests. The binary root retains the
cross-component logger and settlement tests.

Verification:

- `cargo check --lib` and `cargo check --bin glam` passed after moving the root;
- `cargo metadata --no-deps --format-version 1` reported exactly the expected
  `lib` and `bin` target kinds;
- `cargo test -q --bin glam` passed all 22 binary unit tests, the same count as
  before extraction;
- `cargo clippy --bin glam --tests -- -D warnings`, `cargo fmt --check`, and
  `git diff --check` passed; and
- `src/main.rs` no longer exists and both binary source files are present.

#### Phase 1C — Extract binary-owned logger effects and supervision

Status: complete (2026-08-18).

- Move `MainEffects`, its request/snapshot/journal protocol, logger task host,
  `LogHost`, and logger supervisor ownership beneath
  `configuration/logger/` according to the approved map. Rename generic-looking
  items when their actual owner is the configured logger.
- Preserve diagnostic ingress lifetime, runtime FIFO semantics, settlement
  ordering, callback lock boundaries, and failure reporting.
- Partition unit tests by effect handler versus supervisor lifecycle while
  retaining cross-component shutdown tests.

Treat effect protocol and supervisor lifecycle as separate checkpoints if the
Phase 1A map shows that one can move without the other.

Result: `configuration/logger/effects.rs` now owns the configured logger's
effect specialization, request/snapshot/journal protocol, and task host;
`configuration/logger/supervisor.rs` owns the long-lived diagnostic ingress,
logger lifecycle supervision, settlement-report conversion, and fallback
delivery; and `configuration/logger/mod.rs` owns configured logger startup and
the running logger handle. No logger policy moved into the library.

Tests are partitioned by responsibility: diagnostic-ingress counting, rearm,
and teardown tests live with the supervisor; output-bus isolation lives with
the effect host; and retry, settlement, and shutdown-order tests remain at the
binary root because they deliberately cross configuration, runtime, and
supervisor boundaries.

Verification: `cargo fmt --check`,
`cargo clippy --bin glam --tests -- -D warnings`, and
`cargo test -q --bin glam` passed; all 22 binary tests remain present.

#### Phase 1D — Extract configuration and batch orchestration

Status: complete (2026-08-18).

- Move `GLAM_CONF` discovery, configuration loading and assembly, `conf.env`
  construction, and the interfaces used to invoke `conf.cli` and `conf.log`
  beneath `configuration/`.
- Move batch assembly, output, runtime pump/settlement/reporting, and final exit
  policy to `batch` only where they form directed child dependencies.
- Keep the executable entry point and top-level command dispatch readable in
  `main.rs`.
- Preserve the existing order of configuration, assembly output, runtime
  settlement, fallback rendering, logger completion, and exit-code selection.

Result: `configuration/mod.rs` now owns `GLAM_CONF` discovery, the dormant
runtime and assembler construction, canonical process/reflection argument
promises, `conf.env` loading, configuration contexts, and the
`PreparedAssembly` handoff. `batch.rs` owns file finalization, worker
activation, assembly and stdout output, runtime settlement, fallback report
delivery, configured CLI execution, and process exit policy. `main.rs` is
reduced to top-level dispatch plus the still-to-be-moved command-specific
adapters.

Configuration-load failure crosses the boundary as a boxed internal
`PreparationFailure` carrying the partially constructed assembler, source
tracker, and log host. Batch can therefore publish and render the error and
finalize a requested manifest without giving configuration terminal output or
exit-code responsibilities. This is a private ownership handoff, not a new
semantic error category.

Verification: `cargo fmt --check`,
`cargo clippy --bin glam --tests -- -D warnings`, and
`cargo test -q --bin glam` passed; all 22 binary tests remain green, including
the cross-component settlement and logger-order tests.

#### Phase 1E — Complete the generic effect-host embedding boundary

Status: complete (2026-08-18).

Add only the reusable library facilities established by Phase 1A. Expected
categories, subject to that dependency map, are:

- handler-side runtime-local value construction and outer-WHNF demand without
  exposing `CoreValue` or `EvalContext`;
- safe immutable isolated-search host construction and a nested-search entry
  which preserves the current demand/dependency semantics;
- standard and specialization-owned effect construction needed for `.fail`
  and scoped close operations;
- an explicit reusable environment-and-diagnostic request subset;
- a constrained host-token/capability mechanism for invocation-local path
  handles, rather than public arbitrary opaque Rust values; and
- the shared capture-free text-pattern operation used by macros and the CLI.

Latch this boundary with an integration test that implements and runs a small
effect specialization as an external consumer using only public `glam` APIs.
The checkpoint is incomplete if the test imports implementation modules or if
configuration-specific vocabulary appears in the library API.

Host-token checkpoint discovered during migration:

- The configured CLI currently returns a private `OpaqueValue<PathHandle>`
  from `.read.path` and later downcasts it in `.write.file` or
  `.write.manifest`. Moving that code across the crate boundary must not expose
  arbitrary opaque-value construction or downcasting, and encoding the handle
  as an ordinary dictionary would make a capability forgeable.
- The proposed generic boundary is an `EffectTokenDomain<T>` created for one
  runtime and one host-owned scope. It issues runtime-local opaque token
  values and resolves only tokens issued by that exact domain to `Arc<T>`.
  There is no generic inspection or downcast on `Value`.
- Token values hold only a domain-scoped ID and a weak domain reference. The
  domain owns the payload map; dropping the domain revokes every outstanding
  token, and dropping the last clone of one token removes its payload. This
  avoids a `Value -> payload -> Value/runtime` ownership cycle and gives token
  lifetime an explicit host owner.
- The configured CLI would put a fresh domain in each invocation snapshot.
  Path handles therefore cannot cross invocations by construction, allowing
  removal of `EvaluationRuntime::allocate_cli_invocation_id` and the
  CLI-specific runtime-global allocator.
- This is deliberately narrower than a `HostValue`: it is an effect-handler
  capability transport, not a persistence or IPC value, and it cannot be
  observed without possession of the issuing Rust domain.

The remaining Phase 1E boundaries are straightforward projections of existing
internals: public outer-WHNF demand and `Values` access on `RequestContext`, a
request-spec-owned constructor for hidden scoped-close effects, a standard
fail-effect constructor, immutable isolated-search host construction and
nested search, the restricted environment/diagnostic request profile, and the
shared capture-free text-pattern parser.

Result: the library now exposes runtime-local `EffectTokenDomain<T>`, public
immutable `IsolatedTaskHost`, standard fail-effect construction, request-spec
effect construction for specialization-owned hidden operations,
`RequestContext` value construction/outer-WHNF demand/nested search, the
restricted environment-and-diagnostic request profile, and the versioned
capture-free `TextPattern`. `Values::unit` and `EvaluatedValue::as_u64` fill
the corresponding ordinary semantic construction/extraction gaps without
exposing core representations.

`EffectTokenDomain<T>` uses weak token-to-domain references and domain-owned
payload records. It provides revocation and cleanup without allowing generic
opaque-value downcasts or creating token/payload ownership cycles. The CLI now
uses one path-token domain per invocation, and the obsolete CLI invocation ID
was removed from `RuntimeIds`.

Verification:

- `tests/effect_embedding.rs` implements and runs an external effect
  specialization using only public `glam` APIs;
- the focused token-domain test proves exact-domain resolution, rejection by
  another domain, rejection of ordinary values, and weak revocation ownership;
- a private-dependency scan of `src/cli/` finds no remaining `core`, `eval`,
  `evaluation`, `g_syntax`, raw `EvalContext`, opaque downcast, or CLI ID use;
- `cargo clippy --all-targets --all-features -- -D warnings` passed after the
  boundary changes; and
- the focused external embedding and token-domain tests passed.

#### Phase 1F — Migrate the CLI implementation into the binary

Status: complete (2026-08-18).

- Move the bootstrap parser, command models, output formatting, completion
  protocol/adapters, configured `conf.cli` effects, isolated search, token
  parser, path policy, and their tests from `src/cli/` to
  `src/bin/glam/command_line/`.
- Use `command_line/configured/` for the effectful configuration parser; do not
  create a second library facade or duplicate command models during the final
  state.
- Partition the mechanical move according to the Phase 1A dependency graph.
  Pure leaf modules may move first, but the library must never depend on the
  binary and temporary forwarding must have an explicit removal checkpoint.
- Preserve configured parsing, ambiguity, completion, path-handle,
  diagnostic-evidence, and `--parse_cli` behavior exactly.
- Keep CLI unit tests beside the binary modules and retain process-level tests
  in `tests/`.

Result: bootstrap parsing, command models, output formatting, completion,
configured effects and search, token parsing, path policy, and their 46 unit
tests now live under `src/bin/glam/command_line/`. The effectful implementation
is grouped under `command_line/configured/`; pure bootstrap/basic completion
remains at the command-line root. Process-level contracts remain in
`tests/cli.rs`.

The temporary semantic mismatch found by the move was in the new embedding
facade, not CLI behavior: `Values::unit` initially constructed an empty list
instead of projecting the runtime's cached unit atom. The migrated success-path
tests reliably exposed it, and the constructor now returns the existing
semantic unit.

Verification: `cargo clippy --all-targets --all-features -- -D warnings`
passed, `cargo test -q --bin glam` passed all 68 binary tests (the prior 22 plus
46 moved CLI tests), and `cargo test -q --test cli` passed all 49 process-level
CLI tests.

#### Phase 1G — Remove accidental library CLI policy

Status: complete (2026-08-18).

- Remove `pub mod cli` and every configuration/command-specific export from
  the library.
- Remove private compatibility entry points added only for the old in-library
  CLI, including the runtime CLI invocation-ID allocator if the completed host
  token design makes invocation identity binary-owned.
- Verify that the library no longer owns `GLAM_CONF`, `conf.env`, `conf.cli`,
  `conf.log`, or `conf.ide` policy. Generic value, source, diagnostic,
  effect-search, and text-pattern mechanisms remain library-owned.
- Treat removal of `glam::cli` as the deliberate pre-release API correction
  authorized by this plan, not as an accidental path-preservation failure.

Result: `src/cli.rs`, `src/cli/`, and `pub mod cli` are removed. All binary
callers use the private `command_line` module directly. The CLI-specific
runtime invocation allocator and its `RuntimeIds` field are gone; path token
identity is invocation-domain-local instead. No forwarding facade remains.

Verification: source scans find no `glam::cli`, `crate::cli`,
`allocate_cli_invocation`, or `next_cli_invocation` references. A policy scan
outside `src/bin/` finds no executable ownership of `GLAM_CONF`, `conf.cli`,
`conf.log`, or `conf.ide`; the lone `conf.env` occurrence is evaluator test
data. Cargo metadata still reports one `lib` target and one `glam` `bin`
target in the same package.

#### Phase 1H — Tighten, document, and audit the executable boundary

Status: complete (2026-08-18).

- Narrow visibility introduced by the moves.
- Remove forwarding used only during extraction.
- Update `src/README.md`, assembly/diagnostic architecture, and CLI user docs to
  distinguish executable configuration ownership from the generic library
  mechanisms used to implement it.
- Run `cargo check --lib`, focused binary/CLI/logger tests, process integration
  tests, the external effect-host API test, and the full required gates.
- Confirm through Cargo metadata or target checks that the package still
  contains one library and the one primary `glam` binary; no package split is
  introduced.
- Review Phases 2–5 for drift before proceeding.

Result: every command-line model and operation is now at most binary-crate
visible; the direct `batch` and `configuration` handoffs are parent-visible,
while nested logger items remain crate-visible only where the sibling batch or
cross-component tests consume them. No extraction-only forwarding module or
compatibility entry point remains. The binary compiles cleanly through the
private `command_line`, `configuration`, `batch`, and `rendering` ownership
graph.

`src/README.md` now maps the directory-form binary and its private policy
modules. The assembly and diagnostic architecture documents explicitly
separate executable-owned configuration, completion, configured logging,
rendering, and exit behavior from the generic library mechanisms used to
implement them. `docs/CLI.md` records that this is a library/binary crate
boundary inside one package, not a package split. The assembly agent context
and cleanup-review links now point at the moved tests and configured hosts.

Verification:

- `cargo check --lib` passed independently of the binary;
- the 46 command-line unit tests and four logger-local tests passed;
- all 49 process CLI integration tests passed;
- `tests/effect_embedding.rs` passed as an external public-API consumer;
- `cargo fmt --check` and
  `cargo clippy --all-targets --all-features -- -D warnings` passed;
- `cargo test -q` passed all 1,302 tests (1,120 library, 68 binary, and 114
  integration tests); and
- Cargo metadata reports one `lib` target at `src/lib.rs` and one `glam` `bin`
  target at `src/bin/glam/main.rs` in the same package.

Post-Phase-1 drift review:

- **Phase 2 remains correctly ordered.** Phase 1E deliberately added public
  `EffectRequestSpec` construction, `RequestContext` demand/search helpers,
  `IsolatedTaskHost`, and `EffectTokenDomain`. Phase 2A must include these in
  the specialization/host-protocol map; Phase 2B should move them with that
  protocol rather than strand embedding facilities in the machine or lifecycle
  layer. This enlarges the inventory, not the extraction risk or dependency
  direction.
- **Phase 3's intended seams are stronger.** `Values`, `EvaluatedValue`, and
  evaluator roles now follow the completed Value Facade boundary, while the
  default renderer and all configured diagnostic policy have left `api.rs`.
  Phase 3A should treat generic diagnostic transport as library-owned and must
  not recreate an executable logger or rendering child. No phase reorder is
  needed.
- **Phase 4 has no Phase-1-dependent semantic drift.** The external effect-host
  test now provides an additional contract for any reflection/evaluation move:
  isolated demand and nested search must remain usable without private
  `EvalContext` access. The combined dependency map is still the appropriate
  first checkpoint for the two large files.
- **Phase 5 must inventory the new binary tree.** Its census should distinguish
  the 46 command-line tests and logger/rendering tests now colocated with the
  executable from library production size. The package-boundary documentation
  is already current, but Phase 5 still owns the final post-split audit and
  cleanup-review closure.

### Phase 2 — Split reflection protocol, lifecycle, and machine

#### Phase 2A — Exact dependency and move map

Status: complete (2026-08-18).

- Map the public specialization/host transaction protocol, scheduled effect
  lifecycle and launchers, task-machine/continuation state, transaction state,
  and request decoding.
- Account explicitly for existing `requests`, `search`, and `store` children.
- Decide whether transaction structures belong with the public effect protocol
  or the machine; keep continuation, cut, retry, and fixpoint state together.
- Record public re-exports and filtered tests before moving code.

Resulting ownership map:

| Current range/child | Responsibility | Target owner |
| --- | --- | --- |
| `reflection.rs:63-494` | specialization request specifications/results, reasoning identity, host snapshots/commits, host/environment contracts, task outcome and structured halt | `reflection/protocol.rs` |
| `reflection.rs:495-1114` | host-observable lifecycle, scheduled/synchronous effect runs, coordinator-root activation, result policy, and type-erased launchers | `reflection/lifecycle.rs` |
| `reflection.rs:1115-4278` | effect vocabulary, task interpreter, branches/cuts/retry/fix/reset/continuations, transaction state, request context and decoding | divided between `reflection/machine.rs` and the protocol-owned transaction boundary below |
| `reflection/requests.rs` | reusable reflection request family and reflection-specific journal/host services | remains a protocol-adjacent child |
| `reflection/search.rs` | isolated all-results policy, immutable isolated host, and pollable search wrapper | remains a machine-adjacent child |
| `reflection/store.rs` | persistent reflection volumes, queries, journals, and conflict analysis | remains the independent store child |
| `reflection.rs:4279-end` | shared cross-layer effect harness and 104 protocol/machine/lifecycle integration tests | move intact to `reflection/machine/tests.rs`; feature-local child tests remain in `requests`, `search`, and `store` |

The dependency direction is:

```text
reflection facade
  -> protocol -> requests, search API, store, evaluation value/wait types
  -> machine  -> protocol, requests, search policy, store, evaluator
  -> search   -> protocol + machine
  -> lifecycle -> protocol + machine + evaluation coordinator facade
```

`RequestActivity`, `Transaction<S>`, `RequestContext`, and
`TransactionContext` move to `protocol`, despite currently appearing beside
the machine. They contain only host snapshots, reflection-store journals, and
specialization journals; none contains branch, continuation, cut, reset, or
fixpoint state. Keeping them with the public host contract removes what would
otherwise be a protocol-to-machine dependency. The small sealed request-value
constructor used by `EffectRequestSpec::effect` moves with them and is shared
privately with request decoding.

All branch, control, retry, fixpoint, continuation, reset-frame, task-block,
and terminal state moves together to `machine`. Request decoding and the
standard effect API also stay there because their tags and outcomes directly
drive that state machine. Lifecycle depends on the machine to construct and
erase tasks; the machine does not depend on lifecycle publication.

Public paths remain rooted at `reflection::*` through explicit facade
re-exports. The Phase 1E additions are accounted for as follows:

- `EffectRequestSpec`, `RequestContext`, and their public construction/demand
  helpers move with `protocol`;
- `IsolatedTaskHost` remains in `search`, re-exported by the facade;
- `EffectTokenDomain` remains in `api.rs` for the forthcoming Phase 3 ownership
  map—it transports public runtime values and is not part of reflection task
  interpretation; and
- `TextPattern` remains its independent library facility.

The latched reflection suite contains 134 tests: 104 cross-layer root tests,
five request tests, one search test, and 24 store tests. Moving the root suite
intact first avoids manufacturing a test-support facade merely to classify
integration tests by filename. Pure request/search/store tests are already
owned locally; lifecycle-only tests may move later only if they cease to rely
on the shared full-machine host fixture.

#### Phase 2B — Extract specialization and host protocol

Status: complete (2026-08-18).

- Move `TaskSpecialization`, host/snapshot/journal/commit contracts, standard
  and reflection effect markers, and closely owned request result types.
- Preserve existing `reflection::*` paths through private modules/re-exports.

Result: `reflection/protocol.rs` now owns specialization request
specifications/results, reasoning identity, host snapshots and commits,
task environment/host contracts, task outcomes and structured halts, plus the
host/store transaction and public request contexts. The latter move eliminates
a protocol-to-machine dependency: a transaction contains no interpreter
control state. Existing `reflection::*` paths are explicit facade re-exports.

Verification: `cargo check --lib`, formatting, and all 134 filtered reflection
tests passed after the extraction.

#### Phase 2C — Extract lifecycle and launchers

Status: complete (2026-08-18).

- Move effect lifecycle state, scheduled runs, result policy, run builders,
  and type-erased launchers as one ownership layer.
- Preserve weak/strong ownership, runtime settlement participation, terminal
  publication, and diagnostic routing.

Result: `reflection/lifecycle.rs` owns lifecycle status publication,
coordinator terminal policies, scheduled and synchronous composed runs, result
policy, and both direct and coordinator-capable type-erased task launchers.
The layer constructs machine tasks but the machine has no lifecycle
dependency. Public paths and the crate-private coordinator launcher remain
facade re-exports.

Verification: formatting and all 134 filtered reflection tests passed,
including lifecycle terminal publication, observer drop, abandonment, child
failure, and diagnostic-consumer activation cases.

#### Phase 2D — Extract the task machine

Status: complete (2026-08-18).

- Move branches, continuations, cut/reset/fix state, block/retry state,
  transactions, and request interpretation together.
- Do not alter deterministic alternative scheduling, transactional retry, or
  cycle/error recovery while moving the interpreter.

Result: `reflection/machine.rs` owns the effect vocabulary, persistent task
interpreter, branch/cut/retry/fix/reset/continuation state, blocking and
terminal records, and request decoding. Transaction snapshots and journals
remain protocol-owned because they contain no interpreter control state. The
machine depends on the protocol and focused request/search/store children;
the protocol and machine do not depend on lifecycle publication.

#### Phase 2E — Tests, visibility, and drift review

Status: complete (2026-08-18).

- Place protocol, lifecycle, and machine tests with their new owners; keep the
  cross-layer harness beside the machine whose private state it exercises.
- Tighten visibility, run reflection/store/public API coverage and full gates,
  then refresh the `api.rs` dossier.

Result: the 104 shared effect tests live in `reflection/machine/tests.rs`.
They inspect private interpreter state and exercise protocol and lifecycle
through the real machine, so colocating the harness with its machine owner is
clearer than adding a broad root-level test-support facade. The five request,
one search, and 24 store tests remain beside their focused owners. Production
modules now use explicit imports and sibling-only visibility for internal
construction and polling surfaces. `reflection.rs` is a small facade which
retains the established public and crate-private paths.

Verification: all 134 focused reflection tests, public embedding/API and macro
protocol tests, logger-filtered binary tests, formatting, full-feature Clippy,
and the complete `cargo test -q` suite pass after the split.

Post-Phase-2 drift review: the Phase 3 dossier remains current.
`EffectTokenDomain` still belongs with public runtime-value construction in
`api.rs`, while diagnostic transport and runtime event ownership remain the
unresolved seams Phase 3A must map. The reflection split introduced no new
dependency from the value facade to interpreter internals.

### Phase 3 — Turn `api.rs` into an embedding facade

#### Phase 3A — Exact dependency and ownership map

Status: complete (2026-08-18).

- Map value/evaluator/net construction, diagnostic bus/ingress, runtime
  lifecycle/readiness reports, transactional input/output state, runtime
  resources, assembler construction, and module building.
- Decide whether runtime events are owned by the runtime child or are a sibling
  behind a narrow runtime interface; this unresolved direction must be settled
  before extraction.
- Inventory every public export and internal test helper so paths and
  visibility can be preserved intentionally.

The pre-split `api.rs` contains 10,795 lines and 106 inline tests. Its
production ownership map is:

| Current range | Responsibility | Target owner |
| --- | --- | --- |
| `64-932` | runtime-rooted values, construction, effect tokens, promises, value kinds, and checked core-net construction | `api/value.rs` |
| `933-1841` | diagnostic envelopes, enrichment, counts, bus/subscription, and runtime FIFO ingress | `api/diagnostics.rs` |
| `1842-2111` | assembler reflection host and per-module compilation execution | `api/assembly.rs` |
| `2112-2701` | public runtime readiness/disposition/deadlock projections | `api/runtime/readiness.rs` |
| `2702-3427` | runtime-owned transaction/event state, FIFO journals and endpoints, output delivery and failures | `api/runtime/events.rs` |
| `3428-4895` | runtime value roots, shared resources, task capabilities, runtime construction, combined commit, observation, pumping, and settlement | `api/runtime.rs` |
| `4896-4999`, `5186-6267` | reasoning session, module inputs/results, environment and volume capabilities, assembler builder, source/module construction | `api/assembly.rs` |
| `5000-5185` | embedding error plus retained reasoning-failure projection | `api/error.rs` (`ReasoningFailure` is re-exported through runtime/assembly policy) |
| `5268-5376` | deterministic WHNF evaluator and privileged reflection inspector | `api/evaluator.rs` |
| `6268-6314` | legacy test-only semantic facade | nearest test support; do not expose in production |
| `6315-end` | 106 value, runtime-event, readiness, diagnostics, and assembly integration tests | divide by owning module, retaining only irreducibly cross-layer tests under `api/tests.rs` |

Runtime events are owned below the runtime module, not as a sibling service.
Their authoritative state is part of `RuntimeSharedResources`, combined commit
must validate reflection and event journals under one transaction mutex, and
readiness must observe delivery activity. `runtime/events.rs` and
`runtime/readiness.rs` are directed children of that owner so Phase 3D does
not replace `api.rs` with another monolith. Diagnostics own ingress policy,
but ingress reaches the FIFO through the runtime's narrow event operations;
the runtime retains only the weak lifecycle registration required by that
bridge.

The extraction order is common error/value primitives, evaluator facade,
diagnostics, runtime children and owner, then assembler construction. Public
paths stay rooted at `api::*` and crate-root re-exports remain unchanged.
Sibling-only visibility is acceptable during mechanical moves, then must be
tightened after tests relocate.

#### Phase 3B — Extract value and evaluator facade

Status: complete (2026-08-18).

Split this checkpoint into:

- **3B.1:** common embedding error and value construction, including checked
  net construction and promise resolution; and
- **3B.2:** `ValueEvaluator` and `ReflectionInspector`, which depend on the
  assembler's selected reasoning context but expose only the established
  construction/demand/reflection boundary.

- Move `Value`, `EvaluatedValue`, `Values`, promise resolver, checked net
  builder, evaluator, and reflection inspector according to the approved map.
- Preserve the construction/demand/reflection boundary established by the
  Value Facade transition.

If the dependency map gives checked nets or promise resolution an independent
owner, split this checkpoint rather than forcing an oversized `value` child.

Result: `api/value.rs` owns runtime-rooted value construction, evaluated WHNF
witnesses, effect-token domains, affine promise resolution, value kinds, and
the checked core-net builder. These form one construction boundary and did not
warrant extra net/promise modules. `api/error.rs` owns the shared embedding
error and retained reasoning-failure projection. `api/evaluator.rs` owns the
assembler-selected deterministic evaluator and privileged reflection
inspector, keeping construction, demand, and reflection visibly distinct.
All 106 focused API tests pass after the extraction.

#### Phase 3C — Extract diagnostic publication infrastructure

Status: complete (2026-08-18).

- Move diagnostic envelopes/events/counts, bus/subscription, ingress, and
  routing ownership without changing callback or buffering semantics.
- Keep diagnostic value projection separate from default rendering policy.

Result: `api/diagnostics.rs` owns diagnostic envelopes and enrichment,
sequence/count snapshots, the non-buffering bus and subscriptions, and the
long-lived runtime FIFO ingress. It contains no terminal rendering policy.
The assembler and runtime retain only the sibling-visible construction and
lifecycle hooks needed to publish and register ingress. All 106 focused API
tests pass after the move.

#### Phase 3D — Extract runtime events and lifecycle reports

Status: complete (2026-08-18).

Split this checkpoint into:

- **3D.1:** `runtime/events.rs`, including transaction snapshots/journals,
  endpoints, delivery records, and durable failures;
- **3D.2:** `runtime/readiness.rs`, containing only observational public report
  and settlement-proposal types; and
- **3D.3:** `runtime.rs`, owning shared resources, runtime construction,
  combined commit, pumping, settlement, and the interface between its two
  children.

- Move input/output endpoint state, journals, delivery records/failures,
  readiness/deadlock/settlement reports, and the relevant runtime methods in
  the dependency order chosen by Phase 3A.
- Preserve mutation-admission order, callback-outside-lock rules, FIFO
  conflicts, durable delivery failures, and quiescence validation.

Split event transport from readiness/settlement if the map shows two directed
owners; do not create a monolithic replacement for `api.rs`.

Result: `api/runtime/events.rs` owns transactional FIFO snapshots and
journals, endpoint capabilities, outbox claims, diagnostic routes, and durable
delivery failures. `api/runtime/readiness.rs` owns the observational public
readiness, deadlock, disposition, and settlement projections.
`api/runtime.rs` remains their directed owner: it contains runtime allocation,
shared resources, combined reflection/event commit, observation publication,
pumping, and settlement. The event state still shares one authoritative
transaction mutex with the reflection store, and all callback delivery remains
outside runtime locks and mutation admission.

#### Phase 3E — Extract assembler and module construction

Status: complete (2026-08-18).

- Move assembler builder/host/profile wiring, source/module construction, and
  compilation execution behind the public facade.
- Retain all current public paths and builder staging rules.

Result: `api/assembly.rs` owns the reasoning host and session, compilation
execution, protected volumes, assembler/builder staging, source preparation,
recursive module loading, and built-module results. `api.rs` re-exports the
established embedding surface without owning construction policy. Moving the
code required no change to runtime selection, immutable profile sealing,
environment construction, or source ordering.

#### Phase 3F — Public boundary and test audit

Status: complete (2026-08-18).

- Partition the large inline tests according to their new owners while keeping
  integration-like facade tests at the root.
- Verify exported paths, narrow visibility, run `public_api`, CLI, macro,
  diagnostics, runtime-event, and full repository tests.
- Refresh the evaluation/coordinator dossier after the runtime types move.

Result: `api.rs` is an 85-line facade. Every production child has explicit
imports; cross-child construction hooks are restricted to `api`, `pub(super)`,
or test-only visibility rather than entering the public embedding API. The 106
former inline tests now live under `api/tests`: runtime/event/readiness tests
and diagnostic transport tests have focused files, while value, evaluator,
error, and assembler-composition tests remain at the facade's common owner.
Public paths and the binary's use of the library remain unchanged.

Post-Phase-3 drift review: Phase 4 must not pull public runtime reports or
transactional event transport back into the evaluation coordinator. Those
projections now have explicit owners in `api/runtime/readiness.rs` and
`api/runtime/events.rs`. The combined Phase 4A map should therefore start from
the still-current 8,065-line `evaluation.rs` and 7,547-line
`evaluation/coordinator.rs`: session/task/wait/promise/client-demand protocols
remain in the former, while subscription epochs, work records and queues,
runtime-local coordinator snapshots, and settlement validation remain in the
latter. `evaluation/executor.rs` is already a small worker-lifecycle owner.
Phase 4A should decide their internal extraction order without changing the
now-established API/runtime ownership boundary.

### Phase 4 — Partition evaluation sessions and work coordination

#### Phase 4A — Combined dependency map and ordering decision

Status: complete (2026-08-19).

- Map both `evaluation.rs` and `evaluation/coordinator.rs` together: waits,
  promise producer obligations, task handles/status, client demand, reflection
  profiles, sessions/contexts, completion subscriptions, work records/queues,
  and readiness/settlement projection.
- Identify which types are passive protocol values and which own mutation or
  terminalization.
- Select an extraction order that leaves the coordinator as the sole owner of
  scheduling state and avoids bidirectional sibling dependencies.

The two files contain substantially less production code than their total line
counts first suggest, but still form the largest remaining coupled owner:

| Current owner/range | Production responsibility |
| --- | --- |
| `evaluation.rs:63-670` | task/session IDs, wait terminal cells, task handles, task-owned promise obligations, task policies, exit and machine protocols |
| `evaluation.rs:671-882` | client-demand operation, result cell, sink, and host handle |
| `evaluation.rs:883-1114` | reflection launcher/profile, task-status publication, result policy, and session-report protocol |
| `evaluation.rs:1128-2397` | demand state, owner lease, evaluation contexts, task/deferred admission, wait polling, and session reports |
| `evaluation.rs:2398-3013` | cooperative pumping, claimed-machine release, retirement, and pure-lazy-cycle poisoning |
| `evaluation/coordinator.rs:27-354` | semantic observation epoch and atomic exact-completion subscription protocol |
| `evaluation/coordinator.rs:355-656` | common close control, producer/terminal obligations, work state, and dependency identity |
| `evaluation/coordinator.rs:657-1004` | kind-local spark, client-demand, reflection, and deferred payloads and claims |
| `evaluation/coordinator.rs:1005-1200` | authoritative indexes/queues plus internal readiness and settlement values |
| `evaluation/coordinator.rs:1218-2351` | construction, demand registration/closure, failure ledgers, selection, readiness validation, and settlement |
| `evaluation/coordinator.rs:2352-4494` | work-kind claim, release, cancellation, wake, and reporting transitions |
| `evaluation/coordinator.rs:4495-5457` | common record helpers, queue/index maintenance, dependency cycles, and retirement |

`evaluation.rs` has 3,013 production lines followed by 98 tests;
`evaluation/coordinator.rs` has 5,457 production lines followed by 43 tests.
The 217-line executor and its one focused test are already cohesive.

The review makes these ownership decisions:

- Completion subscriptions are not passive values. They own the subscriber
  mutex, subscribe-and-recheck protocol, weak coordinator route, and detached
  notification. Keep them under the coordinator and move the protocol as one
  unit.
- Settlement is also active. The validated plan is a passive description, but
  validation and `RuntimeSettlementRelease` jointly own terminal obligations,
  machines, client sinks, exact wakes, status publication, and destruction
  after admission is released. Move them together only after work-kind
  transitions have stable owners.
- Preserve one authoritative `WorkRecord` discriminated union and one set of
  indexes/ready queues. The accepted kind-local payload design should not be
  replaced by independent per-kind registries merely to create files.
- The coordinator owns active wait/task terminalization and client-demand
  claims. Session/context code owns demand policy, reflection profiles, task
  construction, and cooperative pumping. `EvaluationDemandState` is the
  intentional bridge: coordinator records may retain it for sparks and client
  demand, while it retains only a weak route back to the coordinator.
- Keep `EvaluationDemandState` and any other irreducibly shared bridge in
  `evaluation.rs` rather than inventing a trait object or broad visibility
  solely to make the facade tiny. This facade need not reach the 85-line shape
  of `api.rs`.
- Internal readiness snapshots remain coordinator protocol projected by
  `api/runtime/readiness.rs`; runtime observation and host-event ownership do
  not move back from `api/runtime/`.

The target hierarchy is provisional by checkpoint, but its direction is:

```text
evaluation.rs                 demand/profile bridge and crate-private re-exports
  observation.rs              semantic observation epoch/state
  session.rs                  owner lease, reports, and evaluation contexts
  pump.rs                     cooperative/runtime polling and release
  coordinator.rs              authoritative record model, indexes, and queues
    completion.rs             exact subscriptions and wake delivery
    task.rs                   task protocol plus wait/promise terminal lifecycle
    client_demand.rs          resumable host demand records
    spark.rs                  best-effort background demand
    reflection.rs             reflection record transitions
    deferred.rs               lazy/promise producers and pure-cycle handling
    settlement.rs             readiness validation and terminal settlement
  executor.rs                 worker lifecycle and dispatch
```

Names are not approval to create empty forwarding modules. A child is kept
only when its state and transitions move together and the coordinator remains
the sole mutation authority.

#### Phase 4B — Extract observation and completion foundations

##### Phase 4B.1 — Semantic observation epoch

Status: complete (2026-08-19).

- Move `RuntimeObservationEpoch` and `RuntimeObservationState` to
  `evaluation/observation.rs` and preserve their crate-private re-exports.
- Move the niche-layout and wait/advance tests with them.
- Do not combine scheduler work generation with semantic observation.

##### Phase 4B.2 — Exact completion subscriptions

Status: complete (2026-08-19).

- Move `WorkDependencyKey`, `WakeRegistration`, `DependencyWakeBatch`,
  `CompletionSubscriptions`, `CompletionWake`, and
  `CompletionSubscriptionOutcome` together under
  `evaluation/coordinator/completion.rs`.
- Move the coordinator's subscribe, recheck, exact-wake, and detached-notify
  helpers with the protocol when doing so does not duplicate record mutation.
- Preserve terminal-before-detach ordering, `(work ID, subscription epoch)`
  validation, no nested subscriber/coordinator mutexes, and notifications
  after mutation admission.
- Verify completion-before, completion-during, and completion-after
  subscription with the existing forced-order tests before proceeding.

#### Phase 4C — Partition coordinator-owned lifecycles

##### Phase 4C.1 — Task and wait protocol

Status: complete (2026-08-19).

- First move scalar/passive task protocol—IDs, status/policy enums, exit
  intent, machine poll/trait, result policy, and status wake wrappers—to
  `evaluation/coordinator/task.rs` without changing behavior. Re-export that
  crate-private protocol through `coordinator` and `evaluation` for evaluator
  and reflection consumers.
- Then move the active wait cell, task handle/preparation, task-owned promise
  obligations, and coordinator terminal publisher into the same child.
  Preserve the existing public-in-crate re-export paths.
- Keep wait/status/failure/promise publication under one mutation admission;
  notifications, cancellation hooks, and destruction remain detached.

##### Phase 4C.2 — Client demand

Status: complete (2026-08-19).

- Move the operation data, result cell, sink/handle, coordinator payload/claim,
  and claim/release/abandon/kill transitions together to
  `evaluation/coordinator/client_demand.rs`.
- Keep evaluation of the operation in `pump.rs`; the client-demand child must
  not import `session.rs` merely to call through `EvalContext`.
- Preserve resumable exact dependencies, result publication after unlock,
  explicit abandonment, and the external handle's ownership of waiting.

##### Phase 4C.3 — Sparks

Status: complete (2026-08-19).

- Move spark payload/claim/retirement and queue/release/abandon transitions to
  `evaluation/coordinator/spark.rs`.
- Preserve best-effort semantics, worker-only selection, exact wait/promise
  subscription, session-close cleanup, and truthful busy state for a claimed
  spark.

Result: `evaluation/coordinator/spark.rs` owns spark demand, claimed work,
poll and retirement records, admission, queueing, exact dependency release,
quiescent abandonment, and detachment. Common dependency subscription and
cross-kind coordinator state remain with the coordinator. The extraction
does not change best-effort retirement, worker-only claims, or demand-session
cleanup.

Verification: formatting, full-feature Clippy, all spark-filtered tests, and
the complete `cargo test -q` suite pass after the extraction.

##### Phase 4C.4 — Reflection work

Status: complete (2026-08-19).

- Move reflection payload/claim/release/cancellation/snapshot and its
  task/wait indexes to `evaluation/coordinator/reflection.rs` only insofar as
  the common record and common ready queue remain coordinator-owned.
- Preserve dormant/reserved/queued/running/blocked/exit-waiting/
  terminalizing transitions and late task-handle observation.

Result: `evaluation/coordinator/reflection.rs` owns the reflection payload,
task/wait indexes, claims and snapshots, failure acknowledgement, reservation
and activation, cancellation, release and exact subscription, exit waiting,
and retirement. `WorkCoordinatorState` retains one child-owned index value;
the common work registry, task ready queue, closure, dependency publication,
and settlement paths remain at the nearest cross-kind owner.

Verification: formatting, all reflection-filtered tests, full-feature Clippy,
and the complete `cargo test -q` suite pass after the extraction.

##### Phase 4C.5 — Deferred work and lazy cycles

Status: complete (2026-08-19).

- Move deferred payload/claim/promotion/release/abandonment, producer indexes,
  and pure-lazy-cycle discovery/terminalization together to
  `evaluation/coordinator/deferred.rs`.
- Keep promise-containing cycles retryable, pure lazy cycles canonical, and
  reusable lazy claims unpoisoned by owner closure or forced kill.

Result: `evaluation/coordinator/deferred.rs` owns deferred producer payloads,
task/wait/value indexes, canonical reservation, demand promotion, claims and
release, abandonment and retirement, plus dependency-cycle discovery and
pure-lazy-cycle terminalization. Cross-kind task lookup, promise ownership,
closure, and settlement remain with the common coordinator.

Verification: formatting, deferred- and lazy-cycle-filtered tests,
full-feature Clippy, and the complete `cargo test -q` suite pass after the
extraction.

Each 4C checkpoint must move its focused tests with the implementation. Common
fairness, closure, cross-kind terminalization, and forced-order tests stay at
the coordinator's nearest common owner.

#### Phase 4D — Settlement and session orchestration

##### Phase 4D.1 — Readiness and settlement

Status: complete (2026-08-19).

- Move internal readiness snapshots, validation, selected terminal
  obligations, and `RuntimeSettlementRelease` to
  `evaluation/coordinator/settlement.rs` as one lifecycle.
- Preserve exclusive settlement admission, stale-generation rejection,
  observational readiness, no implicit commit of divergent exit votes, and
  post-unlock wake/drop order.

Result: `evaluation/coordinator/settlement.rs` owns scheduler-readiness and
deadlock snapshots, disposition validation, selected terminal obligations,
atomic exit/kill publication, and the detached settlement release. Common
work records and producer obligations remain in the coordinator because all
work kinds construct or consume them outside runtime settlement as well.

Verification: formatting, settlement- and readiness-filtered tests,
full-feature Clippy, and the complete `cargo test -q` suite pass after the
extraction.

##### Phase 4D.2 — Session, profile, and evaluation context

Status: complete (2026-08-19).

- Move session report types, `EvaluationSession`, `OwnedEvalContext`, and
  `EvalContext` construction and admission policy to `evaluation/session.rs`.
- Leave `EvaluationDemandState`, `ReflectionTaskLauncher`, and
  `ReflectionTaskProfile` in the facade as the explicit shared bridge. The
  state stores the profile and is retained by coordinator-owned spark and
  client-demand records; moving either side would create a sibling cycle or a
  synthetic abstraction. Do not let an escaped context recover the external
  owner lease.

Result: `evaluation/session.rs` owns session reports and unfinished-work
projections, the external demand-session lease, the owner-retaining direct
context wrapper, evaluation-context construction, and task/deferred/promise
admission policy. `EvaluationDemandState` and the immutable reflection profile
remain in the facade as the deliberate bridge shared with coordinator-owned
records. Sibling access is restricted to the `evaluation` module family, and
an escaped `EvalContext` still retains no route back to its external owner.

Verification: formatting, session- and client-demand-filtered tests,
full-feature Clippy, and the complete `cargo test -q` suite pass after the
extraction.

##### Phase 4D.3 — Cooperative and runtime pumping

Status: complete (2026-08-19).

- Move claimed-machine dispatch, prioritized dependency pumping,
  reflection/deferred release, retirement, and runtime-pump adapters to
  `evaluation/pump.rs`.
- Keep queue selection and state transitions on the coordinator; this child
  orchestrates claims but does not become a second scheduler.
- Preserve same-session FIFO, cross-session dependency assistance, poll
  budgets, lazy-cycle publication, and machine destruction outside locks.

Result: `evaluation/pump.rs` owns client-demand polling, cooperative target
pumping, cross-session dependency prioritization, claimed reflection/deferred
dispatch and release, pure-lazy-cycle publication, and runtime-pump adapters.
Queue selection and lifecycle mutation remain methods of the authoritative
coordinator; the pump only orchestrates detached claims. Session-owned report
construction remains beside the pump because it derives scheduler state for
the cooperative run boundary rather than defining session admission policy.

Verification: formatting, pump-, quiescence-, and cross-session-filtered
tests, full-feature Clippy, and the complete `cargo test -q` suite pass after
the extraction.

#### Phase 4E — Close the evaluation boundary

Status: complete (2026-08-19).

- Leave `evaluation.rs` as a deliberate facade/shared-contract map, not an
  arbitrary line-count target.
- Relocate feature-local tests, retain concurrency and settlement tests at the
  nearest common owner, tighten visibility, and run deterministic ordering
  regressions plus the full gates.
- Update `src/README.md` and evaluation architecture only after the final
  module graph exists. Re-run `public_api`, effect embedding, macro protocols,
  logger integration, worker-count equivalence, all forced-order concurrency
  tests, Clippy, and the complete suite.

Result: `evaluation.rs` is now a 202-line shared-contract facade rather than a
mixed 8,000-line implementation/test owner. `evaluation/coordinator.rs` keeps
the one authoritative cross-kind registry, indexes, queues, dependency model,
and generation/condition-variable state; lifecycle-specific state and
transitions live in focused children. The common evaluation and coordinator
test suites moved to `evaluation/tests.rs` and
`evaluation/coordinator/tests.rs`, preserving their original module scope and
private access without obscuring production ownership. Production session and
pump modules use explicit imports, and sibling construction/polling hooks are
restricted to the `evaluation` family. `src/README.md` and the evaluation
architecture document now describe the final module graph and mutation
boundary.

Verification: public API and external effect-host tests, macro and logger
coverage, worker-count, completion, and settlement filters all pass. Final
`cargo fmt --check`, full-feature Clippy with warnings denied, and the complete
`cargo test -q` suite pass (1,121 library tests plus every integration test
binary).

There is no open semantic question blocking Phase 4B. The review deliberately
rejects two tempting abstractions: a generic completion-wake trait where only
one runtime coordinator exists, and separate work registries which would make
cross-kind readiness and settlement harder to reason about. Revisit either
only for a concrete new consumer or measured bottleneck, not for module shape.

### Phase 5 — Re-inventory and close the dated review

#### Phase 5A — Secondary-candidate decision

Status: complete (2026-08-19).

- Recompute file/production/test sizes and dependency directions after the four
  primary hotspots settle.
- Revisit only the secondary candidates recorded in the inventory.
- Add a small approved extraction phase for a secondary file only when it
  reduces ownership coupling or visibility; otherwise record why it remains
  cohesive or deferred.
- Keep `core.rs` deferred unless a separate GC/value-model transition has
  established its replacement boundary.

The Phase 5 review found no drift that invalidates this scope. The four primary
hotspots now have narrow roots and responsibility-owned children: the binary
root has 294 production lines; `reflection.rs` has 42; `api.rs` has 83; and
`evaluation.rs` has 200. Their remaining large files are implementation or
test owners rather than the former catch-all roots. In particular,
`reflection/machine.rs` owns the persistent effect machine,
`api/assembly.rs` owns assembly construction, and
`evaluation/coordinator.rs` owns the common work registry. Reopening those
completed splits during this phase would conflate a new ownership review with
the dated plan.

The mechanical refresh found 160 Rust files and 106,206 lines: approximately
62,462 production lines and 43,744 test lines under the inventory's original
counting convention. The increase from 127 files is the intended result of the
four primary splits, not newly discovered fragmentation.

| Secondary candidate | Current size | Dependency finding | Decision |
| --- | ---: | --- | --- |
| `reflection/store.rs` | 1,051 production + 497 inline test lines | Snapshots, journals, and commit consume a self-contained conflict-address/index policy. The policy needs only store key and volume identity; query lifetime does not belong to it. | Approve the conflict-analysis extraction in Phase 5B. Keep query allocation, private-volume state, polling, completion, and retirement with the store. |
| `interaction_net/runtime.rs` | 1,859 production + 2,891 dedicated test lines | Frontier observations and cursor dependencies retain `SharedRuntimeNet`, while shared runtime state owns the claims and revisions represented by those protocol values. Splitting either side would broaden private state or create forwarding without a one-way owner. | Defer. Existing `cursor`, `graph`, and `rewrite` children are the useful seams; future net scheduling/normalization work may create another. |
| `g_syntax/parser/logical.rs` | 1,016 production + 131 inline test lines | `DeclarationMacroWork`, macro input discovery, output rendering, and declaration replay form one live declaration-scoped pipeline. The apparent original/generated representation seam is false: `LogicalSource` is used only by a debug round-trip assertion and its own tests, while production expansion re-lexes declaration text. | Do not split. Remove the dormant token mirror and redundant generated-token arenas in Phase 5C, leaving the live replay pipeline cohesive. |
| `g_syntax/parser/structural.rs` | 1,487 production + 438 dedicated test lines | `let`/`where`/`using` and object/`with` syntax share expression floors, resume boundaries, braced/layout body discovery, and helpers consumed by the expression, conditional, pattern, declaration, and `do` parsers. | Keep cohesive. A family split would need a catch-all structural-common module or circular sibling ownership. |
| `g_syntax/parser/source.rs` | 727 production lines | One `StagedSourceParser` owns declaration order, macro lookup and execution, diagnostic framing, reparsing, and language-position validation. | Keep cohesive. Extracting macro staging would leave the source owner as a forwarding shell around the same mutable lifecycle. |
| `g_syntax/resolve/expression.rs` | 1,075 production lines | Dict/path and operator lowering recursively re-enter the general expression resolver. Several small effect/path helpers are deliberately shared by pattern, conditional, `do`, and module lowering. | Defer. A split presently changes navigation but not dependency direction or visibility. Revisit if a semantic IR boundary makes one family independent. |
| `g_syntax/compiler_values.rs` | 802 production + 203 inline test lines | Built-in modules, conditional runners, macro environment, object helpers, and effect paths are constructed as one runtime-local closed-value cache and share lowering/application helpers. | Keep cohesive. Family files would expose cache construction internals without creating independent ownership. |
| `core.rs` | 1,678 production + 516 inline test lines | Values, deferred cells, factory/cache, and functions remain recursively tied to the pending runtime-owned GC/value representation. | Continue the explicit deferral. Do not fossilize the current `Arc` representation as a module boundary. |

This leaves two low-risk follow-ups. One is a genuine ownership extraction;
the other removes an abandoned representation which otherwise makes a false
module seam look architectural. Neither changes Glam semantics.

#### Phase 5B — Extract reflection-store conflict analysis

- Move `ConflictPath`, `ConflictAddress`, the strategy/index traits, and the
  exact, fingerprint, and coarse implementations to
  `reflection/store/conflict.rs`.
- Keep `VolumeId`, query identity/lifetime, snapshots, journals, edits, roots,
  and commits owned by `store.rs`. The conflict child may depend on the
  parent's scalar `VolumeId`; store state depends only on the child's public
  policy surface, not its concrete indexes.
- Re-export the existing public conflict types through `store.rs` and
  `reflection.rs`, preserving every public path. Do not expose the child module
  or widen concrete index visibility.
- Move the store's inline tests to `reflection/store/tests.rs`; keep strategy
  overlap/conservatism tests beside the conflict owner or clearly grouped in
  the store test child. Test movement must not substitute for production
  ownership improvement.
- Verify exact overlap in both directions, volume disjointness, conservative
  fingerprints, coarse invalidation, custom strategy construction through the
  public facade, and the existing snapshot/rebase/query/store suite before the
  full repository gates.

#### Phase 5C — Remove the dormant logical-token mirror

- First latch generated-output validation for reserved `@`/`#`, invalid
  numbers, and unbalanced or mismatched delimiter structure.
- Delete `LogicalSource`, `LogicalToken`, `LogicalTokenKind`, `LogicalIndex`,
  `LogicalGroup`, and their debug-only source round-trip assertion. They do not
  participate in macro expansion or parser input.
- Replace `GeneratedText::classify` and its copied token/number/text arenas
  with a narrow generated-text validator over the authoritative lexer result.
  Preserve the lexical diagnostics and structural-balance behavior without
  retaining a second representation.
- Keep `DeclarationMacroWork`, macro invocation discovery, normalized macro
  input/layout construction, output rendering, embedded values, and replay
  together in `logical.rs`; update its module comment to describe that actual
  role.
- Run the focused logical-parser and macro protocol suites, followed by the
  repository gates. This checkpoint is a representation cleanup, not a macro
  semantics change.

#### Phase 5D — Architecture and cleanup-review closure

- Update `src/README.md` and relevant architecture docs with only the module
  ownership that now exists.
- Record the result under the module-splitting item in
  `CodeCleanup_2026-08-15.md`.
- Audit public paths, visibility, module cycles, stale comments, and the absence
  of new catch-all modules.
- Run all focused suites and the repository-required format, clippy, and full
  test gates.
- Mark this dated plan complete while leaving future growth to a new dated
  module review.

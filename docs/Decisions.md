# Decisions

This log records *why* significant decisions were made: the context, the
choice, the alternatives rejected, and the consequences accepted. Standing
docs keep the rules themselves: `AgentContext.md`, `agent_context/`,
`architecture/`, the design docs, and the collector's `SAFETY.md` and
`VERIFY.md`.

**Status: incomplete.** The log started on 2026-10-03, late in development.
Entries dated earlier were extracted from the plans and reviews that recorded
them. Older decisions may be extracted from git history and other sources
later, so a missing entry does not mean no decision was made.

**Adding an entry.** Copy the template below into its subsystem's section.
Never delete an entry. To supersede one, set its status to `superseded`, add
a **Superseded by:** line naming its successor's slug, and say in the
successor what it supersedes. A decision that needs a long explanation keeps
a short entry here and links out to a standing doc. Refer to an entry by its
slug, a stable identifier that never changes.

**Deciders.** *maintainer* means the maintainer made or ratified the
decision. *agent* means an agent made it while executing a plan or review;
the maintainer accepted these as a group on 2026-10-05.

**Recorded in** names the history doc that recorded a decision, with a
finding or section ID, plus commits where useful. History docs are deleted
once retired, with git as the archive. `docs/plans/README.md` maps each
deleted doc to its last commit, and [History docs cited](#history-docs-cited)
maps the short names used here to file names.

```markdown
### <Descriptive title>
`<slug>` · YYYY-MM-DD · maintainer|agent · accepted|superseded
- **Context:** …
- **Decision:** …
- **Consequences:** …
- **Rule lives in:** <standing doc and section name>   (omit if none)
- **Recorded in:** <history doc short name + finding or section ID>
```

## Process and verification

### Panics are bugs and task-layer interruptions, never semantics
`panics-are-task-interruptions` · 2026-10-04 · maintainer · accepted
- **Context:** two user inputs crashed the process (a net and a parser case),
  and scheduler claims had no unwind containment.
- **Decision:**
  - Code that observes user input detects invalid states first. Evaluation
    reports an `EvaluationFailure`; parsing backtracks and keeps
    closest-match diagnostics.
  - A panic is never an `EvaluationFailure`, a cached lazy result,
    `.alt`/`.fail` input, or a diagnostic. A caught panic leaves the runtime
    usable.
  - Lazies record a `Panicked` evaluation state; tasks end with a `Panicked`
    wait terminal. Caching the panic as a lazy result was rejected: a
    consumer that forgot the case would fail silently into semantics.
- **Consequences:** only the poll boundary creates a panic value, and waiters
  halt instead of re-running the bug. The public surface is
  `ErrorKind::Panic` and `.task.status` `panicked`. Nothing is paid when
  nothing panics.
- **Rule lives in:** `AgentContext.md` "Working Rules";
  `architecture/evaluation.md` "Runtime-owned constructed values".
- **Recorded in:** holistic review X4, Decision 1; panic-safety plan,
  "Containment design" and decision 3.

### Poison recovery follows lock class; runtime-core poison faults the runtime
`poison-recovery-by-lock-class` · 2026-10-04 · maintainer · accepted
- **Context:** a caught panic stranded claims, cascaded poison into aborts,
  and wedged collection.
- **Decision:**
  - Leaf locks recover through `into_inner`; collector traces read through
    poison.
  - A poisoned per-value cell puts its lazy into `Panicked`.
  - Runtime-core poison sets one fault flag: readiness reports `Poisoned`,
    nothing aborts or hangs, and core-reaching destructors become no-ops.
  - Client callbacks outside polls: commits and validations resume the
    client's panic, notifications skip the panicking callback, and a
    panicking launcher interrupts only the task it was launching.
- **Consequences:** a panic in read-only core code is detected lazily, an
  accepted gap. Making the core panic-free is deferred.
- **Rule lives in:** `architecture/evaluation.md` "Runtime-owned constructed
  values" (the core fault only).
- **Recorded in:** panic-safety plan, decision 2 and "Implementation
  sequence" steps 1–5.

### Discover panics by inspection; fuzzing is deferred and never gates
`panic-discovery-by-inspection` · 2026-10-04 · maintainer · accepted
- **Context:** the holistic review's P0-2 asked for a front-end no-panic fuzz
  target.
- **Decision:**
  - Inspect risk-first, classifying sites as I (internal invariant), U
    (user-reachable) or P (poisoning hazard).
  - Fuzz with `cargo-fuzz` only when inspection stalls, using shallow inputs
    and a per-input timeout.
  - Every finding becomes a deterministic regression. No fuzz run joins
    `scripts/check.sh`.
- **Consequences:** supersedes the P0-2 fuzz target. Net fuzzing (polarity
  plan's `polarity-net-fuzzing`) follows this policy: inspection
  (`panic-net-inspection`) comes first, and fuzzing later robustifies and
  finds undiscovered issues, but not while lower-hanging fruit remains
  (maintainer, 2026-10-15).
- **Recorded in:** panic-safety plan, "Method" and decision 1; commit
  `64092e4b`.

### Temporary negative tests during transitions; retire most inventories
`transition-negative-tests-not-inventories` · 2026-10-03 · maintainer · accepted
- **Context:** about 12.8k lines of source-scanning inventories, with exact
  counts and fingerprints, taxed every structural change.
- **Decision:**
  - Retire most inventories.
  - During a major transition, add syntax-backed negative tests keyed by
    module and item. At the closing review, keep only the durable rules.
  - Census totals and fingerprints are never evidence of safety.
- **Consequences:** supersedes "keep exact inventories through Gate G3", and
  overrides the parallel review's RF-003 suggestion to share inventory
  parsing. The surviving set is the holistic review's V4 table (maintainer,
  2026-10-05): one negative rule per area, no counts or fingerprints.
  Applied the same day, leaving about 3.8k lines; the kept rules are listed
  under V4.
- **Rule lives in:** `AgentContext.md` "Working Rules".
- **Recorded in:** holistic review X5, V4, Decision 4.

### Rust code never cites plan or review prose
`no-code-references-to-history-docs` · 2026-10-03 · agent · accepted
- **Context:** a unit test failed because a plan's wording changed.
- **Decision:** `tests/source_doc_coupling.rs` fails if `src/**/*.rs` names
  `docs/plans` or `docs/reviews`.
- **Consequences:** history docs can be edited or deleted freely. Labels
  remain a soft coupling. The guard scans `src/` only.
- **Rule lives in:** `AgentContext.md` "Working Rules".
- **Recorded in:** holistic review X1; parallel review AR-003; commit
  `74088606`.

### Pinned toolchain and three cumulative check levels
`pinned-toolchain-and-check-levels` · 2026-10-03 · agent · accepted
- **Context:** the routine checks were red and incomplete, and reviewers used
  different toolchains.
- **Decision:**
  - `rust-toolchain.toml` pins Rust 1.99.0, matched by `rust-version`.
    Upgrade only in a dedicated commit at a plan boundary.
  - `scripts/check.sh` has three cumulative levels: `fast`, `all` (the
    pre-commit gate) and `full`.
- **Consequences:** Miri and the sanitizers run only when a nightly is
  installed. `full` first ran periodically; `aggressive-gc-rerun-triggers`
  now sets its cadence. Since 2026-10-15 the script checks the `glam`
  package, and glam-gc has its own gate (`crate-scoped-checks`).
- **Rule lives in:** `AgentContext.md` "Verification".
- **Recorded in:** holistic review X2; parallel review AR-004; commits
  `4c849c19`, `5e68163d`.

### Rerun the aggressive-GC check level after risky changes
`aggressive-gc-rerun-triggers` · 2026-10-05 · maintainer · accepted
- **Context:** aggressive-GC verification ran only "periodically", so it
  stayed broken unnoticed from I12A until 2026-10-03.
- **Decision:** rerun `scripts/check.sh full` after changes to runtime or
  collector code, or to unsafe, tracing, mutation, root, admission,
  finalization or scheduler behavior. Documentation-only changes may reuse
  the last result.
- **Consequences:** replaces "run `full` periodically".
- **Rule lives in:** `AgentContext.md` "Verification".
- **Recorded in:** D2h review D2HR-006; GCI11R002 holistic review GCI2HR-006;
  documentation disposition, maintainer answer 5.

### Each crate has its own gate; dynamic tools run by judgement
`crate-scoped-checks` · 2026-10-15 · maintainer · accepted
- **Context:** `scripts/check.sh` tested glam-gc at every level and ran Miri
  and both sanitizers, about an hour, at `full`, after every runtime change.
  The dynamic tools exercise only glam-gc's library suite, so most Glam
  changes gained nothing from them, and glam-gc is a separate crate.
- **Decision:** `scripts/check.sh` checks the `glam` package. glam-gc's
  gate, `crates/glam-gc/scripts/check.sh [all|full|dynamic]`, runs only when
  glam-gc changes, beside Glam's: its suite, then the scale proofs, then Miri
  and the sanitizers, the last by judgement. Glam's `full` keeps the
  aggressive-GC pass and the cursor-stress proofs, with its triggers
  unchanged (`aggressive-gc-rerun-triggers`). Fuzz-style runs, such as the
  parser's oracle sweep, are judgement calls as well.
- **Rule lives in:** `AgentContext.md` "Verification"; `AGENTS.md` "Routine
  Checks".

### Fast tests come from fixing their causes
`fast-test-suite` · 2026-10-09 · maintainer and agent · accepted
- **Context:** workspace tests took about 225 s in `scripts/check.sh fast`.
  The library suite used under three of eight cores. The maintainer asked
  for slow tests to move to a slower tier, keeping faster versions in
  `fast`, and suggested isolated runtimes if tests contended.
- **Decision:**
  - Shared test runtimes are per thread, so per test, instead of
    process-wide. Tests had contended on one heap, which also grew for
    the whole run.
  - The dev profile builds the workspace at opt-level 1 and dependencies at
    opt-level 3.
  - No slow tier yet: afterwards the slowest test takes 2.5 s. Rejected for
    now: moving tests whose slowness came from contention or an
    unoptimized build.
- **Consequences:** workspace tests take about 20 s. A rebuild after an
  edit takes 15 s instead of 12 s, and stepping in a debugger needs an
  opt-level 0 override. A test that again takes more than a few seconds
  gets a slow tier (`test-slow-tier`).
- **Rule lives in:** `AgentContext.md` "Verification";
  `agent_context/evaluation.md` "Verification Discipline".
- **Recorded in:** fast checks plan, `test-fast-tier`.

### Release builds use one codegen unit
`release-one-codegen-unit` · 2026-10-14 · maintainer · accepted
- **Context:** with the default 16 codegen units, how the `glam` crate was
  partitioned decided which hot helpers were inlined, so unrelated edits
  moved instruction counts by about 1%, and some heap-hold steps needed
  `#[inline]` hints to recover regressions that were only partitioning.
- **Decision:** `[profile.release]` sets `codegen-units = 1`.
- **Consequences:** across the profiling workloads, instructions fall 2.3%
  (most workloads 2% to 5%) and CPU time 3.9%. `parse_ifs_16` rises 1.4%,
  and `chain_where` rises with size (+5.8% at 800), in the top-level token
  scan of `contextual_keywords`, a quarter of that workload. A release
  build of `glam` takes about 35% less CPU time but about twice as long,
  since the crate compiles on one thread; `check.sh` builds no release
  binary, so only profiling pays. Instruction baselines from before this
  change are not comparable with later ones.
- **Rule lives in:** `Cargo.toml` `[profile.release]`.
- **Recorded in:** explicit heap holds plan, `gc-hold-boundaries` fourth
  slice.

### Tests follow the runtime, not legacy paths
`tests-follow-the-runtime` · 2026-10-14 · maintainer · accepted
- **Context:** tests that hold the heap differently from production keep
  legacy and transitional entry paths alive and test something production
  no longer does.
- **Decision:** tests reflect runtime behavior where feasible; test-only
  hooks are the exception. When the architecture moves, the tests move with
  it, and a test that existed only for a retired mechanism retires with it.
  Transition aids go as early as is safe rather than staying for tests.
- **Consequences:** `Heap::mutate_recursive` goes before the test
  conversion (`gc-hold-outer-boundaries`).
- **Recorded in:** explicit heap holds plan, "Decisions, 2026-10-14".

### Retired plans and reviews are deleted, not archived
`retired-history-is-deleted` · 2026-10-05 · maintainer · accepted
- **Context:** retirable history docs add search noise, and several state
  superseded behavior in the present tense.
- **Decision:** delete a retired history doc, with git as the archive, once
  its durable content lives in standing docs or this log, nothing current
  references it, and `docs/plans/README.md` lists its last commit.
- **Consequences:** recovery is through `git show`. An in-tree archive was
  rejected as a third tier that is neither authority nor gone. This matches
  earlier deletions (`233fde61`, `d35d899e`, `4e1d7925`).
- **Rule lives in:** `docs/plans/README.md`, retention rule and Retired
  history table.
- **Recorded in:** holistic review X8, Decision 5; documentation disposition,
  "Delete vs archive" and maintainer answer 1.

### One decision log
`one-decision-log` · 2026-10-05 · maintainer · accepted
- **Context:** decision rationale was spread across dozens of plans and
  reviews that are being retired.
- **Decision:** decisions live in this one log, grouped by subsystem. A
  decision that needs a long explanation links out from its entry.
  `AgentContext.md` links the log.
- **Consequences:** one file per decision was rejected as recreating the
  doc-count sprawl being cleaned up.
- **Rule lives in:** `AgentContext.md` "Where to Look".
- **Recorded in:** documentation disposition, "Proposed target structure" and
  maintainer answer 2.

## Language and front end

### Local bindings take parameters
`local-bindings-take-parameters` · 2026-10-10 · maintainer · accepted
- **Context:** `let` and `where` bindings accepted only a name, so a local
  helper was written `go = \k -> Body`, and `go k = Body` failed as an
  invalid name; the syntax docs had no examples either way.
- **Decision:** a local binding takes parameters as a module definition
  does, as lambda sugar: `let add a b = a + b in Body` binds
  `add = \a b -> a + b`. Parameters are local names separated from the
  binding's name.
- **Rule lives in:** `Syntax.md` "Local Definitions".
- **Recorded in:** structural overheads plan, `perf-module-definition-cost`.

### Lazy locals pass directly
`lazy-locals-pass-directly` · 2026-10-11 · agent · accepted
- **Context:** lowering made a thunk for every lazy argument or list item
  that was not a constant, a local among them, so `f x` built
  `\c -> c` over `x`: an applied net, a copy and a bind join to reach a
  value already in hand. These were 17 to 48% of all applications in the
  profile families.
- **Decision:** a lazy argument or list item that is a local takes the
  local as it is. A local is a value, possibly a lazy one, and an operator
  waits for its operand either way, so strictness and sharing are
  unchanged.
- **Consequences:** instructions fall 7 to 23% across the profile
  families (`list_map_2000` 22.7%, `hello_elf` 7.3%).
- **Rule lives in:** `ResolvedNetLowerer::compile_lazy_into`
  (`g_syntax/net_lowering.rs`).
- **Recorded in:** structural overheads plan,
  `perf-thin-wrapper-applications`.

### Pure conditionals build their results directly
`pure-conditionals-build-results` · 2026-10-11 · maintainer · accepted
- **Context:** a pure `if` or `match` is a search of effects, `.cut` over
  `.alt` of `.seq` steps ending in `.r`, which the list handler runs. Every
  operation is an effect value, `{eff: \api -> api.op}`; applying one to
  each operand builds another effect, and running it applies the handler
  through each of those layers to reach the list builtin. In a countdown
  iteration 45 of 51 lazies served this interpretation. Booleans are
  `.r ()` and `.fail`, so the guards themselves stay effects. The
  maintainer accepted skipping the interpretation layer for this common
  template as an acceleration.
- **Decision:**
  - Lowering builds a pure conditional's results as the list handler
    would give them, from list builtins (`ResolvedChoice::emit_results`):
    alternatives join with `list.concat`, a step that binds an effect maps
    the rest over that effect's results and concatenates them, a result is
    a one-item list, and no alternative is the empty list. Only the effects
    a guard evaluates run through the handler.
  - `if` and a cut `match` select the first result, so the search's cut is
    left out; an open match's value is the result list itself. Host forms
    keep the effect search for their ambient handler.
  - `map` and `list.concat` are structural and force nothing
    (`structural-lazy-map-and-concat`), so results are still observed in
    source order and a later alternative runs only when the earlier ones
    are exhausted.
  - The compiler's `defined_or` helper was built the same way; since
    2026-10-12 it is the `default` builtin (`definition-journal`).
- **Consequences:**
  - Instructions against `nets-compile-on-request`: `countdown` −49.0%,
    `append_walk` −41.8%, `list_sum` −35.5%, `hello_elf` −12.0%, `minimal`
    −4.6%, `hello_do` −3.2%, others within 0.6%.
  - A shallow countdown now finishes before the first collection, so the
    constant-heap test compares two depths past it.
  - Lowering a known `.op x y` as `{eff: \api -> api.op x y}`, one handler
    for all operands, was measured first and not adopted: closures cost
    more to build and apply once than the effects they replace (`hello_elf`
    +31%; with jit, `countdown` −11% but `do_chain` +14%, `minimal` +8%).
- **Rule lives in:** `agent_context/g_syntax.md` "Do, Patterns, and
  Conditionals"; `ResolvedChoice::emit_results`
  (`g_syntax/resolve/conditional.rs`) and
  `for_each_effect_result_resolved` (`g_syntax/resolve/effect_steps.rs`).
- **Recorded in:** structural overheads plan, `perf-pure-conditionals`.

### Binding groups lower by dependency components
`binding-groups-lower-by-components` · 2026-10-10 · agent · accepted
- **Context:** a `let` or `where` group is documented as mutually recursive,
  but lowered as `(\x1 x2 -> body) e1 e2`, so a binding could not name a
  sibling: `let { y = x; x = 42 } in y` failed to compile, and the name
  analysis agreed with the implementation. The maintainer prioritized the
  fix.
- **Decision:**
  - A group's names are in scope in every value and the body, in the
    resolver, the name analysis and the unused-local warnings.
  - The group lowers by its strongly connected components, dependencies
    first: an acyclic component as a plain application, a cycle through
    `fixpoint` (the lazy `x = f x`), one binding directly and several
    through one dictionary of the members, each bound to its own entry.
  - A cycle with no value, such as `let { x = y; y = x } in x`, fails as a
    lazy dependency cycle.
- **Consequences:** groups without cycles lower as before, in dependency
  order; the dependency analysis costs 0.1 to 0.3% on the profile
  workloads.
- **Rule lives in:** `agent_context/g_syntax.md` "Names and Scope".
- **Recorded in:** structural overheads plan, `perf-module-definition-cost`.

### Script extensions select a compiler; unknown ones are rejected
`script-extension-selects-compiler` · 2026-10-05 · maintainer · accepted
- **Context:** `--script.json` silently parsed as `.g`, contradicting
  `CLI.md`, which says the extension selects the front-end compiler.
- **Decision:** reject an extension that has no front-end compiler, failing
  fast as the language declaration does.
- **Consequences:** a script's extension is meaningful, and adding a
  front end means registering its extension. Applied to files and
  non-binary imports as well, through one `FrontEnd` selector in
  `api/assembly.rs`, since a file extension selects the compiler in the same
  way.
- **Rule lives in:** `CLI.md` and `architecture/assembly.md` "Module
  Construction".
- **Recorded in:** holistic review A5, Decision 7.

### Language declarations fail fast; source is ASCII unless `utf8`
`fail-fast-language-declaration` · 2026-10-05 · maintainer · accepted
- **Context:** `language g9 with nonsense` compiled and ran, contradicting
  `DistilledDesign.md` and `Syntax.md`.
- **Decision:**
  - A base other than `g0`, or an extension other than `utf8`, is an error
    that stops parsing, so nothing is lowered under a language the source
    did not ask for.
  - Without `utf8`, the first non-ASCII character anywhere, texts and
    comments included, is an error.
- **Consequences:** compile, inspect and test paths all check the
  declaration. The `demo` extension tests were inverted, and three invalid
  samples were added.
- **Rule lives in:** `agent_context/g_syntax.md` "Lexical and Layout
  Ownership"; `architecture/front_end.md` "Built-in `.g` Pipeline".
- **Recorded in:** holistic review F7, Decision 8; commit `8853d4e8`.

### A joint colon after a name always makes a tag
`joint-colon-makes-a-tag` · 2026-10-06 · maintainer · accepted
- **Context:** the parser oracle found that the grammar read `(and:a)` as a
  prefix section of `and` but `(and:1)` as a tag, choosing by whether the
  rest parsed. In argument position, `f and:a` was already a tag.
- **Decision:** a name followed by a joint `:` and a joint payload is always
  a tag, the operator names `and` and `or` included. Sections are spelled
  `(and x)`, `(x and)` or `(and)`.
- **Consequences:**
  - A paren's first token decides its form without a trial parse, and both
    expression parsers agree.
  - The grammar applies the tag lookahead to every infix operator. So
    `x and:y` is a tag argument even when the payload is invalid, as in
    `x and:and`, instead of backtracking to the operator (2026-10-07).
- **Rule lives in:** `SyntaxCheatSheet.md` "Atoms, Tagged Data, Dicts".
- **Recorded in:** parser backtracking plan, "Progress 2026-10-06".

### A keyword before a joint `:` or `.` is a key, never a keyword
`keyword-before-key-mark-is-a-key` · 2026-10-15 · maintainer · accepted
- **Context:** testing keyword regions found that the structural layer chose
  keyword forms and contextual keywords by spelling alone. `if:1`,
  `where:1` and `object:1` were rejected as malformed forms, `(f if:1)` was
  read as a postfix `if`, and `if.x then a else b` parsed as an `if` on the
  effect `.x`. The term parser already treated `if:1` as a tag. Four
  parser-local checks judged keyword position, each slightly differently.
- **Decision:** one predicate, `in_keyword_position`, decides for every
  keyword head and contextual keyword. A keyword before a joint `:` or `.`
  is a tag's key or a path's root, never a keyword. The term parser's
  keyword regions and the grammar's keyword forms (`form_keyword`) apply
  the same test.
- **Consequences:**
  - `if:1` and `if.x:1` are tags in every expression position, as
    `Syntax.md` "Keywords" requires.
  - A keyword path root such as `if.x` or `object.x` reports the keyword as
    reserved. `then.x`, `else.x` and `in.x` are paths too, so their form
    reports its missing keyword.
  - No `.g` source used a keyword before a joint `.` or `:`.
- **Rule lives in:** `Syntax.md` "Keywords"; `agent_context/g_syntax.md`
  "Names and Scope".
- **Recorded in:** parser backtracking plan, "Keyword regions 2026-10-15".

`patterns-parse-as-cover-terms` · 2026-10-16 · maintainer · accepted
- **Context:** the pattern parser split views, predicates and guards at
  each candidate token and retried, so trial order decided precedence and
  nested patterns cost quadratic time. `(Nat n as m)` was a predicate over
  `n as m`, `(p as Nat n)` was rejected, and `(f <| g x)` was a predicate
  `f <| g` over `x`.
- **Decision:** the term parser reads a pattern once in cover mode, into a
  role-neutral term that converts to a pattern or an expression by role.
  Loosest first, `when`, one view arrow and `as` sit above the expression
  grammar, whose precedence holds below them. Views, predicates and local
  guards need a complete position, a parenthesized pattern or a whole
  match-arm head. Completeness passes through `as`, a view's pattern and a
  guarded pattern.
- **Consequences:**
  - `(Nat n as m)` is `(Nat n) as m`, which binds the same, since a
    predicate passes its input on. `(p as Nat n)`, `(f -> Nat n)` and
    `(Nat n when n > 0)` become valid.
  - `(f <| g x)` and `(Prefix "x" rest ++ [y])` become errors: only `++`
    joins patterns, and a predicate inside `++` needs its own parentheses.
  - No `.g` source used a shape whose meaning changed.
  - Pattern errors come from the shared grammar, so some messages changed;
    the precise ones for view arrows, `?:` markers and path splices remain.
- **Rule lives in:** `Syntax.md` "Pattern Matching"; `SyntaxCheatSheet.md`
  "Patterns".
- **Recorded in:** parser backtracking plan, "Patterns 2026-10-16".

`bindings-split-before-keyword-forms` · 2026-10-16 · maintainer · accepted
- **Context:** a do statement split at its first top-level `<-` or `=`,
  else tried the whole statement as an expression, else tried each `->`
  from the right; guard clauses did the same. So `do { if (a, b) = .r 1
  then .r a else .r 0 }` and `do { let x = 1 in .r x }` were split inside
  their keyword form and rejected, and an operation holding a parenthesized
  keyword form was parsed again at every level, exponential in nesting.
- **Decision:** a statement or guard clause binds with `<-` or `=` only when
  that token is at its top level before any `\` and any keyword but `as`
  and `when`; a pattern holds no lambda or keyword form, so a keyword form
  owns every binder after its head. Otherwise the clause is an operation
  parsed once, in a context where a top-level `->` ends its open tails: an
  `else` branch, a `let` or `using` body, a postfix `if`'s fallback and a
  lambda body. A pattern follows that `->`.
- **Consequences:**
  - The four statement shapes above parse, and `if c then .a else .b -> x`
    binds without parentheses, as the trial split happened to allow.
  - Outside bindings and views, `->` after a tail is still an error.
  - `and` splits guard clauses only in keyword position, so `f and:x` is one
    clause, as `keyword-before-key-mark-is-a-key` requires.
  - Expression sides keep the full expression parser, so `y = z where
    z = 1` and `.r d with {...} -> y` parse as before; an inline `where`
    statement in braces is now an operation, no longer a pattern error.
- **Rule lives in:** `Syntax.md` "Effects"; `SyntaxCheatSheet.md` "Effects &
  Do Notation".
- **Recorded in:** parser backtracking plan, "Patterns 2026-10-16".

### Tail forms run to the end of their region, which separators end
`tail-forms-end-at-separators` · 2026-10-16 · maintainer · accepted
- **Context:** `tail-position` let only a closer, a separator or a line end
  follow a tail form that ends on its line. That rejected
  `if a then if b then c else d else e`, `if c then .a else .b -> P` and
  `if any xs \x -> x > 0 then …`, though each has only one valid reading:
  every prefix `if` and `try` has an `else`, so their keywords pair as
  brackets do, and nothing in an expression continues with `then`, `else`
  or a binding's `->`.
- **Decision:** a tail form runs to the end of its region. A region ends at
  a closer, `,` or `;`, a line end, or a separator keyword: one that no
  expression continues with, which are `then`, `else`, `in`, `where`, `=>`,
  `=>?`, and a binding's or view's `->`. A tail form never ends at a token
  that could continue it: an `and` between guard clauses, which could be a
  boolean `and`, or a `with` after a match's subject, which could begin an
  update. After a tail form's `}` or dedent only an operator or a keyword
  may follow, never an argument or a path; postfix `if` is an operator.
- **Alternatives rejected:** keeping `tail-position`, which needs
  parentheses where no reader could misread the form; and a parse error
  for a lambda before a binding's `->` (`f \a -> E -> P`), whose only
  reading a linter can flag instead.
- **Consequences:**
  - Legal again: nested inline `if`s, a conditional or lambda before a
    binding's `->`, a lambda in guards before `then` or `=>`, and
    `if c then do a else b`. A linter may flag deep inline nesting and the
    lambda before `->`.
  - Still parenthesized: a conditional bound in a guard that `and`
    follows, a match subject that `with` ends inline, and an argument or a
    path after any tail form.
  - Supersedes `tail-position`, and restores the reading of a binding's
    `->` in `bindings-split-before-keyword-forms`.
- **Amended by:** `tail-forms-are-never-arguments` (2026-10-17): a tail form
  no longer stands as an application's final argument.
- **Rule lives in:** `Syntax.md` "Tail Position"; `SyntaxCheatSheet.md`
  "Operators, Pipes, Application".
- **Recorded in:** grammar review, `grammar-tail-position`.

### A tail form is never an application's argument
`tail-forms-are-never-arguments` · 2026-10-17 · maintainer · accepted
- **Context:** a tail form could stand as an application's final argument:
  `f do …`, `map xs \x -> …`, `g tag: f x`, `describe match v as …`. The
  maintainer found each hard to read, since nothing before the form shows
  that the rest of the region is the argument. An inline one also takes the
  operators after it: in `map values \value -> transform value |> finish`
  the lambda's body takes `|> finish`. A parser probe over the valid samples
  and the standing docs found 10 sample `f do` arguments and about a dozen in
  the docs, one tail lambda (that one), and no tail tags or tail `match`
  outside the rule's own examples.
- **Decision:** an application's arguments are atoms. A tail form stands only
  after an operator, a spaced tag's prefix included, after a separator
  keyword, or where a region begins. `f <| do …`, `f (\x -> …)` and
  `g (tag: f x)` replace the tail arguments; the error suggests parentheses
  or `<|`.
- **Alternatives rejected:** exempting `do`, whose block ends visibly at a
  dedent or `}`, which would make one keyword a special case; and allowing
  only forms closed by a block, which would keep `describe match v as`.
- **Consequences:**
  - `|>` and `<|` still do not mix, since no reader remembers a precedence
    between them. A pipeline stage that takes a tail form parenthesizes it,
    or a `let` or `where` names one fragment of the pipeline. A suggestion
    fitted to the context would read better than the one-size-fits-all
    parentheses, which suffice for the bootstrap.
  - Pipeline stages read alike: `xs |> filter (\x -> …) |> map (\x -> …)`,
    where before only the last stage could drop its parentheses.
  - The term parser's tail-lambda and tail-tag layouts go, and the printer
    parenthesizes every tail form in argument position.
- **Rule lives in:** `Syntax.md` "Tail Position"; `SyntaxCheatSheet.md`
  "Operators, Pipes, Application".
- **Recorded in:** tag prefix plan, `tail-argument-retirement`.

### A `{` on the line of a match's `with` delimits its arms
`brace-after-with-delimits-arms` · 2026-10-16 · maintainer · accepted
- **Context:** after `with`, a `{` began braced arms or a first layout
  arm's dictionary pattern, depending on whether an arm mark followed the
  group's closing `}` on its line: a reader had to find the group's end
  and look past it.
- **Decision:** a `{` on the line of `with`, or of a match's `when`,
  delimits the arms; a line break begins layout arms, and anything else a
  hanging arm. A first arm with a dictionary pattern begins on the next
  line.
- **Consequences:** `match v with {a:x} => x` and arms in braces on the
  line after `with` are errors.
- **Rule lives in:** `Syntax.md` "Match"; `SyntaxCheatSheet.md`
  "Conditionals & Patterns".
- **Recorded in:** grammar review, item 1.

### A `when` that ends its line heads nested arms
`when-ending-its-line-heads-nested-arms` · 2026-10-16 · maintainer · accepted
- **Context:** an arm's `when` began guards when an arm mark followed later
  on its line, and nested arms otherwise, so its role depended on the rest
  of the line, and guards could not continue onto another line.
- **Decision:** a `when` that ends its line heads nested arms, and any
  other begins guards, which may continue onto later lines before the
  arm's arrow. A `when` before a `{` still heads braced nested arms only
  when no mark follows the group on its line, since a guard may bind a
  dictionary pattern (`p when {} <- e => r`); that case is open in the
  review.
- **Consequences:** multi-line guards parse.
- **Rule lives in:** `Syntax.md` "Match".
- **Recorded in:** grammar review, item 2.

### After an operand, an `if` is postfix
`conditional-after-an-operand-is-postfix` · 2026-10-16 · maintainer · accepted
- **Context:** `f a if b then c else d` and `f a if b else c` differed
  only at the `then` or `else` after a condition of any length.
- **Decision:** an `if` after an operand is postfix, so a prefix `if` as an
  argument is parenthesized; so is a prefix `try`, since `if` and `try`
  follow the same rules, as `match` and `try_match` do.
- **Consequences:** `f if c then a else b` is an error;
  `f (if c then a else b)`, `x + if c then a else b` and `f a if c else b`
  parse.
- **Rule lives in:** `Syntax.md` "If Then Else"; `SyntaxCheatSheet.md`
  "Operators, Pipes, Application".
- **Recorded in:** grammar review, item 3.

### Guards are an `and` chain
`guards-are-an-and-chain` · 2026-10-16 · maintainer · accepted
- **Context:** guards split at every `and`, so `if a or b and c then …`
  read as `(a or b) and c`, while the expression `a or b and c` is an error
  because `and` and `or` have no precedence relationship.
- **Decision:** guards are an `and` chain whose operands may bind; an `or`
  among them is parenthesized, as in any `and` chain.
- **Consequences:** `if a or b and c then …` is an error; `if a or b then`
  and `if (a or b) and c then` parse.
- **Rule lives in:** `Syntax.md` "Guard Clauses".
- **Recorded in:** grammar review, item 4.

### A match's arms follow `as`
`match-arms-follow-as` · 2026-10-16 · maintainer · accepted
- **Context:** `with` began a match's arms, an object or `extend` body, and
  an update. After a subject it could also begin an update, so an updated
  subject was parenthesized and the parsers needed rules for which match
  claims a `with`. Guard arms already follow `when`.
- **Decision:** a match's arms follow `as`: `match Expr as Arms`, likewise
  for `try_match` and the starred forms. The first ungrouped `as` after a
  match head, innermost first, ends its subject, and is a separator keyword
  there. `Dict as Name with Defs` lets an `as` continue an expression
  elsewhere, so such an update is parenthesized in a subject. The lexer
  records which `as` ends a subject (the match heads still reading their
  subjects, per group), so a `{` after it opens `;`-separated arms while an
  as-pattern's `{` stays a `,`-separated dictionary.
- **Alternatives rejected:** keeping `with`; separating braced arms after
  `as` with `,`; and a keyword with no other role, such as `of`, which
  loses the pairing of pattern arms with `as` and guard arms with `when`.
- **Consequences:**
  - `with` has one meaning, "with these definitions", and an update needs
    no parentheses in a subject.
  - `brace-after-with-delimits-arms` applies to `as`.
  - The maintainer may revisit `Dict as Name with Defs`.
- **Rule lives in:** `Syntax.md` "Match"; `SyntaxCheatSheet.md`
  "Conditionals & Patterns".
- **Recorded in:** grammar review, item 11.

### A `{` opens a block only on its keyword's line
`braces-open-blocks-on-their-keywords-line` · 2026-10-16 · maintainer · accepted
- **Context:** after `brace-after-with-delimits-arms`, a `{` after a `when`
  still headed nested arms or began a guard binding a dictionary pattern
  depending on whether an arm mark followed the group on its line, and a
  `do` still looked past line breaks for its `{`, so a statement binding a
  dictionary pattern on the next line read as a braced block.
- **Decision:** a `{` on the line of `with`, `when` (heading arms) or `do`
  delimits that block; on a later line it begins an arm's pattern or a
  statement. After `when`, a `{` on its line always heads nested arms, so a
  guard that begins with a dictionary pattern is parenthesized:
  `p when ({}) <- e => r`. Settles the case
  `when-ending-its-line-heads-nested-arms` left open.
- **Consequences:**
  - `do` followed by `{a:x} <- op` on the next line binds; `do` followed by
    `{}` alone on the next line is no longer an empty braced block but a
    layout block holding an empty dictionary, the one change in the review
    so far that alters a meaning instead of rejecting it. A formatter is
    expected to favor a `{` on its keyword's line wherever braces are used.
  - Errors for a misplaced `{` name the rule.
- **Rule lives in:** `Syntax.md` "Effects" and "Match";
  `SyntaxCheatSheet.md` "Effects & Do Notation" and "Conditionals &
  Patterns".
- **Recorded in:** grammar review, items 12–14.

### A nested match's arrows may share its arm's line
`nested-arrows-share-an-arms-line` · 2026-10-17 · maintainer · accepted
- **Context:** the structural parser split arms by scanning, so it allowed
  one `=>` or `=>?` on an arm's first line. After
  `brace-after-with-delimits-arms`, a later arrow on that line can only
  belong to a nested match's hanging arms.
- **Decision:** the rule is dropped. `P => match E as Q => R` nests a
  match with one arm in the outer arm's result; a second arrow of one arm,
  as in `P => 1 => 2`, is still an error.
- **Consequences:** a linter may flag same-line nesting.
- **Rule lives in:** `Syntax.md` "Match".
- **Recorded in:** grammar review, item 5.

### A `where` scopes its whole definition, statement or arm
`where-scopes-its-definition` · 2026-10-17 · maintainer · accepted
- **Context:** `where` attached to the nearest expression whose body had
  closed at its indentation, so changing only its indentation could change
  its owner. In practice it attached to whatever view-level dispatch found:
  an `else` branch or a `let` body owned a trailing `where`, and a `then`
  branch one before its `else`. One in a layout `do` statement failed to
  parse, and `f (a where a = 1)` was rejected.
- **Decision:** a `where` scopes the whole definition, statement or arm it
  ends: a declaration's, member's or local binding's right-hand side, a
  `do` statement's expression, a match arm's result, or a parenthesized
  expression's contents. It ends the open tails before it. Layout decides
  which of these a `where` line belongs to; indentation never selects a
  smaller expression. Each takes one `where`, whose one mutually recursive
  group holds all its bindings: chained suffixes no longer compose as an
  expression, and one group says what they said (maintainer). A `where`
  binding is a definition, so a `where` on its line scopes it.
- **Errors:** a `where` in a conditional's guards or `then` branch, a
  match's subject, an arm's head or guards, a list, dictionary or tuple
  item, or after a bind's pattern; one that begins a statement or arm; a
  second `where`; and anything after a `where`'s bindings.
- **Alternatives rejected:** keeping attachment by indentation; keeping
  chained `where` groups, the later the outer scope.
- **Consequences:**
  - Meanings change: `if c then a else b where …`, `a if c else b where …`
    and `let … in e where …` scope the whole form, no longer the last branch
    or the body; `Body where x = y where y = 1` reads as
    `Body where x = (y where y = 1)`, no longer as a left-associative chain.
    The second change is the agent's reading of the decision, taking a
    `where` binding as a definition.
  - The `chain_where` profiling workload, one line of `where`s, now nests
    each group in the binding before it. Parsing it costs about five times
    the CPU time it did at 1,600 bindings, since each nested binding's
    structural parse rescans the rest of the line, until
    `parser-keyword-frames-suffixes` makes `where` a frame. Its baselines
    from before are not comparable. (Resolved 2026-10-17: with `where`
    frames it parses in its earlier time.)
  - A parenthesized expression is a body wherever it stands, so
    `f (a where a = 1)`, `f (let a = 1 in a)` and `f (a with B := 1)` parse
    as arguments, in guards and in statements.
  - Implements `where` as the separator keyword
    `tail-forms-end-at-separators` lists.
  - The legacy grammar keeps the old attachment while the oracle compares
    it.
- **Rule lives in:** `Syntax.md` "Layout Bodies and Expression Resumption"
  and "Local Definitions"; `SyntaxCheatSheet.md` "Functions, Locals".
- **Recorded in:** grammar review, item 7.

### Aliases and locals are body declarations
`aliases-and-locals-are-body-declarations` · 2026-10-17 · maintainer · accepted
- **Context:** `as Name` in object and `extend` headers and
  `Dict as Name with Defs` named a body's alias outside the body, and the
  update form let an `as` continue an expression, so an aliased update was
  parenthesized in a match's subject (`match-arms-follow-as`). A helper
  that needed the alias was a `where` on each member, a member leaking into
  the result, or a `let … in` around the update, which cannot see the
  alias.
- **Decision:**
  - `alias Name` is a declaration that may stand only as the first member
    of a `with` body (object, `extend` or update), at most once. It binds
    `Name` and `_Name`; `alias _Name` marks it as possibly unused. `alias
    self` localizes the body's names: a bare `foo` is `self.foo`, and the
    host's names and locals take `^name` or `^(Expr)`. Only `alias self`
    binds `self`; under a named alias or `alias _`, `self` keeps its host's
    meaning. `alias _` states that there is none, with no way to reference
    the object, and leaves names to the host. Defaults: `self` for objects, `_` for updates.
    A module has `alias self` too, so `self.foo` reaches its namespace
    from its own scope, and `module` is a reserved alias for it from any
    scope. `alias` is reserved in `g0`, and headers lose `as Name`.
  - `using Dict` localizes as `alias self` does, hiding the host's locals
    behind `^` too: a controlled, coarse-grained shadowing in a language
    that otherwise blocks it, so a bare name there reads the scope's own
    even beside a hidden local.
  - `let` groups, as body members or file declarations, are private
    bindings, not entries of the result. Each is mutually recursive within
    itself and seen only by what follows it. A body's group binds where it
    stands, against the definitions before it. A file's is compiler state,
    lowered once where it stands against the module's final self and the
    definitions visible there, and shared by the declarations after it;
    a macro invocation sees the file's `let`s before it.
  - Locals have one meaning in a file: a `let` may not share a name the
    file uses for a global or a local in scope, and `_x` naming any local
    is an error.
  - `let` hinders extensibility by hiding logic, so it is not encouraged:
    a lint heuristic may flag it, and its performance is not a target.
- **Alternatives rejected:** keeping `as Name` in headers; private helpers
  as members or per-member `where`s; one recursive group per body or per
  file, which would let a declaration see a `let` below it whose
  `_module` reads differ; a hidden module entry for a module `let`, which
  no user-defined front end could make; no `let` at all, which leads to
  awkward workarounds.
- **Consequences:**
  - `as` no longer continues an expression, so an update needs no
    parentheses in a match's subject.
  - In every scope, `foo` is a visible local or `self.foo`, and each `^`
    steps out of the innermost `self` to its lexical parent, a body without
    its own `self` being transparent; `^` at module scope is an error. An
    escape sees the locals lexically visible where it lands, and its own.
  - Before this, a named object alias, even `self`, left bare names to the
    host, an object's `as _` localized them, `self` named the innermost
    object or update whatever its alias, `^` stepped out of one body
    whatever it bound and saw every local, and lexical locals outranked
    localized names everywhere, `using` included: drift from the intended
    reading, which this restores.
  - A `let` macro, or any `let`, that reads a global waits on the final
    module, as a global helper does.
- **Recorded in:** grammar review, item 15.

### A lone `do` statement still runs as an effect
`lone-do-statements-run-as-effects` · 2026-10-17 · maintainer · accepted
- **Context:** a `do` block with no statement before its final one lowered
  to that expression, so `do 42` was `42`, and `using d do x ++ y` a text.
  The final statement must express an effect, yet nothing checked it until
  something ran the block, and used as data the block passed for its
  statement.
- **Decision:** a lone statement lowers as `.r () >>= \_ -> X`. For an effect
  that is `X` by left identity; anything else fails when the block runs.
- **Alternatives rejected:** `.r () =>> X`, which adds a unit assertion that
  cannot fail; and a builtin that checks for an effect, needless at the
  measured cost. Asserting that a value is an effect belongs to a
  typechecker or an annotation; this lowering is a workaround until one
  exists (maintainer, 2026-10-17).
- **Consequences:** on the profiling suite the change costs +0.01% of all
  instructions, within noise; `hello_elf` takes 71 more WHNF delegations and
  14 more builtin steps, and no more reflection steps. A test that put a
  text under `using … do` now uses `using … in`.
- **Rule lives in:** Syntax.md "Effects"; `SyntaxCheatSheet.md`.
- **Recorded in:** syntax type errors review, question 2.

### Tags are prefix operators, read alike everywhere
`tags-are-prefix-operators` · 2026-10-16 · maintainer · accepted
- **Context:** tags and dict members read differently by context. `:foo`
  constructed outside braces but punned as a whole member; `foo:f x` was
  `(foo:f) x` outside braces but `foo:(f x)` as a member, whose value ran to
  its comma; and `foo: x` was a member in braces and an error outside them.
  Braces carried a member grammar of their own.
- **Decision:**
  - `path:` is a prefix operator building tagged data. Joint to its
    operand, it binds tighter than juxtaposition. Followed by a space or
    line break, its operand is the rest of its item, as a lambda's body is,
    and an operator cannot begin it. With nothing after it in its item, it
    is its constructor: `(foo:)`, `[foo:, bar:]`, `x |> foo:`. Every path
    form takes part, and in patterns the optional `path?:`.
  - `:name` is `name:name` everywhere, a pun on one bare name.
  - A dict literal is the union of its items, each an ordinary expression;
    braces lose their member grammar.
  - Patterns read alike: `foo:p`, `foo: p` and `(foo:) p` are one tag
    pattern, and `:x` captures `x` under its key. A spaced payload is a
    complete position, pattern operators included; a joint one is nested.
- **Alternatives rejected:** keeping the brace member grammar; path puns
  (`:a.b`), deferred until capturing patterns into structured dicts has a
  consistent approach.
- **Consequences:**
  - `{foo:f x}` changes from `foo:(f x)` to `(foo:f) x`, and `:foo` from a
    constructor to a pun. The corpus moved; until it is retired
    (`tags-prefix-diagnostic-retirement`), a brace item that begins with a
    joint tag yet is no tag is an error, which catches what a sweep missed.
  - A joint tag's payload is an atom, so a lambda after one needs the
    spaced form: `update: \event -> …`.
- **Amended by:** `tags-need-payloads` (2026-10-17): only the section
  `(foo:)` stands without a payload. `tags-are-not-predicates-or-views`
  (2026-10-17): `(foo:) p` is no tag pattern but an error.
- **Rule lives in:** Syntax.md "Tagged Data" and "Dicts".
- **Recorded in:** tag prefix plan.

### A tag needs a payload; its constructor is its section
`tags-need-payloads` · 2026-10-17 · maintainer · accepted
- **Context:** under `tags-are-prefix-operators` a tag with nothing after
  it in its item was its constructor, so a line break decided: `x = foo:`
  was the constructor when the next line began at its indentation, and a
  tag with that line as its payload when the next line was indented.
- **Decision:** a tag always takes a payload. Its constructor is its
  section, `(path:)`, a group holding only the prefix, as `(+ 1)` is an
  operator's. Elsewhere a tag with no payload is an error naming the
  section.
- **Consequences:** `[foo:, bar:]` and `x |> foo:` become `[(foo:), (bar:)]`
  and `x |> (foo:)`. A tag before a stop such as `where` is an error rather
  than its constructor. Joint tags that end with no payload, `a:b:`, may
  mean a tag of a constructor or one path's constructor, so the error names
  both, `a:(b:)` and `(a.b:)` (maintainer, 2026-10-17).
- **Rule lives in:** Syntax.md "Tagged Data".
- **Recorded in:** tag prefix plan, `bare-tag-retirement`.

### A tag is neither a predicate nor a view
`tags-are-not-predicates-or-views` · 2026-10-17 · maintainer · accepted
- **Context:** in a pattern, `F p` applies the predicate `F`, which passes
  or fails, and `(f -> p)` the view `f`, which yields `.r View`. Under
  `tags-are-prefix-operators`, `(foo:) p` was read as the tag pattern
  `foo: p`, a reading no other application has, and the joint `a:Nat n`
  as the predicate `a:Nat` over `n`, almost surely meant as `a: Nat n`.
- **Decision:** a tag builds data, so the front end rejects tagged data or
  a tag's constructor as a predicate or a view: `(foo:) p`, `{(foo:) p}`,
  `a:Nat n`, `(:a n)`, `((foo:) -> p)` and `((foo: f) -> p)`. The message
  names `path: Pattern`.
- **Alternatives rejected:** exempting the keys that make a dict applicable
  (maintainer, 2026-10-17). An `apply:` tag does not change what a view
  means, applying the view as the view alone would, and an `eff:` one works
  only for an effect that strips `eff:` to fit the case, so both are
  rejected too.
- **Consequences:** compilers lack the whole namespace, so they do not
  catch type errors in general, but they may reject those a pattern's
  syntax shows.
- **Rule lives in:** Syntax.md "Dicts" and "Pattern Matching".
- **Recorded in:** tag prefix plan, `tag-predicate-view-rejection`.

### Keyword forms and lambdas stand alone only in tail position
`tail-position` · 2026-10-16 · maintainer · superseded
- **Context:** the grammar review's first item. Forms whose last part
  extends to the right read as atoms wherever they stood:
  `f match x with {…} y` applied the match, `if x <- if a then b else c
  and y then …` ended the inner `else` at the `and`, and `if a then if b
  then c else d else e` gave the second `else` to the outer `if`. The parser
  resolved each, but a reader could not see where such a form ends without
  its rules. A survey found no sample or doc example that relied on them,
  only tests that pinned them.
- **Decision:** prefix `if` and `try`, `match` and `try_match`, `do`,
  lambdas, `let`, `using` and `object` are tail forms, which stand without
  parentheses only in tail position:
  - one whose last part ends on its line may be followed only by the end of
    its statement, definition or group;
  - a braced body's `{` and `}` stand for an indent and a dedent, and after
    a tail form's `}` or dedent only an infix operator or a keyword of an
    enclosing form may follow, never an argument or a path;
  - `where` ends the expression it follows;
  - postfix `if` is an operator, not a tail form.
- **Alternatives rejected:** keyword forms only at the head of a region,
  which loses a match or `do` block as a final argument; and treating `}` as
  an inline end, which loses `if c then do { a } else do { b }`.
- **Consequences:**
  - Parenthesized now: `f (match …) y`, `f (do { a }) y`,
    `(do { … }).x`, `(if c then a else b) -> P`, `f (\a -> E) -> P`,
    `if y <- (if a then b else c) and d then …`, an inline
    `if a then (if b then c else d) else e`, and
    `if any xs (\x -> x > 0) then …`.
  - Unchanged: a match or `do` block as a final argument, `P <- f \a -> E`,
    an operator resuming after a layout block, `a if c else b -> P`, and
    `match match a with {…} with {…}`, which a linter may flag.
  - Every change is a restriction, so the term oracle compares an
    expression the term parser rejects as it reads without the rule.
  - Supersedes `match-is-an-atom`: a match is a tail form. A match nested
    in a subject still takes the first `with` after its own head, and nested
    arms still end their arm.
  - Narrows `bindings-split-before-keyword-forms`: a bind's `->` ends a
    postfix `if`'s fallback or a tail a block ends; another tail form before
    it is parenthesized.
  - The bootstrap enforces the rule for conditionals, `do` blocks, matches
    and lambdas; `let`, `using` and `object` follow when they become frames
    in `parser-keyword-frames-suffixes`.
- **Rule lives in:** `Syntax.md` "Tail Position"; `SyntaxCheatSheet.md`
  "Operators, Pipes, Application".
- **Recorded in:** grammar review, `grammar-tail-position`.
- **Superseded by:** `tail-forms-end-at-separators`.

### A match is an atom, and nests in its subject
`match-is-an-atom` · 2026-10-16 · agent · superseded
- **Context:** making `match` a frame of the term parser's token pass, which
  reads every form as an atom of the item it stands in. View-level dispatch
  let only an operator, `where` or a postfix `if` follow a match that led
  its view, so `match x with {…} y` failed while `f match x with {…} y` and
  `do {…} y` applied the form. The outer match took every ungrouped `with`,
  so a match in a subject never had one. Lines after an arm's nested `when`
  arms, at a shallower indentation or after a braced block, were silently
  dropped.
- **Decision:** a match reads alike wherever it stands: whatever follows
  its arms reads as the expression grammar reads it. A match nested in a
  subject takes the first `with` after its own head, as nested `if`s take
  their `then`s. Nested arms end their arm, and anything after them in it
  is an error. The structural parser changed to agree while it remains the
  oracle.
- **Consequences:**
  - `match x with {…} y` applies the match, and so does a layout match
    followed by a continuation line, as after a layout `do`.
  - `match match a with {…} with {…}` parses.
  - Text after nested arms is an error, no longer lost.
- **Rule lives in:** `Syntax.md` "Match"; `SyntaxCheatSheet.md`, the match
  lines.
- **Recorded in:** parser backtracking plan, "Native keyword frames
  2026-10-16".
- **Superseded by:** `tail-position`.

### Only a lone quoted name is an atom key in a key path
`lone-quoted-name-is-an-atom-key` · 2026-10-06 · maintainer · superseded
- **Context:** the grammar committed to a leading `'name` as the whole key.
  So `['a b]` was a valid list but a syntax error as a key path, an
  artifact of the parser rather than a rule.
- **Decision:** a key-path item that is exactly `'name` is an atom key. Any
  other item is an index expression, read as it would be in a list:
  `['a b]:v` indexes by `'a b`. Applying an atom is an error for a later
  front-end check, not a parse error.
- **Consequences:** key paths and lists read their items alike.
- **Rule lives in:** `SyntaxCheatSheet.md` "Atoms, Tagged Data, Dicts".
- **Recorded in:** parser backtracking plan, "Progress 2026-10-06".
- **Superseded by:** `key-path-items-are-expressions`.

### Key-path items are ordinary expressions
`key-path-items-are-expressions` · 2026-10-07 · maintainer · accepted
- **Context:** the maintainer clarified that `'name` is itself an ordinary
  expression, one that evaluates to an atom, so a key path has no reason to
  read it separately. Supersedes `lone-quoted-name-is-an-atom-key`. The
  declaration and pattern parsers already parsed key items as expressions
  and normalized a constant atom afterwards; only the expression grammar
  had a separate `'name` alternative. That alternative was most likely a
  vestige of the bootstrap phase: paths such as `foo.[42,'name]` were
  parsed before evaluation existed.
- **Decision:**
  - Every key-path item is parsed as an expression, exactly as in a list.
  - A constant atom result becomes a static key through one helper,
    `SyntaxKeyExpr::from_item`; any other result is an index.
  - Applying an atom, as in `'a b`, is an error for a later front-end
    check.
- **Consequences:** all three parsers share the conversion. A grouped atom,
  `[('a)]`, is now a static key in expressions as well; that means the same
  key as an index by that atom.
- **Rule lives in:** `SyntaxCheatSheet.md` "Atoms, Tagged Data, Dicts".
- **Recorded in:** parser backtracking plan, "Progress 2026-10-06".

### An open lambda's body takes a trailing postfix `if`
`lambda-body-takes-postfix-if` · 2026-10-07 · maintainer · accepted
- **Context:** `\a -> b if c else d` read as `(\a -> b) if c else d` at the
  top of a definition, because the structural layer split the postfix off
  first. Inside a list it read as `\a -> (b if c else d)`. Forcing grouping
  was considered and rejected.
- **Decision:** an open lambda binds a maximal trailing expression, so a
  postfix `if` after it belongs to its body, whether on the same line or a
  continuation line: `\a -> b if c else d` is `\a -> (b if c else d)`.
  Write `(\a -> b) if c else d` for a conditional function.
- **Consequences:**
  - The parsed chain records whether it ends in an open lambda
    (`ChainTail`). When it does, the structural layer no longer splits the
    postfix `if` off and leaves the whole view to the expression parser.
  - Both expression parsers accept a postfix `if` on a continuation line,
    as the structural layer already did at the top of a view.
- **Rule lives in:** `SyntaxCheatSheet.md` "Conditionals & Patterns".
- **Recorded in:** parser backtracking plan, "Production switch".

### A dict path member's colon is joint to its path
`dict-member-colon-joint-to-path` · 2026-10-07 · maintainer · accepted
- **Context:** the grammar's dict member accepted spaces and line breaks on
  either side of the colon, because its colon parser never checked
  adjacency. So `{f :tag}` read as `{f: tag}`, and `{f :if}` reached the
  expression `f (:if)` only by backtracking.
- **Decision:**
  - In braces, a path member is `path:Expr` with the colon joint to its
    path.
  - A space or line break may follow the colon, so multi-line literals
    need no grouping: `{a: 1}` and `{key:⏎ value}` are path members.
  - The value runs to the member's end.
  - Any other member is an expression. So `{f :tag}` applies `f` to the
    constructor `:tag`, and `{a : 1}` is an error.
  - Outside braces, a tag stays joint on both sides.
- **Consequences:** both expression parsers commit once they see a path
  followed by a joint colon, with no trial. A strictly joint rule, which
  would have required grouping multi-line values, was considered and
  rejected for multi-line aesthetics.
- **Rule lives in:** `SyntaxCheatSheet.md` "Atoms, Tagged Data, Dicts".
- **Recorded in:** parser backtracking plan, "Production switch".

### Group errors surface only when the group is used
`parse-errors-surface-on-use` · 2026-10-07 · maintainer · accepted
- **Context:** the prefix-shared term parser interprets each delimiter group
  once, before knowing its role. A keyword form's own groups, such as a
  `do` or `match` body, are not expression groups.
- **Decision:** a group whose contents fail as an expression keeps its
  error in its cover. The error surfaces only if a role consumes the group,
  much as macros respect group boundaries so only observed errors matter.
- **Consequences:** a keyword form's groups raise nothing when the form is
  delegated. Eager parsing remains in the bootstrap's structural parsers.
- **Recorded in:** parser backtracking plan, "Production switch".

### `map` and `list.concat` are structural and non-forcing
`structural-lazy-map-and-concat` · 2026-09-17 · maintainer · accepted
- **Context:** recursive list operators forced whole spines on the Rust
  stack.
- **Decision:**
  - Each step unfolds one representation node and forces nothing.
  - `map f (A ++ B)` yields deferred halves, and `map` over a strict leaf
    yields lazy item applications.
  - `list.concat` defers both halves of a source concatenation. Over a
    strict outer leaf it joins the segments pairwise to O(log n) depth, and
    an invalid item becomes a deferred failing hole.
- **Consequences:** laziness is user-visible: a failure appears only when its
  item is demanded.
- **Rule lives in:** `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** resumable-WHNF plan W6D.4a and W6D.4b.

### List pops reshape the remainder toward the popped end
`list-pop-reshapes-remainder` · 2026-10-08 · maintainer · accepted
- **Context:** popping the front of a strict left-deep spine rebuilt the
  remaining spine in the same shape, so every pop cost O(n). The core list
  operator built literals as such spines, which made a loop of `len`,
  `head` and `tail` cubic.
- **Decision:**
  - Lists built lazily by concatenation are the normal case, so the list
    observers shape lists on demand rather than relying on construction to
    produce good shapes (maintainer direction).
  - A front pop returns its tail as a right-leaning spine, and a back pop
    its init as a left-leaning one. Neither forces a lazy chunk.
  - A walk from one end takes each `Concat` apart once: O(depth) for the
    first pop, then O(1) amortized.
  - Core list literals are one value leaf.
- **Consequences:**
  - Pops that alternate between the two ends of one list reshape it each
    time, O(n) per pop.
  - Popping the same unshaped list again pays the first pop's cost again,
    since the reshaped remainder is not remembered in the original.
  - Index operations still walk; see `list-observers-walk-leaves`.
- **Amended by:** `list-literals-compose` (2026-10-10): a literal of closed
  data, or of at most eight items, is still one value leaf; a longer
  literal with computed items is a concatenation of such leaves.
- **Recorded in:** structural overheads plan, `perf-list-front-walk`.

### List literals compose bounded lists with `++`
`list-literals-compose` · 2026-10-10 · maintainer · accepted
- **Context:** the core list operator collected any number of items, one
  partial application per item. Each application copied the items so
  far, and each step walked the rest of the application chain from the
  net's interface, so a literal cost time quadratic in its length: 35 G
  instructions for `list_map_8000`'s two literals. The variable arity
  came from the interaction-net spike, which lowered every aggregate to a
  chain of its one unary host agent; it was never a design choice.
- **Decision:**
  - No variable-arity list primitive. Lists are built from singletons
    and concatenation, and an annotation changes a list's representation
    (maintainer). The list operator is an accelerator for lists of at
    most eight items (`LIST_OPERATOR_MAX_ARITY`).
  - Literals are a grammar-layer concept (maintainer). A literal whose
    items are all closed data resolves to its list value. Any other
    literal lowers to parts, runs of eight or more closed items as list
    values and the other items in lists of at most eight, joined with
    `++` in the shape of a finger tree: `first ++ (middle ++ last)`, the
    middle built the same way from pairs (maintainer's suggestion).
  - A strict concatenation, if wanted, is an accelerator of `++` and
    `seq`, not a core primitive (maintainer).
- **Consequences:**
  - Literals build in time linear in their length; each step's walk is
    bounded by the tree's logarithmic depth plus one part.
  - A long computed literal evaluates to a concatenation, with one lazy
    `++` result per join, forced as observers reach it. A program that
    needs a flat or indexable list asks for it with an annotation.
  - The eight-item bound is a heuristic, not a measured one.
- **Rule lives in:** `ResolvedNetLowerer::list_into` in
  `src/g_syntax/net_lowering.rs`; `resolved_list` in
  `src/g_syntax/resolve/expression.rs`.
- **Recorded in:** structural overheads plan, `perf-list-map-growth` and
  `perf-list-literal-composition`.

### Dict literals fold when closed and otherwise join as a finger tree
`dict-literals-compose` · 2026-10-10 · maintainer · accepted
- **Context:** a dict literal resolved to a left-nested chain of binary
  unions, one per member, as deep as the literal was long. Lowering
  walked it quadratically, and at 5,000 computed entries lowering and
  evaluation overflowed the Rust stack.
- **Decision:** dict literals are grammar-level, like list literals
  (`list-literals-compose`; maintainer):
  - A literal whose members are single-key entries of closed data, with
    atom, number or text keys and no key repeated, resolves to its
    dictionary. An entry whose value is undefined adds nothing, as at run
    time.
  - Any other literal joins its members with union in the same
    finger-tree shape (`finger_join`). Union is associative and forces
    its operands left to right, so the shape keeps the result and the
    order in which members are demanded.
- **Consequences:**
  - Nesting is logarithmic in the number of members.
  - A balanced union re-inserts each entry about log n times where a
    chain inserted it once: 6% slower at 2,000 computed entries.
  - Repeated keys, list and dict keys, and computed keys still build at
    run time, where merging and key conversion live.
  - Readiness, which never evaluates, can now read a closed literal
    payload's diagnostic text.
- **Rule lives in:** `lower_dict_union_resolved` and `closed_dict` in
  `src/g_syntax/resolve/expression.rs`.
- **Recorded in:** structural overheads plan, `perf-lowering-free-bindings`.

### List observers walk leaves; what they take is a flat slice or a rope
`list-observers-walk-leaves` · 2026-10-09 · maintainer · accepted
- **Context:** `len`, `at`, `split`, `slice` and `split_end` advanced one
  item per builtin step, and `split` and `slice` copied what they took into
  a new value leaf. A loop that tested `len` stayed quadratic after
  `list-pop-reshapes-remainder`. Caching lengths in `Concat` nodes was
  proposed and rejected.
- **Decision:**
  - These observers take one strict leaf (a byte slice, value slice or
    finger tree) per step of the list projection, and every strict leaf in
    one builtin step. They stop only to force a deferred chunk. `head` and
    `tail` still take one item.
  - What `split`, `split_end` and `slice` take comes back as a flat slice
    when it lies within one leaf, and otherwise as a finger-tree rope that
    shares the leaves it spans. Runs of pieces under 32 items are copied
    into one chunk. The remainder is shaped as a pop leaves it.
  - List nodes hold no cached lengths: value representation will make them
    much smaller, with pointer-tagged singleton lists, concatenation pairs
    and other tagged shapes (maintainer).
  - A program that indexes one list repeatedly asks for an indexable form
    with the `array` or `deque` annotation; `at` itself does not rebalance
    (maintainer).
- **Consequences:**
  - An observer's builtin steps grow with the lazy chunks it crosses, and
    its work with the strict leaves it passes, not with items.
  - `len` and `at` on a list of many small leaves still pass each leaf
    every call.
  - A small slice taken from one large leaf keeps that leaf's storage alive,
    as a `tail` already did.
  - The 32-item run size is a heuristic, not a measured one.
- **Rule lives in:** `src/eval/list_observation_machine.rs` module docs.
- **Recorded in:** structural overheads plan, `perf-list-leaf-walk`.

### Namespaces journal their definitions in `meta.journal`
`definition-journal` · 2026-10-12 · maintainer · accepted
- **Context:** each module definition carried a run-time assertion that
  the definitions before it did (or did not) define its name. It evaluated
  only when the definition was demanded, so `x = 1` then `x = 2` passed
  unless `x` was, and each assertion kept an intermediate dictionary alive:
  about half of `chain`'s cost. Compilers cannot see other files' names,
  since top-level `import "file.g"` mixins are include-like.
- **Decision:**
  - A file's own conflicts fail the module at compile time.
  - Each namespace records what is introduced, overridden or updated in it
    in `meta.journal`, as entries `{op:intro:['foo], at:g:{origin, line}}`
    (`over` for `:=`, `update` for `::=`). Each stretch of declarations
    between top-level imports appends its bundle lazily, `(default []
    prior) ++ bundle`, so a journal is a plain list. Paths are lists; `at`
    is a context frame, so other front ends can use their own addressing.
  - `f.foo = 1` records `intro:['foo]` in `f`'s journal and `update:['f]`
    in the host's; `refl` and `meta` keep no journals, so paths through
    them are recorded whole. A dict is a module insofar as it is used as
    one. Compilers reject `spec` at module scope.
  - A namespace's reflection boundary replays its final journal and logs
    as errors introductions of a path already defined, overrides of one
    not defined, and declared paths never defined. The check runs only
    when a definition in the namespace is forced, compatible with lazy
    loading.
  - Names from outside a journal reach it at their source: the assembler
    journals what it provides and expects (`intro` of `asm.args` and
    `env`, `declare` of `asm.result`, `at:asm:()`), `import ... as`
    journals `env`, and users write `assume Name`. `declare Name` (renamed
    from `abstract Name`, as abstract object methods are) concerns only the
    final state. A dict defined by value and then used as a module is its
    author's responsibility.
  - Object bodies journal their members, `declare` and `assume` in the
    object's own journal, with whole paths; a declared object's boundary
    replays it in place of member assertions. Object expressions, which
    have no boundary, keep their assertions. A nested definition
    carries its namespace's boundary as well as its file's; a definition in
    a declared object carries only the object's. A target within `refl`,
    `meta` or `spec` carries none.
- **Consequences:**
  - `meta.*` conventions are not formal semantics; a macro can write the
    journal too.
  - A name the journal records stays defined: `x := {}` no longer frees
    `x` for reintroduction.
  - A cross-file conflict is a diagnostic, not a failed value, and a
    module's `=`, `:=` and `extend` no longer carry run-time assertions;
    computed targets and object members still do.
  - Recording costs 1 to 2% of instructions on `chain` and `defs_unused`,
    mostly tracing the live journal at each collection; a nested boundary
    costs about 5 M instructions the first time.
  - `default fallback value` became a builtin, also in `std`, replacing the
    closed helper `defined_or`, which built the conditional from pattern
    builtins.
- **Rule lives in:** `src/g_syntax/module_lowering/journal.rs` module docs.
- **Recorded in:** structural overheads plan, `perf-definition-assertions`.

### A namespace's reflection task runs its `meta.refl`
`reflection-operation-in-meta-refl` · 2026-10-12 · maintainer · accepted
- **Context:** a module's reflection boundary launched a fixed `refl.*`
  scanner, so users could not replace, extend or disable it.
- **Decision:**
  - The language declaration installs the module's reflection operation,
    the scanner, as `meta.refl` unless the module has one; a language's
    version or extensions may later choose otherwise.
  - Each file's compiler boundary, forced once per file by its first used
    definition, claims the namespace's key and spawns a task running
    `default scanner final.meta.refl`. The task, not the forcing thread,
    reads `final`: `seq final.meta.refl Definition` would force the final
    namespace's structure and cycle on computed keys.
  - Modules and nested namespaces run `meta.refl`; declared objects run
    their scanner, as they inherit parents' `meta.refl`.
  - A namespace without `meta.refl` runs `.r ()`, so only what it
    declares decides, never the first file to spawn its task.
  - Reflection state lives under `[name, 'meta, 'refl]:()`: a module's or
    nested namespace's abstract path, or a declared object's final
    `spec.name`, shared by a module and an object of one name. The
    scanner takes ownership of it before scanning, so it runs once even
    when spawned twice.
- **Consequences:**
  - `meta.refl := .r ()` disables a module's `refl.*` tasks.
  - The ownership check costs about 5 M instructions per scanner, since
    each reflection effect step costs about a million.
- **Recorded in:** structural overheads plan, `defs-meta-refl`.

### Local analysis is one walk; file `let`s are read through the resolver
`scope-analysis-is-one-walk` · 2026-10-17 · agent · accepted
- **Context:** each binder walked its whole scope to mark its own locals,
  each `let` group walked its values for its dependency edges, and every
  declaration seeded all the file's `let` names as locals, checking each
  against the rest. Nested `where` grew as n^1.61, and 800 file `let`s
  before 800 definitions took 11.3 G instructions, cubic.
- **Decision:**
  - Unused-local analysis is one walk with a scope stack: each name
    resolves to its innermost binder through an index of the binders in
    scope, and a `let` group's dependency edges are recorded as the walk
    meets them. A binder that warned before its scope's contents reserves
    its place, so warnings keep their order.
  - The resolver computes a group's edges with those of every group in its
    values in one walk, kept by the groups' source addresses until the
    walking group's values are resolved. Macros expand as a declaration
    parses, before it lowers, so the source is fixed meanwhile.
  - File `let` names lie beneath the locals, read on a lookup that misses
    them, with the first binding identities; a scope sees them at module
    level and in bodies transparent to it (`NameScope::sees_file_lets`).
- **Alternatives rejected:** storing dependency edges in the syntax tree,
  which every constructor and test of `let` would carry; and copying a
  template of the file's locals per declaration, cheaper but still
  O(lets) per declaration.
- **Consequences:**
  - Where a name is bound twice, analysis reads the innermost binder, as
    the resolver does; before, a body's `let` did not hide an outer local
    from the outer binder's walk. Rebinding a local is an error in the
    resolver anyway.
  - `chain_where` grows as n^1.06 and file `let`s as n^1.17.
    `rustc-hash` hashes the scope names (maintainer, 2026-10-17), which
    keeps `hello_elf` within 0.1% of its cost before.
- **Recorded in:** parser backtracking plan, "Scope analysis 2026-10-17".

## Diagnostics

### Senders never evaluate diagnostics; renderers do
`senders-never-evaluate-diagnostics` · 2026-10-15 · maintainer · accepted
- **Context:** the runtime built a failure's diagnostic eagerly, through an
  isolated session and nested drivers, wherever it published one: a failed
  task's `.task.status`, `.eval` results, public `Error`, macro causes and
  readiness reports. Public enrichment (`Diagnostic::enrich`,
  `with_context`, `apply_updates`) evaluated too. Settlement had to build a
  status diagnostic before taking the runtime gate
  (`status-diagnostic-before-gate`), and a quantum boundary had to lend its
  mutator to the drivers that built it (`boundary-release-under-mutator`).
- **Decision:** a diagnostic is evaluated only to render it, usually on the
  logger's thread. Senders build and enrich diagnostics as unevaluated
  compositions, closer to a lowering than an evaluation, without observing
  the emission: an object normalization of the emission with mixins
  applied. Context frames prepend through the mixin
  `msg.context ::= \ prior -> frames ++ prior`; a compiler-private builtin
  may serve as a convenience or accelerator but is not required
  semantically. A message that fails to evaluate is an error in the error,
  reported to whoever renders it under a `diagnostic:{from:Frames}` frame
  that carries the composed frames, where the diagnostic came from, with no
  fallback text (maintainer). Rust summaries are not read from diagnostic
  values: a diagnostic built from Rust text keeps it, and one built from a
  value summarizes statically (maintainer). Reflection semantics may change
  with it (maintainer): `.eval`'s `err:Error` and `.task.status`'s `err`
  are compositions. The ordering of `status-diagnostic-before-gate` was
  vestigial and goes.
- **Consequences:** settlement, the quantum boundary and the public API stop
  evaluating diagnostics, so nothing evaluates to publish a failure.
  Diagnostics nobody renders cost nothing. A Rust reader that inspects a
  diagnostic's fields evaluates it first, or sees only what is immediately
  built.
- **Rule lives in:** `agent_context/diagnostics.md` "Publication and
  Counts".
- **Recorded in:** lazy diagnostics plan.

### A failure's cause is a nested `msg` frame; the headline states only its own finding
`failure-cause-as-nested-msg-frame` · 2026-10-05 · maintainer and agent · accepted
- **Context:** macro, `conf.cli` and killed-work failures were flattened to
  text, and one `.ok()` silently swallowed a configured error.
- **Decision:**
  - Carry the original diagnostic as a cause: a nested message in the
    context list.
  - The headline states only the compiler's or host's own finding and never
    repeats the cause (maintainer). Without a structured cause, the headline
    keeps the reason text.
  - A failing `conf.completion_script.NAME` fails the command with a
    `{conf:{entry:"completion_script"}}` frame.
- **Consequences:** genuinely new validation errors, such as macro arity,
  stay as text. Removing the nested cause instead of the headline copy was
  rejected: the nested message is the structure.
- **Rule lives in:** `agent_context/diagnostics.md` "Context Frames".
- **Recorded in:** holistic review A3; commits `4cceabbc`, `3f3ed946`,
  `f3b22303`.

### A killed task's status carries its kill diagnostic
`killed-task-status-carries-diagnostic` · 2026-10-05 · maintainer · accepted
- **Context:** a killed task's `.task.status` was the bare atom `'killed`,
  although `.task.join` already failed with the client's kill reason.
- **Decision:** `.task.status` reports `killed:Diagnostic`, shaped like
  `err:Diagnostic`, and `.task.error` returns it. The kill's emission is
  stored as built, not normalized: normalizing evaluates, and nothing can
  evaluate while a deadlocked runtime settles.
- **Consequences:** status, `.task.error` and `.task.join` report the same
  kill reason.
- **Rule lives in:** `agent_context/reflection.md` "Child Tasks and
  Evaluation"; `architecture/reflection.md` "Reusable Reflection Requests".
- **Recorded in:** holistic review A3; commit `f3b22303`.

### Unrecognized annotations go through a runtime ledger; the library never writes stderr
`annotation-warnings-via-runtime-ledger` · 2026-10-05 · maintainer · accepted
- **Context:** the evaluator wrote unknown-annotation warnings to stderr under
  managed access, on every evaluation, bypassing the diagnostic bus.
- **Decision:** evaluation records each distinct unrecognized annotation in a
  deduplicated leaf-lock ledger, with no I/O or callback. The assembler
  drains it and publishes one `Warning` per annotation per runtime. The
  review's alternative, a warning outcome crossing the region boundary, was
  not chosen.
- **Consequences:** the library no longer writes stderr. `'deprecated` and
  `'TBD` will share the route.
- **Rule lives in:** `agent_context/diagnostics.md` "Logger and Rendering
  Boundaries" (the rule, not the mechanism).
- **Recorded in:** holistic review E7; commit `d701396d`.

### Rust `Display` of evaluation failures is an edge-free classification
`failure-display-is-edge-free` · 2026-09-30 · agent · accepted
- **Context:** `Display` cannot carry a mutator, so it cannot observe managed
  values.
- **Decision:** `EvaluationFailure`'s `Display` prints a classification, with
  no cached eager summary.
- **Consequences:** full rendering goes through the configured logger.
- **Rule lives in:** `architecture/diagnostics.md` "Structured Failures".
- **Recorded in:** aggressive-GC remediation plan GCI11R-002D.2h; D2h review
  D2HR-003.

### One public frame for interaction-net construction
`single-net-construction-frame` · 2026-09-21 · agent · accepted
- **Context:** builder operands added redundant context frames.
- **Decision:** one `eval:{op:'net_construction}` frame covers the whole
  public pipeline. Private operand demands (copy counts, wire ports,
  reset/shift keys, paths, state) add none. The legacy `copy_count` frame is
  dropped.
- **Consequences:** immediate validation strings are not compatibility
  promises.
- **Rule lives in:** `agent_context/diagnostics.md` "Context Frames";
  `Syntax.md` "Errors" (partly).
- **Recorded in:** PNC4 review PNC4R-001.

### Lazies and promises carry no labels
`lazies-carry-no-labels` · 2026-10-12 · maintainer · accepted
- **Context:** every lazy carried a label, a name for what it computes,
  copied into each root made for it, and every promise constructor took one
  (discarded since 2026-09-09). Only a lazy dependency cycle's message read
  it, and there it named runtime machinery (`net computation`, `builtin
  call`, `application`), not source. Glam locates a failure with the
  context annotations it passes through, such as each module definition's
  origin and line.
- **Decision:** remove lazy labels and the promise label parameter. A cycle
  reports its members' lazy ids. The public `promise` constructors keep the
  host's label, which only their resolver's messages use.
- **Consequences:**
  - The lazy cell shrinks from 152 to 128 bytes and its root from 40 to 16;
    instructions fall 0.5 to 1.8% (`countdown_800` −1.5%, `list_sum_400`
    −1.8%, `hello_elf` −1.0%), `append_walk_800` +0.15%.
  - A runtime developer who needs a lazy's construction site could record
    a `#[track_caller]` location in debug or profiling builds instead.
- **Rule lives in:** `architecture/values.md` "Recursive Identities".
- **Recorded in:** structural overheads plan, `perf-lazy-force`.

## Interaction nets

### Application nets start with their function copied in
`applications-copy-function-in` · 2026-10-10 · agent · accepted
- **Context:** applying a function value, whether a closure's code to its
  captures, a stage to more arguments, or a saturated call, built an
  application spine around `Data(Net(f))` through the template builder,
  then claimed the `Bind >< Data` call to copy `f` in. Function values stay
  nets so that partial application shares work (maintainer).
- **Decision:** `RuntimeNet::applied` builds the spine directly in its
  fixed shape and installs a copy of `f`, prepared in the same access, at
  its head. For a net callable the claim only classified the value and
  copied it, so the copy is the same, made at construction; a cursor copy
  still shares its source's evaluation, and a whole copy is made only of a
  source with none left.
- **Consequences:** no application claims a call; each commits as its
  spine joining the function's. A closure's code is copied when its last
  capture arrives; closures are built lazily, so in practice that follows
  demand for the closure, except under `par` or `seq` (maintainer).
  `countdown_800` falls 15.1%, `minimal` 8.1%.
- **Rule lives in:** `RuntimeNet::applied` (`interaction_net/runtime/cursor.rs`).
- **Recorded in:** structural overheads plan, `perf-runtime-net-attach`.

### A function that only wraps an operator is called as that operator
`functions-wrap-operators` · 2026-10-11 · agent · superseded
- **Context:** after lazy locals, the commonest application was a saturated
  call of a closed `\x -> x.path`, mostly the effect handlers
  `{eff: \api -> api.seq}` that every `if` and `do` step runs: an applied
  net, a copy and a net driver to yield one access. The maintainer asked
  for thin wrappers around operators to skip the net, since applying them
  starts no work worth sharing.
- **Decision:**
  - Lowering marks a closed function whose body applies one operator to
    its parameters, in order, and does nothing else with them, with that
    operator (`WrappedOperator`): today an access of the first parameter,
    indexed by any others.
  - A saturated call of a marked function yields the operator's result
    directly; a net calling one installs the operator where the call
    stood. Partial application and every other use go through the stage
    net, which stays the function's representation.
  - The mark holds no values, so the stage remains the function's only
    managed edge. A saturated call's lazy source keeps only the stage, as
    that is all it uses, so lazy cells keep their size.
- **Consequences:** instructions fall a further 3.4 to 12.4% on the
  profile families (`append_walk_800` 12.4%, `countdown_800` 8.2%,
  `hello_elf` 3.6%). Operators that take values or code, such as closure
  captures, are not marked.
- **Rule lives in:** `wrapped_operator` (`g_syntax/net_lowering.rs`);
  `apply_whnf_function` (`eval/whnf.rs`) and `classify_core_callable_in`
  (`eval/net.rs`).
- **Recorded in:** structural overheads plan,
  `perf-thin-wrapper-applications`.
- **Superseded by:** `thin-wrappers-compile`.

### Functions that wrap an operator compile like any other
`thin-wrappers-compile` · 2026-10-12 · maintainer · accepted
- **Context:** `functions-wrap-operators` gave a closed `\x -> x.path` a
  mark that calls apply directly. Compiled nets came after it and cover the
  same case: such a function's net is normal and compiles to one access
  step. The mark was a second acceleration for one shape, kept beside the
  compiled store. The maintainer asked to remove it unless that cost too
  much.
- **Decision:** lowering builds these functions as it builds every closed
  lambda, marked jit. A saturated call runs the compiled step, and a net
  calls the function through the compiled path. Supersedes
  `functions-wrap-operators`.
- **Consequences:** instructions rise 0.06 to 0.5% (`minimal` +0.5%,
  `do_chain_800` +0.35%, `hello_elf` +0.12%). `FunctionValue` loses its
  operator field.
- **Rule lives in:** `function_into` (`g_syntax/net_lowering.rs`);
  `compiled_for_application` (`eval/net.rs`).
- **Recorded in:** structural overheads plan,
  `perf-thin-wrapper-applications`.

### Nets with no evaluation left compile to operator steps
`compiled-nets` · 2026-10-11 · maintainer · accepted
- **Context:** after thin-wrapper calls, most applications left were
  chains such as `\c -> f (thunk c)` and `\c -> g c 1`, whose code net
  holds `f` or `g` only once it has reduced. The maintainer prefers
  modeling direct execution as an acceleration attached to a net, which
  annotations request (`anno 'normalize`, `anno 'compile`), over programs
  lowering emits; lowering may fold the annotation for code it generates.
- **Decision:**
  - A net with no evaluation left compiles, read as a function of its
    arguments, when each interaction left once they arrive only builds a
    value: to steps, each an operator applied to arguments, constants or
    earlier steps until it yields data. Anything else stays a net.
  - The managed net cell holds the outcome once, set under the net mutex
    through an exact-delta edge transition, and traces it. A function
    value and its code share the cell; the net cannot change after it.
  - Saturated calls, forced function-call lazies and completed thunks run
    a compiled net instead of building an applied net. Compiling only a
    normal net keeps sharing: a closure's stage compiles with what its
    captures started as constants.
  - A completed thunk builds its value at once from compiled code unless
    the code builds closures, which would build nets a thunk never forced
    does not need; otherwise it waits for a force. Building forces
    nothing, as operators never force their operands: the value is a lazy
    application, builtin call or access, or a container of operands, and
    is evaluated only when the thunk would have been. A thunk already
    built its applied net when completed, unreduced; this replaces that
    work. Thunks completed inside compiled steps always wait, so compiled
    runs never nest on the Rust stack.
- **Consequences:**
  - Instructions fall 26.3% (`countdown_800`), 38.2% (`list_map_2000`),
    36.0% (`list_sum_400`), 30.2% (`append_walk_800`), 32.8% (`sum_800`)
    and 23.7% (`hello_elf`), and up to 2.1% elsewhere, with no
    regressions.
  - Code applied once, such as `do_chain`'s per-step thunks, gains
    nothing until normalization runs before a first application.
  - The managed net cell grows from 200 to 216 bytes.
- **Rule lives in:** `RuntimeNet::compile`
  (`interaction_net/runtime/compiled.rs`); `ManagedCoreNetAccess::compiled`
  (`core/managed/recursive_cells.rs`); `apply_compiled` and
  `eagerly_completed_thunk` (`eval/operator.rs`).
- **Recorded in:** structural overheads plan, `perf-compiled-nets`.

### Nets normalize before their first application
`nets-normalize-before-use` · 2026-10-11 · maintainer · superseded
- **Context:** compiled nets helped only code applied repeatedly: a net
  compiled once the first application's cursor copy had reduced it, so
  code applied once, such as `do_chain`'s per-step thunks, gained nothing.
  The maintainer noted that annotations could always force normalization
  of nets before they are applied, and asked for it, with no implicit
  `seq`: forcing earlier only where the value would be forced anyway.
- **Decision:**
  - The first application to find a net with evaluation left normalizes
    it in place, once, under fuel (four reductions per node, nets of at
    most 256 nodes), then compiles it.
  - Normalization reduces only what builds: rewrites, and operators
    meeting data, which construct values and force nothing. A call of
    data waits for demand, since classifying its callable may force a
    lazy, as do cursors, checkpoints and pairs another evaluation holds.
  - Every net normalizes, not only lowered code: normalizing closure and
    partial-application stages too doubles the gain on `hello_elf` (20.6%
    against 13.1% for code nets alone).
  - A completed thunk whose code still awaits normalization defers to its
    force, which normalizes, compiles and runs it; without that, the gain
    on `do_chain_800` falls from 29.0% to 11.9%.
  - Normalization follows the copies a net is making (maintainer: a copy
    is logically local and only shares its source's work, so normalization
    should reach through it). It normalizes each copy's source in place,
    on a stack of nets in progress, then makes whole each copy that has
    not advanced, as if the copy began once its source was reduced, and
    goes on reducing what that joins. Before, every net normalization
    stopped at was a closure stage's untouched cursor into code never
    normalized: 368 such nets in `hello_elf`.
  - A value behind data, such as a closure or a partial application, is
    not part of the net that holds it, and building one normalizes
    nothing: a net normalizes only when asked, by an application or the
    annotation (maintainer). Its first application follows its copy
    instead. A first version normalized the code when a closure or
    partial application was built, treating the copy as an application;
    the maintainer rejected it.
- **Consequences:**
  - Instructions fall 29.0% (`do_chain_800`), 20.6% (`hello_elf`), 15.5%
    (`minimal`), 11.7% (`hello_do`), 9.6% (`list_sum_400`) and 6.5%
    (`append_walk_800`), within 1.5% elsewhere.
  - `list_map_2000` rises 3.0%. Mapping alone falls 7.3% and comparing
    constant lists 4.7%; comparing mapped elements hits list-effect lazies
    at a WHNF boundary about once per six elements more often. The cause
    is not yet known.
  - `anno 'normalize` asks for the same normalization ahead of any
    application, without fuel: each reduction is charged to the
    evaluation budget, which yields between polls. Wrapping every helper
    function the compiler builds in it did not help `list_map_2000`
    (+1.7% more) and cost `minimal` 15%, normalizing helpers that are
    never applied, so the compiler does not.
  - Normalizing the code a copy reads: `hello_elf` falls a further 8.0%,
    `minimal` 6.1%, `hello_do` 5.4%, others within 0.8%, measured for the
    first version; following copies at the first application instead
    stays within 0.25% of it on every family.
  - The managed net cell grows to 224 bytes.
- **Rule lives in:** `compiled_net` and `normalize_in` (`eval/net.rs`);
  `RuntimeNet::next_constructive_pair`
  (`interaction_net/runtime/compiled.rs`); `RuntimeNet::make_copy_whole`
  (`interaction_net/runtime/whole_copy.rs`).
- **Recorded in:** structural overheads plan, `perf-compiled-nets`.
- **Superseded by:** `nets-compile-on-request`.

### Nets normalize and compile when an annotation asks
`nets-compile-on-request` · 2026-10-11 · maintainer · accepted
- **Context:** supersedes `nets-normalize-before-use`, which normalized
  every net at its first application, a runtime policy chosen because it
  measured best. The maintainer rejected it: applying a function evaluates
  only what the application demands, following auxiliary ports through
  principal ports to the next pair to reduce, so parts that do not
  contribute may stay unreduced. A net is normalized only when an
  annotation asks, written by hand or injected by a front end. Lowering may
  express only what a front end could, though not necessarily the same
  way, and annotations are specific to an assembler version, so their
  meaning can be tuned.
- **Decision:**
  - `anno 'compile F`, when demanded, normalizes F's net in place as far
    as building goes (rewrites, operators meeting data; no call of data,
    cursor or checkpoint), under fuel of four reductions per node on nets
    of at most 256 nodes, so it cannot diverge, then compiles a function's
    net. It follows the copies the net is making, as before.
  - `anno 'jit F` marks F's net. Its first application, partial or
    saturated, does what `'compile` does, so a function demanded but
    never applied costs nothing more. A closure of marked code and a
    partial application of a marked function are marked too, so each is
    specialized to what it holds at its own first application.
  - Lowering marks each function it builds, as a front end could write
    `anno 'jit`: a closed lambda's net and a thunk's code directly, and a
    lambda's code, which each closure built from it inherits. (A lambda
    that only wrapped an operator was applied as that operator and not
    marked, until `thin-wrappers-compile`.)
  - A net already normal still compiles when applied, at most 64 nodes in
    a supported shape: that only changes representation.
  - A thunk is lowered as its code, a marked function of its captures,
    applied to them, as a front end would write it. A saturated
    application of an evaluated function whose compiled code builds no
    closures builds its value at once, as a completed thunk did
    (`compiled-nets`); inside compiled steps it becomes a call of the
    compiled code, forced later.
  - `anno 'normalize F` keeps `'compile`'s reductions, charged to the
    budget without fuel, until it gets full normal-form semantics.
- **Consequences:**
  - Instructions against `nets-normalize-before-use`: `hello_elf` +1.35%,
    `countdown` +0.5%, `list_map` +0.7%, `append_walk` +1.9%, `minimal`
    +2.6%, `list_sum` +3.8%, `do_chain` +4.2%.
  - Lowering first wrapped each function in a lazy `anno 'compile`:
    `hello_elf` +7.2%, `minimal` +8.8%, `do_chain` +5.9%; without any
    annotation, `hello_elf` takes 45% more. A compiler-private builtin for
    the wrapper, as `seq` is for `anno seq:`, gained only 0.6%: building
    and forcing the wrapper lazy, once per function, was most of the cost.
    Marks took `hello_elf` to +1.9%, and partial applications inheriting
    them to +1.35%.
  - A map item's lazy holding a plain function, rather than a forced
    wrapper, is forced inline under list-effect checkpoints about once
    per element more: `list_map` +4.8% against the wrappers, the effect
    `nets-normalize-before-use` recorded.
  - A compiled partial application with its arguments as constants was
    rejected: work in the function that depends only on those arguments,
    shared through the partial application's net, would run at every
    call.
  - The managed net cell holds a jit state byte in place of the old
    normalization flag, at 224 bytes.
- **Rule lives in:** `compiled_for_application`, `request_jit_in`,
  `compile_value_in` and `normalize_in` (`eval/net.rs`); `jit`
  (`g_syntax/net_lowering.rs`); `apply_whnf_function` (`eval/whnf.rs`);
  `eagerly_completed_application` and `apply_operator`
  (`eval/operator.rs`).
- **Recorded in:** structural overheads plan, `perf-compile-annotations`.

### Interface demand walks resume from the surviving part of their route
`interface-walks-resume` · 2026-10-10 · maintainer · accepted
- **Context:** a demand poll walked from the interface to the active pair
  every time, so a chain rewritten at its far end, polled after each
  rewrite, cost time quadratic in its length. The maintainer asked for
  the approach used for lazy routes: know what to review after an
  interruption, detect disruption, and rebuild or backtrack, noting that
  node names are never reused.
- **Decision:**
  - Each interface keeps the end of the route its last walk took. The
    next walk drops missing nodes from the tip and resumes at the last
    surviving one, or at the interface when none survives.
  - The invariant it rests on: a rewrite or runtime operation consumes
    nodes, adds fresh ones, and rewires only ports that faced consumed
    nodes, so a wire between two surviving nodes never changes. A route
    node faces the next one's auxiliary port with its principal, so it
    joins an active pair only once that next node is consumed; consumed
    route nodes are therefore a suffix of the route, and a surviving node
    implies that every node before it survives.
  - Debug builds check every resumed walk against a full walk, so an
    operation that broke the invariant would fail the test suite.
  - A fresh walk records its route only after 16 nodes; cycles are
    noticed with Brent's algorithm (`WalkCycle`).
- **Consequences:**
  - A runtime operation that rewired two surviving nodes would need to
    clear the remembered routes.
  - Each managed net cell carries one more optional pointer.
- **Amended by:** `interface-routes-belong-to-evaluation` (2026-10-10): the
  demanding evaluation keeps the route, not the net.
- **Rule lives in:** `RuntimeNet::walk_interface_route`.
- **Recorded in:** structural overheads plan, `perf-interface-demand-walk`.

### The evaluation demanding an interface keeps its route
`interface-routes-belong-to-evaluation` · 2026-10-10 · maintainer · accepted
- **Context:** `interface-walks-resume` kept each interface's route in the
  runtime net, keyed by interface. The route is the continuation stack of
  a weak-head reduction: each node waits through its principal port on the
  next; a rewrite at the tip pushes the nodes it leaves waiting, and a
  result reaching the tip pops it. Each net has one interface, and every
  resumed poll measured came straight after one rewrite.
- **Decision:**
  - The evaluation demanding an interface owns its route: the net driver
    keeps an `InterfaceRoute` and lends it to each root poll. Shared nets
    keep only their topology (maintainer: this state belongs to the
    evaluation).
  - The route stays a full stack. Nodes visited on `list_computed` at
    500, 2,000 and 8,000 items: 27,920, 142,030 and 739,510 walking from
    the interface each time; 14,499, 67,650 and 341,357 remembering only
    the node before the pair; 8,386, 16,438 and 46,120 with the stack.
    One node is lost whenever a result returns to it, so its walks grow
    with depth (5 to 16 nodes per walk); the stack's stay at 2 to 3.
  - Logarithmic checkpoints, O(log d) memory and O(d log d) visits to
    unwind d levels, were rejected: every node on the stack is a live node
    in the net, so the stack is a small fraction of memory already spent.
  - A fresh walk still records nothing for its first 16 nodes. Without
    that, every driver allocates a stack: common workloads cost 0.3 to
    0.5% more instructions, and `list_computed_2000` gains only 0.3%.
- **Consequences:**
  - Net cells no longer carry routes, and removing a node no longer
    touches them.
  - A driver that starts fresh, or polls a net another driver walked deep,
    walks the route once. Consumed route nodes are a suffix of the route
    whoever rewrites the net, so any holder's route stays valid.
- **Amended by:** `interface-route-ring-record` (2026-10-10): the record
  keeps the newest route nodes exactly and a mark every 8 below them,
  from depth 4.
- **Rule lives in:** `InterfaceRoute` and `RuntimeNet::walk_interface_route`.
- **Recorded in:** structural overheads plan, `perf-interface-route-ownership`.

### Interface routes keep a ring of recent nodes and strided marks
`interface-route-ring-record` · 2026-10-10 · maintainer · accepted
- **Context:** an evaluation's interface route was a full stack, recorded
  only after a fresh walk's first 16 nodes. The maintainer expected that
  skip to cost nets of moderate depth and proposed a boxed record: an
  exact ring of the newest nodes plus a strided spill of the rest. A first
  measurement put the ring about 1% behind everywhere, which turned out to
  be the compiler's inlining along the driver path
  (`driver-step-boundaries`); with that pinned, the variants separate.
- **Decision:**
  - An `InterfaceRoute` boxes a record of the route: its 8 newest nodes
    exactly, in a ring, and every 8th node below them as a mark. It records
    the nodes a walk passes from depth 4, not the pair's near node, which
    the following rewrite consumes.
  - A walk resumes at the newest surviving recent node, else the newest
    surviving mark, else the interface. A result returning down the route
    walks each 8-node stretch once more, leaving it among the recent
    nodes, so unwinding stays amortized O(1) per level, in memory for 8
    nodes plus one byte per node of depth.
  - Instructions against the full stack with its 16-node skip:
    `literal_loop` (routes 3 to 16 deep) -1.8%, `list_computed` -0.2 to
    -0.4%, every other family 0 to -0.2%. Recording from the first node
    cost the shallow families up to 0.3% more (a full stack from the first
    node, 0.3 to 0.6%).
- **Consequences:** an evaluation whose routes stay within 4 nodes never
  allocates a record; a deep route costs one byte per node, not eight.
- **Rule lives in:** `RouteRecord` and `RuntimeNet::walk_interface_route`.
- **Recorded in:** structural overheads plan, `perf-driver-path-inlining`.

### The net driver's step boundaries are pinned
`driver-step-boundaries` · 2026-10-10 · agent · accepted
- **Context:** changes inside net operations, such as the interface walk
  or the copy path, moved the compiler's inlining along the evaluator's
  net driver loop and shifted instruction counts on every workload by
  about 1%, unrelated to the work changed.
- **Decision:** the core access's cursor and pair steps
  (`step_cursor_if_current`, `step_active_pair_if_current`) and the net's
  `poll_interface_demand` stay out of line; the cell's step functions are
  always inlined into their one evaluator caller. Instructions fell 0.4
  to 1.0% on every family against the build before.
- **Consequences:** a perturbation inside a net operation now moves counts
  by 0.1 to 0.2%, the noise floor for comparisons on this path. Changes to
  the driver loop itself can still move its own inlining; a new hot entry
  point from the driver into a net should get the same treatment.
- **Rule lives in:** comments at those attributes (`core_net.rs`,
  `interaction_net/runtime.rs`).
- **Recorded in:** structural overheads plan, `perf-driver-path-inlining`.

### A source with no evaluation left is copied whole
`reduced-sources-copy-whole` · 2026-10-10 · maintainer · accepted
- **Context:** a call to a net callable copied the callable's net through
  a remote cursor, node by node, so the copy shared whatever evaluation
  the source had left. Nearly every source had none: in 97 to 99% of
  cursor inspections the source had no active pair, copy or cursor
  obligation, and such sources averaged about five nodes. The maintainer
  sees this as the first case of a direction for interaction nets:
  tracking chunks to materialize together, based on partial
  normalization. It starts from an obviously safe subset.
- **Decision:**
  - A source with no active pair, copy, cursor obligation or callable
    checkpoint, and at most 64 nodes reachable from its interface, is
    captured under its own lock when the copy is prepared, and installed
    under the target's in one transition: the same nodes and wiring, with
    payloads duplicated and fan sites translated to fresh ones for the
    copy, as a cursor copy would end.
  - Nets are modified after construction only by evaluation (maintainer,
    2026-10-10), and nothing in such a net can reduce, so the capture is
    the copy a cursor would make.
  - Any other source still copies through a cursor.
  - The size limit bounds the work done under the source's lock and the
    copying of parts a caller may never demand.
- **Consequences:**
  - A whole copy reports its installed payloads to the collector as a run
    of fresh nodes (`RuntimeNetEdgeSet`), and the target keeps no edge to
    the source.
  - Remote cursors now serve sources with evaluation left, and copies
    begun before their source finished reducing. What remains on them
    measures what a general chunking scheme must cover.
- **Rule lives in:** `RuntimeNet::whole_copy`
  (`interaction_net/runtime/whole_copy.rs`).
- **Recorded in:** structural overheads plan, `perf-reduced-source-copy`.

### Interaction nets are polarized; `Bind >< Bind` joins crossed
`polarized-interaction-nets` · 2026-10-05 · maintainer · accepted
- **Context:** N8's random-net generator and net fuzzing need a well-defined
  space of well-formed nets, and the positional bind join gave a bind's
  auxiliaries no fixed signs.
- **Decision:**
  - Every wire joins a `+` (providing) port to a `−` (consuming) port.
    Reduction rules are unchanged; runtime links carry the polarity type
    (see `remote-polarity-runtime-type`).
  - `Bind >< Bind` joins crossed, as Lafont's γγ rule does. A function bind
    lists `[result, argument]` and an application `[argument, result]`. Fans
    still join positionally.
  - The exposed port is `+` by fiat, consistent with `Data(Net)`.
  - A constructed component unreachable from the exposed port is an error.
    Reduction garbage is allowed.
  - User merge fans are allowed. Erasers take either sign; in `+` position
    an eraser is an error value.
  - Build the checker before N8's generator and any net fuzzing.
- **Consequences:** a linear union-find checker runs at `try_finish` in every
  build and reports `NetBuildError::Polarity`. User netlists get diagnostics
  that name their constructors and ports. Rewrites must preserve polarity.
  GAL levels are deferred.
- **Rule lives in:** `agent_context/interaction_nets.md` "Polarity" and
  "Templates and Construction"; `Design.md` "Interaction Nets".
- **Recorded in:** polarity plan, "Decisions"; holistic review N8; commits
  `d5cf8c6f`, `13082fe1`, `0ccdf59e`.

### Runtime nets carry a remote-polarity type in their link words
`remote-polarity-runtime-type` · 2026-10-05 · maintainer · accepted
- **Context:** `polarity-runtime-invariant` tests polarity-type preservation
  (subject reduction).
  Later rewrites will depend on signs: translating positive erasure into
  error data, and GAL level nodes. Net performance must improve a lot, so
  no new runtime side tables.
- **Decision:**
  - Each stored link packs the peer's sign into a reserved port bit.
  - Remote polarity was chosen over storing each port's own sign: a wired
    port carries the same information, inverted. Rewrites mostly move typed
    references, and a new node binds to a remote port without any rule
    assigning its signs.
  - Every runtime node is typed, including interface anchors, callable
    checkpoints and remote cursors.
  - Only debug builds check: every wire joins opposite signs, and each
    created node satisfies its rule as an expected peer type.
- **Consequences:**
  - Template wires are stored provider-first.
  - The entire test suite is a preservation test; in its first run, the
    only violation was an unpolarized hand-built test fixture.
  - Test builds add one flag to each runtime net, so the core-net cell's
    test layout is 8 bytes larger.
- **Rule lives in:** `agent_context/interaction_nets.md` "Runtime Polarity
  Type".
- **Recorded in:** polarity plan, "Runtime Invariant Design".

### Callable WHNF runs inline first and spills to a linear checkpoint node only on suspension
`inline-first-callable-spill` · 2026-09-16 · agent · accepted
- **Context:** `Bind >< Data` forced callables synchronously.
- **Decision:** run the callable inline first. Only budget exhaustion or a
  real dependency installs `CallableCheckpoint(NetWhnfState)`, a one-port
  linear node whose only rule is with `Bind`. An `Operator >< Data` encoding
  and a `Gc<WhnfState>` were rejected.
- **Consequences:** no side table and no semantic value variant; the payload
  is boxed.
- **Recorded in:** callable-spill plan; resumable-WHNF plan W6B.4b.2.

### Net construction is pure state over ordered `ListEffect` search plus hidden strict-netlist replay
`pure-listeffect-net-construction` · 2026-09-20 · agent · accepted
- **Context:** the reflection-task interpreter for net construction was
  root-heavy and replayed routes.
- **Decision:** a pure builder with first-two selection and a hidden
  `InteractionNetFromNetlist` replay. It has no reflection, heap, task, log
  or environment access.
- **Consequences:** replay is synchronous with a one-shot proof. Budgeting
  large-netlist replay is deferred.
- **Rule lives in:** `agent_context/interaction_nets.md` "Ownership" and
  "Templates and Construction" (the mechanism, not the rationale).
- **Recorded in:** pure-construction plan.

## Evaluation and scheduling

### Retiring a runtime joins its workers
`runtime-retirement-joins-workers` · 2026-10-16 · maintainer · accepted
- **Context:** retiring a runtime detached its worker threads, on the view
  that a worker might run divergent work. A worker holds the value domain
  within a quantum, so the domain could outlive the last runtime handle,
  and a collection test that expected it gone failed under load.
- **Decision:** a worker is a weak holder of the runtime: once stopped it
  takes no new work, and retirement joins every worker but the calling one,
  waiting for each quantum in flight. A quantum is bounded, so one that
  never ends, or a worker taking quanta after the stop, is a bug to debug,
  not a case to tolerate. Retirement waits, so it must not happen under a
  heap mutator, which `assert_outside_mutator` checks.
- **Consequences:**
  - No worker holds the value domain once retirement returns.
  - Nothing a worker runs may own the runtime (maintainer, the same day):
    workers are not owners equal to the library's clients, so retirement
    asserts, in every build, that it is not running on one of its own
    workers. The CLI's effect specialization holds `Values` instead of an
    `Assembler`, and its stderr decoder reads evaluated bytes through
    `Values::evaluated_bytes` instead of evaluating.
  - Tests that observed a detached worker after shutdown now shut down on
    another thread and observe it while shutdown waits.
- **Rule lives in:** `architecture/evaluation.md`, the executor paragraph;
  `architecture/values.md` "Value Domain and Ownership".

### The public API runs turns on keyed machines, not handles
`runtime-api-turns-on-machines` · 2026-10-17 · maintainer · accepted
- **Context:** the lifecycle review's target model kept the API's shape: a
  thread-affine owner, with `Send` handles for everything else. A survey for
  `lifecycle-owner` found owners in every handle, and assemblers that
  create their own runtimes. The maintainer took the step as an occasion to
  re-envision the public API from user stories: a heap of handles, pointers
  through which code asks the runtime to act, is not necessarily the best
  interface.
- **Decision:** the runtime is an environment in which Rust threads act.
  - It is thread-affine: `!Send`, so it stays and retires on the thread
    that opened it, but `Sync`, so scoped threads may borrow it.
  - A thread runs a *turn* on a machine named by a user-chosen `Key`. A
    machine has a stack and local state, as reflection tasks have; it
    starts empty, and `clear` returns it there, so it has no lifecycle.
  - Transactions are steps within a turn, and nest; `retry` waits for a
    read to change.
  - Fork-join comes from scoped threads.
  - No communication is point-to-point: tasks and machines share state and
    logs in the same heap or volume, and abstract global paths serve as
    capabilities within it.
- **Alternatives rejected:** an owner with `Send + Sync` handles, which
  `lifecycle-owner` would have built; one transaction per turn, since
  hierarchical transactions need steps anyway; runtime-allocated machine
  IDs; and point-to-point channels, which hinder extension.
- **Consequences:** handles go, and retirement is dropping the runtime on
  its thread, scoped threads joined by construction. The ownership plan's
  remaining steps are re-planned from the review. Communication's
  conventions (retention, backpressure, discovery) remain open.
- **Amended by:** `turn-cuts-bound-transactions-and-search` (2026-10-18): a
  turn's transactions are cuts, which also bound backtracking search.
  `machines-form-a-tree` (2026-10-18): only the thread holding the runtime
  takes turns, machines form a tree, and fork-join comes from forks within
  turns rather than threads that borrow the runtime. The runtime is affine
  rather than thread-affine: `Send`, so it may move between threads, but
  not `Sync` (maintainer, the same day).
- **Recorded in:** runtime API review.

### Effect providers answer within turns; background effects go through queues
`effect-providers-answer-within-turns` · 2026-10-18 · maintainer · accepted
- **Context:** the configured CLI parses its arguments in an isolated
  search whose Rust effects return alternatives, edit per-branch journals,
  and query the file system within branches that may never commit. A
  machine answers only committed requests, through the heap, so turns and
  heap communication alone cannot serve them.
- **Decision:** Rust code answers effects synchronously within a turn, on
  that turn's thread, as the maintainer clarified the same day:
  - A turn runs a monadic effect by evaluation: it supplies the effects
    API, evaluates the program to weak-head normal form, and observes a
    yielded request with its continuation, or the final result. It
    interprets a request and resumes the continuation.
  - Helpers interpret the standard effects, so a user handles only the
    unique ones. A *provider*, the successor to `TaskSpecialization`, is
    the Rust code that answers those. Backtracking maps to the turn's cuts
    (`turn-cuts-bound-transactions-and-search`).
  - A provider answers what is local to its run. Shared state is state at
    paths in the heap.
  - Background tasks reach Rust only through request queues served by
    machines, as the lifecycle review's target model has it: Rust code
    never runs on a worker. Queues, data buses and publish-subscribe are,
    logically, transactional operations on shared state, in the heap or a
    specialized volume; how far the API specializes them is open.
  - Volumes and resolvers are opaque glam values. The runtime enforces a
    resolver's single assignment, and a resolver collected unresolved
    breaks its promise; its holder may read the request's status, or break
    the promise itself (maintainer, the same day).
  - The ownership plan's remaining steps (queues, host messages, leases,
    host calls, the retirement protocol) merge into the runtime API plan.
- **Alternatives rejected:** the parser rewritten in glam, which has no
  file access within a branch; a search in a turn that pushes every branch
  as data over glam primitives, which has the same gap; and queues stored
  apart from the heap (the lifecycle review's decision 8), since all
  communication is modeled in shared state.
- **Consequences:** a provider stores no capabilities and receives them
  per call. A committed effect within a cut is a request published when
  the cut commits, whose result arrives after it.
- **Recorded in:** runtime API plan, "Effect providers" and "From the
  ownership plan".

### A cut bounds a turn's transactions and its backtracking search
`turn-cuts-bound-transactions-and-search` · 2026-10-18 · maintainer · accepted
- **Context:** `runtime-api-turns-on-machines` made transactions nested
  steps within a turn (`m.transact`). Plans then spoke of a turn's work as
  if it ran in a transaction. In reflection tasks, `.cut` is both a
  transaction boundary and the commitment of a backtracking search.
- **Decision:** a turn is no transaction.
  - Each action on shared state is atomic by itself, without the setup of
    an isolated transaction.
  - `m.cut(|t| …)` groups actions into one atomic, isolated transaction,
    with abort, retry and backtracking. Cuts nest.
  - The Rust interface matches reflection tasks', where search and
    transactions share the boundary. Rust code may still choose by
    ordinary control flow; it is not restricted to backtracking.
  - Failure is explicit and retry implicit, as in glam (maintainer, the
    same day): `t.fail()`, after `.fail`, is the intent to backtrack, an
    error of its own kind. Each cut it passes through undoes what it did.
    The outermost cut it reaches is non-productive: it awaits a change in
    what it observed, then runs again. A conflicting commit runs it again
    at once. Any other error also undoes the cuts it passes through, but
    ends the outermost. So an alternative operation, like `.alt`, can
    backtrack on failure alone, and the runtime can recognize failure where
    it wraps a step.
  - The contract is a change in a cut's observations. How the runtime
    detects one is no part of the API: the first implementation wakes on
    any change and validates, and a finer wake may replace it.
- **Alternatives rejected:**
  - a turn as one transaction (already rejected by
    `runtime-api-turns-on-machines`);
  - a transaction step beside a separate search operation, two boundaries
    where reflection tasks have one;
  - backtracking on any error, with an explicit retry, as first built the
    same day: it reverses glam's dynamic, and leaves unclear how far an
    alternative operation should carry an arbitrary error.
- **Consequences:** `api-transaction-steps` becomes `api-cuts`, and effect
  searches run as cuts, so a branch's journal is machine state that a
  failed cut restores. Rust side effects within a cut tolerate a rerun,
  wait for commit, or are undone on abort: commit hooks run in order once
  the outermost cut commits, and abort hooks in reverse as a failure
  backtracks through them, on a best-effort basis (maintainer, the same
  day). A hook runs on its machine, with the local state as it stands,
  over a fresh stack of the values it captured when registered and an
  empty stash (maintainer, the same day), so it reaches glam values and
  the machine without borrowing the turn's.
- **Recorded in:** runtime API plan, `api-cuts`.

### Machines form a tree under the owner thread; forks are scoped to turns
`machines-form-a-tree` · 2026-10-18 · maintainer · accepted
- **Context:** `api-scoped-threads` built the first reading of
  `runtime-api-turns-on-machines`: a `Sync` runtime that threads borrow,
  each taking turns on machines by key, as on per-key locks. A turn waited
  for a busy machine, `try_run` declined one, and a check failed a wait
  that would close a cycle. Waiting on any of several machines had no
  answer short of polling, and a turn that joined a thread waiting for its
  own machine still deadlocked. Settlement could not know whether a thread
  would take another turn.
- **Decision:** the runtime controls concurrency (maintainer, the same day).
  - `rt.run(path, …)` takes the runtime mutably, so only the thread
    holding it takes turns, and none nests. The runtime is `Send` but not
    `Sync` (maintainer, the same day): one thread at a time holds it, and
    may hand it to another, which takes turns and retires it. Nothing in
    it depends on the thread that opened it, as forks already run its
    operations elsewhere, and no user code runs on its workers, where it
    must not retire. A path of keys names a machine; the empty
    path is the root, and a key names a child of the root.
  - A machine's state holds its children. A turn reaches its descendants
    alone: it takes synchronous turns on them (`m.run`), and moves values
    to and from their stacks (`m.give`, `m.take`), operations kept apart so
    that machine error states can build on them later.
  - Concurrency is a fork of a descendant onto a thread (`m.fork`), which
    joins at `m.join` or by the end of the turn that made it: the turn is a
    thread scope, so a fork may borrow what outlives the turn's
    `rt.run`. `m.scope` gives a group of forks the same rule. A fork moves
    no values; a joined machine does, synchronously.
  - A synchronous turn within a cut joins the cut: its actions on shared
    state join the transaction, and its state, part of its parent's,
    returns when the cut undoes.
  - A fork within a cut joins the cut too, with its siblings serializable
    (maintainer, the same day; built in `api-forks-in-cuts`). It works on
    a branch of the cut's journal that merges at its join, in join order,
    so the cut behaves as if each fork ran at its join. A fork that read
    what the cut wrote after the fork makes the outermost cut run again.
    A fork joins in the cut it was made in, and a cut's end joins the
    forks it made.
  - Forks are branded (maintainer, the same day): each turn, cut attempt
    and `m.scope` opens a thread scope, and its lifetime marks the forks
    made in it, so the compiler admits a join only on the machine, and in
    the cut or scope, that made the fork, and no fork leaves its scope. A
    fork within a cut may borrow what its turn declared before the cut.
- **Alternatives rejected:**
  - threads taking turns on arbitrary machines (as built in
    `api-scoped-threads`), with waiting, declining, an `await_any` or
    `run_any` for several machines, and deadlock checks;
  - a separate thread-scope type beside the machine, since the turn
    already brackets its forks;
  - forks with `'static` closures only;
  - moving values within `m.run`, since separate operations explain more
    simply.
- **Consequences:**
  - Hooks are `Send`, since a fork's hooks return to its parent's thread
    at the join.
  - Machines cannot deadlock over machines: holding is a tree, joins point
    downward, and nothing waits for a machine. The waits that remain are
    failed cuts and joins, both visible to the runtime, so settlement and
    deadlock detection see every thread.
  - A tree of synchronous turns communicates as monadic effects do: a
    child leaves a request in its stack, and the parent answers it.
  - A host embeds its threads as forked machines within a long turn.
- **Recorded in:** runtime API plan, `api-machine-tree`.

### An error that ends a turn leaves its machine in error until recovered
`machine-errors-mark-and-recover` · 2026-10-19 · maintainer · accepted
- **Context:** a turn is not atomic, so an error that ends one may leave
  its machine's state spoiled. Errors could also go unobserved: a fork
  joined at its scope's end dropped its error and its Rust result, and a
  hook's error replaced its cut's outcome by a rule the maintainer was
  unsure of.
- **Decision:**
  - An error or panic that ends a turn sets its machine's error flag, part
    of the machine's state. Only that sets it, and only as the turn
    returns. Observing an error, as a turn's or a join's result, never
    clears it.
  - Recovery is an entry: `rt.recover` and `m.recover` clear the flag and
    enter the machine as the failed turn left it, to inspect and repair by
    its own conventions, while `run`, `give`, `take` and `fork` fail on a
    machine in error. No machine runs normally with a pending error. Only
    the target's own flag matters, not that of machines between.
  - `rt.failed(path)` and `m.failed(path)` read the flag without clearing
    it, so a host or parent decides whether and how to recover.
  - Forks return nothing to Rust: what a fork computes stays on its
    machine's stack. A join returns the fork's outcome, keeping success,
    failure (`Backtrack`) and divergence (another error) apart, and an
    implicit join propagates them: the body's own outcome first, then the
    forks' in the deterministic order they were made.
  - Hooks return nothing and handle their own errors.
  - Panics resume at the join, explicit or implicit, and flag each machine
    whose turn they unwind.
- **Alternatives rejected:**
  - first built the same day: a list of the errors' diagnostics in the
    machine's state, pushed onto its stack by recovery, with
    `m.register_error` to add one before returning. Diagnostics serve
    people, not recovery, which needs the machine's state, and the
    registration took only Rust errors (maintainer, the same day);
  - marking only errors nobody received, which assumed the tree;
  - a separate acknowledgement operation, or acknowledgement within a turn,
    either of which conflates recovery with running;
  - diagnostics captured in machine state, which a long turn would hold
    back; communication takes them instead;
  - failure winning over an earlier divergence among joins, which would
    open non-determinism's interaction with transactions and backtracking.
- **Consequences:** settlement can find every machine left in error. The
  open question on hook errors is settled. A fork's results travel as
  glam values, as a machine's always have. Of several forks that fail at
  an implicit join, only the first's error reaches the outcome; the
  others leave their flags, and their diagnostics await the diagnostics
  channel of the ports re-plan.
- **Recorded in:** runtime API plan, `api-machine-errors`.

### Resumable WHNF through bounded regional quanta and durable checkpoints
`resumable-whnf-regional-quanta` · 2026-09-12 · agent · accepted
- **Context:** the recursive evaluator replayed prefixes after retryable
  halts, and reflection decoding created unbounded fresh lazies.
- **Decision:**
  - One `WhnfComputation` with a shared explicit work stack and a
    nine-variant vocabulary.
  - Each quantum is callback-free under access. A durable checkpoint is
    taken only at real boundaries.
  - Per-source phase enums were rejected: a census found 157
    demand-then-inspect sites against 2 tail demands.
- **Consequences:** no replay and a small Rust stack, at the cost of an edge
  walk per quantum.
- **Rule lives in:** `architecture/evaluation.md` "WHNF Submachine Flow".
- **Recorded in:** resumable-WHNF plan W0B and W0C.

### Partial lazy production belongs to the lazy; `LazySource` is an immutable recipe
`lazy-owns-partial-progress` · 2026-09-21 · agent · accepted
- **Context:** session-owned progress kept cycles alive.
- **Decision:** partial progress lives beneath the owning lazy, and
  coordinator routes are session-neutral.
- **Consequences:** cycles stay collectible.
- **Rule lives in:** `architecture/evaluation.md` "Lazy Producers".
- **Recorded in:** resumable-WHNF plan W6G.1c.

### One reflection boundary per module or declared object
`reflection-boundary-shared` · 2026-10-10 · agent · accepted
- **Context:** the g front end wrapped each module definition, and each
  member of a declared object, as `anno refl:ensure_tasks value`. The
  effect records the `refl.*` scanner in the module's heap state if no
  scanner is there yet, and otherwise does nothing; but every demanded
  definition ran it again as its own reflection task, about 98 reflection
  steps. That was 70% of the instructions in a chain of module
  definitions.
- **Decision:** a module, or a declared object instance, binds one
  boundary value, `anno refl:ensure_tasks ()`, and each wrapped value
  becomes `seq boundary value`. The boundary still runs to completion
  before any wrapped value is exposed, and its effect, idempotent once the
  scanner is recorded, runs once per module or object instance.
- **Consequences:** `chain` falls 66 to 67%, with 499 reflection steps at
  800 definitions where it took about 78,000. A failure in the boundary is
  cached by the shared value, so every definition behind it sees the same
  failure. The first boundary of a module still launches the scanner,
  about 22 M instructions in `minimal`.
- **Rule lives in:** `agent_context/reflection.md` "Front-End and Logger
  Integration".
- **Recorded in:** structural overheads plan, `perf-module-definition-cost`.

### Autonomous reflection tasks publish through a managed completion promise
`reflection-tasks-publish-via-completion-promise` · 2026-09-19 · agent · accepted
- **Context:** reflection had been misclassified as a lazy checkpoint, with an
  external task-observation sidecar.
- **Decision:**
  - The activated task runs to terminal. A terminal mapper fulfills a
    managed completion promise, which the lazy's checkpoint waits on.
  - Retiring the last subscriber never cancels the task.
  - A spark needs only at-most-once scalar admission.
- **Consequences:** supersedes the sidecar, which was removed. Some standing
  docs still describe it; the documentation disposition's stale-docs list
  tracks the fix.
- **Recorded in:** W6G1 design review; resumable-WHNF plan W6G.1f.3.

### Specialization callbacks use pollable request work
`pollable-specialization-requests` · 2026-09-14 · agent · accepted
- **Context:** synchronous evaluation inside request preparation forced
  replay.
- **Decision:** request work is specialization-owned and pollable, and
  `RequestContext` loses evaluation. Rejected: per-argument Raw/Whnf flags,
  declarative preparation with a second continuation, and a universal
  reflection continuation.
- **Consequences:** a source-breaking change to `TaskSpecialization`.
- **Rule lives in:** `architecture/reflection.md` "Persistent Effect
  Machine".
- **Recorded in:** resumable-WHNF plan W5C.5a; W5 review.

### No user-controlled semantic recursion on the Rust stack
`no-semantic-recursion-on-rust-stack` · 2026-09-24 · agent · accepted
- **Context:** deep user data overflowed the stack.
- **Decision:** the only permitted forms are bounded plumbing, log-depth
  balanced containers, and owned worklists.
- **Consequences:**
  - Tests use small-stack controls.
  - Parsing `.g` files is not yet covered: the parser still recurses on the
    Rust stack. The deep-nesting remedy, from the syntax tree's drop
    through the later front-end walks, belongs to the roadmap's
    `perf-pre-eval-stack-depth` (maintainer, 2026-10-15; first assigned to
    the panic-safety plan).
  - Core-value drop depth is still open (holistic review V3).
- **Rule lives in:** `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** resumable-WHNF plan W7A.1; W7 review; documentation
  disposition, maintainer answer 4.

### Every reduction costs one budget unit; observation is free
`reduction-costs-one-budget-unit` · 2026-10-05 · maintainer · accepted
- **Context:**
  - The net driver charged only semantic handoffs, so one poll could run a
    long or divergent pure reduction inside one access region.
  - Elsewhere the WHNF driver charged observation, and builtin machines
    charged nothing of their own.
  - A separate admission after a net claim needed a refund path, which
    panicked on a blocked checkpoint.
  - One-unit polls never finished a call on a lazy callable.
- **Decision:** a step pays one unit when it changes state; observing is
  free; a step needs a unit available to start.
  - **Nets:** every rule application is charged at its claim, through an
    admission callback. Resuming a callable checkpoint continues an earlier
    call: it needs a unit available but is not charged.
  - **WHNF:** a delegation costs one unit; reporting a ready value, a
    boundary or a failure is free.
  - **Builtins:**
    - applying an immediate builtin costs one unit;
    - a builtin machine step whose operand evaluation cost nothing pays one
      unit for its own work.
  - **Reflection:** steps charge as before, and waiting pumps within the
    caller's budget.
- **Consequences:**
  - A poll performs at most its budget of reductions. Free observation
    cannot loop, because a poll without a reduction ends in a result,
    handoff, contention or block.
  - With no units left, WHNF stops before its next step. A ready result
    after the last paid step is therefore reported by the next poll, where
    nets report it at once.
  - Budgets stay simple heuristics rather than cost models: one step may
    still do work proportional to its data.
  - Batches, once they exist, are admitted while a unit remains and may
    finish past the budget, with the overrun forgiven (maintainer,
    2026-10-05). Exact reduction counts come from profiling counters, not
    budgets. See the performance roadmap.
- **Rule lives in:** `architecture/evaluation.md` "Context and Session";
  `agent_context/interaction_nets.md` "Reduction and External Work".
- **Recorded in:** holistic review N9 and its budget consistency audit.

### The step budget is a reservation; a poll spends at most its budget
`step-budget-is-reservation` · 2026-09-25, extended 2026-10-05 · agent · accepted
- **Context:** exact spend and foreground allowance were conflated, and
  isolated effect search could spend about steps² units.
- **Decision:**
  - `EvaluationStepBudget` is exact. Nested machines borrow it and never
    mint a fresh allowance.
  - The foreground demand pump treats its scalar as a reservation
    allowance.
  - `EffectTask::poll(steps)` keeps one budget for the whole call and
    charges the caller for every pumped quantum (2026-10-05).
- **Consequences:** `Yielded` still conflates progress and budget (holistic
  review E1, open).
- **Rule lives in:** `architecture/evaluation.md` "Context and Session" (not
  yet the per-call poll budget).
- **Recorded in:** resumable-WHNF plan W7C.0; holistic review R3; commit
  `4a0fa1b3`.

### Foreground demand keeps a validated route hint; full traversal stays authoritative
`validated-exact-route-hint` · 2026-09-23 to 2026-09-28 · agent · accepted
- **Context:** the W6G.4 profile showed 18,800 chain walks over 2.76M edges.
- **Decision:** a caller-local route hint, checked by `work_generation` plus a
  private hazard revision. A queued ancestor runs before its stale block is
  followed. Rejected: an eager root-to-leaf cache, an authoritative
  ready-descendant index, and a hasher as the primary repair.
- **Consequences:** 23% fewer instructions. Route storage is still O(depth),
  an accepted cost (resumable-WHNF holistic review WHNFHR-006).
- **Rule lives in:** `architecture/evaluation.md` "Context and Session"
  (partly).
- **Recorded in:** W6G4 review; resumable-WHNF plan W9C.3.

### The shared condvar is never notified with `notify_one`
`shared-condvar-notification-policy` · 2026-09-28 · agent · accepted
- **Context:** `notify_one` caused a real spark/client admission bug.
- **Decision:** never use `notify_one` on the shared condvar. Suppress
  notification only for mutation kinds that enable no waiter. The generation
  always advances.
- **Consequences:** 33.6% fewer `notify_all` calls. Splitting the condvar
  waits for a profile with workers enabled.
- **Rule lives in:** `agent_context/evaluation.md` "Sessions and Workers".
- **Recorded in:** resumable-WHNF plan W9C.4 and W9D.4.

### Deterministic hashing only for bounded runtime work-ID sets
`trusted-hasher-scope` · 2026-09-28 · agent · superseded
- **Context:** randomized hashing costs show up in hot scheduler
  traversals, but deterministic hashing over user-influenced keys invites
  pathological collisions.
- **Decision:** `TrustedWorkIdHasher` is used only for bounded traversal sets
  of runtime-allocated work IDs. Indexes keyed by user data, and persistent
  coordinator indexes, keep randomized hashing.
- **Consequences:** a new deterministic-hash use must show its keys are
  runtime-allocated and its set is bounded.
- **Rule lives in:** `agent_context/evaluation.md` "Sessions and Workers".
- **Recorded in:** resumable-WHNF plan, exact-route work.
- **Superseded by:** `trusted-key-hashing`.

### Maps keyed by runtime-allocated ids use a checked trusted hasher
`trusted-key-hashing` · 2026-10-07 · maintainer and agent · accepted
- **Context:** SipHash showed up in hot maps keyed by runtime ids. The
  maintainer asked for trusted-key hashing widely and opportunistically,
  with drift caught by static checks or a review latch.
- **Decision:**
  - Maps keyed by runtime-allocated ids use `crate::trusted_hash`: one
    folded widening multiply per integer, then a rotation. It has no
    collision resistance.
  - `TrustedState<K>` requires `K: TrustedKey`, implemented only in
    `trusted_hash.rs` with the reason a program cannot choose the key; a
    test rejects implementations elsewhere.
  - Keys a program can influence keep `RandomState`. The collector crate
    keeps its own private copy.
  - Supersedes `trusted-hasher-scope`, which allowed deterministic hashing
    only for bounded traversal sets of work ids.
- **Consequences:** the countdown at depth 100 fell from 735 M to 454 M
  instructions.
- **Rule lives in:** `agent_context/evaluation.md` "Sessions and Workers";
  `src/trusted_hash.rs`.
- **Recorded in:** performance roadmap `perf-fast-id-hashing`; commits
  `d80bbb7c`, `5847644f`, `3791da97`.

### Condvars notify only registered waiters
`counted-condvar-notifications` · 2026-10-08 · maintainer and agent · accepted
- **Context:** the standard futex condvar makes a syscall on every
  notification, waiter or not. Single-threaded `countdown_400` made about
  600,000 such syscalls, half of its CPU time. The maintainer had suspected
  excessive `notify_all`.
- **Decision:**
  - Every glam condvar is a `CountedCondvar`. It counts waiters, each
    registered under the waited mutex before it sleeps, and skips a
    notification with none. A test rejects the standard condvar elsewhere
    in the crate.
  - The collector's admission condvar counts waiters in its locked
    coordinator, so notifying requires the lock.
  - The maintainer's alternative, one coalesced notification per quantum,
    is the separate experiment `perf-coalesced-wakeups`. It waits for a
    profile with workers that shows wake storms.
- **Consequences:**
  - Single-threaded evaluation makes no futex syscalls.
  - `countdown_400` fell from 854 to 330 ms of CPU, `hello_elf` from 1,937
    to 827 ms.
  - `shared-condvar-notification-policy` still holds for notifications
    that reach waiters.
- **Rule lives in:** `src/counted_condvar.rs`; `agent_context/evaluation.md`
  "Sessions and Workers".
- **Recorded in:** evaluation-recursion plan, "Finding 2026-10-08";
  `perf-admission-wakeups`, `perf-idle-wakeups`.

### A `TaskHalt` is only a failure or a panic
`taskhalt-is-failure-or-panic` · 2026-10-05 · agent · accepted
- **Context:** `TaskHalt::Blocked` was a leftover suspension channel that
  production never produced.
- **Decision:** remove the `EvaluationHalt`→`TaskHalt` conversion and the
  blocked variant. Reflection machines absorb evaluator waits as
  `WorkDependency::Wait` before any halt is built.
- **Consequences:** reflection has one halt vocabulary, and the inverted
  `protocol` → `machine` import is gone.
- **Rule lives in:** `agent_context/reflection.md` "Effect Boundary".
- **Recorded in:** holistic review R7; commit `6215f517`.

### Worker activation is all-or-nothing and retryable
`transactional-worker-activation` · 2026-10-05 · agent · accepted
- **Context:** a spawn failure part-way through left workers live and
  rejected a retry.
- **Decision:** workers spawn behind a start gate. On failure: abort, join,
  and publish nothing. On success: publish, then release the gate.
- **Consequences:** a test-only spawn-failure hook covers the failure path.
- **Recorded in:** parallel review AR-002; commit `1f58d0f0`.

### A route forces the lazies it needs inline; only a suspended lazy gets a route
`inline-lazy-forcing` · 2026-10-07 · maintainer and agent · accepted
- **Context:** every forced lazy got its own coordinator route, about 60 per
  countdown level, each costing an admission, claims, releases, retirement
  and notifications. The maintainer asked for the easiest equivalent of
  tail-call optimization and approved the inline mechanism.
- **Decision:**
  - A claimed route forces an uncached lazy inline when the lazy has no
    route, no other inline claim, and a resumable source or checkpoint.
    Host calls and reflection tasks keep their routes. At most 32 inline
    lazies stack above the route's lazy.
  - The claim is a coordinator set beside the route index, not an
    `InlineForcing` marker in the lazy cell as first proposed. One lock
    then decides both "has a route" and "is claimed inline", and a claim
    ends with the poll, so no cell state needs recovery or tracing.
  - An inline lazy that suspends spills: its claim ends, it gets a route,
    and the route's lazy blocks on it. Spilling the top lazy means the
    lazies between are polled again once, when it completes, not after
    every quantum.
  - Contention and cycles need no new protocol. A second demander admits
    the lazy's route, which stays busy (`InlineForced`) and becomes
    claimable when the inline claim ends.
    A cycle through inline lazies spills into routes, where the existing
    cycle detection reports it.
- **Consequences:** route admissions fell 7 to 15 times on the profiling
  workloads, and countdown_400 fell from 1.23 s to 0.79 s. Polls at family
  handoffs with budget left now continue in the route instead of yielding to
  the scheduler (part of holistic review E1). A non-tail chain still spills
  about every 32 lazies; tail forwarding is the next step.
  A lazy forced inline that panics records the panic, and so does the
  route's own lazy as the route unwinds, although its own code did not
  panic: its machine unwound mid-step, so its progress is torn. Stickiness
  thus spreads one level, to the forcing lazy, beyond the panic-safety
  plan's "never spreads past where the panic happened" (accepted,
  maintainer, 2026-10-15).
- **Rule lives in:** `architecture/evaluation.md`, the paragraph on claimed
  routes forcing lazies inline.
- **Recorded in:** evaluation-recursion plan, `eval-recursion-inline-forcing`.

### A lazy reached in tail position forwards to its target
`lazy-tail-forwarding` · 2026-10-08 · maintainer and agent · accepted
- **Context:** the maintainer wanted whatever equivalent of tail-call
  optimization could be had easily. A tail-recursive countdown kept every
  level's lazies alive through a chain of checkpoints and blocked routes:
  175 MB at depth 10,000.
- **Decision:**
  - A route-driven WHNF checkpoint that reaches an uncached lazy with no
    continuation left becomes `Forward(target)`, a fourth lazy producer
    state holding a traced edge.
  - A forwarding lazy's machine follows the chain to its end, shortens its
    own forward, and caches a cached end's result. Tail calls on the inline
    stack shorten the forward below and leave the stack; a route whose
    suspended top has only forwarders below suspends in place.
  - A lazy between a chain's ends is no longer cached when the chain
    completes. It stays a forward and caches its value when observed, as
    the approved proposal said.
  - A forward chain that closes a cycle fails each member with one
    dependency cycle. Followers detect cycles, rather than installation,
    since two routes may close one concurrently.
- **Consequences:** memory at depth 10,000 is constant (46 MB, the same as
  at depth 2,000), and instructions fall 11%. Non-tail recursion still
  grows the stack and spills. Inline claims are per session, so a lazy
  shared between sessions may be forwarded under another session's
  machine; every checkpoint poller resyncs from the lazy when its
  checkpoint is gone (2026-10-09, `eval-recursion-forward-resync`).
- **Rule lives in:** `architecture/evaluation.md`, the paragraph on tail
  calls in constant space.
- **Recorded in:** evaluation-recursion plan,
  `eval-recursion-tail-forwarding` and `eval-recursion-forward-resync`.

### A blocked record awaiting its wake is busy
`pending-wake-is-busy` · 2026-10-09 · agent · accepted
- **Context:** with workers, `chain_800` took 108 to 283 s of CPU against
  1.25 s without them. Publication sets a terminal before it wakes the
  blocked dependents. In between, the exact walks found no producer and
  reported `NoProgress`; the client retried at once, walking the chain
  under the coordinator mutex the worker needed for the wake.
- **Decision:** a blocked record whose dependency is terminal is `Busy`:
  the publishing or releasing thread owns its wake, and the client waits
  on the work generation. One classifier, `dependency_edge_locked`, serves
  the exact route, the background probe and the causal-child probe. Route
  validation also returns a route with a retired tip to its nearest
  registered parent, extending `validated-exact-route-hint`, which
  recovered only a route with one parent. Rejected: making the client wait
  whenever the abandon check declines. It would hide a misclassification
  rather than fix it, and `NoProgress` also drives retryable halts
  elsewhere.
- **Consequences:** `chain_w4_800` takes 2.7 s of CPU and 8% more
  instructions than without workers. Workers still walk from the root on
  each claim, and a mid-route mismatch still rebuilds the route
  (`perf-worker-route-walks`).
- **Rule lives in:** `agent_context/evaluation.md` "Sessions and
  Workers"; `architecture/evaluation.md`, the exact-demand zipper
  paragraphs.
- **Recorded in:** structural overheads plan, `perf-worker-scaling`.

### Nothing evaluates under the runtime mutation gate; a failed status's diagnostic is built first
`status-diagnostic-before-gate` · 2026-10-14 · agent · superseded
- **Context:** publishing a failed reflection task's `.task.status` built
  the failure's diagnostic under the gate's shared guard, through an
  isolated session and a nested driver that took the gate again. The gate
  is a `std::sync::RwLock`, which on Linux refuses readers while a writer
  waits, so a settlement that queued between the two reads deadlocked both
  threads. A test that queues one there hung.
- **Decision:** a `RuntimeFailureRoot` builds its diagnostic once and
  shares it (`RuntimeFailureRoot::diagnostic`). Settlement builds it before
  taking the gate when the task's protected status publisher will publish
  it, checking under the same coordinator lock that takes the update and
  retrying once if it found it unbuilt. Debug builds panic when a thread
  takes the gate it already holds. Rejected: building it for every failed
  task, which forces diagnostics nobody reads; and publishing it lazily,
  which changes what a status reader sees.
- **Consequences:** with the fix disabled, the check fails 12 existing
  tests that publish failed statuses, besides the new one. The gate check
  runs in every debug build and test; the maintainer kept it as a panic on
  2026-10-14. It then found `route-leases-drop-outside-locks`.
- **Rule lives in:** `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** explicit heap holds plan, `gc-hold-boundaries` (fourth
  slice, `status-diagnostic-before-gate`).
- **Superseded by:** `senders-never-evaluate-diagnostics`.

### A task block, dependency or wait is dropped outside runtime locks
`route-leases-drop-outside-locks` · 2026-10-14 · agent · accepted
- **Context:** a wait cloned from a lazy route's demand holds a lease on
  the route, and dropping the last one releases the route's demand, which
  takes the mutation gate and the coordinator lock. Releasing claimed work
  dropped its clone of the dependency's wait under the gate, and the gate
  check fired once a CLI test's woken task was claimed and completed in
  between, dropping its block first. Retiring abandoned work dropped its
  block under the coordinator lock as well, which relocks that mutex: a
  hang in every build when the block held the last lease.
- **Decision:** the rule that machines are destroyed after runtime locks
  are released extends to task blocks, dependencies and waits cloned from
  them. Work releases drop their promoted wait, and a client demand its
  superseded subscription, after their guards; detaching reflection or
  deferred work returns its block for the caller to drop. Rejected for now:
  deferring a lease's release whenever its thread holds the gate, a new
  mechanism where the existing rule covers the sites.
- **Consequences:** two tests force the orderings: a second thread claims
  and completes a woken task while its first release pauses, and a closing
  session retires a task blocked on a route only it demands. Both failed
  before the fix, the first through the gate check, the second through it
  on the way to the relocked mutex.
- **Rule lives in:** `architecture/evaluation.md`; `agent_context/evaluation.md`
  "Values and Forcing".
- **Recorded in:** explicit heap holds plan, `gc-hold-outer-boundaries`.

## Assembly

### Manifest writes are identity-checked and atomically published
`atomic-identity-checked-manifest` · 2026-10-05 · agent · accepted
- **Context:** an output that aliased an input through a symlink or hard link
  truncated the input.
- **Decision:** reject an output whose file identity matches an input, then
  write a sibling temporary file and rename it into place.
- **Consequences:** a symlinked destination is replaced, not written through.
- **Recorded in:** parallel review AR-001; commit `1f58d0f0`.

## Managed values and collection policy

### The public `Value` is an opaque transport handle
`public-value-is-opaque-handle` · 2026-08-28 · agent · accepted
- **Context:** weak roots cannot rebuild equality after the domain is torn
  down, and keeping the heap alive from every `Value` contradicts the
  teardown model.
- **Decision:** a public value is inline or one registered root. It has no
  `PartialEq`, `Eq`, `Ord` or `Hash`, and its `Debug` is content-free.
  Observation needs a live, matching runtime.
- **Consequences:** clients derive host keys only through authorized
  observation.
- **Rule lives in:** `architecture/evaluation.md` "Collector Boundary".
- **Recorded in:** GC integration plan I2A and I2B; 2026-08-25 integration
  review GCI-002.

### Managed destruction is passive; every family carries a mandatory drop record
`passive-managed-destruction-with-drop-records` · 2026-09-02, extended 2026-10-02 · agent · accepted
- **Context:** `Trace` does not constrain `Drop`.
- **Decision:** managed destructors are passive, and active cleanup lives in
  external-owner RAII. Since I13A, `ManagedFamily` requires a non-empty
  `ManagedDropRecord` covering direct and transitive destruction.
- **Consequences:** every new managed family needs a reviewed destruction
  record.
- **Rule lives in:** `architecture/values.md` "Managed Families";
  `agent_context/evaluation.md` (the new-family checklist).
- **Recorded in:** GC integration plan I4.0; I13 cleanup inventory; G4
  review.

### Recursive identities are managed edges; aggregate shells stay `Arc`-shared and acyclic
`recursive-identities-are-managed-edges` · 2026-09-04 to 2026-09-10 · maintainer · accepted
- **Context:** cycles through lazies, promises and nets could not be
  reclaimed.
- **Decision:**
  - Lazies, promises and nets are the only mutable recursive identities,
    and they are exact managed edges.
  - Aggregate shells stay acyclic `Arc` sharing until Value Representation
    Refinement (VRR).
  - Roots never stand in for internal cycle edges.
  - Promise liveness comes from producer-owned roots plus a root-free route.
    Collector weak pointers, failure-from-wait and stronger rooting were
    rejected.
- **Consequences:** the shells become VRR input.
- **Rule lives in:** `architecture/evaluation.md` "Collector Boundary" and
  "WHNF Submachine Flow" (not yet the acyclicity rule).
- **Recorded in:** I5I10 review; GC integration plan I5.0; ownership ledger
  classification.

### Fresh allocations are published before regional access ends
`publish-before-access-ends` · 2026-09-08 · agent · accepted
- **Context:** fresh edges escaped access regions unrooted.
- **Decision:** install or publish a fresh allocation before its access
  region ends. No self-opening constructors, and no root created only to
  bridge adjacent statements. Rejected: a fresh-allocation typestate, a
  temporary root per allocation, and recursive `Value` branding.
- **Consequences:** every new managed family must follow this rule.
- **Rule lives in:** `architecture/evaluation.md` "WHNF Submachine Flow".
- **Recorded in:** I5 review GCI5R-001 and GCI5R-001G; GC integration plan,
  "I6+ Regional Allocation Migration Rule".

### Collector traversal is separate from mutator observation
`collector-traversal-separate-from-observation` · 2026-09-11 · agent · accepted
- **Context:** the bootstrap collector is stop-the-world, but concurrent or
  moving collection must not have to unpick an API that assumes no mutator
  is active.
- **Decision:** tracing receives only a collector-created visitor and
  delegates to each family's crate-private `trace_managed_edges`. Rooting and
  observation use `RuntimeValueAccess`. Neither API encodes "no active
  mutator". Canonical runtime values root only the initial metadata carrier;
  edge-free atoms such as unit are built inside the caller's access, not held
  as permanent roots.
- **Consequences:** a future collector can supply its own visitor without
  changing mutator code.
- **Rule lives in:** `architecture/evaluation.md` "Collector Boundary".
- **Recorded in:** aggressive-GC remediation plan.

### Owner-qualified edge-transition gateways are kept as no-op barrier sites
`owner-qualified-edge-gateways` · 2026-09-10 · agent · accepted
- **Context:** a future concurrent collector needs exact barrier sites.
- **Decision:** every edge-set change goes through an owner-qualified
  gateway. Nets report exact per-edit deltas under the net mutex.
- **Consequences:** zero cost under stop-the-world, and ready for
  snapshot-at-the-beginning (SATB) barriers.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "`mutation::Mutator`
  edge-transition gateways"; `architecture/evaluation.md` "WHNF Submachine
  Flow".
- **Recorded in:** I5 review GCI5R-002; I8 review.

### Reflection computations trace effect and target as managed edges
`reflection-computation-traces-effect-edges` · 2026-09-10 · agent · accepted
- **Context:** a reflection cycle (`meta_refl`) stayed alive through registry
  roots.
- **Decision:** a reflection lazy's effect and gate target are direct
  semantic edges. Rejected: parser rejection, evaluator cycle detection, and
  weak registry roots.
- **Consequences:** the cycle is collectible. The companion external
  task-observation sidecar was superseded on 2026-09-19 by
  `reflection-tasks-publish-via-completion-promise`.
- **Rule lives in:** `architecture/evaluation.md` "Runtime-owned constructed
  values".
- **Recorded in:** I5 review GCI5R-005.

### Persistent managed edges are move-only; raw core values are regional
`persistent-edges-move-only` · 2026-09-12 · agent · accepted
- **Context:** implicit `Copy` and `Eq` hid ownership handoffs.
- **Decision:**
  - `Gc<T>` has no `Copy`, `Clone`, `PartialEq`, `Eq`, `Debug` or `Hash`.
    Duplication and identity are explicit, through `duplicate_in` and
    `same_allocation_in`.
  - `ErasedGc` is the only copyable identity. Raw `core::Value` loses
    `Clone`, `Eq` and `Debug`. Roots stay clonable, without equality.
- **Consequences:** an LLVM-IR codegen latch keeps duplication a single
  pointer load. No moving or branding claim is made.
- **Rule lives in:** `architecture/evaluation.md` "WHNF Submachine Flow";
  `crates/glam-gc/VERIFY.md` introduction. `SAFETY.md` is stale here.
- **Recorded in:** persistent-edge plan; aggressive-GC remediation plan D.2a.

### Opaque payloads are stored only as passive external owners
`external-only-opaque-storage` · 2026-09-11 · maintainer · accepted
- **Context:** a sealed managed opaque arm would need a per-type family,
  tracing, passive drop, and a redesign of the task-handle lifecycle.
- **Decision:** opaque payloads are passive external owners. Reopen only
  through a new design review, and only for a concrete recursive edge whose
  retention is materially harmful and whose destruction can be passive.
- **Consequences:** a task result that holds its own handle is retained until
  runtime teardown, an accepted cost.
- **Rule lives in:** `architecture/evaluation.md` "WHNF Submachine Flow"
  (the outcome only).
- **Recorded in:** opaque-representation review.

### Moving collection waits for parallel-root retirement
`moving-gc-retirement-gate` · 2026-09-09 to 2026-09-11 · agent · accepted
- **Context:** parallel roots are transition scaffolding.
- **Decision:** moving collection is blocked until no bare `Value` lives
  across a yield, no data sits beside a parallel root,
  `CompatibilityValueEdges` has no implementation left, and every persistent
  edge is rewritable or stable. Project with `Root::as_gc`; never cache a
  `Gc` beside its `Root`.
- **Consequences:** VRR prework must make progress on these conditions.
- **Recorded in:** aggressive-GC remediation plan, "Poll-spanning ownership
  and moving-GC retirement policy"; I5 review GCI5R-008.

### Lifetime-branded `ScopedGc` is deferred until a defect demands it
`scoped-gc-deferred` · 2026-10-01 · agent · superseded
- **Context:** an unbranded `Gc<T>` can outlive its region; today's proof
  rests on audit.
- **Decision:** defer branding. Adopt it only after reproducing a concrete
  defect.
- **Consequences:** no concurrent or moving readiness is claimed.
- **Rule lives in:** `architecture/evaluation.md` "WHNF Submachine Flow".
- **Recorded in:** D2h review D2HR-004; scoped-pointer plan SP0.
- **Superseded by:** `owned-mutator-views` (2026-10-12), which branded
  views (`GcRef`) for their cost, not after a defect.

### Runtimes never collect on mutator entry (`NoAuto`); drivers collect at quantum boundaries
`noauto-runtime-collection-policy` · 2026-10-02, revised 2026-10-05 · maintainer · accepted
- **Context:**
  - Automatic collection would put a lease and wake around every outer
    access, and pause placement would be accidental.
  - The 2026-10-02 version acted on pressure only at a stable pump, so a
    busy runtime, and every CLI assembly, never collected until its work was
    done. The maintainer called that a serious oversight (2026-10-05).
- **Decision:**
  - Every heap is `NoAuto`, fixed at construction, and ordinary mutator
    entry never collects.
  - Drivers collect at their quantum boundaries when the collector's
    pressure latch is set: after a claimed task, spark or client demand is
    polled and before it is released. The poll's access region has closed,
    and the claim keeps readiness `Busy` through the collection.
  - `pump_until_stable` still promotes pressure to `MaintenanceRequired`
    for embedders that service explicitly.
  - Rejected: `Automatic` by default, a hybrid, and public configuration.
- **Consequences:**
  - Long foreground work, CLI assembly included, collects as it goes.
  - Pauses fall only between quanta.
  - A driver nested inside an access region skips: the collector reports
    `ActiveMutator`, which records no failure.
  - Work that stays inside one access region defers collection until its
    next boundary.
- **Rule lives in:** `architecture/evaluation.md` "Context and Session";
  `agent_context/evaluation.md`.
- **Recorded in:** RuntimePolicy review; GC integration plan I1A; 2026-08-25
  integration review GCI-001 and GCI-015; holistic review decision 2 and
  the performance roadmap (2026-10-05 revision).

### Runtime-owned GC maintenance state with actionable readiness
`runtime-owned-gc-maintenance-state` · 2026-10-02 · agent · accepted
- **Context:** an anonymous `Busy` state hid pending maintenance.
- **Decision:** one runtime-owned maintenance record with unwind-safe leases.
  Readiness precedence is `Busy`, then `MaintenanceFailed`, then
  `MaintenanceRequired`. Collector statistics are never readiness authority.
- **Consequences:** any maintenance failure fails a batch.
- **Rule lives in:** `architecture/evaluation.md` "Context and Session"
  (partly).
- **Recorded in:** readiness review; explicit-maintenance review.

### Settlement validation ignores the collector's maintenance revision
`settlement-ignores-maintenance-revision` · 2026-10-03 · agent · accepted
- **Context:** revision churn from no-op leases rejected valid settlements.
- **Decision:** settlement rechecks work generation and exits, the
  observation epoch, empty outputs and a clean maintenance state, but not
  the maintenance revision. The revision stays only the compare-and-swap
  token for explicit service. Rejected: stopping no-op leases from bumping
  the revision.
- **Consequences:** supersedes the revision comparison. Correctness relies on
  non-moving, root-preserving collection, so the concurrent-GC plan's Open
  Design Gate 9 must revisit settlement.
- **Rule lives in:** `architecture/evaluation.md` "Context and Session".
- **Recorded in:** aggressive-GC regression plan D2 and D3.

### Aggressive-GC verification reuses the `NoAuto` stable-pump decision
`aggressive-gc-via-stable-pump` · 2026-10-03 to 2026-10-04 · maintainer and agent · accepted
- **Context:** a per-entry collection lease deadlocked against the settlement
  gate, and the mode had been red since I12A.
- **Decision:**
  - Change only the pressure input, to "allocated since the last stable
    pump". Under the feature, the pump services its own promotion.
  - Erase `Heap::enable_collection_before_outer_entry`; it must not become an
    API contract (maintainer).
  - Tests primarily about `NoAuto` do not run under the feature
    (maintainer).
  - Rejected: skipping the lease on `try_read`, a publication-only gate, a
    lease driven by glam-gc, and settlement as an active region.
- **Consequences:** supersedes per-entry aggressive collection. About one
  collection per stable cycle instead of per outer entry; `full` took 911 s
  instead of about 4 h.
- **Rule lives in:** `architecture/evaluation.md` "Context and Session".
- **Recorded in:** aggressive-GC regression plan D4–D11; commits `df60695d`,
  `94ce5952`.

## Collector crate (`glam-gc`)

### Exact, non-moving, stop-the-world full collector; one heap per runtime
`exact-nonmoving-stop-the-world-collector` · 2026-08-19 to 2026-08-21 · agent · accepted
- **Context:** the goal was reclaiming recursive cycles such as fixpoints, not
  building a performance collector.
- **Decision:** explicit roots, no stack scanning, and no cross-heap edges.
  Moving, generational and concurrent collection each need a new plan.
- **Consequences:** barriers stay structural no-ops.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Implemented Phase Status";
  `architecture/evaluation.md` "Collector Boundary".
- **Recorded in:** GC roadmap, "Scope decision" and invariants 1–5.

### Fixed typed-run geometry with no large-object path
`fixed-typed-run-geometry` · 2026-08-21 · agent · accepted
- **Context:** owner lookup must be constant-cost.
- **Decision:** 64 KiB typed runs in 8 MiB chunks, with 128 B payload
  alignment. Owners are found from aligned run bases. Chunks are kept until
  heap destruction. There are no variable runs or large objects.
- **Consequences:** an object must fit in one run. Tag-encoded run classes
  are deferred to VRR.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Arena Ownership Invariants"
  and "Run Topology Invariants".
- **Recorded in:** collector implementation plan C2A.1; VRR plan, "GC-Facing
  Run Lookup".

### Collection is elected at an idle outer entry; admission is a mutex-and-condvar state machine
`idle-entry-election-and-condvar-admission` · 2026-08-22 · agent · accepted
- **Context:** the queued-writer drain protocol was too complex.
- **Decision:** collection requests are coalesced hints. An idle outer entry
  elects collection, and outer exit never collects. `RwLock` was rejected:
  reader/writer priority is not portable, and admission needs more than
  shared and exclusive modes (idle-only election, the collector's mutator
  obligation through finalization, epochs, and poison).
- **Consequences:** an entry prepares, is admitted, then activates its
  thread-local entry state.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Regional Mutator Admission
  Invariants".
- **Recorded in:** collector implementation plan C3E; C2C review GC2C-003.

### Admission is one gate: the coordinator mutex with a lock-free count
`admission-gate-fast-path` · 2026-10-09 · maintainer · accepted
- **Context:** every outer access region locked the coordinator mutex on
  entry and again on exit, about 5% of a reduction-heavy workload's samples.
  `idle-entry-election-and-condvar-admission` required one coordinator
  mutex; its purpose is that no second lock can deadlock against it.
- **Decision:** the active-outer-mutator count moves from the mutex into an
  atomic word beside it, with a `COORDINATED` bit. While the phase is
  `Ordinary` and nobody waits, an outer entry is one compare-and-swap and an
  exit one decrement; otherwise both take the mutex. One module owns the
  gate and every phase change, as a specialized mutex rather than a second
  lock (maintainer). No admission decision changes: election stays idle-only.
- **Consequences:**
  - Reading the count under the mutex sets the bit, so elections and
    waiters see every entry and exit; the Loom models compile the gate
    itself.
  - Overlapping mutators can still starve collection; bounding that is
    planned separately, on top of one open heap per thread.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Regional Mutator Admission
  Invariants".
- **Recorded in:** structural overheads plan, `perf-access-region-cost`.

### A thread holds mutators for one heap at a time
`one-heap-per-thread` · 2026-10-09 · maintainer · accepted
- **Context:** a thread could hold mutators for several heaps at once. That
  nesting is what made a draining collection request too complex
  (`idle-entry-election-and-condvar-admission`): a thread waiting to enter
  one heap could hold a mutator another heap's collection waits for. Glam's
  runtimes never nest heaps, and the collector need serve only Glam.
- **Decision:** a thread holds mutators for at most one heap at a time,
  enforced in every build. A thread-local current-heap slot names the heap
  the thread entered last; preparing an entry for another heap while that
  one is active panics before any TLS record or admission. The maintainer
  suggested debug-only enforcement; the slot makes the check one comparison,
  cheap enough to keep, and also serves repeated entries without a registry
  lookup. Rejected: supporting nesting, which only tests used.
- **Consequences:** the cross-heap nesting tests and the reciprocal Loom
  model retired or became tests of the rule; no production path nested.
  A collection that holds back new entrants while it waits becomes
  possible; that is `gc-bounded-collection-wait`, still to be designed.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Regional Mutator Admission
  Invariants"; `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** structural overheads plan, `gc-one-heap-per-thread`.

### Each evaluation quantum holds one heap region
`quantum-held-region` · 2026-10-09 · maintainer and agent · superseded
- **Context:** the evaluator opened about one outer access region per
  reduction, each an admission. A bounded collection wait also needs a
  boundary where an entrant holds no lock, and the quantum boundary is one.
  The maintainer chose this direction and accepted starvation until
  `gc-bounded-collection-wait`.
- **Decision:** deferred and lazy-route task polls, client demands and
  sparks hold one region (`Heap::with_held_region`), whose value accesses
  are recursive entries. Points that wait on another thread or call out to a
  host release it early (`Heap::release_held_region`), and a released region
  is not reacquired. Rejected: reacquiring after the wait, which could block
  on admission while the waiter holds a glam mutex that a finalizer needs;
  holding regions in reflection polls, which call their host between steps.
- **Consequences:** outer regions fall about a hundredfold and instructions
  0.7 to 2.7%. A collection waits for in-flight quanta. A missed release
  point delays collection, or deadlocks if the wait depends on a collecting
  thread; the documented release points cover condvar waits, host calls,
  launchers, synchronous drivers and pressure servicing.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Regional Mutator Admission
  Invariants"; `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** structural overheads plan, `perf-quantum-region`.
- **Superseded by:** `quantum-mutator`.

### A waiting collection is bounded at quantum boundaries
`quantum-boundary-collection-join` · 2026-10-09 · maintainer and agent · accepted
- **Context:** with one held region per evaluation quantum, overlapping
  quanta could starve a collection that waits for an idle heap. The
  maintainer chose to hold back new entrants at the quantum boundary, where
  a driver holds no lock.
- **Decision:** no change to the collector's admission. A waiting
  `collect_full` already sets the heap's request, and every driver services
  pressure at each quantum boundary after its region ends, so drivers join
  the collection there rather than start another quantum. Aggressive-GC
  verification's pressure input now honors a request too. Rejected:
  draining entrants in the gate, which the convergence makes unnecessary and
  which would hold back settlement while it holds the runtime mutation gate.
- **Consequences:** under two busy workers, a collection took 5 to 15 ms;
  with joining disabled, 0.5 to 15 s. The bound is Glam's: `glam-gc` still
  elects only at an idle heap, so a driver loop must keep its boundary.
- **Rule lives in:** `agent_context/evaluation.md` "Values and Forcing";
  `crates/glam-gc/SAFETY.md` "Regional Mutator Admission Invariants".
- **Recorded in:** structural overheads plan, `gc-bounded-collection-wait`.

### Holding a heap is separate from accessing it
`explicit-heap-holds` · 2026-10-09 · maintainer · superseded
- **Context:** any `with_mutator` deep in a call stack may become an outer
  entry, so the points where admission can block on a collection are not
  enumerable. Arbitrary recursive entry serves almost no Glam use.
- **Decision:** a hold is the only admission, taken explicitly at
  boundaries, one per thread; a second hold panics. Access requires a hold
  and panics without one in every build; its token derives from the
  thread-local hold, and nested accesses are plain scope proofs.
  `Hold::suspend` is a scoped GC safepoint, valid only with no access
  open. Holds cache allocation classes.
- **Consequences:** about 80 production and 290 test sites need explicit
  holds, so the work is its own plan, after lower-hanging steps. It
  replaces `gc-two-level-mutator-access`.
- **Superseded by:** `owned-mutator-views`.
- **Recorded in:** explicit heap holds plan.

### An owned mutator per thread; access borrows it
`owned-mutator-views` · 2026-10-12 · maintainer · accepted
- **Context:** the 2026-10-10 design review of explicit heap holds, and
  the costs it would remove: about 180 instructions per heap entry and
  about 1,400 per transient root, a fifth to a third of a lazy-heavy step
  (`perf-reflection-step-cost`).
- **Decision:** adopt the review. Admission is an owned, `!Send` mutator
  per thread, taken at boundaries and branded `'mu`; access is a borrow of
  it, not counted; `suspend(&mut self)` re-admits. `GcRef<'m, T>` views
  borrow the mutator, so none outlives a suspend; `Root::as_gc` returns
  one. `LocalRoot` waits until measured. Transition aids are allowed,
  including a suspend generation in views, checked in debug builds and
  tests. Supersedes `explicit-heap-holds`; revises `scoped-gc-deferred`,
  `quantum-held-region`, `publish-before-access-ends` and
  `noauto-runtime-collection-policy` as its steps land.
- **Consequences:** about 690 access parameters gain a mutator borrow
  over time; evaluator polls need no suspend, as their release points
  move to drivers; reflection polls release or suspend around dependency
  pumping. Landed by 2026-10-15, when the explicit heap holds plan
  finished. `LocalRoot` was never built: pooled roots (`pooled-roots`)
  brought a root to about 82 instructions, and a root made from a view
  skips validation (`root-evidence`).
- **Recorded in:** explicit heap holds plan, "Decisions, 2026-10-12".

### An owned mutator is taken outermost; legacy release under it panics
`owned-mutator-outermost` · 2026-10-13 · maintainer · accepted
- **Context:** `gc-hold-api` added `Heap::mutate`, `Mutator::suspend` and
  `GcRef` beside the legacy closure token, renamed `MutatorAccess`.
- **Decision:** `mutate` panics inside any active heap entry, legacy ones
  included, except within a suspension. A legacy release point reached
  under an owned mutator panics rather than doing nothing. A suspended
  mutator's thread record stays registered. A suspension whose closure or
  re-admission unwinds leaves the mutator without access, which panics on
  use. Views check a per-thread admission generation in debug builds.
- **Consequences:** boundaries convert outside-in. During migration, a
  boundary still reached inside unconverted accesses uses the separately
  named `Heap::mutate_recursive`, a recursive mutator that cannot suspend
  when nested, and moves to `mutate` once its callers convert, so the
  names show where a transition has stabilized (maintainer). The aid
  retired on 2026-10-14, ahead of the legacy token, which retired the
  same day with its release points (`gc-hold-outer-boundaries`,
  `gc-hold-pass-access`, `no-recursive-heap-entries`).
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Regional Mutator
  Admission Invariants".
- **Recorded in:** explicit heap holds plan, `gc-hold-api`.

### Coarse-grained mutators: the budget bounds them, suspensions are packed
`coarse-grained-mutator` · 2026-10-13 · maintainer · accepted
- **Context:** reflection steps made about 30 single-access heap entries
  each, and their step loop calls client code and pumps dependencies, so a
  mutator spanning it must suspend from deep inside the machine.
- **Decision:** coarse-grained evaluation. A poll holds its mutator for its
  whole quantum, and only the evaluation budget bounds how long; adjacent
  operations that must not run under it share one suspension where that
  costs little. Local reasoning about safety and refactoring, and
  performance, matter more than the Rust expression. The poll context
  packages the mutator as a parameter, with static borrows (no `RefCell`):
  machine polls take it mutably, value access borrows it shared, and
  `suspend` needs it exclusively. Suspend only around operations with a
  long or unbounded wait on the world outside the heap, and avoid those
  where feasible (maintainer, 2026-10-13); the required suspension sites
  get a later review to reduce them.
- **Consequences:** reflection polls hold a mutator and suspend around host
  callbacks and specialization requests (started and polled under one
  suspension). Work a quantum pumps shares its mutator, and releasing it
  runs suspended, since settling may evaluate diagnostics. Host calls run
  suspended, and the same quantum consumes their result. Per `.r ()` step,
  heap entries fell from 34.6 outer and 117.6 recursive to 15.4 and 3.0;
  instructions per step 515 K to 480 K. `#[inline]` on the access wrappers
  recovered a codegen regression from the `&mut` context.
- **Rule lives in:** `docs/architecture/evaluation.md`;
  `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** explicit heap holds plan, `gc-hold-lend-mutator`.

### No heap entry nests; capabilities pass from caller to callee
`no-recursive-heap-entries` · 2026-10-14 · maintainer · accepted
- **Context:** after `driver-mutator`, most heap entries borrow the
  driver's mutator, but code can still open a legacy access or take a
  boundary's mutator inside another entry, as a recursive entry. glam-gc
  supports that with an entry depth in each thread record and an
  outer-or-recursive branch in every entry path.
- **Decision:** in the end state glam-gc has one entry, `Heap::mutate`,
  outermost only, with `Mutator::suspend` and `readmit`. Glam's boundaries
  take the mutator, and reaching one under a mutator is a bug. Interior
  code takes access, which cannot suspend, or `&mut RuntimeValueMutator`,
  which may, from its caller. The transition aid `Heap::mutate_recursive`
  goes first, then the legacy outer entry, then the legacy recursive one.
- **Consequences:** suspension safety and heap use show in signatures and
  are checked by the borrow checker, where a nested entry relied on a
  runtime panic in `Mutator::suspend`. Values can travel between steps as
  views, which a nested entry would have to root. Entry cost was not the
  reason: the profiling workloads make 17 to 23 recursive entries a run
  outside one per definition. Each public `Values` call is a boundary of
  its own: `gc-hold-values-api` closed on 2026-10-15 without a client scope
  that holds one mutator across calls (maintainer), which would run client
  code under the mutator. Nor does Glam find its runtime through the
  thread's mutator: `gc-hold-thread-context`, which would have, was skipped
  (maintainer, 2026-10-15), since code takes `&` access or a `&mut`
  mutator from its caller and its signature shows whether it may suspend.
  Landed 2026-10-14
  (`gc-hold-pass-access`): `Heap::with_mutator` and held regions went, a
  thread record's depth is 0 or 1, and `Heap::thread_holds_mutator`
  answers whether a thread may wait. A managed destructor may not enter
  the heap on its own thread, which would nest the finalizer's entry; it
  hands such work to another thread.
- **Rule lives in:** `agent_context/evaluation.md` "Values and Forcing";
  `crates/glam-gc/SAFETY.md` "Regional Mutator Admission Invariants".
- **Recorded in:** explicit heap holds plan, "Decisions, 2026-10-14".

### A quantum boundary releases under the mutator
`boundary-release-under-mutator` · 2026-10-14 · maintainer · accepted
- **Context:** after `driver-mutator`, a driver suspended its mutator for
  each quantum boundary's whole release, about five times per `.r ()` step.
  Most of the release is internal: coordinator transitions, terminal
  publication, route-lease release. A failed task's status diagnostic was
  built by a driver holding its own mutator, which needed the suspension.
- **Decision:** the release runs under the driver's mutator, suspending
  only to run deferred work, join a requested collection, run client code
  (status callbacks, machine cancellation and disposal) or wait.
  Diagnostics a terminal result needs are built under the mutator, which is
  passed to the code that needs it. Rejected: keeping the whole boundary
  suspended, a single safepoint costing about 300 instructions.
- **Consequences:** the coordinator lock and the runtime gate are taken
  under a mutator. That cannot deadlock on admission, since a synchronous
  collection admits entries until no mutator is active; nobody may call
  `collect_full` while holding them. Code the release reaches takes access
  from its caller instead of opening a boundary. A release expected to
  change a published task status runs suspended, under one gate with its
  transition, so a runtime observed stable has published every blocked
  state (corrected 2026-10-15).
- **Rule lives in:** `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** explicit heap holds plan, `gc-hold-suspension-review`,
  `gc-hold-boundary-release`.

### Drivers hold the thread's mutator across quanta
`driver-mutator` · 2026-10-14 · maintainer · accepted
- **Context:** converting Glam's boundaries left a choice for code that
  drives evaluation: hold a mutator across the drive and suspend at each
  wait, or drive without one and take short mutators for access phases.
  Measured: single-threaded runs never block on a condvar, plain evaluation
  suspends nowhere, and a `.r ()` step suspends 5.1 times, each releasing
  pumped work; entries and suspensions are 0.1 to 1.3% of instructions.
- **Decision:** a driver takes the thread's mutator at its outermost
  boundary, and the quanta it runs borrow it, as pumped work already does.
  A wait suspends the mutator before taking its lock, and a quantum
  boundary suspends it only to join a requested collection. Entry cost is
  the lesser reason: a threaded mutator lets views carry values between
  steps instead of roots, and `&Mutator` against `&mut Mutator` marks which
  code may suspend, a local capability for reasoning about suspension.
  Rejected: driving without a mutator.
- **Consequences:** a collection still waits for at most one quantum, and
  memory is unchanged. Drivers, the pump entry points and the coordinator's
  claimed polls take the mutator as a parameter. Quanta may grow later; 64
  steps is fine-grained, small only while steps are costly. Release between
  quanta runs client code and, for failed statuses, nested drivers, so each
  quantum boundary is one suspension until `gc-hold-suspension-review`. A
  driver that catches a poll's panic re-admits its mutator
  (`Mutator::readmit`), since the panic may have unwound out of one of the
  poll's suspensions.
- **Rule lives in:** `docs/architecture/evaluation.md`;
  `agent_context/evaluation.md` "Values and Forcing".
- **Recorded in:** explicit heap holds plan, `gc-hold-boundaries`.

### Root slots are reclaimed apart from heap collections
`root-chunk-reclamation` · 2026-10-14 · maintainer · accepted
- **Context:** under `pooled-roots`, dropped slots returned only at a heap
  collection, so a runtime heap that rarely collects grew its root pool with
  every transient root: 59 to 98 chunks at the end of the profiling runs.
  The maintainer asked to compare freeing on release against reclaiming in
  bulk, and to reclaim root chunks apart from heap collections.
- **Decision:** a thread leases the chunk whose free list it took until it
  takes the next. When no chunk has a free list, the pool rebuilds the lists
  of its unleased chunks from their zero-count slots, while mutators run,
  once it has handed out as many slots since the last scan as that scan
  found live; otherwise it allocates a chunk. Collections still rebuild
  every list and end every lease. The admitted thread's list lives in a
  thread-local `Cell`. Rejected: freeing a slot on its root's final drop,
  which saved 4 instructions per root in a loop benchmark and cost 1% per
  reflection step, since every drop reads its chunk header.
- **Consequences:** the pool stays near twice the live roots (2 to 3
  chunks at the end of the profiling runs), and instruction counts match
  `pooled-roots`. Scanning costs about two slot visits per slot handed out.
  A thread record dropped while it holds a lease leaves the chunk leased
  until the next collection.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "`root::Root<T>`
  construction and access".
- **Recorded in:** explicit heap holds plan, `gc-hold-pooled-roots`.

### Roots are counted slots in heap-owned pools
`pooled-roots` · 2026-10-14 · maintainer · accepted
- **Context:** after `root-evidence`, a root still cost about 527
  instructions to create and drop: 61% malloc and free of its `Arc` cell,
  whose frees waited for a collection, 13% the registry push under the
  heap's data mutex, 8% reference counts, one of them a weak count on the
  heap that every thread shares, and 6% the collector's registry walk.
  Carrying values without roots (`gc-hold-local-roots`) would have avoided
  this only for roots that die within their admission (maintainer: make
  roots as cheap as possible first).
- **Decision:** a root is a pointer to a slot, an atomic count and one word,
  in a heap-owned chunk of 255 slots aligned to its 4 KiB size. Clone and
  drop touch only the count. A thread takes one chunk's whole free list into
  its cache and creates roots from it without a lock or shared atomic.
  Nobody creates roots during a collection (maintainer), so each collection
  rebuilds every chunk's free list from its zero-count slots, lowest address
  first, just before publishing the next allocation lease epoch, which
  discards the lists threads took. A free slot's word links the list. A
  heap dropped with live roots orphans their chunks to a process-wide list
  that later heap drops sweep, so a root adds no atomic for outliving its
  heap (maintainer); its chunk header holds a never-reused heap identity.
  Registration counts move to per-thread region metrics. Built in-crate for
  control over layout and reclamation (maintainer). Local roots are
  shelved.
- **Consequences:** a root costs about 82 instructions, 20 of them zeroing
  new chunks. A dropped slot waits for a collection to be reused (until
  `root-chunk-reclamation`), and a chunk with an escaped root outlives its heap until a later heap drop
  finds it unreferenced, or until exit. Concurrent marking would need the
  rebuild to stay in a phase that excludes root creation.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "`root::Root<T>`
  construction and access" and "`root_pool` chunks and slots".
- **Recorded in:** explicit heap holds plan, `gc-hold-pooled-roots`.

### Each root construction states its liveness evidence
`root-evidence` · 2026-10-13 · maintainer · accepted
- **Context:** every root construction validated heap ownership,
  representation and the allocation bit in every build, about 790 of a
  registration's 1,130 instructions. Glam's root sites root edges they
  have just dereferenced unchecked under the same access, where the check
  proves nothing new. Validation-free release builds measured -8.8% per
  reflection step and -4 to -9% on lazy-heavy workloads. With immediates
  in `Value`, a pointer will be told apart by its tag, so some proofs must
  rest on provenance rather than type.
- **Decision:** a root names its evidence at construction. A view
  (`GcRef`) is proof: `MutatorAccess::root` takes one and skips
  validation in release builds; debug builds validate when a view is
  made. Fresh allocations return views (`Allocator::alloc_view`). Glam
  views a detached edge through one function whose `EdgeLiveness`
  argument names its provenance claim, so each claim can be found and
  audited; it costs nothing at run time. A bare `Gc` roots through
  `root_checked`, validated in every build.
- **Consequences:** a false provenance claim roots a dead edge, which is
  undefined behaviour in release builds rather than a panic; debug builds
  and tests still validate. The same evidence argument may later extend
  to Glam's unchecked edge reads (`get_edge`, `duplicate_edge`), when
  immediates make their claims more varied.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "`root::Root<T>`
  construction and access".
- **Recorded in:** explicit heap holds plan, `gc-hold-root-evidence`.

### Each evaluation quantum holds the thread's mutator; host work runs after it
`quantum-mutator` · 2026-10-13 · agent · accepted
- **Context:** `owned-mutator-views` replaces held regions with owned
  mutators, under which a release point panics.
- **Decision:** quanta hold a mutator instead of a held region
  (`EvaluationPollContext::with_quantum_mutator`), through
  `Heap::mutate_recursive` while some quanta still run inside another
  access. Work that waits on another thread or calls out to a host is
  deferred with `after_admission` and runs once the mutator ends, before
  the driver releases its claim: reflection activations and the net
  contention wait (host calls too, until `coarse-grained-mutator` let them
  suspend instead). A poll with deferred work ends its quantum.
  Nested synchronous drivers do not run inside quanta; test sources that
  did now read their inputs as evaluator steps or run as host calls.
  Supersedes `quantum-held-region`, whose release points no longer exist
  inside quanta.
- **Consequences:** a release point reached inside a quantum panics instead
  of silently ending admission. Reflection polls joined under
  `coarse-grained-mutator`.
- **Recorded in:** explicit heap holds plan, `gc-hold-defer-release-points`
  and `gc-hold-quantum-mutator`.

### Clear mark bitmaps before marking
`clear-before-mark-bitmaps` · 2026-08-22 · agent · accepted
- **Context:** the collector's correctness surface should stay small.
- **Decision:** clear marks before marking. No colors, journal or
  allocation-time marking.
- **Consequences:** mark cost includes the clearing pass.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Mark Attempt Invariants".
- **Recorded in:** collector implementation plan, post-C3E review.

### An irreversible collector invariant failure poisons the heap instead of aborting
`poison-heap-instead-of-abort` · 2026-08-24 · agent · accepted
- **Context:** a panic during sweep or finalization leaves uncertain state.
- **Decision:** poison the heap permanently.
- **Consequences:** Rust resources may leak, but no destructor is retried on
  uncertain state.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "GC6-002A topology
  irreversibility and permanent poison".
- **Recorded in:** C6 review GC6-002.

### Collection pressure is proportional to survivors
`survivor-proportional-pressure-target` · 2026-08-22 · agent · accepted
- **Context:** a fixed collection threshold either collects too often for a
  large live heap or too rarely for a small one.
- **Decision:** after a successful collection, publish the assigned-run count
  `S` and the target `S + 112 + ceil(S / 2)` runs. A failed mark or
  pre-publication sweep keeps the prior baseline.
- **Consequences:** collection frequency scales with what survives. The
  one-half ratio, the 112-run floor, and run size were never measured; the
  value-representation plan owns that measurement.
- **Rule lives in:** `crates/glam-gc/SAFETY.md` "Worker-Local Allocation-Word
  Invariants".
- **Recorded in:** collector implementation plan, C2C.

### Terminal heap destruction supplies no mutator
`terminal-destruction-without-mutator` · 2026-08-24 · agent · accepted
- **Context:** the last heap owner may drop on any thread, possibly while
  unwinding, with no collector capability left to lend.
- **Decision:** terminal destruction supplies no mutator and traces no
  pending payloads. It visits detached finalization runs, then attached
  class runs, and propagates the first destructor panic.
- **Consequences:** every managed representation must be safely droppable
  without heap capability. A destructor that panicked during finalization
  retires only its own identity and is never attempted again.
- **Rule lives in:** `crates/glam-gc/SAFETY.md`, the terminal-destruction
  contract.
- **Recorded in:** collector implementation plan, C6D.1–C6D.2; C6 review.

### Plain mark stack; no paged range tracing
`plain-mark-stack-no-paged-tracing` · 2026-10-03 · agent · accepted
- **Context:** a fan-out of 1M edges needed a 16 MiB worklist.
- **Decision:** keep the plain `Vec<TraceWork>` mark stack.
- **Consequences:** reopen only together with a real contiguous managed
  container and fresh measurements.
- **Rule lives in:** `crates/glam-gc/VERIFY.md` "C8 collector measurements",
  including the reopen condition.
- **Recorded in:** C8 review C8B.3; commit `afa3d963`.

## Superseded decisions

These decisions were superseded before this log began, so they have no
entries of their own. Each successor is named by slug.

| Superseded decision | Decided | Superseded by | On | Recorded in |
| --- | --- | --- | --- | --- |
| Aggressive verification collects before every eligible outer entry (glam-gc's pre-entry hook) | 2026-09-11 · agent | `aggressive-gc-via-stable-pump` | 2026-10-03 | aggressive-GC remediation plan; GC integration plan I11D.1 |
| Settlement validation compares the maintenance revision | 2026-10-02 · agent | `settlement-ignores-maintenance-revision` | 2026-10-03 | readiness review; explicit-maintenance review |
| Reflection lazies keep an external stable-task-observation sidecar | 2026-09-10 · agent | `reflection-tasks-publish-via-completion-promise` | 2026-09-19 | I5 review GCI5R-005 |
| Keep exact brittle inventories through Gate G3 | 2026-10-01 · agent | `transition-negative-tests-not-inventories` | 2026-10-03 | D2h review D2HR-005 |
| New runtimes construct `Automatic` collection by default (recommended, never shipped) | 2026-08-25 · agent | `noauto-runtime-collection-policy` | 2026-10-02 | 2026-08-25 integration review GCI-015 |
| Add a front-end no-panic fuzz target in P0 (recommended) | 2026-10-03 · agent | `panic-discovery-by-inspection` | 2026-10-04 | holistic review P0-2 |
| Run the `full` check level periodically | 2026-10-03 · agent | `aggressive-gc-rerun-triggers` | 2026-10-05 | holistic review X2; parallel review AR-004 |

## History docs cited

Short names used under **Recorded in**, with their files under `docs/`.

| Short name | File |
| --- | --- |
| holistic review | `reviews/HolisticArchitecturePrePerformance_2026-10-03.md` |
| parallel review | `reviews/ArchitectureAndVerification_2026-10-03.md` |
| documentation disposition | `plans/DocumentationDisposition_2026-10-05.md` |
| panic-safety plan | `plans/UserInputPanicSafety_2026-10-04.md` |
| polarity plan | `plans/NetPolarityChecker_2026-10-05.md` |
| resumable-WHNF plan | `plans/ResumableWhnfEvaluation_2026-09-12.md` |
| resumable-WHNF holistic review | `reviews/ResumableWhnfHolistic_2026-09-28.md` |
| W5 review | `reviews/ResumableWhnfW5_2026-09-15.md` |
| W6G1 design review | `reviews/ResumableWhnfW6G1Design_2026-09-19.md` |
| W6G4 review | `reviews/ResumableWhnfW6G4_2026-09-23.md` |
| W7 review | `reviews/ResumableWhnfW7_2026-09-27.md` |
| callable-spill plan | `plans/InteractionNetCallableWhnfSpill_2026-09-16.md` |
| pure-construction plan | `plans/PureInteractionNetConstruction_2026-09-20.md` |
| PNC4 review | `reviews/PureInteractionNetConstructionPNC4_2026-09-20.md` |
| GC roadmap | `plans/GarbageCollectionRoadmap_2026-08-19.md` |
| collector implementation plan | `plans/GarbageCollectorImplementation_2026-08-19.md` |
| GC integration plan | `plans/GarbageCollectorIntegration_2026-08-19.md` |
| ownership ledger | `plans/GarbageCollectorOwnershipLedger_2026-08-20.md` |
| persistent-edge plan | `plans/GarbageCollectorPersistentEdgeTraits_2026-09-12.md` |
| scoped-pointer plan | `plans/GarbageCollectorScopedPointerSafety_2026-09-09.md` |
| aggressive-GC remediation plan | `plans/GarbageCollectorAggressiveVerificationRemediation_2026-09-11.md` |
| aggressive-GC regression plan | `plans/GarbageCollectorAggressiveVerificationRegression_2026-10-03.md` |
| concurrent-GC plan | `plans/ConcurrentGarbageCollection_2026-08-28.md` |
| VRR plan | `plans/ValueRepresentationRefinement_2026-08-19.md` |
| performance roadmap | `plans/PerformanceRoadmap_2026-10-05.md` |
| evaluation-recursion plan | `plans/EvaluationRecursionPerformance_2026-10-04.md` |
| structural overheads plan | `plans/StructuralOverheads_2026-10-08.md` |
| fast checks plan | `plans/FastChecks_2026-10-09.md` |
| explicit heap holds plan | `plans/ExplicitHeapHolds_2026-10-09.md` |
| lazy diagnostics plan | `plans/LazyDiagnostics_2026-10-15.md` |
| parser backtracking plan | `plans/ParserBacktrackingPerformance_2026-10-04.md` |
| grammar review | `plans/GrammarReview_2026-10-16.md` |
| tag prefix plan | `plans/TagPrefixSyntax_2026-10-16.md` |
| syntax type errors review | `reviews/SyntaxTypeErrors_2026-10-17.md` |
| runtime API review | `reviews/RuntimeApi_2026-10-17.md` |
| runtime API plan | `plans/RuntimeApi_2026-10-17.md` |
| C2C review | `reviews/GarbageCollectorC2C_2026-08-22.md` |
| C6 review | `reviews/GarbageCollectorC6_2026-08-24.md` |
| C8 review | `reviews/GarbageCollectorC8_2026-10-03.md` |
| 2026-08-25 integration review | `reviews/GarbageCollectorIntegration_2026-08-25.md` |
| I5I10 review | `reviews/GarbageCollectorIntegrationI5I10_2026-09-03.md` |
| I5 review | `reviews/GarbageCollectorIntegrationI5_2026-09-07.md` |
| I8 review | `reviews/GarbageCollectorIntegrationI8_2026-09-10.md` |
| opaque-representation review | `reviews/GarbageCollectorOpaqueRepresentation_2026-09-11.md` |
| D2h review | `reviews/GarbageCollectorAggressiveD2h_2026-10-01.md` |
| GCI11R002 holistic review | `reviews/GarbageCollectorGCI11R002Holistic_2026-10-01.md` |
| I13 cleanup inventory | `reviews/GarbageCollectorI13CleanupInventory_2026-10-02.md` |
| G4 review | `reviews/GarbageCollectorGateG4_2026-10-02.md` |
| readiness review | `reviews/GarbageCollectorReadinessIntegration_2026-10-02.md` |
| explicit-maintenance review | `reviews/GarbageCollectorExplicitMaintenance_2026-10-02.md` |
| RuntimePolicy review | `reviews/GarbageCollectorRuntimePolicy_2026-10-02.md` |

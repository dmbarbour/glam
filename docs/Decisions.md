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
  plan's `polarity-net-fuzzing`) follows this policy.
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
  now sets its cadence.
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
- **Recorded in:** structural overheads plan, `perf-list-front-walk`.

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

## Diagnostics

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

## Interaction nets

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
    Rust stack. The deep-nesting remedy belongs to the panic-safety plan.
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
- **Recorded in:** aggressive-verification remediation plan.

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
`scoped-gc-deferred` · 2026-10-01 · agent · accepted
- **Context:** an unbranded `Gc<T>` can outlive its region; today's proof
  rests on audit.
- **Decision:** defer branding. Adopt it only after reproducing a concrete
  defect.
- **Consequences:** no concurrent or moving readiness is claimed.
- **Rule lives in:** `architecture/evaluation.md` "WHNF Submachine Flow".
- **Recorded in:** D2h review D2HR-004; scoped-pointer plan SP0.

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

# Parser Backtracking Performance Plan — 2026-10-04

Status: design agreed with the maintainer on 2026-10-06. `parser-term-parser`
is in production since 2026-10-07 (see [Production switch](#production-switch-2026-10-07));
`parser-patterns`, `parser-keyword-frames` and `parser-grammar-retirement`
remain, and `parser-cover-ir` is done only for expressions (see
[Steps](#steps)). Rechecked 2026-10-15 (`01ec2fbc`): no parser step had
landed since `adcd994b` (2026-10-07), and a keyword form in parentheses
nested in another was exponential ([Measures 2026-10-15](#measures-2026-10-15)).
Keyword regions, the first part of `parser-keyword-frames`, remove that
exponential and the whole-source scans; what remains, and a proposed order
for it, is in [Keyword regions 2026-10-15](#keyword-regions-2026-10-15).
This is the `perf-parser` step of the
[performance roadmap](PerformanceRoadmap_2026-10-05.md). The fix is a merged
grammar with prefix sharing, parsing into a cover syntax (IR) for patterns and
expressions, implemented with an explicit stack; see [Design](#design). It
replaces the lookahead guards first proposed here. It was found during the
[user-input panic safety](UserInputPanicSafety_2026-10-04.md) parser
inspection.

## Problem

Parse time grows exponentially with the nesting depth of parentheses and
lists. Release-build timings for `x = ((…1…))` with `--parse`, measured
2026-10-04 under the Chumsky grammar (historical; the term parser made the
first two rows linear):

| Form | Growth per level | Measurements |
| --- | --- | --- |
| Parentheses | ~3× | depth 12: 3.9 s; depth 13: 12 s; depth 14: 38 s |
| Lists | ~2× | depth 18: 2.8 s; depth 20: 11 s |
| Dictionaries | linear | depth 10,000: 0.14 s |

Realistic source nests 5–8 levels, so ordinary code already pays a
measurable share of this cost. It likely explains part of the slow compile
times recorded by the holistic review (F3 and its 60-second `g_syntax` unit
test).

## Investigation 2026-10-06

### Survey

A survey measured `glam --parse` in release mode for every construct, nested
in itself, with a 10 s timeout. Per-level growth bases are measured; the
causes below are read from the code.

| Construct | Growth |
| --- | --- |
| `( )` grouping and anything inside a paren: `f (…)`, `1 + (…)`, `a:(…)`, `(\z -> …)`, `(if …)`, `(do {…})`, `f (match …)` | ×3 per level |
| `[ ]` lists; postfix sections `((1 +) +)` | ×2 per level |
| A paren as a dict member, `{(…)}` | ×4 per level |
| Alternating `([`, `({a:`, `[{a:[` | ×6, ×3, ×4 per cycle |
| `do { <nested do> -> z; .r z }` | ×2 per level |
| A forward bind containing parens, as in guards or `f (do …) -> z` | ×6 per level |
| Patterns `(P) as q` and `(P) ++ [q]` | ×3 per level |
| Pattern `((P) as q)` | ×9 per level |
| A long line of braced `do` or `match` members; a list of `if` members | ~n² |
| A file of `do` or `if` declarations | ~n² and ~n^1.4 |
| A flat infix chain or application of 32k terms; an if-else chain 4k deep | Rust stack overflow |

**Linear:**
- Expressions: dict values; lambdas; tagged and computed tags; tuples that
  do not lead with a group.
- Blocks and objects: `match`, `do` and `object` bodies; `let`, `where`,
  `with`.
- Patterns without the `(P) as`/`++` shapes: list, dict, tuple, view,
  predicate, `when`.
- Flat sequences up to 128k items.

Patterns add no new growth base; they expose the expression grammar's.

### Causes

1. **Alternatives that fail only after the group closes**
   (`parser/expression.rs`). A paren group is parsed three times: by
   `computed_tagged`, then `postfix_operator_section`, then
   `grouped_or_trailing_tuple`. A bracket group is parsed twice, and a paren
   as a dict member four times, through `data_path`. Structural forms reached
   from Chumsky rerun their whole parse on each retry.
2. **Hand-written trial-then-fallback.**
   - `do_expr.rs::parse_statement` parses `op -> P` as a whole expression,
     then parses `op` again.
   - `pattern.rs::parse_guard_clause` does the same.
   - `parse_predicate_pattern` parses prefixes of every parenthesized
     pattern group as expressions, so it inherits cause 1.
3. **Linear scans per structural atom**, read from the code and not
   measured one by one. Together they give the quadratic rows:
   - `structural_view_at` and `conditional_hard_end` iterate over every
     delimiter group in the file;
   - cursor lookups use linear `position` scans;
   - indentation walks back to the start of the line;
   - floor validation rescans nested views.

The token-tree layers are already linear: `TokenView::top_level` jumps over
a nested group through its paired close token in constant time, and the
pattern parser splits at top-level symbols before parsing the sides.

### Options

- **A. Lookahead guards.** This is the Fix section below. Each doomed
  alternative checks, in constant time, the token after the group's close
  or the token before it. It removes every row from cause 1, but not causes
  2 and 3, and nothing stops a new alternative from reintroducing the
  problem. It is cheap.
- **B. Memoization.** Chumsky's `memoization` feature and `.memoized()`
  cache a rule's result per input position. Alternatively, our own cache
  keyed by token range and context could sit at the view-parse entry points,
  so every repeated "parse this view as X" becomes constant time. That
  bounds causes 1 and 2, but costs memory, leaves the duplicated attempts in
  place, and does nothing for the stack or cause 3.
- **C. Groups as cut points.** Left-factor so that each group's role is
  chosen by bounded lookahead around it, before its contents are parsed. A
  parse never re-enters a completed group; a group boundary acts as a cut.
  Split `op -> P` at its boundary first, as patterns already do. This is A
  made into a grammar rule.
- **D. Token-tree parsing with an explicit stack.** Apply C throughout.
  - Parse every delimiter group exactly once, in the role chosen for it,
    from an explicit worklist over the delimiter tree. A parent sees each
    nested group as one atom.
  - Inside a group, parse operators and applications with an iterative
    precedence parser using an explicit operator stack. This also removes
    the flat-chain overflows. Keyword nesting without delimiters, such as
    if-else chains, needs the same iterative treatment.
  - This gives linear time, no Rust-stack recursion across groups, and
    removes the whole-file scans of cause 3, since every view knows its
    group.
  - It is a substantial rewrite of the expression layer, about 5k of the
    parser's 11k lines. Chumsky could stay for flat sequences, or go.
- **E. A CPS parsing machine.** Hand-written, with an explicit continuation
  stack and `alt` and `cut`, so backtracking is general but delimited at
  chosen boundaries. It replaces Chumsky.
  - It is the most general option: any grammar with bounded, cut-delimited
    backtracking, and no Rust recursion.
  - It mirrors Glam's own choice effects (`.alt`, `.cut`), the natural model
    for the user-defined, monadic front ends in `Design.md`.
  - It is the most machinery, and a missed cut silently restores
    super-linear corners.

### Assessment

D is C applied systematically, and E is the general engine of which D is a
deterministic special case: cuts at every group boundary, and choices made by
bounded lookahead. The `.g` grammar is already designed to be decidable this
way; the hand-written layers show it. That suggests:
- **The bootstrap parser** takes D: groups are parsed once, group boundaries
  are cuts, and an explicit stack replaces recursion.
- **General `alt`/`cut` backtracking** belongs to the Glam-level parser
  library for macros and user-defined syntax. There, Glam's choice effects
  already supply the cut semantics.
- **A, B or both** could serve as a stopgap if D is scheduled later. Their
  value disappears once D lands.

**Maintainer direction, 2026-10-06:**
- Avoid fragile lookahead; option A is rejected, including as a stopgap.
- Think of the fix as merging parsers with prefix sharing.
- Patterns and expressions share a cover syntax, an IR not decided until
  context fixes its role.

The pattern parser shows why patterns added no new cause: it finds a
top-level `<-` or `->` first, then parses each side once. It falls back to
trial and error only where that split is ambiguous: several arrows,
predicate patterns, and `op -> P` statements.

### Incidental findings

- `(\w -> match w with { z => w })` fails with "structural expression
  extends beyond its token range".
- `(1, match y with {…})` is rejected.

Both are possibly parser bugs unrelated to speed.

**Fixed 2026-10-06.** Both were the structural layer claiming a `match`'s
own `with` as a record update.
- `find_structural_body` now follows the documented rule that the first
  ungrouped `with` after a `match` or `try_match` head belongs to it. That
  covers lambda bodies and other non-leading positions.
- A parenthesized group that leads with `match` and holds a top-level comma
  is a tuple. The expression grammar parses it and delimits the member.
- **Still unsupported:** a `with` update as a list or tuple member, as in
  `[d with {…}, 2]`. Only the structural layer knows `with` updates, and it
  sees whole views. `parser-keyword-frames` moves keyword forms into the
  parser machine.

## Design

### Principles

- **Prefix sharing.** Alternatives that begin alike are one production.
  The shared prefix is parsed once, and the parser then branches on the
  token it must consume next. Nothing is parsed twice and nothing is tried
  and abandoned, so every delimited group is parsed exactly once. The only
  lookahead is the next token after a shared prefix, as in any LL(1)
  parser; nothing peeks past a group.
- **One cover syntax for patterns and expressions.** The parser builds a
  term in a cover IR covering what the two share. When a token fixes the
  role (`=>`, `<-`, `=`, `->`, an argument position), a conversion turns
  the term into a `SyntaxPattern` or a `SyntaxExpr`. A term the role cannot
  accept is a precise error, such as "`+` is not allowed in a pattern".
- **An explicit stack.** A deterministic parser is a stack machine. Groups,
  pending operators and open keyword forms are frames on a heap stack, so
  source nesting never becomes Rust recursion. Conversion and later
  syntax-tree walks are iterative as well.

### The cover IR

The IR covers the overlap between patterns and expressions:
- names, including `_` and `_name`;
- number, text and embedded literals;
- quoted paths;
- unit, grouping, tuples and operator sections;
- lists with `++` segments, and dicts with punned (`:x`), optional (`x?:`)
  and remainder members;
- static, computed and path tags;
- juxtaposition, infix operator chains, `as`, `when`, and the view arrows
  `->` and `<-`.

Forms with an expression-only head become expression terms directly:
lambdas (`\`), `if`, `match`, `try`, `do`, `let`, `using`, `object`, and
effects (`.op`). Converting one to a pattern fails, except where a pattern
holds an expression: a view's function or a predicate.

**Resolved by conversion, not by search:**
- A pattern `(f x -> P)` is a view of the expression `f x`.
- A pattern `(P <- f x)` is a view written the other way round.
- A predicate pattern such as `(Prefix "x-" rest)` is one juxtaposition
  term. In pattern role its last item is the pattern, and the rest is the
  predicate application.
- `a:b` is a tagged value or a tag pattern, and `{x:P, rest}` a dict literal
  or a dict pattern, by role.
- A `do` statement parses one term up to its terminator:
  - a top-level `<-` makes the left side a pattern and the right an
    operation;
  - `->` makes the left an operation and the right a pattern;
  - `=` makes the left a binding pattern;
  - otherwise the term is an operation.

### Operators and keyword forms

- Infix chains, applications and sections use an iterative precedence
  parser with an operator stack. This removes the overflows on long flat
  chains.
- Keyword forms are productions with frames, not trial parses with
  extents. These are `if`/`then`/`else`, postfix `if`, `match` and `try`
  arms, `let`, `where`, `with`, `object`, and `do` blocks. Layout blocks
  end where the lexer's indentation facts say, much as a closing delimiter
  ends a group.
- The structural layer's whole-file scans go (cause 3): a view knows its
  enclosing group, and line indentation is precomputed.

### What stays

- **Kept:** the lexer, delimiter pairing, declaration staging, macro
  expansion, and the `SyntaxExpr`/`SyntaxPattern` syntax trees, so lowering
  is untouched.
- **Chumsky:** the new parser does not use it. Simple declarations may keep
  it, or move to the same machine once it exists.

### Diagnostics

A deterministic parser reports at the first token that fits no production,
with the expected set of the current frame. This should be at least as
precise as today's merged expectations from abandoned alternatives. Every
diagnostic change in the invalid-syntax samples is reviewed, not
rebaselined blindly.

## Steps

Steps are referred to by name; see the plans README, "Step names".

1. **Cover IR and conversion** (`parser-cover-ir`). The IR types; conversion to `SyntaxExpr`
   and `SyntaxPattern`; unit tests of the role rules.
2. **Term parser** (`parser-term-parser`).
   - The explicit-stack, prefix-shared parser for atoms, groups,
     collections, tags, operators and applications.
   - Expression leaves use it first.
   - **Differential oracle:** the old parser stays and must produce the
     same syntax tree for every sample, every test source, and generated
     programs.
3. **Patterns** (`parser-patterns`). Pattern positions use the same parser plus conversion.
   This retires the split-and-retry code for views, predicates, guards and
   `op -> P`.
4. **Keyword forms and structural scans** (`parser-keyword-frames`). Move keyword forms into the
   machine, remove the whole-file scans, and retire Chumsky from
   expressions.
5. **Retirement** (`parser-grammar-retirement`). Delete the old expression and pattern parsers once the
   oracle has held across the corpus. Keep the differential corpus as
   regression tests.
6. **Nested scope analysis** (`parser-scope-analysis`; added 2026-10-15,
   the maintainer placing it with the parser steps). Name analysis is
   superlinear in `where` nesting: `chain_where` grows as n^1.76 with no
   collections. At 1,600 nested definitions, about 520 M of 1,411 M
   instructions exceed linear: the structural parser's top-level token
   scans 245 M (`parser-keyword-frames`), `mark_used_locals` and
   `NestedLocals::bind` about 160 M, name comparisons 71 M, and binding-view
   parsing the rest. Make analysis linear in nesting depth.

`parser-cover-ir` and `parser-term-parser` are additive and low-risk:
production keeps the old parser until the oracle agrees.

**Step status 2026-10-15** (`01ec2fbc`):
- `parser-cover-ir`: partly done, inside `parser-term-parser` (`8d36263e`,
  2026-10-06). Each group is interpreted once into a `Cover` in
  `parser/term.rs`, but covers hold `SyntaxExpr` items and convert only to
  expression roles. The shared pattern-and-expression IR, conversion to
  `SyntaxPattern`, and covers below group level were not built;
  `parser-patterns` needs them.
- `parser-term-parser`: done; in production since 2026-10-07
  (`parser-term-parser-first`, `eb84587c`; `parser-keyword-delegation`,
  `adcd994b`).
- `parser-patterns`: not started.
- `parser-keyword-frames`: first part built 2026-10-15 on its branch:
  keyword regions, the lexer's token tables and a lazy keyword search
  ([Keyword regions 2026-10-15](#keyword-regions-2026-10-15)). Native
  keyword frames remain.
- `parser-scope-analysis`: added 2026-10-15; not started.
- `parser-grammar-retirement`: not started. Chumsky still parses the
  views the term parser reports as unsupported, and runs as the oracle in
  tests.

**Progress 2026-10-06.** The core of `parser-term-parser` is in
`parser/term.rs`; it is test-only so far.
- **How it works:**
  - an explicit stack of open groups;
  - each group's contents interpreted once into a role-neutral cover;
  - covers held in an arena and taken exactly once by the role that
    consumes them;
  - a flat per-group interpreter, with pending lambdas as a stack.
- **Covered:**
  - atoms and literals;
  - unit, grouping, tuples and sections;
  - lists and dicts, including leading and trailing separators;
  - tags, constructors, path suffixes, quoted literals, effects and
    escapes;
  - applications, infix chains and lambdas;
  - embedded data;
  - line breaks: group padding, line-led application arguments and tail
    lambdas, and line-led operators with their resumption anchors. Anchors
    are checked against the caller's `ExpressionContext`, so the chains
    compare equal to the grammar's, anchor included.
- **Not covered yet** (reported as unsupported): keyword-headed forms.
- **The oracle:** `expression::term_oracle` re-parses every expression the
  Chumsky grammar parses while a test enables it. It reports an empty
  disagreement list over:
  - 232 edge-case expressions, including line-break layouts;
  - 801 expressions from the samples;
  - 66 generated nested expressions.

  Deliberate mutations are caught: reversed tuple items, and dropped
  resumption anchors (10 disagreements). In the samples, 83 expressions
  are still unsupported, all keyword-headed.
- **One grammar fix.** The oracle found that the Chumsky grammar rejected a
  prefix section whose close paren sits on its own line, as in
  `(+ 1⏎ )`. Every other parenthesized form accepts that layout; the prefix
  section alone lacked trailing padding. The grammar now pads it, and an
  expression test keeps the case.
- **Wider oracle reach:**
  - **A sweep.** `GLAM_TERM_ORACLE=1 cargo test --lib` checks every
    expression that any library test parses, and panics on the first
    disagreement. Across all 1,886 tests it found none.
  - **Seeded generated programs:**
    - token soup (10,000 per run) checks that both parsers accept and
      reject the same inputs;
    - grammar-directed expressions (800 per run) check that both build the
      same tree.

    `GLAM_TERM_FUZZ=<n>` multiplies both counts for a deeper local search.
- **Rules that generation found:**
  - `and` and `or` start an argument when they are a tag's key, as in
    `f and:a`. The term parser mirrors this.
  - A trailing `and` or `or` that is joint after `.`, `'` or `:` belongs to
    the expression. `(:and)` is a grouping, not a section; mirrored.
  - The grammar read a leading `'name` in a key path as the whole key, so
    `['a b]` was a valid list but an invalid key path. **Decided
    2026-10-07** (`key-path-items-are-expressions`):
    - every key-path item is an ordinary expression, since `'name` itself
      evaluates to an atom;
    - a constant atom becomes a static key through the helper the
      declaration and pattern parsers now share;
    - a later front-end check can catch applying an atom.
  - The grammar read `(and:a)` as a prefix section but `(and:1)` as a tag,
    deciding by whether the rest parsed. **Decided 2026-10-06**
    (`joint-colon-makes-a-tag`): a joint `:` after a name always makes a
    tag. The grammar now looks ahead for a named tag before a prefix
    operator; the term parser checks the same shape without a trial.
- **Speed and depth:** without Rust recursion in the term parser,
  - every survey nesting shape parses at depth 2,000 within two seconds;
  - flat infix and application chains of 100k terms parse and resolve.
- **Finding: deep syntax trees overflow the stack.** The survey's overflow
  on 32k-term chains is not only the parser's.
  - A resolved 100k-term chain is a 100k-deep `SyntaxExpr`, and dropping
    it overflows the stack, so the test drops it on a large-stack thread.
  - Later passes over the tree probably recurse as well.
  - Long chains end to end therefore need one of two things, not yet
    decided and outside the parser:
    - iterative drop and traversal;
    - flatter trees, such as an application holding an argument list, or
      n-ary chains of associative operators.

    The performance roadmap's `perf-pre-eval-stack-depth` has owned this
    since 2026-10-06; it has not started (2026-10-15).

## Production switch (2026-10-07)

**Term parser first** (`parser-term-parser-first`, `eb84587c`).
`parse_expression_chain_view` parses every
expression view with the term parser first. The Chumsky grammar parses only
the views the term parser reports as unsupported. While a test enables the
oracle, it still parses each covered view with the grammar and compares.

Term parser errors carry the token where parsing stopped:
- Diagnostics report that token's line.
- Messages say what was expected and what was found.
- A misaligned leading operator is reported at the operator.
- A space after `.` or `:` gets a message naming the missing adjacency.

Three invalid-sample expectations changed with the wording.

Measured in a release `glam-prof` build. These single-size workloads were
replaced on 2026-10-08 (`c65f9cfd`) by families at three depths:
`parse_parens` and `parse_lists` at 100/200/400, `parse_ifs` at 4/8/16.

| Workload | Baseline 2026-10-06 | After `parser-term-parser-first` |
| --- | --- | --- |
| `parse_parens_10` | 470.9 ms | 36.6 ms |
| `parse_lists_16` | 708.9 ms | 37.5 ms |
| `minimal` | 36.7 ms | 36.8 ms |

**Keyword forms in the term parser** (`parser-keyword-delegation`,
`adcd994b`).
- **Delegation.** Keyword heads (`if`, `match`, `try`, `try_match`, `do`,
  `using`) go to the structural parsers through `parse_structural_atom`,
  and postfix `if` goes through `parse_postfix_if_tail`. The Chumsky
  adapters call the same functions. The cursor then skips to the end the
  structural parser reports.
- **`abstract_global_path`** is parsed directly.
- **Deferred group errors.** A group's error surfaces only if a role
  consumes it (`parse-errors-surface-on-use`), so a keyword form's own
  groups raise nothing.
- **Rules settled along the way:**
  - `lambda-body-takes-postfix-if`, through `ChainTail`;
  - `dict-member-colon-joint-to-path`;
  - the tag lookahead of `joint-colon-makes-a-tag`, now applied to every
    infix operator.
- **Chumsky's remaining role:**
  - views with invalid tokens or keyword lambda parameters, which are
    errors or rare;
  - the oracle.

  `parser-grammar-retirement` retires the grammar.
- **Oracle**, under the final rules:
  - samples: 1,155 expressions compared, nested keyword-form bodies
    included;
  - a 20× generated run, with keyword forms in both generators: 215,614
    soup cases and 332,342 grammar-directed expressions;
  - the sweep over all 1,894 library tests.

  None disagree. The only unsupported case left is a keyword lambda
  parameter.
- **Measured** in a release `glam-prof` build:
  - `parse_ifs_10`, a new workload of `(if c then … else 0)` nested ten
    deep, parses in 2.7 ms and runs 41.8 ms in total, against 35.9 ms for
    `minimal`. The survey measured ×3 growth per level for this shape under
    the grammar.
  - Parsing in `hello_elf` fell from 3.2 ms to 1.7 ms.
- **Remaining superlinear costs:**
  - a keyword form nested inside groups re-parses its text once per level;
  - `structural_view_at` scans every group in the source for each keyword
    atom (cause 3).

  The frames of `parser-keyword-frames` remove both.
- **Correction 2026-10-08: the nested keyword form is exponential.** The
  scaling workloads of `perf-scaling-workloads` measured
  `(if c then … else 0)` nested 4, 8 and 16 deep: parsing doubles with
  each level, 0.8 ms at depth 8 and 178 ms at depth 16, and depth 100 did
  not finish in ten minutes. A keyword atom inside a group delegates to the
  structural parser, which parses each branch with a fresh term parse. That
  parse covers the branch's groups again, though the enclosing term parse
  has already covered them, so each level costs twice the one inside it.
  The same holds for a keyword form inside an argument,
  `f (if c then f (if …) else 0)`, and inside a lambda body. Without the
  parentheses, else-if chains and `then if …` parse linearly.
  `parser-keyword-frames` removes it; until then, real code with a few
  levels of parenthesized `if` or `match` pays a factor of 2 per level.
  *Removed 2026-10-15* by keyword regions
  ([Keyword regions 2026-10-15](#keyword-regions-2026-10-15)).

## Measures 2026-10-15

User-space instructions at `01ec2fbc` from `scripts/profile.sh` (release
`glam-prof`, no workers). `minimal` is 24.3 M. k is the script's growth
exponent, with cost ≈ a + b·n^k.

| Family | Depths | M instructions | k |
| --- | --- | --- | ---: |
| `parse_parens` | 100/200/400 | 24.9 / 25.2 / 25.9 | 0.83 |
| `parse_lists` | 100/200/400 | 25.2 / 25.8 / 27.0 | 0.83 |
| `parse_ifs` | 4/8/16 | 26.7 / 33.1 / 1,439.4 | 7.78 |

- Nested parentheses and brackets cost about 3 K and 6 K instructions per
  level, as in the roadmap's 2026-10-11 snapshot.
- `parse_ifs` is unchanged since that snapshot (k 7.78). Beyond `minimal`,
  depth 8 costs 8.8 M and depth 16 1,415 M: about 1.9× per level. Its
  reduction count is the same at every depth, so the growth is parsing.

## Keyword regions 2026-10-15

The first part of `parser-keyword-frames` (branch
`parser-keyword-frames`).

**The exponential.** A test-only counter confirmed the 2026-10-08 reading:
`(if c then … else 0)` nested d deep cost 2^d − 1 group interpretations.
The term parser interpreted every group as it closed, before knowing that a
keyword form would claim it; the form's structural parser then parsed the
same groups again from their tokens, so each level doubled the work below
it.

**Keyword regions.** The term parser's token pass now knows where keyword
forms are, without parsing them:
- A keyword head (`if`, `match`, `try`, `try_match`, `do` or `using`)
  opens a region in its frame, up to the frame's separator or close: the
  extent `parse_structural_atom` gives the form, found by the same
  `group_separator`.
- A keyword that is surely a key opens none: after a joint `.` (a path
  segment or effect name), after a joint `'` (a quoted atom or path), after
  a constructor's `:`, or before a joint `:` (a tag or member key). The term
  parser delegates a keyword form only where the same test holds, so every
  delegated form has its region.
- Groups closing inside a region are deferred, so each group is interpreted
  by exactly one parser. A deferred group the term parser consumes anyway is
  interpreted when consumed.
- Regions never change a parse; they decide only which parser interprets a
  group. A missed region would cost time: the form's groups interpreted
  twice, doubling the work below it. A region the term parser takes anyway,
  at the root of a tag path such as `if.x:1` or after a form that ends
  before its group member does, as in `(do {…} (…))`, costs a deferred
  interpretation: one level of Rust recursion each, where such regions nest.
- The rule is a prediction of where the structural layer takes over, from
  a keyword and its neighbouring tokens; nothing peeks past a group, and no
  alternative is guarded. It shares its head test with the term parser,
  but still mirrors the structural layer's extent, until native keyword
  frames replace both.

**Whole-source scans.** Four structural sites found a keyword's enclosing
group by walking every group in the source (`structural_view_at`,
`conditional_hard_end`, `structural_expression_hard_end`, and macro
invocations in `logical.rs`), and `line_indentation_at` walked back to the
line start on every call. The lexer now records each token's innermost
enclosing group and line indentation once.

**Keyword search.** `contextual_keywords` collected every match in a view,
though most callers wanted the first; a `where` suffix rescanned the whole
view for the next `where`, quadratic in a chain of them. It is now an
iterator, and a suffix searches only what follows it.

**Testing the regions** (2026-10-16, after the maintainer asked how we know
they are not fragile):
- `keyword_regions_never_change_a_parse` parses every sample, edge case,
  keyword-as-key case, nesting shape (depths 1–6) and generated program
  with regions and without, 11,392 parses, and compares trees and
  diagnostics.
- `keywords_as_keys_agree_with_the_grammar` runs the oracle over keywords as
  path keys, quoted atoms and paths, tag, constructor and dict keys, effect
  names and words containing them, each before a group a region would defer.
- The count tests nest eight keyword forms and nine keyword keys 12 deep and
  fail if any group is interpreted twice; 12 levels fail at once (4,095
  interpretations of 12 groups) instead of hanging the suite.
- `keywords_as_keys_nest_without_recursion` nests the keys 2,000 deep.
- Each test was checked against a deliberate fault: deferred groups the
  term parser takes become errors (three tests fail, first on `x.if (a)`,
  which no sample or edge case reaches); `do` opens no region (8,190
  interpretations of 24 groups); the first rule, which missed `if.x` and
  took keys after `.`, `'` and `:` as heads (4,095 interpretations of 12;
  stack overflow at 2,000 deep). Every token opening an unclosed region
  leaves results unchanged but overflows the 2,000-deep nesting test.

These found two faults in the first rule, both fixed: `(if.x then … else
0)`, which the term parser delegates, opened no region and stayed
exponential (126 M instructions at depth 12 on `main`, about 1 M now); and
keys such as `(x.if (…))`, `(f 'if (…))` and `(:if (…))` opened regions,
so 10,000 levels overflowed the stack where `main` parses them.

**The oracle.** The sweep (`GLAM_TERM_ORACLE`) compared every expression
the grammar's keyword forms parsed through the term parser while the oracle
ran the grammar, so it reran the grammar at every nesting level below and
never finished the depth-40 nesting test. It no longer compares inside its
own grammar parse, which the term parser's own parse covers, and a test
that counts the term parser's work suspends it. The sweep agrees across all
1,970 library tests, in 16 s instead of 61 s.

**Measured** (release `glam-prof`, instructions):

| Workload | `main` | Keyword regions |
| --- | ---: | ---: |
| `parse_ifs_16` | 1,439 M | 31.3 M |
| `parse_ifs` growth (4/8/16) | n^7.77 | n^1.03 |
| `chain_where_800` | 456 M | 309 M |
| `chain_where` growth (200–800) | n^1.62 | n^1.44 |

Across the 60 profiling workloads, 12.3% fewer instructions; `minimal`,
`hello_do` and `hello_elf` within 0.03%. Long flat literals pay about 1%
(`list_literal_4000` +1.1%, `dict_lookup_2000` +0.65%), mostly building the
token tables. Parse only, nested 128 deep: a parenthesized `if` 18 M (`main`
does not finish 32 deep), a parenthesized `match` 5.7 M (7.1 M), a `where`
chain 5.2 M (9.3 M). Nested patterns cost 4 to 10% more than on `main`
(`((p) as q)` 30.7 M against 27.8 M at 128 deep), from the token pass's
region checks repeated by the pattern parser's trial parses, which
`parser-patterns` replaces.

### What is left

- **`op -> P` is still exponential**, through cause 2's trial parses: a
  `do` statement or guard whose operation holds a parenthesized keyword
  form, as in `do { (do {…}) -> z; .r z }` (depth 8: 11.6 M instructions,
  205 M on `main`; depth 16: 2.9 G). `parse_braced_body` parses the whole
  statement as an expression, which fails at `->` after interpreting
  everything before it, and then parses the operation again. Guards do the
  same. A deterministic split needs to know which top-level `->` belongs to
  a lambda head, a guard or a layout arm; `parser-patterns` does that by
  parsing one cover term and converting by role, as the design says. A
  lexical pre-check would be the rejected option A.
- **Patterns are quadratic** in nesting (`((p) as q)`, `(p) ++ [q]`):
  n^1.9–2.0 at depths 32–128, on `main` and with keyword regions alike.
  `parser-patterns`.
- **Keyword nesting rescans.** Each keyword level still parses its own view
  afresh, so nested keyword forms cost O(n·d) (n^1.77 for a parenthesized
  `if` at depths 32–128) and recurse on the Rust stack per level: parse
  only overflows near 2,000 levels in a release build. Native keyword frames
  remove both.
- **Name analysis** is quadratic in `where` nesting (`chain_where`'s
  remaining excess, about 160 M at 1,600): `parser-scope-analysis`.
- **Downstream depth.** Nested 512 deep, a parenthesized `if` now parses,
  then overflows the stack after parsing (`perf-pre-eval-stack-depth`).
- **Forms that end early, nested.** `(do { .r 1 } (…))` nested 10,000 deep
  overflows the stack in a debug build, where `main` parses it: each level's
  argument is a deferred group the term parser takes on demand. It is
  keyword nesting, which overflows sooner for a parenthesized `if`; native
  keyword frames remove both.
- **Keywords as tag keys** (on `main` too). `if:1`, `where:1` and
  `match:[a] (b)` are rejected as malformed forms, and `(f if:1)` is read as
  a postfix `if`, though the syntax reference makes `where:Value` tagged
  data. The structural layer dispatches on a keyword without the key test
  that the term parser and regions apply. And `if.x then a else b` parses as
  an `if` on the effect `.x`, though a keyword may not be a bare path root.

### Proposal

For review; not yet agreed.

1. **Land keyword regions as this step's first part.** They remove the
   measured exponential and every whole-source scan, and leave the
   structural parsers in place.
2. **`parser-patterns` next.** It removes the remaining exponential
   (`op -> P` in `do` statements and guards) and the quadratic patterns,
   and brings the cover IR to patterns. Native frames for `do` statements,
   guards and `match` arms belong with it, since those are where patterns
   live.
3. **Then native keyword frames** for the rest: `if`/`try`, postfix `if`,
   `let`, `where`, `with`, `object`, `using`. Each form becomes frames on
   the term parser's stack, its phases split by its own keywords as the
   token pass meets them, so keyword nesting costs heap frames and each
   token is visited once. The payoff now is depth and the O(n·d) rescans,
   not the common case; it fits with `parser-grammar-retirement`.
4. **`parser-scope-analysis`** is independent and small: one marking pass
   with a scope stack instead of a walk per binding group.

## Verification

- The differential oracle above, while both parsers exist.
- Regressions parse depth-40 nesting of every survey construct, and flat
  chains of 100k terms. The fixed grammar finishes immediately; a
  reintroduced exponential would hang the test visibly rather than slowly
  degrade. The profiling parser workloads must grow linearly.
  As built, `deep_nesting_parses_in_linear_time_without_recursion` parses
  each of the term parser's `NESTING_SHAPES` at depth 2,000 within two
  seconds, and `long_flat_chains_parse_without_recursion` parses 100k-term
  chains. Survey constructs outside the term parser, such as patterns, `do`
  blocks and keyword forms, have no depth regression in the parser
  tests yet.
  `NESTING_SHAPES` in the term parser's tests holds no keyword form, which
  is how the exponential above went unnoticed: `parser-keyword-frames` adds
  parenthesized `if`, `match`, `try` and `let` shapes to it.
  As built (2026-10-16), `KEYWORD_NESTING_SHAPES` holds eight keyword
  shapes: parenthesized `if`, `try`, `match`, `let` and `do`, an `if` as an
  argument, an `if` in a lambda body, and `if.x`, which the term parser
  delegates. Its test counts group interpretations at depth 12 rather than
  timing the parse, and fails if any group is interpreted twice. A timing
  bound waits for native keyword frames, since keyword nesting still costs
  O(n·d). See [Keyword regions 2026-10-15](#keyword-regions-2026-10-15) for
  the tests of keywords as keys and of regions themselves.
- The invalid-syntax samples keep their diagnostics. Chumsky merges expected
  tokens from failed alternatives, so check closest-match wording explicitly.
- Compare compile time on the samples and the X3 corpus before and after.

## Related candidates

Other hot spots from holistic review F3 fit the same plan once the
performance phase begins:

- the Chumsky grammar is rebuilt for every leaf expression;
- `op -> pat` statements are trial-parsed at least twice;
- structural keywords scan every group in the file.

As of 2026-10-15, the first is resolved in production: Chumsky runs only
for views the term parser reports as unsupported, and as the test oracle.
`parser-patterns` owns the second. The third is resolved by keyword
regions (2026-10-15): the lexer records each token's enclosing group.

Stack-overflow aborts on very deep nesting are a separate concern, tracked
in the panic-safety plan, because an overflow crashes the process.

## Open questions (2026-10-15)

- **Interim fix for the keyword-form exponential?** It has stood since
  2026-10-08, and `parse_ifs_16` alone costs 1.4 G instructions. The
  2026-10-06 direction rejected lookahead guards (option A), including as
  a stopgap, but did not rule on option B, memoizing structural parses by
  token range. Wait for `parser-keyword-frames`, or bound it sooner?
  *Resolved 2026-10-15:* keyword regions, the first part of
  `parser-keyword-frames`, remove it without memoization or lookahead
  guards.
- **Keywords as tag keys.** Should the structural layer use the term
  parser's key test, so `if:1` is tagged data as the syntax reference says,
  and `if.x` a bare-root error? See What is left under
  [Keyword regions 2026-10-15](#keyword-regions-2026-10-15).
- **What follows keyword regions?** Proposed in
  [Keyword regions 2026-10-15](#keyword-regions-2026-10-15): land them, take
  `parser-patterns` next for the remaining `op -> P` exponential and the
  quadratic patterns, then native frames for the other keyword forms.
- **`parser-cover-ir` as designed.** The shared pattern-and-expression
  IR was never built; the term parser's covers are expression-only and
  group-level. Should `parser-patterns` absorb the step, or does the
  design change? *Answered (maintainer, 2026-10-15):* `parser-patterns`
  absorbs what remains of it; the design stands.
- **Who owns deep-source stack depth?** *Answered (maintainer,
  2026-10-15): the roadmap's `perf-pre-eval-stack-depth`.* `Decisions.md`
  (`no-semantic-recursion-on-rust-stack`) says the parser still recurses
  and assigns the remedy to the panic-safety plan. The roadmap says
  `perf-parser` closes that exception, and gives syntax-tree drop and later
  passes to `perf-pre-eval-stack-depth`. The term parser is iterative, but
  the structural and pattern parsers still recurse on nested input.
- **Standing documentation.** `docs/architecture/front_end.md` and
  `src/README.md` describe only the Chumsky input adapter; the term parser,
  its oracle (`GLAM_TERM_ORACLE`, `GLAM_TERM_FUZZ`) and the Chumsky
  fallback are documented only here and in `parser/term.rs`. Document them
  now, or after `parser-grammar-retirement`?

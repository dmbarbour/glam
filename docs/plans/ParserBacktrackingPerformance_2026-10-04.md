# Parser Backtracking Performance Plan — 2026-10-04

Status: under investigation (2026-10-06); this is the parser track of the
[performance roadmap](PerformanceRoadmap_2026-10-05.md). The maintainer asked
for a more general solution than per-construct guards before any fix:
something like CPS, with backtracking delimited or cut at boundaries. See
[Investigation](#investigation-2026-10-06). It was found during the
[user-input panic safety](UserInputPanicSafety_2026-10-04.md) parser
inspection.

## Problem

Parse time grows exponentially with the nesting depth of parentheses and
lists. Release-build timings for `x = ((…1…))` with `--parse`:

| Form | Growth per level | Measurements |
| --- | --- | --- |
| Parentheses | ~3× | depth 12: 3.9 s; depth 13: 12 s; depth 14: 38 s |
| Lists | ~2× | depth 18: 2.8 s; depth 20: 11 s |
| Dictionaries | linear | depth 10,000: 0.14 s |

Realistic source nests 5–8 levels, so ordinary code already pays a
measurable share of this cost. It likely explains part of the slow compile
times recorded by the holistic review (F3 and its 60-second `g_syntax` unit
test).

## Cause

The expression grammar in `parser/expression.rs` tries several alternatives
that open with the same delimiter, and each parses the whole group before it
can fail:

- **`computed_tagged`** (inside `atom`) parses `computed_path` followed by
  `:`, for path-dict literals such as `[a, b]:payload` and `(expr):payload`.
  `computed_path` parses the entire bracketed or parenthesized group,
  including every nested expression, before discovering that no `:` follows.
- **`postfix_operator_section`** parses `(expr` and then fails when no infix
  operator precedes the `)`.
- **`grouped_or_trailing_tuple`** finally parses the same `(expr` again.

Each level therefore re-parses its contents twice for lists
(`computed_tagged`, then `list`) and three times for parentheses
(`computed_tagged`, `postfix_operator_section`, then grouping). That gives
roughly 2ⁿ and 3ⁿ.

## Fix (option A)

Fail the doomed alternatives in constant time using the lexer's existing
delimiter pairing, which assigns each group a `GroupId` with known open and
close tokens:

1. Start `computed_tagged` only when the group's closing delimiter is
   immediately (jointly) followed by `:`.
2. Start `postfix_operator_section` only when the token before the closing
   `)` is an infix operator.

Grouping, tuples, and lists then parse their contents once, so total parse
time becomes linear in nesting depth. Semantics are unchanged, because a
guarded alternative only skips inputs it would have rejected anyway.

An alternative is to left-factor these forms: parse the delimited group
once, then decide its role from the following token. That restructures more
of the grammar. Chumsky memoization would also bound the cost, but it adds
memory and leaves the duplicated work in place.

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

These are proposals for the maintainer to decide.

### Incidental findings

- `(\w -> match w with { z => w })` fails with "structural expression
  extends beyond its token range".
- `(1, match y with {…})` is rejected.

Both are possibly parser bugs unrelated to speed.

## Verification

- A regression parses depth-40 nested parentheses and lists. The fixed
  grammar finishes immediately; a reintroduced exponential would hang the
  test visibly rather than slowly degrade.
- The invalid-syntax samples keep their diagnostics. Chumsky merges expected
  tokens from failed alternatives, so check closest-match wording explicitly.
- Compare compile time on the samples and the X3 corpus before and after.

## Related candidates

Other hot spots from holistic review F3 fit the same plan once the
performance phase begins:

- the Chumsky grammar is rebuilt for every leaf expression;
- `op -> pat` statements are trial-parsed at least twice;
- structural keywords scan every group in the file.

Stack-overflow aborts on very deep nesting are a separate concern, tracked
in the panic-safety plan, because an overflow crashes the process.

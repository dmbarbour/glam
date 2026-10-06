# Parser Backtracking Performance Plan — 2026-10-04

Status: design agreed with the maintainer on 2026-10-06; implementation not
started. This is the parser track of the
[performance roadmap](PerformanceRoadmap_2026-10-05.md). The fix is a merged
grammar with prefix sharing, parsing into a cover syntax (IR) for patterns and
expressions, implemented with an explicit stack; see [Design](#design). It
replaces the lookahead guards first proposed here. It was found during the
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
  sees whole views. Slice 4 moves keyword forms into the parser machine.

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

## Slices

1. **Cover IR and conversion.** The IR types; conversion to `SyntaxExpr`
   and `SyntaxPattern`; unit tests of the role rules.
2. **Term parser.**
   - The explicit-stack, prefix-shared parser for atoms, groups,
     collections, tags, operators and applications.
   - Expression leaves use it first.
   - **Differential oracle:** the old parser stays and must produce the
     same syntax tree for every sample, every test source, and generated
     programs.
3. **Patterns.** Pattern positions use the same parser plus conversion.
   This retires the split-and-retry code for views, predicates, guards and
   `op -> P`.
4. **Keyword forms and structural scans.** Move keyword forms into the
   machine, remove the whole-file scans, and retire Chumsky from
   expressions.
5. **Retirement.** Delete the old expression and pattern parsers once the
   oracle has held across the corpus. Keep the differential corpus as
   regression tests.

Slices 1 and 2 are additive and low-risk: production keeps the old parser
until the oracle agrees.

**Progress 2026-10-06.** Slice 2's core is in `parser/term.rs`; it is
test-only so far.
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
    2026-10-06** (`lone-quoted-name-is-an-atom-key`): only a lone `'name`
    is an atom key. Any other item is an index expression, and a later
    front-end check can catch applying an atom. Both parsers changed.
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

## Verification

- The differential oracle above, while both parsers exist.
- Regressions parse depth-40 nesting of every survey construct, and flat
  chains of 100k terms. The fixed grammar finishes immediately; a
  reintroduced exponential would hang the test visibly rather than slowly
  degrade. The profiling parser workloads must grow linearly.
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

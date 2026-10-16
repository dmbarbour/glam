# Tag Prefix Syntax Plan — 2026-10-16

Status: design agreed with the maintainer on 2026-10-16; not started. It
follows the performance work, and in particular `parser-grammar-retirement`
in the [parser backtracking plan](ParserBacktrackingPerformance_2026-10-04.md),
so the syntax changes in one parser rather than in the term parser and the
Chumsky grammar both.

## Problem

Tags and dict members read differently by context:
- `:foo` is a constructor, `\x -> foo:x`, outside braces, but as a whole
  dict member it is a pun: `{:foo}` is `{foo:foo}`. A bare `:x` is not a
  pattern.
- `foo:f x` is `(foo:f) x` outside braces, but a dict member's value runs to
  its comma, so `{foo:f x}` is `foo:(f x)`.
- `foo: x`, with a space, is a dict member inside braces and an error
  outside them.

So braces carry a member grammar of their own, and the cheat sheet needs
rules such as "`{:tag Value}` is invalid; parenthesize the constructor".

## Design

*Agreed (maintainer, 2026-10-16).* `path:` becomes a prefix operator that
builds tagged data, `{path: x}`, read alike everywhere:
- **Joint to its operand, it binds tighter than juxtaposition:**
  `foo:f x` is `(foo:f) x`, in braces too. `eff:f x` still applies an
  effect.
- **Followed by a space or line break, its operand is the rest of its
  item**, as a lambda's body is: `foo: f x` is `foo:(f x)`, and
  `foo: a + b` is `foo:(a + b)`. The operand is an open tail, so it ends at
  `->` where tails do (the parser backtracking plan, "Patterns 2026-10-16").
- **With nothing after it in its item, it is the constructor function.**
  `(foo:)` is its section, as `(+ 1)` is an operator's: `map (foo:) xs`,
  `[foo:, bar:]`, `x |> foo:`.
- **Every path form takes part:** `a.b:`, `[k]:` and `(p):`, and in patterns
  the optional `path?:`.
- **`:name` is `name:name` everywhere**, a pun on one bare name. It no
  longer constructs.
- **A dict literal is the union of its items**, each an ordinary
  expression: `{a:1, b: f x, :c, D}`. Braces lose their member grammar.
- **Patterns read alike:** `foo:P`, `foo: P` and `(foo:) P` are the same tag
  pattern, and `:x` captures `x` under the key `x`. A bare `foo:` is not a
  pattern.

Consequences:
- `{foo: f x}` keeps its meaning. `{foo:f x}` changes from `foo:(f x)` to
  `(foo:f) x`; see [Migration](#migration).
- `{foo :x}` applies `foo` to `x:x`, and `{:foo Value}` applies `foo:foo`
  to `Value`, in braces as outside them, so the special diagnostic for the
  latter goes.
- The keyword rule is unchanged: a keyword before a joint `:` is a key, so
  `if:` and `(and:)` are constructors (`keyword-before-key-mark-is-a-key`).

**Deferred: path puns.** `:a.b` could mean `a.b:a.b`, which is useful when
building dicts. It waits for a consistent approach to capturing patterns,
and perhaps locals generally, into structured dicts, so that construction
and matching read alike. Until then a pun is one bare name, as today.

## Migration

Measured on `main` at `3264414d`, excluding the invalid samples:
- **Joint members with a compound value** change meaning silently: 13, all
  in `samples/config/direct_assembly.g`, such as
  `{identity:x86.cursor_handle id}`. Each takes a space after its colon.
- **Constructors written `:name`** become puns: `message = :greeting` in
  `samples/hello/hello_tagged.g`, and `{ (:hello "Hello"), … }` in
  `samples/hello/hello_dict.g`. Both become `name:` forms.
- **Docs:** `Syntax.md` "Tagged Data" and "Dicts", `SyntaxCheatSheet.md`
  "Atoms, Tagged Data, Dicts" and "Patterns", and `DistilledDesign.md` where
  it shows constructors.

A joint member followed by arguments in braces is where code silently
changes meaning, so a diagnostic there, for a while after the change, would
catch anything the corpus sweep missed.

## Steps

1. `tags-prefix-operator`: the parser. `path:` as a prefix operator, `:name`
   as a pun, dict literals as unions of items, and the pattern readings,
   with tests of each form inside and outside braces.
2. `tags-prefix-migration`: rewrite the corpus and the docs, and add the
   transitional diagnostic for a joint member followed by arguments in
   braces. Record the decision in `Decisions.md`.
3. `tags-prefix-diagnostic-retirement`: remove the transitional diagnostic
   once it has served.

## Open questions (2026-10-16)

- **Spaced payloads in patterns.** A spaced payload is bounded by its
  item's comma or brace, much as an arm head is bounded by `=>`. Should it
  be a complete position, so that `{x: Nat n}` is a predicate without
  parentheses? Today it needs them, `{x: (Nat n)}`.
- **An operator after a spaced prefix.** `foo: |> g` has no operand, and
  reads as an error under the design. Is that right, or should `foo:` there
  be the constructor?

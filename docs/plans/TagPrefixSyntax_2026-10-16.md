# Tag Prefix Syntax Plan — 2026-10-16

Status: design agreed with the maintainer on 2026-10-16;
`tags-prefix-operator` and `tags-prefix-migration` built 2026-10-17 (see
[As built](#as-built-2026-10-17)); `tags-prefix-diagnostic-retirement`
waits. It
follows `parser-grammar-retirement` in the
[parser backtracking plan](ParserBacktrackingPerformance_2026-10-04.md), so
the syntax changes in one parser rather than in the term parser and the
Chumsky grammar both, and then `grammar-body-declarations` in the
[grammar review](GrammarReview_2026-10-16.md). It comes before the
remaining parser performance steps and other work (maintainer,
2026-10-17).

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
  `foo: a + b` is `foo:(a + b)`. In an expression the operand is an open
  tail, so it ends at `->` where tails do (the parser backtracking plan,
  "Patterns 2026-10-16"); in a pattern, see below.
- **An operator cannot follow a spaced prefix** (maintainer, 2026-10-16):
  `foo: |> g` is an error, since the prefix has no operand. `(foo:) |> g`
  passes the constructor.
- **With nothing after it in its item, it is the constructor function.**
  `(foo:)` is its section, as `(+ 1)` is an operator's: `map (foo:) xs`,
  `[foo:, bar:]`, `x |> foo:`.
- **Every path form takes part:** `a.b:`, `[k]:` and `(p):`, and in patterns
  the optional `path?:`.
- **`:name` is `name:name` everywhere**, a pun on one bare name. It no
  longer constructs.
- **A dict literal is the union of its items**, each an ordinary
  expression: `{a:1, b: f x, :c, D}`. Braces lose their member grammar.
- **Patterns read alike:** `foo:p`, `foo: p` and `(foo:) p` are the same tag
  pattern, and `:x` captures `x` under the key `x`. A bare `foo:` is not a
  pattern.
- **A spaced payload in a pattern is a complete position** (maintainer,
  2026-10-16). Its item's comma or brace bounds it, as `=>` bounds an arm
  head, and it runs to the end of its item, pattern operators included:
  `{x: Nat n}` tags a predicate, `{x: f -> p}` a view, and `{x: a as b}`
  both captures. A joint payload stays nested, as in expressions:
  `x:Nat n` is a predicate `x:Nat` over `n`. A spaced tag as a view's
  function needs parentheses: `((foo: f) -> p)`.

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

## As built (2026-10-17)

- **One detector.** The term parser's item interpreter measures a tag
  prefix (`tag_prefix_at`): a data path, an optional `?` in patterns, a
  joint `:`, and what follows, joint, spaced or nothing (nothing including a
  stop such as `where`). A joint prefix stays the atom-level loop it was; a
  bare one is the constructor wherever an operand or argument stands.
- **Spaced tags as levels.** In an expression a spaced tag opens a level of
  its item, as a lambda does, at an operand's start or as a tail argument
  (`g tag: f x`), so it ends where tails do. In a pattern its payload is the
  rest of the item read as a pattern item and marked complete
  (`cover::Term::Complete`), which converts as a complete position without
  a group of its own; this recurses once per spaced tag after `as` or a
  view's arrow, not per nesting of the tag itself.
- **Puns and braces.** `:name` is an atom anywhere; a `:` starts an argument
  only joint to a name. A brace item is an ordinary item, and the brace
  member grammar and its validator are gone. Cover conversion reads a tag,
  a pun, a constructor applied, or one of these grouped, as a dict entry.
- **Transitional diagnostic.** A brace item whose tag prefix is joint yet
  whose term is no tag (`{a:f x}`, `{a:x + 1}`, `{a:p as q}`) is an error
  naming both spellings. The test printer parenthesizes such items.
- **Migration.** 13 members in `samples/config/direct_assembly.g` (two of
  them lambdas, which the joint form now rejects with a message naming the
  spaced one), the constructors in `hello_tagged.g` and `hello_dict.g`,
  `tagged_spacing.g`'s expectations, Rust test sources, and the docs. The
  measured counts held.

## Open questions (2026-10-16)

None. The maintainer answered both raised while planning, and the answers
are in [Design](#design): a spaced payload is a complete pattern position,
and `foo: |> g` is an error.

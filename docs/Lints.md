# Lints

A linter flags what is easy for a machine to parse but confusing to people,
a refinement type on the grammar guided by clarity and intuition
(maintainer, 2026-10-16). The parser rejects only what a reader could
plausibly structure differently; a lint may have exceptions, severities and
opt-outs, because a warning costs a reader nothing to learn. No linter
exists yet, and `glam --lint` is possible later work.

This is a list of candidates, brainstormed and collected from decisions as
they arise. Each has a name to cite, what it flags, and why; a source names
the decision or discussion it came from. Add candidates as they come up.
Examples are inline, since each must parse.

## Tails and Nesting

- **`lint-nested-inline-if`.** Deeply nested inline conditionals, such as
  `if a then if b then c else d else e`. Every `if` pairs with an `else`,
  so the parse is unique, but a reader must count. Source:
  `tail-forms-end-at-separators`.
- **`lint-lambda-before-arrow`.** A lambda before a binding's `->`, as in
  the `do` statement `op <| \a -> e -> p`, which reads like a curried
  lambda but binds `p`. Source: `tail-forms-end-at-separators`.
- **`lint-same-line-match`.** A match nested in an arm on the arm's line,
  `P => match E as Q => R`. Source: `nested-arrows-share-an-arms-line`.
- **`lint-match-in-subject`.** A match in a match's subject,
  `match match a as {…} as {…}`. Source: grammar review.
- **`lint-back-pipe-into-pipe`.** `<|` before a lambda whose body holds
  `|>`, as in `f <| \x -> g x |> h`: the lambda takes `|> h`, which reads as
  if it pipes `f`'s result. Source: maintainer, 2026-10-17.
- **`lint-deep-escape`.** `^^name` and deeper, which counts `self`s a reader
  must find. A name or alias for the outer object reads better.

## Names and Scope

- **`lint-let`.** `let`, in expressions, bodies and files. It hides logic
  from extension, so it is not encouraged, though it avoids awkward
  workarounds. A heuristic, not a ban. Source:
  `aliases-and-locals-are-body-declarations`.
- **`lint-postfix-if-binds-left`.** A postfix `if` whose guard binds names
  used to its left, as in `value if (value, rest) = decompose input else
  fallback`. Kept as comprehensions keep it, but the reader meets a name
  before its binding. Source: grammar review.
- **`lint-unused-using`.** A `using` whose body reads no name from its
  dict, so it only hides locals.
- **`lint-using-hides-used-local`.** A `using` whose dict has a static key
  that names a local the body would otherwise read: the body's `x` now
  reads the dict, and only `^x` the local. Hiding is `using`'s purpose, so
  this suits an opt-in severity.

## Idiom

- **`lint-redundant-do`.** `do X` with one statement that is an effect, as
  in `do .op`. It means `X`, at the cost of a step
  (`lone-do-statements-run-as-effects`); keep it where layout needs a
  block.
- **`lint-pun`.** `{x: x}` where `:x` says the same.
- **`lint-applied-constructor`.** `(tag:) x`, where `tag: x` says the same.
- **`lint-guard-of-unit`.** `if c then .r () else .fail`, which is `c`.
- **`lint-else-if-chain`.** A long `else if` chain over one subject, which
  a match states once.

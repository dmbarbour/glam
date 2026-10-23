# Tag Tail Syntax — 2026-10-23

A tentative experiment (maintainer, 2026-10-23): a tag reads alike whether
its `:` is joint or spaced, and appears only where a tail can. It runs on a
branch until the maintainer judges its look and feel and its clarity.

## Problem

Since `tags-prefix-operator` ([Tag Prefix Syntax](TagPrefixSyntax_2026-10-16.md)),
a space decides what a tag holds:
- joint, `tag:f x` is `(tag:f) x`: the tag holds one term, and binds
  tighter than application;
- spaced, `tag: f x` is `tag:(f x)`: the tag holds the rest of its item, as
  a lambda's body does.

The rule is consistent, but easy to miss when writing and hard to see when
reading. Writing tests for `pure-handler-core`, an agent wrote
`{value:s.handler, state:s with {…}}` and `{eff:\api -> …}`, meaning the
spaced forms, and the parser stopped each. The maintainer expects human
users to stumble the same way.

## Proposal, to try

- **A tag is a tail form, like a lambda.** Its payload is the rest of its
  item, whether its `:` is joint or spaced: `tag:f x`, `tag: f x` and
  `tag:(f x)` are one value. A dict's or a list's item is a tail position,
  so `{value:s.handler, state:s with {…}}` means what it seems to.
- **Tags only in tail position.** Where a lambda needs parentheses, so does
  a tag: `g tag:x` is an error, written `g (tag:x)` or `g <| tag:x`.
- **Unchanged:** the section `(tag:)` constructs; `:name` puns on one bare
  name; path, computed and optional tags; the rule that a keyword before a
  joint `:` is a key; a dict literal as the union of its items.
- **Patterns read alike:** a tag pattern's payload runs to the end of its
  item either way, so `x:Nat n` is `x: Nat n`, a tag over a predicate.

What changes:
- A joint tag followed by more of its item changes meaning: `eff:f x`
  turns from applying the effect `eff:f` to `x` into tagging `f x`. An
  effect applied is then written `(eff:f) x`, or built by `eff: \api -> …`.
- A joint tag as an argument becomes an error until parenthesized:
  `f tag:x y` becomes `f (tag:x) y`.

## Steps

1. `tags-tail-survey` — count the uses that would change: joint tags
   followed by more of their item, whose meaning changes, and joint tags
   outside tail positions, which become errors, in samples, tests, docs and
   the CLI's prelude.
2. `tags-tail-parser` — on an experiment branch, the parser reads a joint
   tag as a spaced one and rejects a tag outside tail position, with a
   diagnostic naming the parenthesized form.
3. `tags-tail-migration` — on the branch, migrate the samples, tests and
   docs, the cheat sheet first.
4. `tags-tail-review` — the maintainer reads representative sources both
   ways and adopts the change, with an ADR superseding the joint rule, or
   drops the branch.

## Open questions

1. Is a tag a tail after `<|` and in a pipeline's stages, as a lambda is?
2. Should a section `(tag:)` followed by arguments, `(tag:) x`, stay the way
   to tag one term within an application?
3. Do patterns keep a way to tag a nested term, now `(tag:p) q`?

# `language g0` Macro Contract Fixtures

These are phase-zero contracts for the built-in compiler. Production macro
expansion is not implemented yet. Executable macro-head cases live in
`src/g_syntax/parser/macro_contract.rs`; the production text-pattern contract
and conformance cases live in `src/text_pattern.rs`. This note records the
effect semantics that require later runner and layout machinery.

## Accepted macro heads

```g
@name
@name.child
@outer @inner input
```

`@` and every static path component are joint. `@name .child` invokes `name`
and leaves `.child` as input; it does not select `name.child`. All original
invocations in one declaration use one prior-module snapshot and expand
exactly once from right to left, so `inner` above completes before `outer`.

## Rejected macro heads

```g
@
@ name
@(dynamic)
@.name
@name.
@name. child
@name.[computed]
```

Dynamic lookup and computed path components remain outside `language g0`.

## Cursor and replacement

- The root reader begins after the static macro head and cannot consume a peer
  logical item.
- Root `.read.anchor` invokes non-consuming `.fail`. Only a parser entered by
  `.read.layout` can read child-layout anchors.
- A successful branch replaces the macro head and exactly the input prefix it
  consumed. Unread input remains after the replacement.
- No writes means an empty inline replacement. It removes only that committed
  range; it removes the whole logical item only when the macro begins that item
  and consumes through its end.
- Writing fewer logical elements than were consumed is ordinary shrinking and
  needs no separate effect.
- A leading `.write.anchor` selects sibling output and must be followed by one
  or more nonempty items. An anchor alone, consecutive anchors, and a trailing
  anchor are invalid.
- Anchored output must replace a complete logical item. Inline output never
  introduces sibling boundaries.

## Generated source markers

Every `.write.text Text` containing `@` or `#` is invalid, even if separate
writes might otherwise place those characters harmlessly. `.write.data Value`
is atomic and never inspected for either character.

## Text patterns

The shared `g0` text-pattern language is capture-free, anchored, ordered, and
greedy. Its executable accepted, rejected, and matching fixtures do not call
`regex-lite`. A valid pattern is at most 16,384 UTF-8 bytes and contains at
most 64 nested grouping parentheses.

Plain parentheses group without capturing. The borrowed `(?:...)` form,
captures, flags, lazy or counted repetition, class negation, anchors,
lookaround, backreferences, shorthand classes, and Unicode-property classes
are invalid. Empty patterns and empty alternatives are valid.

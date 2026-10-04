# Parser Backtracking Performance Plan — 2026-10-04

Status: preliminary and deferred. This is low-hanging fruit for a later
performance phase, not current work. It was found during the
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

## Fix

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

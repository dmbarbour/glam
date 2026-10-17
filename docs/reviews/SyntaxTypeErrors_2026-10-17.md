# Syntax-Layer Type Errors Review — 2026-10-17

Status: review complete. No code changed. This records findings and a
recommended remediation, `kinds-` steps, which wait for the maintainer's
answers to the [open questions](#open-questions-2026-10-17) and a place in
the order of work.

Revision reviewed: `6ef3f598` (clean `main`).

## Purpose and Scope

The maintainer asked (2026-10-17) that the front end report type errors
the syntax shows: obvious non-predicates and non-views in predicate and
view positions, and application in general. `(3 4)` is an obvious type
error, and its message could ask whether a tuple was meant. The principle
is already recorded in `tags-are-not-predicates-or-views`: compilers lack
the whole namespace, so they do not catch type errors in general, but they
may report and reject those they see.

Coverage: what the runtime applies, what each effect position requires,
where the runtime reports a wrong kind today, and the front end's existing
checks. A read-only survey gathered the facts; the claims below were checked
against the code.

## What the Runtime Requires

**Application.** `apply_whnf_callable` (`src/eval/whnf.rs:1156`) applies
builtins, partial builtins, functions, and dicts. A dict applies as a
lightweight effect when its only defined member is `eff`,
`(eff:f) x = eff:(\api -> f api x)`, and otherwise through a defined `apply`
member, `obj x = obj.apply x` (`whnf.rs:971-1154`). Every other value fails
when the application is evaluated: "application requires a function value,
received Number" (`whnf.rs:1175`), likewise `Binary` for a text, `List`,
`Atom`, `Unit`, `Dict` for a plain dict or a tuple (a tuple is
`{tuple: [items]}`), and `Net`.

**Effects.** Comparisons yield `.r ()` or `.fail`
(`src/eval/comparison_machine.rs:200`), so they are effects, and so are
`and`, `or`, `>>=`, `=>>`, `>=>`, `!>` and `<!`
(`src/g_syntax/resolve/expression.rs:642-728`). Effect positions:
- **A predicate and a view** are applied to the subject and run as effects
  (`src/g_syntax/resolve/pattern.rs:147-160`): a predicate's result must be
  `.r ()` or `.fail`, a view's `.r View`.
- **A `do` statement**, bare or final, is an effect; a bare one must return
  unit ("effect-only `do` statement: unit expected"). A `do` with no
  statements before its final one lowers to that expression
  (`resolve/do_expr.rs:225`, pinned by a test at `:514`), so `do 42` is `42`, and nothing reports it
  until something runs it as an effect.
- **A binding's source**, `p <- E` or `E -> p`, is an effect.
- **A bare guard clause** is an effect returning unit or failing
  (`resolve/pattern.rs:207-235`, `resolve/conditional.rs:202`).
- **`and` and `=>>`** take an effect returning unit on the left; `or`, `>>=`
  and the applicatives take effects.

When a number runs as an effect, the handler running it reports the error:
"list effect handler requires an effect dictionary, got Number(42)"
(`src/eval/effect_machine.rs:31-53`), "reflection task requires an effect
object" (`src/reflection/machine.rs:2202`), or the net builder's own message.

**Member access** needs a dict: `.x` on a list or text fails with "value
access base is not a dictionary" (`src/eval/access_machine.rs:1086`). Texts
are `Binary` values, not dicts, and there are no list methods; `list.len`
lives in `std.list`.

## The Front End Today

The parser and front end check names (`name_analysis.rs`), unused locals,
lets and aliases (`analysis.rs`), and since `d9bdc947` one kind: a tag as a
pattern's predicate or view (`parser/term/cover.rs:176,194`). Nothing else
checks what kind of value an expression is. Every finding below is reported
only when evaluation reaches it, if it ever does, in the runtime's words,
without the hint a front end could give.

## Findings

Each finding is a position with a requirement and expressions whose kind the
syntax shows: an *evident kind*. Only leaves and forms have one: a number, a
text, unit, an atom, a list or quoted path, a tuple, a dict literal of
static-key tags, a tag, a tag's constructor, a lambda or operator section (a
function), or a `do`, a comparison or an effect operator's result (an
effect). A name, an application, an access or a conditional has none, and
neither does a dict literal with a computed key or a merged dict. Nothing
without an evident kind is ever reported.

**K1. Applying a value that is no function.** `3 4`, `"text" x`,
`[1, 2] 0`, `'name x`, `() x`, `(a, b) c` and `{a: 1} x` fail at runtime.
Tags apply only by `apply` or `eff`, so `a:f x`, which reads as `(a:f) x`,
is the same error. `tags-prefix-migration`'s transitional diagnostic
catches that spelling in braces (`JOINT_ITEM`, `parser/term.rs:948`); K1
would catch it everywhere, with the hint `a: f x`, though not the
diagnostic's other cases, `{a:x + 1}` (K5) and `{a:p as q}`.
For data applied to data, as in `(3 4)`, the hint is a tuple or list:
`(3, 4)` or `[3, 4]`. Exempt: functions, effects, constructors, and tags
whose first key is `apply`, `eff` or computed.

**K2. A non-predicate or non-view in a pattern.** Any data kind as a
predicate (`(42 n)`, `("x" n)`) or a view (`([1] -> p)`), and functions
whose result the syntax shows is data: an arithmetic or append section, as
in `((+ 1) -> p)`, and a tag's constructor, which the tag check already
rejects. A comparison section, `((== 3) n)`, is a valid predicate. Per
`tags-are-not-predicates-or-views`, tags are rejected here even with an
`apply` or `eff` key, unlike K1.

**K3. Data in an effect position.** A literal as a `do` statement
(`do { .a; 42 }`, `do 42`), a binding's source (`x <- 42`), a guard
(`if 42 then …`), or an operand of `and`, `or`, `=>>`, `>>=`, `!>` or `<!`.
Hints: `.r 42` to return a value, `x = 42` to bind one, and for a guard, a
comparison such as `x == 42`.

**K4. Member access on data.** `[1, 2].len`, `"s".x` and `42.x`. Hint for a
list or text: `list.len xs` and the `std.list` functions.

**K5. Operands of data operators.** `"a" + 1`, `1 ++ 2`, `{a: 1} + 1`, or
an effect such as `(a == b) + 1` as an arithmetic operand. These need each
operator's accepted kinds, which take more care than K1–K4 for less value.

**K6. Where the check runs.** The tag check runs during cover conversion,
so only patterns get it. The other positions are clearest in one walk over
each declaration's expression after it parses, where `warn_unused_locals`
runs (`parser/declaration.rs:361,456`), so `glam --parse` reports them and
an error fails the module. Like `name_analysis.rs`, such a walk recurses on
nesting; `perf-pre-eval-stack-depth` covers deep nesting for all of them.

## Recommended Remediation

One classifier of evident kinds and one walk, reporting errors with hints.
Each step tests every spelling it reports and a near miss it must not, such
as an `eff:` tag applied, a comparison section as a predicate, a name, and
a dict merged from an unknown one.

1. `kinds-classifier`: the evident-kind classifier over `SyntaxExpr`, and
   the walk that visits each position once, reporting nothing yet.
2. `kinds-application`: K1 and K4, with the tuple, list and spaced-tag
   hints. It catches the transitional diagnostic's main case everywhere,
   so `tags-prefix-diagnostic-retirement` might follow it.
3. `kinds-pattern-functions`: K2, moving the tag check from cover
   conversion into the walk and keeping its messages and sample.
4. `kinds-effect-positions`: K3.
5. `kinds-operands`: K5, if the maintainer wants it now; otherwise
   deferred.

Each step records its rule in `Syntax.md` where the position is described,
and the series gets one `Decisions.md` entry, amending
`tags-are-not-predicates-or-views`.

## Open Questions (2026-10-17)

1. **Order of work.** The maintainer's order is the parser performance
   steps, then the runtime ownership plan. Where do the `kinds-` steps go?
   They touch only the front end, and `kinds-classifier` is small.
2. **A literal final `do` statement.** Syntax.md says the final statement
   "should express an effect". Is `do 42` an error, with the hint `.r 42`,
   though it lowers to `42` today?
3. **K5 now or deferred**, and if now, which operators: arithmetic only, or
   `++` too?
4. **Embedded values.** A macro may embed a value (`SyntaxExpr::Embedded`)
   whose kind is known when the macro expands. Should the classifier read
   it, so macro output is checked too, or treat it as unknown?
5. **Severity.** Errors throughout, as the maintainer said, including in
   code that never runs, such as an unused definition?

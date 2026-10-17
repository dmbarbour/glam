# Grammar Review — 2026-10-16

Status: started 2026-10-16 at the maintainer's request. The first item,
[`grammar-tail-position`](#grammar-tail-position), was decided and built on
2026-10-16 and refined the same day with separator keywords
(`tail-forms-end-at-separators` in `Decisions.md`). Further items 1–14
were decided by 2026-10-17: those that change the grammar are built, and
the others recorded. `parser-keyword-frames-suffixes` in the
[parser backtracking plan](ParserBacktrackingPerformance_2026-10-04.md)
resumes on the settled grammar.

## Goal

The grammar grew organically, by feel. This review checks it for clarity
when read, using Green's cognitive dimensions of notation, and for ease of
parsing. A context-free grammar is welcome where it keeps a good feel.
Performance is not a driver: the term parser's frames parse in linear time
whichever way these questions are settled.

## Criteria

- **Left to right.** A reader never revises the structure of what they have
  read: a token's role follows from the tokens before it, never from later
  on its line. This is the reader's side of a grammar that needs bounded
  lookahead.
- **Layout is a separate pass.** Indentation becomes open, separator and
  close tokens without feedback from the parser, unlike Haskell's
  `parse-error(t)` rule. The grammar over those tokens is context-free.
- **The dimensions most at stake:**
  - *hard mental operations*: finding where a form ends;
  - *hidden dependencies*: a token whose role depends on distant context;
  - *role-expressiveness*: one spelling with several roles (`->`, `with`,
    `when`, `and`);
  - *error-proneness*: a near miss that means something else instead of
    failing;
  - *consistency*: an item reads alike wherever it stands;
  - *viscosity*: the cost of an edit, such as the parentheses needed to
    move an expression into an argument.

## Parser, linter, formatter

Some rules belong to tools that do not exist yet: `glam --lint` and
`glam --format` are possible later work.
- **The parser** rejects what a reader could plausibly structure
  differently, and what would make the grammar ambiguous or need unbounded
  lookahead. Its rules are few and uniform, one sentence per construct,
  because every exception is something a reader must know.
- **A linter** flags what is easy for a machine to parse but confusing to
  humans, such as a match nested in a match's subject: a refinement type on
  the grammar, guided by clarity and intuition (maintainer, 2026-10-16). Its
  rules may have exceptions, severities and opt-outs, because a warning
  costs a reader nothing to learn.
- **A formatter** owns secondary notation: indentation, alignment, braces
  versus layout, and parentheses the grammar allows but does not require.
  It must preserve the parse, which a round-trip test can check. Syntax.md
  already leaves it "a stricter house style".

## Items

### `grammar-tail-position`

*Proposed by the maintainer and decided 2026-10-16, with the three
recommendations under "Open questions"; built the same day.*

**Tail forms** are the forms whose last part extends to the right: prefix
`if` and `try`, `match` and `try_match`, `do`, lambdas, `let`, `using` and
`object`. Syntax.md already gives lambdas and layout `do` blocks a tail
position (the final argument or right-hand operand), and its layout section
speaks of "a final child that owns the remainder of its host expression".
This item extends that to every tail form.

**The rule**, as first decided; [Separators](#separators-2026-10-16)
refines it:
- A tail form stands without parentheses only in tail position.
- A braced body's `{` and `}` stand for an indent and a dedent.
- **Inline end:** a tail form whose last part ends on its line, not at its
  own `}` or a dedent, may be followed only by a closing bracket, a
  separator, or the end of its statement or definition.
- **Block end:** after a tail form's `}` or dedent, only what may resume
  after a dedent may follow: an infix operator, or a keyword that continues
  an enclosing form (`else`, `with`, `then`, `and`, a bind's `->`, `where`).
  Never an argument or a path suffix.
- Postfix `if` is an operator, not a tail form: `a if c else b -> P` binds
  the whole conditional, as it does today.

**Consequences:**
- Errors, needing parentheses:
  - `f (match …) y`, `f (do { a }) y`, and a layout block followed by an
    argument line;
  - `if y <- (if a then b else c) and d then …`;
  - `(if c then a else b) -> P` and `f (\a -> E) -> P`;
  - an inline `if a then (if b then c else d) else e`: no dangling `else`;
  - `if any xs (\x -> x > 0) then …`.
- Unchanged:
  - `f match x with` followed by layout arms, `P <- f \a -> E`,
    `.cut do …`;
  - `|> process do …` followed by `|> finish` on a later line;
  - `if c then do { a } else do { b }`, and its layout form;
  - `match match a with {…} with {…}`, a lint candidate.
- Every change is a restriction: a program keeps its meaning or fails with
  an error that says where the parentheses go.

**Survey (2026-10-16).** The experiment `tail-position-survey`, kept on a
branch of that name (`cd50e7c6`), adds hooks to the term parser's token
pass. Set `GLAM_TAIL_SURVEY` to a file to collect its records. They
record every tail form that:
- ends inline at a keyword or arrow (`cut`);
- is followed by an argument or path suffix (`argument`);
- is followed by a keyword or operator after its `}` or dedent
  (`after-block`, which the rule allows).

It ran over three corpora:
- **The 95 sample `.g` files**, about 2,200 lines with 12 `if`s, 30
  `match`es, 59 `do`s and 82 lambdas: one record, the allowed
  `|> process do …` resumed by `|> finish`.
- **The `g` examples in the syntax docs:** the same single record.
- **The test suite**, without the generated-expression fuzz: 349 distinct
  records. 321 come from generated sources (random token soup and nested
  shapes), which reach every shape. The other 28 are hand-written, in
  tests that pin these very edge cases:
  - `cut`: `if c then .a else .b -> y` (twice);
    `if x <- if a then b else c and y then …` (twice); a dangling `else`
    after an inner `if` or `try` (twice); `.r 1 |> \v -> v -> y`;
    `(\x -> x -> p)` as a view pattern; `using` in an inline `then`
    branch; deeply nested `if if …`.
  - `argument`: `match a with {…} b` (twice), `consume do { .r 1 } next`,
    and a path after a braced `do`, as in `do { .r value }.result`
    (three times).
  - `after-block`, allowed: `match match a with {…} with {…}` (three
    times); `|>`, `*` and a postfix `if` resuming after a layout `do`
    (six); `do { … } -> y`.

Forms the term parser still delegates are not covered: `let`, `using` and
`object` inside another form's region, and views led by `let`, `object`,
`where` or a `with` suffix.

**Findings.**
- No sample or doc example breaks the rule. Every hand-written test that
  does pins the behavior the rule changes, and would become an error test.
- The corpus is small and written in today's style. The survey shows that
  the rule costs existing code nothing, not that it reads well; that
  remains the maintainer's judgment.
- A consequence not discussed before: a path suffix after a braced body,
  as in `do { … }.result`, is an error, since nothing resumes after a
  dedent with `.`. Syntax.md calls braced `do` an atom, and three tests
  use the suffix.

**Open questions**, each settled as recommended (maintainer, 2026-10-16):
1. **A path suffix after `}`** is an error, for consistency;
   `(do { … }).result` remains.
2. **`where` after an inline tail**, as in `f x = if x then a else b
   where …`, is allowed: `where` attaches to the whole definition, so it
   ends the region.
3. **`with`, `then` and `and` after a block end**, as in a match nested in
   a subject, are allowed by the parser; whether a linter flags them waits
   for the linter.

**As built (2026-10-16).**
- The term parser's token pass checks a tail form where it closes: a
  conditional, a hanging `do` block or hanging arms closed by a keyword or
  arrow on their line fails, unless the conditional's `else` branch ends
  with a block. Each frame records the first `\` of its current member, so
  a lambda that a guard's `and`, a `then` or `else`, a match's `with`, an
  arm mark or a bind's `->` would end fails too, as does one ending a
  postfix `if`'s fallback.
- The item interpreter rejects an argument or a path after a form piece.
- The first violation fails the view: "parenthesize this `if` expression:
  `->` follows it, so it is not in tail position".
- `let`, `using` and `object` are still delegated to the structural
  parsers, and follow the rule when they become frames in
  `parser-keyword-frames-suffixes`. So does an inline `let` binding that
  `in` ends, as in `let f = \x -> x in f 1`, which the rule rejects.
- **The oracle.** The Chumsky grammar and the structural parsers predate
  the rule, which only rejects. An expression the term parser rejects is
  compared as the term parser reads it with the rule off (a test-only
  switch), and the reference parse runs with it off too, since the
  structural parsers call back into the term parser.
- Tests that pinned the old readings now check the errors and the
  parenthesized forms; `tail_forms_stand_alone_only_in_tail_position`
  states the rule in one table.

#### Separators (2026-10-16)

*Proposed by the maintainer after the first version; decided the same day
(`tail-forms-end-at-separators`).* The first version rejected
`if a then if b then c else d else e`, `if c then .a else .b -> P` and
`if any xs \x -> x > 0 then …`, though each has one valid reading. Every
prefix `if` and `try` has an `else`, so their keywords pair as brackets do,
and nothing in an expression continues with `then`, `else` or a binding's
`->`.
- **The test.** A keyword ends an open tail when no expression continues
  with it: `then`, `else`, `in`, `where`, `=>`, `=>?`, and a binding's or
  view's `->` are *separator keywords*, like `,` and `;`. `and` between
  guard clauses (a boolean `and` could continue the tail), `with` after a
  match's subject (an update could), and `when` and `as` (patterns
  continue with them) are not.
- **The rule.** A tail form runs to the end of its region, which closers,
  separators, line ends and separator keywords end. After its `}` or a
  dedent only an operator or a keyword may follow, never an argument or a
  path.
- **To the linter:** deeply nested inline `if`s, and a lambda before a
  binding's `->` (`f \a -> E -> P`), which reads like a curried lambda
  but has only one reading.
- **As built.** A token ends a tail without error unless it could continue
  it (`continues_tail` in the pass), so the lambda checks remain only at
  `and`, `with` and an arm's `when`.

### Further items

Each needed options, a recommendation and the maintainer's decision.
Items 1–4 were decided as proposed and built on 2026-10-16, and items 5–10
on 2026-10-17, those that change the grammar with an entry in
`Decisions.md`. Item 15 was decided on 2026-10-17 and waits for its step.
1. **`{` after `with`** (`brace-after-with-delimits-arms`). Whether it began
   a layout arm (a dict pattern) or braced arms depended on what followed
   its matching `}` on its line. Decided: a `{` on the line of `with`
   delimits the arms, a line break begins layout arms, and anything else a
   hanging arm; a dict-pattern first arm moves to the next line.
2. **`when` in an arm** (`when-ending-its-line-heads-nested-arms`). A guard
   or nested arms, depending on whether an arm mark followed later on its
   line. Decided: a `when` that ends its line heads nested arms; any other
   begins guards, which may now continue onto later lines before the arm's
   arrow, so this item also accepts more. A `when` before a `{` keeps the
   old reading; see item 12.
3. **Prefix or postfix `if`** (`conditional-after-an-operand-is-postfix`).
   `f a if b then c else d` and `f a if b else c` differed only at the
   `then` or `else` after a condition of any length. Decided: an `if` after
   an operand is postfix, so a prefix `if` as an argument is parenthesized,
   and a prefix `try` too, since `if` and `try` follow the same rules, as
   `match` and `try_match` do.
4. **`and` in guards** (`guards-are-an-and-chain`). `if a or b and c then …`
   read as `(a or b) and c`, while `x = a or b and c` is an error, since
   `and` and `or` have no precedence relationship. Decided: a guard is an
   ordinary `and` chain whose operands may be bindings, so the same error
   applies.
5. **One `=>` per arm's first line** (`nested-arrows-share-an-arms-line`),
   kept from the structural parser, which split arms by scanning. With
   item 1, a second arrow belongs to a nested match's hanging arms.
   Decided and built: the rule is dropped, so `p => match b as q => r`
   nests a one-arm match, and two arrows of one arm are still an error. A
   linter may flag same-line nesting.
6. **Roles of one spelling.** `->` is a lambda arrow, a bind and a view
   arrow; `with` ended a match's subject, opened an object body and
   updated; `when` begins guards, nested arms and `match when`. After item
   11, `with` means only "with these definitions", and `when` heads guarded
   alternatives in each role. Decided: keep the roles; the separator test
   tells `->`'s apart, and the linter flags `f \a -> E -> P`. The spelling
   of the update alias, `Dict as Name with Defs`, the one role of `as` that
   continues an expression, is settled separately.
7. **`where` by indentation** (`where-scopes-its-definition`). Changing only
   the indentation of a `where` could change its owner. Decided and built:
   a `where` scopes the whole definition, statement or arm it ends, or a
   parenthesized expression, and `let … in` or `(e where …)` scope less.
   See "As built" below.
8. **Postfix `if` binds names used to its left**, as in
   `value if (value, rest) = decompose input else fallback`. Decided: keep
   it, as comprehensions do, with a lint.
9. **Resumption after a dedent.** Closed by the tail rule, with no change:
   after a block's dedent only an operator or a keyword may follow, so
   indentation alone tells the layout pass that a line continues the
   enclosing statement, and whether the line is valid is ordinary parsing.
10. **The trailing bind `E -> P`**, the one place a pattern follows its
    expression, against the lambda arrow. With separators, `->` ends any
    open tail, and a lambda before it is a lint. Decided: keep it, since
    it makes columns of `.op args -> result` work.
11. **`match Expr as Arms`** (`match-arms-follow-as`; decided and built
    2026-10-16; the maintainer may revisit `Dict as Name with Defs`). `as` would
    replace a match's `with`, leaving `with` one meaning, "with these
    definitions" (object bodies, `extend`, updates). After `as` come pattern
    arms, as `P as Q` views a value through a pattern, and after `when`
    guard arms, as in `match when` and an arm's trailing `when`. But
    `Dict as Name with …` (an update naming its base) lets an `as` continue
    an expression, so `as` would not be a separator keyword, and an aliased
    update in a subject would still need parentheses, as an update does
    today. Options: accept that; change the alias spelling so no expression
    continues with `as`; or choose another keyword. Decided: accept it.
    **Found while building it:** the lexer's layout check picks a `{`
    group's separator from the token before it, and `as {` would need `;`
    for braced arms but `,` for an as-pattern such as `p as {a:x, b:y}`.
    Only scope tells them apart: the first `as` after a match head at its
    depth is the match's. Options: give the layout check that scope (a
    stack of open match heads per depth, as the parser keeps owners);
    separate braced arms after `as` with `,`; or choose a keyword with no
    other role, such as `of`. Decided: give the layout check that scope.
    **As built:** the lexer records each match's `as` in one pass, beside
    each token's enclosing group (`layout::match_arms_keywords`), and the
    `{` separator check consults it. The term parser claims `as` as it
    claimed `with`; in a subject `as` is a separator keyword, since nothing
    there continues with it, so `match if a then b else c as …` parses.
    A script renamed the samples, tests and standing docs, replacing the
    first `with` after each match head at its depth; history docs keep the
    syntax of their time.
12. **`when` before `{`** (`braces-open-blocks-on-their-keywords-line`).
    `p when {c => r}` headed braced nested arms and `p when {} <- e => r`
    began a guard binding a dictionary pattern, told apart by whether an arm
    mark followed the group on its line. Decided and built: braced nested
    arms after `when {`, with a parenthesized dictionary pattern in a guard.
13. **`do` and a `{` on the next line** (the same decision). `do` skipped
    line breaks to find its `{`. Decided and built: item 1's rule applies to
    `do`, and a formatter is expected to favor a `{` on its keyword's line.
    `do` followed by `{}` alone on the next line changes meaning, from an
    empty braced block to a layout block holding an empty dictionary, so
    the oracle now compares the legacy reading wherever the readings differ.
14. **Errors for item 1.** Built: a misplaced `{` after `with`, `when` or a
    nested `when` reports the rule.

15. **Aliases and locals as body declarations**
    (`aliases-and-locals-are-body-declarations`; proposed and decided by
    the maintainer 2026-10-17, built in `grammar-body-declarations`).
    `as Name` in object and `extend` headers and `Dict as Name with Defs`
    named a body's alias outside the body, and the update form let an `as`
    continue an expression (item 11). A helper that needed the alias had
    to be a `where` on each member, a member leaking into the result, or a
    `let … in` around the update, which cannot see the alias. Decided:
    - **`alias Name`** is a declaration that may stand only as the first
      member of a `with` body (object, `extend` or update), at most once.
      It binds `Name` (the result) and `_Name` (the prior value); `alias
      _Name` marks it as possibly unused, `alias self` localizes an
      update's names, and `alias _` states that there is none. Defaults
      are unchanged: `self` for objects, none for updates. `alias` is
      reserved in `g0`. Headers lose `as Name`, so `as` no longer
      continues an expression, and the error for an aliased update in a
      match's subject retires.
    - **`let` members** are body-private bindings, not members of the
      result. A body's `let`s form one mutually recursive group, visible to
      its members and each other, not to the base expression or the
      `extends` list. Lowering wraps the update inside the mixin function:
      `\prior instance -> let … in prior with …`. Like any local, a `let`
      is never referenced as `_name`, and a name it shares with another
      local, or with a member in scope, is a shadowing error.
    - **Modules** take `let` declarations too, as file-private helpers;
      a module's alias is implicitly `module`.

    **Refined (maintainer, 2026-10-17):**
    - **Groups are sequential.** A `let` group, in a body or a file, is
      mutually recursive within itself, and what follows it sees it;
      nothing before it does. `_module.foo` and a body's `_alias.foo`
      mean the namespace where they are written, and macros see only what
      precedes their invocation, so a group sees what precedes it.
      Recursion with later definitions goes through `module.x`.
    - **A module `let` is compiler state**, a file-level binding
      environment: the compiler may do only what a user-defined front end
      can, and the only hidden names, `abstract_global_path`, are bound to a
      place in the namespace, not to a file. Each group is lowered once at
      its position, against the module's final self and the definitions
      visible there, and later declarations bind its names to that shared
      value. A macro invocation's captured environment includes the `let`s
      before it, so a file-private helper or macro needs no export.
    - **One file, one name, one meaning.** The shadowing checks block a
      `let` named like any name the file uses to reach a global, even only
      in another definition's value, and like any local in scope; only an
      explicit `module.tmp` reaches past a local `tmp`.
    - **`alias self`** localizes an object's or an update's names, as the
      default does for objects; an explicit object alias no longer makes
      bare names read the parent's definitions when it is `self`.
    - **`_x` naming a local** is an error, for every kind of local; today
      it reads the prior definition `x`.

    **Alias syntax as built (2026-10-17):**
    - `alias Name`, `alias self` or `alias _` is a `with` body's first
      member, at most once, in object, `extend` and update bodies; a header
      `as Name`, including a match subject's `Dict as d with`, reports the
      move; a module-level `alias Name` says where an alias belongs.
    - Names are localized iff the alias is `self`, written or by an
      object's default. `alias _` names nothing and leaves names to the
      host scope for objects too, as Syntax.md's `object _ as _ extends
      Object` reading of `Object with Body` always said; before, an
      object's `as _` localized.
    - An update in a match subject needs no parentheses.

**Item 7 as built (2026-10-17).
- **Where a `where` ends.** The token pass closes the `else` tails before a
  `where` and stops the view there; a statement or arm holding one, and a
  parenthesized group, are parsed from their views. The structural parsers
  no longer attach a `where` to any expression they parse: an expression
  may end at one, and only a body takes it (`parse_body`): a declaration's
  or binding's right-hand side, a statement's or an arm's side, or a
  group's contents.
- **One `where` per body.** Its group ends at its `}` or its layout
  block's end, which ends the body; the legacy grammar ended a group at the
  next `where` anywhere and chained the next. The maintainer dropped chains
  (2026-10-17): they no longer compose as an expression, and one group says
  what they said.
- **Errors** name where the `where` stands: in guards, a `then` branch, a
  subject, an arm's head, a list, dictionary or tuple item, or after a
  bind's pattern; at a statement's or arm's anchor; as a second `where`; or
  before anything else after its bindings.
- **Cost.** `chain_where`, one line of `where`s, now nests each group in
  the binding before it, and parsed in about five times the CPU time at
  1,600 bindings: each nested binding's structural parse rescanned the rest
  of the line. Resolved the same day: the suffix slice's `where` frames read
  it in its earlier time.
- **Found while building it.** A `where` in a layout `do` statement failed
  to parse, and a parenthesized `where`, `let` or `with` was rejected as an
  argument, in guards and in statements: view-level dispatch read only
  whole views. Both parse now.
- **Legacy reading.** Unlike the earlier items, this one changes the
  structural parsers, since they parse `where` in both readings until
  `parser-keyword-frames-suffixes` makes it a frame; each change checks the
  legacy switch. Two changes alter meanings rather than reject: a trailing
  `where` scopes a whole conditional or `let`, no longer its last branch or
  body, and `Body where x = y where y = 1` nests the second group in `x`
  (the agent's reading: a `where` binding is a definition). The oracle
  compares the legacy reading wherever they differ.

## Method

- One row per construct: where it ends, the roles of its tokens, its parse
  class (local, layout, bounded or unbounded lookahead, context), and the
  dimensions it strains.
- The target grammar as EBNF over layout tokens, checked by an LR(1)
  generator. Its conflicts roughly mark where a reader must look ahead too.
  A tree-sitter grammar for editors could follow.

## Sequencing

- `parser-keyword-frames-suffixes` waits for this review: `let`, `where`,
  `with`, `using` and `object` carry several items.
- **Order (maintainer, 2026-10-17):** `parser-keyword-frames-suffixes`,
  then `parser-grammar-retirement`, then `grammar-body-declarations`
  (item 15), then the [tag prefix syntax plan](TagPrefixSyntax_2026-10-16.md),
  before other work. The frames replace the structural parsers under the
  oracle with the syntax unchanged; once the legacy parsers retire, each
  syntax change goes to one parser. `grammar-body-declarations` changes the
  declaration vocabulary that module, object and `with` bodies share, and
  resolution, lowering and name analysis with it; a script moves each
  `as Name` into its body, as the `as` rename did.
- Grammar changes go to the term parser only. The Chumsky grammar and the
  structural parsers are not updated to keep the oracle; the grammar's own
  examples and a round-trip test (generate trees, print them with the
  fewest parentheses, parse, compare) replace it. The printer is a start on
  the formatter.
- Until the legacy parsers retire, the term parser can read the legacy
  grammar (a test-only switch, `with_legacy_grammar`). Where it and the
  legacy parsers disagree on whether an expression parses, the oracle
  compares that reading instead, and the reference parse always uses it,
  since the structural parsers call back into the term parser.

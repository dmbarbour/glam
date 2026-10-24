# Glam Initial Syntax

This document describes an initial syntax for ".g" files, and design motives for it. Design goals include:

- a syntax that I find pleasant to work with
- supports an assembly programming look and feel
- concise, vertical columns of assembly mnemonics
- generalist, not specialized for targets or domains
- extraordinarily high abstraction ceiling

## Language Version Declaration

Reproducibility requires that the same sources produce the same outcome, but there is an implicit condition: an outcome is produced. A language version declaration simplifies reproducibility because the compiler can fail fast, refusing to produce any outcome rather than a result that drifts as the compiler is updated.

Proposed syntax:

        language (BaseVer) (with Extensions)?
        language g0 with utf8

The version declaration should be the first toplevel declaration in a ".g" file. The BaseVer is a recognized name for a package of features. Extensions may modify that package. The parser for extensions may flexibly depend on BaseVer.

In practice, if a compiler halts on language version, we must be using different executable or configuration (if configuration defines `conf.env.lang.["g"].compile`) from development conditions. Users can resolve this by reproducing the executable (e.g. via nix) or by defining compatible compilers in the module system.

## Character Set

We'll start with printable ASCII and some whitespace (0x21-0x7E, SP, CR, LF). It is not difficult to extend to UTF-8, though I'm concerned about legibility. We'll recognize CR, LF, and CRLF as line endings. The compiler shall emit a warning if the file uses inconsistent line endings.

## Comments

We'll support Python-style line comments, i.e. `#...` to end of line. There are no multi-line comments. An editor with vertical selection is recommended if users intend to comment out large sections of code. Comments are treated as whitespace by compiler and macros, but may be structured for purpose of external tooling (literate programming, projectional editing, extracting API docs, etc.).

## Toplevel Structure

The module toplevel consists of a sequence of 'declarations'. Each declaration
starts a new line. If a declaration requires more than one line, every
nonblank continuation line must be indented by at least one space. A terminal
line containing only closing delimiters and an optional comment may instead
align with the declaration boundary:

```g
result = consume (
  do
    .r value
)
```

This exception is terminal punctuation, not a general continuation rule. If
the expression continues after its closing delimiter, that line must remain
indented:

```g
result = consume (
  do
    .r value
  ) |> finish
```

A later indented line may not resume a declaration after a boundary-aligned
closer. The same rule applies relative to the indentation of a declaration
nested in an object body. The goal is to simplify error isolation, local
reasoning, and parallel processing of declarations without sacrificing the
ordinary aligned-closing style for delimited declarations.

Balanced `()`, `[]`, and `{}` groups are hard expression boundaries, but
newlines inside them still validate the visual structure. Commas and
semicolons remain the only member separators; indentation never supplies a
missing separator. Content written on the opening delimiter's line does not
set an indentation anchor. Instead, the first later line that begins a member
after a separator or contributes a leading separator establishes the group's
content anchor:

```g
dense = [1,2,3,4,
  5,6,7,8]

leading = [
  ,1,2
  ,3,4
  ,5,6
  ]
```

Later member or separator contribution lines align with that anchor. Other
lines remain ordinary continuations of the current member expression and do
not move the anchor:

```g
values = [
  build
    first_input,
  second_value
]
```

When the first item itself begins after the opening line, it establishes the
same anchor. Every content line remains strictly to the right of the enclosing
expression's floor. Closing delimiters retain the terminal-closer rule above.
These acceptance rules permit dense and leading-separator styles; a formatter
may consistently choose a stricter house style.

### Layout Bodies and Expression Resumption

A declaration, object member, local binding, or `do` statement establishes an
exclusive continuation floor. Its continuation lines must remain strictly to
the right of that floor. The position of an inline right-hand expression does
not establish a new floor:

```g
result = Operation1 >>= \r1 ->
  Operation2 r1 >>= \r2 ->
  finish r1 r2
```

A layout body takes its sibling anchor from its first member. An inline first
member uses its token column as a hanging anchor; a first member on a later
line freely chooses an anchor to the right of the enclosing floor. Lines at
the anchor begin siblings, while deeper lines continue the current member.

A final child that owns the remainder of its host expression inherits the host
floor. This is why a final lambda, `do`, `let`, object, or `with` expression
does not require indentation to drift progressively right. A dedent closes a
layout body and leaves its boundary unconsumed. Only an enclosing grammar that
expects a postfix or infix continuation may resume there.

A `where` scopes the whole definition, statement or arm it ends: a
declaration's, member's or local binding's right-hand side, a `do`
statement, or a match arm's result. A parenthesized expression is a body
too. A `where` ends the open tails before it, so in
`f x = if x then a else b where …` it scopes the whole conditional. One
that would scope only part of an expression, as before an `else` or in a
match's subject, is an error; write `(e where …)` or `let … in` there.
Layout decides which definition, statement or arm a `where` line belongs
to, as for any line, but indentation never selects a smaller expression.
One dedent may close several nested `with` or object bodies:

```g
configured = source
  |> configure with
    A := 42
    B := derive A
  |> finish
  where
    derive = transform
```

Here the `where` scopes the whole definition. Indented past `B`'s column,
it would continue `B`'s definition and scope that instead; at the members'
anchor it would begin no member, which is an error. Nothing may follow a
`where`'s bindings.

A leading infix operator uses the same yielded boundary. The first leading
operator establishes a resumption anchor; later leading operators in that
chain must align with it:

```g
result = .r pair
  <! do
    input <- .read
    .r (transform input)
  <! .read
```

The recovered operators form one ordinary infix chain. Newlines do not change
precedence or associativity. A trailing operator remains an incomplete-right-
operand continuation:

```g
result = source |>
  decode |>
  finish
```

The operator indentation is the continuation floor of its right operand, so a
nested layout body must begin strictly farther right. A formatter should make
these ownership relationships visible by aligning siblings and resumption
operators. It may choose a fixed indentation increment or add parentheses and
braces, but it must preserve the parsed grouping.

Each declaration starts with either a keyword (such as `import`, `object`, or `unique`) or is a basic definition of form `name = Expr` or one of its variants (args in lhs, `:=`, `::=`, etc.). We'll favor basic definitions where feasible, thus keywords are mostly for special forms.

In context of errors, the errors can be reported but we can also make a best effort to proceed with errors. This might depend on configuration options or command-line arguments.

## Keywords

Keywords are names reserved by the selected language version. Users may not
introduce them as definitions or locals, use them as ordinary references, or
use them as bare path roots. Reservation is independent of the position in
which a keyword normally has meaning: contextual words such as `where` and
`as` do not become ordinary names merely because they occur outside a valid
`where` or `match`.

The active `g0` table is:

| Role | Keywords |
| --- | --- |
| declaration heads | `language`, `import`, `declare`, `assume`, `unique`, `object`, `abstract object`, `extend`, `alias`, `let` |
| expression forms | `abstract_global_path`, `let`, `where`, `using`, `do`, `if`, `match`, `try`, `try_match`, `object`, `abstract object` |
| do statements | `declare` |
| expression operators | `and`, `or` |
| special expression references | `module`, `module_origin`, `self` |
| special object/`with` alias | `self` |
| contextual modifiers | `abstract`, `as`, `at`, `binary`, `else`, `extends`, `in`, `then`, `when`, `with` |

A word may have more than one role, but the parser still has one
version-owned source of truth for whether it is reserved.

Explicit key positions are not lexical names and may use keyword spellings.
For example, `'where` is atom data, `module.where` and `self.where` select
members, `where:Value` is tagged data, and `.['where] = Value` introduces a
keyword-named root definition through a computed key. Quoted paths and effect
request paths such as `'.where` and `.where` likewise remain valid. Bare
spellings such as `where = ...`, `\where -> ...`, and an ordinary reference to
`where` are errors.

Words proposed for later syntax do not become reserved until their
language-version feature is introduced. The recognized table may therefore
vary by base language version and extension without retroactively changing an
older version.

*Note:* Pre-release, a language version may freely adjust keywords, as reproducibility
is not an issue yet.

## Names and Paths

We accept a subset of C names, mostly restricting use of underscores. A viable regex:

        Part = [a-zA-Z][a-zA-Z0-9]*
        Name = Part('_'Part)*
        Path = Name('.'Name)*

Namespaces are modeled as hierarchical dictionaries, accessed via dotted path, e.g. `foo.bar.baz`. To index the dictionary, we translate each names into an atom, i.e. name `foo` translates to atom `'foo` (see *Atoms*), which is used as a key for a dictionary. Users may similarly quote path suffixes into lists, e.g. `'.foo.bar.baz` evaluates as `['foo, 'bar, 'baz]`. 

In the general case, we also support expression-indexed paths using `.(ListExpr)` or `.[...]` for a literal list. These indices are interpreted such that `.([1, 'two] ++ [3])` is equivalent to `.[1].two.[3]`. The empty list is permitted, e.g. `foo.[]` is equivalent to `foo`, and `foo.[ ].bar` admits spaces, newlines, and comments in names if needed.

Best practice is to avoid expression-indexed paths in module or object namespaces, but it's available as an escape hatch for integration. Users may define `.[Idx] = Def` at the module toplevel. Later access to this name requires `module.[Idx]`. Users may understand `module` as a keyword that aliases the module toplevel namespace, and `self` as the current object namespace (`self` aliases `module` at toplevel to simplify macros).

`module_origin` is the opaque assembler provenance token for the current
source compilation. It is stable and shared throughout that source input, so
users may place it in their own static context frames without exposing paths:

        guarded = anno context:{origin:module_origin} Expr

When several source inputs contribute to one final module, each input receives
its own origin. The token is not a namespace and has no `_module_origin` prior
form. Reflection may inspect it explicitly through the capability described
under [Errors](#errors).

`abstract_global_path StaticName` constructs an abstract path qualified to the
current module:

        abstract_global_path foo
        abstract_global_path foo.path
        abstract_global_path module.foo.path

The operand is static syntax, not an evaluated expression. An unqualified root
is accepted only where ordinary resolution would bind that name through the
module namespace. A lexical local, object member, object alias, or `using`
namespace therefore cannot accidentally determine a global identity; use the
explicit `module.` form when escaping such a scope. The current representation
restricts the path to name components. A later representation may admit static
non-name keys such as `abstract_global_path module.[42]`.

### Introductions and Overrides

When defining names, we'll distinguish introductions versus overrides. An introduction `name = Expr`. An override uses `name := Expr`. It is an error to introduce a name that is already defined, or to override a name that isn't already defined. This resists ambiguity issues, i.e. a name is introduced with some intention or purpose, and overrides should preserve purpose. 

Users refer to prior versions of names via `_name`, i.e. `_` prefix. This applies consistently across modules and objects. A local has no prior version, so `_name` is an error where a local `name` is in scope; one name has one meaning in a file, and `_name` cannot reach past the local to a definition.

The compiler enforces explicit overrides by implicit assertions on the prior namespace: `name = Expr` asserts that it does not define `name`, and `name := Expr` that it does. They ask only whether the path is defined, as `dict.defines` does, never reading its value, and an empty dict `{}` is a definition like any other (`dict-empty-is-a-value`). As an escape hatch, I propose a non-observing `name ::= \ prior -> Expr`. This also serves as an in-place update, i.e. `name ::= Update`. Users have more freedom with `::=`. 

### File-Private Bindings

A file may hold `let` declarations: private helpers that are not module
entries. A `let` declaration has the bindings of a `let` expression: one
binding, a hanging layout group whose first binding sets the column, a braced
group, or a group on the indented lines after `let`:

        let greeting = "Hello"
        let
            twice f x = f (f x)
            shout s = s ++ "!"

Each group is mutually recursive within itself and visible only to the
declarations after it. The compiler lowers a group once where it stands,
against the module's final self and the definitions visible there, so `_name`
in it reads the prior definition at that point, and the declarations after it
share its values. A `let` is compiler state rather than a module entry, since
a compiler may do only what a user-defined front end can; `module.greeting`
never reaches it.

Its names are locals, and a name has one meaning in a file: no definition or
use of a global, nor another local, in the file may share them. Within a
localized body, such as an object's, a file's `let` is `^name`, as the
module's own names are. A macro invocation sees the `let`s before it.

### Declarations (Tentative)

To localize errors, and to simplify analysis of name shadowing, names in use shall be defined or declared. A lightweight declaration names what a module expects to be defined, e.g. by a mixin, much as an abstract object declares methods:

        declare Name(, Name)*
        assume Name(, Name)*

`declare` names what must be defined by the time a module's definitions are complete, e.g. by a later mixin; `=` may define it once and `:=` may override it. `assume` names what is already defined outside the module's journal, e.g. by a host or by a dict used as a module; `:=` may override it, and `=` conflicts.

Both also appear in object bodies, recorded in the object's own journal; dict `with` bodies keep no journal and reject them. Compilers record both in the namespace's definition journal, `meta.journal`, beside each introduction (`=`), override (`:=`) and update (`::=`). A namespace's reflection boundary replays its journal when a definition in it is forced, and reports introducing a defined name, overriding an undefined one, and a declared name never defined. Hosts and compilers journal what they provide: the assembler introduces `asm.args` and `env` and declares `asm.result`, and `import ... as` introduces `env`.

### Associated Names (Convention)

In many cases, we'll want to associate one name with another. The proposed convention is a dict named with an `_of` suffix. For example, given a name `foo`, we can also reference `type_of.foo`. The assembler ignores associated names, and I anticipate users mostly work with such names indirectly.

### Final Definitions (Convention)

In some cases, it is useful to guard against accidental updates to definitions. The most obvious example is to block accidental updates to an object instance because users should be updating the specification instead. But it's important to preserve the ability to update definitions regardless. 

To this end, we might use a pattern such as defining `final_of.foo = _foo`. We can assign a reflection task to verify.

### Forbidden Shadows

Name shadowing, where a function argument or local variable accidentally masks another name defined or declared in lexical scope, is a common source of subtle bugs. Humans are a lot more flexible about referential context than our compiler, thus easily overlook the error when reading code. To resist this bug, we'll report an error for local name shadowing.

The bootstrap compiler rejects shadowing between local variables. This includes duplicate parameters, nested lambda or `let` bindings, and suppressed spellings
such as `_name` shadowing `name`; both spellings have the same canonical local name. The inaccessible `_` binder may be repeated because it introduces no
referable name. Reusing a name in disjoint lexical scopes is valid.

The compiler also checks each source file as a whole. A source local may not reuse a global name defined by that file in a visible namespace, or a
global root that the file actually selects through that namespace. Declaration order does not affect this rule. Literal keys and explicit `module` or prior
references do not select an unqualified global; expression-valued keys do. Names that merely exist in an imported or extended namespace may be used as locals until the file introduces, overrides, or otherwise references them.

### Using Scopes

A using scope enables any dictionary as an object namespace.

        using Dict in Expr
        using Dict do Body      # short for `using Dict in do Body`

The first `do` after a nonempty namespace begins the body, so a namespace
that ends with a `do` block is parenthesized.

The namespace expression is bound outside the temporary scope, then lazily
evaluated and shared when that namespace is observed within `Expr`. Names
within the scope are localized as in an object body, and that includes the
lexical locals outside it: in `let tmp = 42 in using d do ... tmp ...`, `tmp`
is `d.tmp` while `^tmp` is the local. A using scope is thus a controlled,
coarse-grained form of shadowing in a language that otherwise blocks it: easy
to reason about where needed, and easily abstracted via its namespace
argument. Within the scope, `self` is equivalent to `Dict` and `_self` is
equivalent to `{}`, which defines nothing, so reading `_name` there is an
error.
Users escape to the surrounding scope just as they do for objects, via
`^name` or `^(Expr)`, as in `write message = using api do ... ^message ...`. `Dict` needs only to provide the dictionary members that
are actually observed; it does not need to be a valid object and gains no
object specification implicitly.

The main use case for `using` is to manage namespaces without polluting them. Not suitable for subexpressions that require many escapes.

### Unused Locals

An unused local, e.g. a lambda or let var, will report a warning, but this may be suppressed by use of `_name` when introducing the local.

        # assume foo undefined, bar defined
        let  foo = 42 in foo    # ok (basic use case) 
        let  foo = 42 in bar    # warns (unused foo!)
        let _foo = 42 in foo    # ok (no '_' in rhs!)
        let _foo = 42 in bar    # ok (error suppressed)

Motives for the `_foo` form include TBD code or if it's unclear whether macros will use names. 

Users may also write just `_` if they know a value won't be used, e.g. `skip _ y = y`. This is much less useful in context of `let _ = 42 in bar`, but still valid.

### Module Metadata

In some cases, compilers need to thread some state through toplevel imports, e.g. to track declared names. To support this, the compiler will store such metadata under `meta.*` within the module. This is also visible to users, e.g. macros can support similar features or as an interaction surface with the compiler. Alternatively, we could use a name that is difficult to write. But it seems better to just be open about it.

### Reflection Tasks

The compiler will arrange to automatically run `refl.*` definitions as reflection tasks. The assembler doesn't interpret `refl.*` implicitly, so this arrangement must be expressed as a compile-time effect or (very awkwardly) a term annotation. 

## Operators

Operators are essentially infix functions. We'll support Haskell-style
operator sections, such that `((>>= k) op)` is equivalent to `(op >>= k)`.
Precedence is deliberately a partial relationship rather than one total
conventional ladder. Homogeneous chains such as `a + b + c`, `a * b * c`,
`A and B and C`, and `A or B or C` remain concise. Distinct arithmetic
operators have no implicit precedence, and neither do `and` and `or`:

```g
a + (b * c)          # parentheses required
(a * b) / c          # parentheses required
A or (B and C)       # parentheses required
```

For `or`, parentheses determine grouping but do not delimit its effectful
choice. See *Conditionals* for the distinction between raw branching and a
complete search enclosed by `.cut`.

The same explicit-grouping rule applies to opposing directional operators
such as `>>` and `<<`. Other deliberately useful relationships remain, such
as arithmetic within a comparison and comparisons within a homogeneous
boolean chain.

We may support a few special non-binary forms, e.g. `(x < y =< z)` as shorthand for `((x < y) and (y =< z))`. We'd also support `(< =<)` operator sections. Risk of confusion is mitigated because we cannot compare booleans for less-than or greater-than.

Operators may support limited ad-hoc polymorphism. For example, `>` will only compare two numbers, two lists, or two tuples. For lists and tuples, we use a lexicographic comparison of elements. Comparing a number to a list, or even a list to a tuple, would simply diverge with an error. As a rule, ad-hoc polymorphism must preserve laws or intuitions, e.g. don't use `+` to append lists because it does not preserve commutativity of `+` on numbers.

Only comparables compare (`only-comparables-compare`): numbers, and lists or
dicts of comparables, texts and binaries among the lists and atoms among the
dicts. `==` and `<>` take any two comparables: two of different kinds are
unequal, two dicts are equal where they define the same keys with equal
values, and an atom equals the dict it stands for, `'tag` and `{tag:()}`.
Ordering is for numbers, lists and tuples, never other dicts. Comparing
anything else, a function, a net, an opaque value, a volume handle, a
resolver or a sealed carrier, for equality or order, directly or within a
list or dict, is an error that reports it incomparable, so `.try` answers it
`err`, never `'fail`. Nor is `x == {}` a test of definedness: `dict.defines`
asks that.

Subtraction and division are non-associative. Their repeated chains also
require parentheses.

### Application

Application is essentially expressed as a special whitespace 'operator', i.e. `f x` applies `f` to `x`. The compiler supports some ad hoc polymorphism for application:

- functions, including interaction nets wrapped by `net_arity`
- method objects, `{apply:f,_} x = f x`
- lightweight effects, `(eff:f) x = eff:(\api -> f api x)`

This is a compiler feature: it does not implicitly extend to other languages or definition of interaction nets. We'll generally model advanced features (multimethods, keyword args, hooks for observability, etc.) via method objects. 

A dot-leading effect path must be parenthesized when used as an application
argument. Thus `foo.bar` is member access, `foo (.bar)` applies `foo` to the
effect path, and `foo .bar` is rejected rather than allowing one accidental
space to change access into application. `.bar` remains valid at the head of
an expression; `foo <| .bar` is the punctuation-light alternative.

### Tail Position

Some forms end with a part that extends to the right: prefix `if` and `try`,
`match` and `try_match`, `do`, lambdas, `let`, `using` and `object`. These
*tail forms* run to the end of their region, and stand without parentheses
only where that end is visible:

- A region ends at a closing bracket, a separator (`,` or `;`), the end of
  its statement or definition, or a *separator keyword*: `then`, `else`,
  `in`, `where`, `=>`, `=>?`, a binding's or view's `->`, or the `as` after
  a match's subject. No expression continues with these where they stand,
  so a reader sees where the form before them ends.
- A tail form never ends at a token that could continue it, such as an
  `and` between guard clauses, which could be a boolean `and`, or the
  `with` after a match's subject, which could begin an update.
- A braced body's `{` and `}` stand for an indent and a dedent. After a tail
  form's `}` or dedent, only an infix operator or a keyword may follow: an
  argument or a path may not.
- Postfix `if` is an operator, not a tail form. After an operand, an `if` is
  postfix, so a prefix `if` or `try` there is parenthesized.

So a tail form may be a definition's whole right-hand side, the right-hand
operand of an infix expression, a spaced tag's payload, or any part a
separator keyword ends. It is never an application's argument, which is an
atom; `<|` applies a function to one instead:

        bound = operation >>= \value -> continue value
        chosen = describe <| match value as
            1 => "one"
            _ => "other"
        resumed = .cut <| do
            input <- .read
            .r (transform input)
        both = if ready then do { .go } else do { .wait }
        inner = if a then if b then c else d else e

`|>` and `<|` do not mix, so a pipeline stage that takes a tail form
parenthesizes it, or a `where` or `let` names it:

        processed = source
          |> process (do
            input <- .read
            .r (transform input))
          |> finish

Every prefix `if` and `try` has an `else`, so their keywords pair as
brackets do: an inner `if` takes the first `else`. Parentheses make a tail
form an ordinary atom, which anything may follow:

        applied = f (match x as {_ => 1}) y
        mapped = list.map (\value -> transform value) values
        field = (do { .r value }).result
        guarded = if x <- (if a then b else c) and d then x else 0
        argument = f (if a then b else c)

The lambda body has rightward extent, so `operation >>= \value -> transform
value |> finish` places `|> finish` inside the lambda body. Parentheses end
a lambda, as they do for an argument in a pipeline:

        values |> list.map (\value -> transform value) |> finish

What parses may still read poorly, such as deeply nested inline `if`s or a
lambda before a binding's `->` (`f <| \a -> e -> p`); a linter, rather
than the grammar, is to flag those (see [Lints](Lints.md)). The current
Rust bootstrap enforces the rule for conditionals, `do` blocks, matches and
lambdas; `let`, `using` and `object` follow.

## Effects

The target design adopts Haskell's do notation. For aesthetic reasons, it
supports both `Pattern <- op` and `op -> Pattern`; the latter is convenient for
vertical columns of assembly mnemonics. `Pattern = Expr` is the pure guard form
and does not use `let`. General pattern matching either captures locals or
evaluates to `.fail`.

The current Rust bootstrap implements pattern-bearing statements in both
layout and braced forms. Patterns currently include names, `_`, general
`P as Q`, unit, numbers, atoms, text, fixed quoted paths, list patterns with at
most one potentially refutable variable-length segment, computed-path
dictionaries and quoted paths, static and computed tags, tuples, views,
predicates, and local guards. Dictionary entries are required by default;
`path?:Pattern` matches the payload pattern against `[]` where the path is
absent and `[V]` where it holds `V`, so it never fails on absence. A path is
present where its key is, whatever its value, which a take never forces; the
dictionaries on the way are forced, and a take prunes those it empties, so an
exact pattern such as `{selector:key, root.[key]:value}` leaves no
`{root:{}}` behind. Entries take in written order, deeper paths before
shallower ones under them. Computed paths and effectful pattern expressions are
evaluated once in source order and may use captures from earlier subpatterns.
A layout block is newline-delimited:

        my_effect = do
            .read 'left -> left
            right <- .read 'right
            unit_op
            total = left + right
            .write total
            .r total

`Pattern <- Operation` and `Operation -> Pattern` are equivalent monadic
binds. `Pattern = Value` is semantically equivalent to
`Pattern <- .r Value`, but the compiler binds the value directly and emits no
synthetic `.r`; only refutable matching steps use effects. A single
irrefutable name remains optimized to ordinary lambda application. Structural
or literal mismatch invokes the ambient `.fail`. `_name` suppresses its
unused-local warning and `Operation -> _` explicitly discards any result. A
producing expression is resolved before its new names enter scope, and active
source locals cannot be shadowed.

A statement binds with `<-` or `=` only when that token comes before every
keyword form and lambda in it, since a pattern holds neither: a keyword form
owns any `<-` or `=` after its head. So `if (a, b) = e then A else B` is an
operation whose guard binds, while `x = if C then A else B` binds `x`. In
`Operation -> Pattern`, the `->` is a separator keyword (see *Tail
Position*): it ends any open tail of the operation before it, so
`if C then .a else .b -> x` binds the conditional's result. A layout block
keeps its own lines, arrows included. Guard clauses follow the same rules.

A non-final bare operation uses the existing `=>>` behavior, including its
requirement that the discarded result be unit. The final statement must
express an effect and is not implicitly wrapped with `.r`. A lone statement
still runs as one: `do X` is `.r () >>= \_ -> X`, so `do 42` fails when it
runs rather than standing for `42`. A layout block must be non-empty and
occupy the trailing position of its containing definition, lambda body,
enclosing do statement, or operator operand such as `<|`'s; it is never an
application's argument. A singleton may be written inline as `do Effect`.
An inline first statement may also establish a hanging block. Later statements
must align with that first statement's token column:

        read_pair = do first <- .read
                       second <- .read
                       .r [first, second]

This differs from `run do` followed by a next-line body: the latter freely
chooses its first statement's indentation above the enclosing expression
floor. Blank and comment-only lines establish neither form's anchor.

Braces delimit do notation and use semicolons as statement separators. The
`{` must stand on the `do`'s line; on a later line it begins a statement,
such as a binding of a dictionary pattern. The closing brace ends the block
as a dedent would (see *Tail Position*):

        inline = do { left <- .read 'left; .write left; .r left }
        nested = consume [do { .r first }, do { .r second }]

One leading and one trailing semicolon are accepted around a non-empty block,
so `do {; A; B;}` means `do { A; B }`; interior empty statements such as
`do { A;; B }` remain errors. A trailing semicolon is punctuation and does not
synthesize a result, so a block still cannot end with a binding. The special
separator-free `do {}` means `.r ()`, while `do {;}` is invalid. Semicolons are
owned by the nearest enclosing grammar: `do { x = do A; B }` has the two outer
statements `x = do A` and `B`; the inner singleton do does not consume the
semicolon. Computed-path/tag, view, predicate, and guarded do patterns remain
target syntax.

Lightweight effects are supported: we desugar `.name` to `eff:(\api -> api.name)`, and we support application `(eff:f) x = eff:(\api -> f api x)`. This enables us to work with APIs concisely without redefining things:

        my_loop = do
            .movl 'eax ['ebx, 4]
            ...

Aside from do notation, we'll support the `>>=` composition and `>=>` Kleisli composition, and `=>>` for dropping a unit result.

### Recursive Do

Use of fixpoint within do notation is not implicit. It's problematic for it to be implicit because it easily conflicts with shift-reset and features that build upon it. Instead, we'll forward-declare the names we need via `declare`.

        do
            declare foo
            ... wire foo up, but don't observe foo ...
            op -> foo
            ... at this point foo is defined ...

The compiler will leverage `.fix` to capture the name.

The current Rust bootstrap implements this explicitly declared, name-only
form. `declare Name, ...` is valid only as a non-final do statement and makes
those names visible to following statements. The first later direct bind or
pure name binding with the same canonical name fulfills each declaration.
References before fulfillment use the `.fix` future; references afterward use
the ordinary resolved local.

Each declared name has its own recursive interval from its declaration through
its fulfillment and lowers to an independently completable `.fix`. Thus
`declare X, Y, Z` creates three fixpoints with the same source start. The
compiler may reorder their nesting by fulfillment point without warning: a
name fulfilled earlier becomes observable through old captures of its future
while later names remain pending. Each `.fix` privately returns that one
resolved value plus a continuation; this payload is a compiler protocol, not a
stable source-level data shape.

Direct declarations in one do block may be disjoint, hierarchically contained,
or syntactically crossing. Disjoint intervals lower sequentially and contained
intervals lower as nested `.fix` requests. For crossing intervals, the later
ending fixpoint starts earlier so the resulting scopes are hierarchical. The
name remains unavailable to source expressions until its written `declare`
statement, but the compiler emits a warning because moving `.fix` changes its
scope relative to shift/reset. Reordering names declared together does not move
their shared source boundary and does not warn.

Missing fulfillment, duplicate declarations, and source-scope conflicts are
also diagnosed. Strictly observing an unresolved forward value follows the
standard fixpoint failure behavior. The selected effect handler must provide
`.fix` just as it must for an explicitly written `.fix` request. `_name`
retains warning suppression, but the inaccessible `_` cannot be declared.

### Applicatives

`!>` and `<!` support applicative style programming. These correspond to Haskell's `<**>` and `<*>` respectively. I despise Haskell's choice of syntax here. Note that `!>` and `<!` correspond to `|>` and `<|` for pure functions. 

        (!>) : Eff a -> Eff (a -> b) -> Eff b   # right associative
        (<!) : Eff (a -> b) -> Eff a -> Eff b   # left associative

We always 'run' these effects from left to right, preserving order. 

Their monadic expansions make that order explicit:

        mf <! mx = mf >>= (\f -> mx >>= (\x -> .r (f x)))
        mx !> mf = mx >>= (\x -> mf >>= (\f -> .r (f x)))

`<!` is left-associative and `!>` is right-associative. The opposing
directions have no implicit precedence relationship, so mixing them requires
parentheses.

Because `.r` is concise, users can directly write `.r f <! op1 <! op2`. No need for a `<$>` equivalent.

## Source Macros

`language g0` accepts a joint static macro path:

        @macro
        @macro.path

This selects `_module.macro` or `_module.macro.path` from the module
definitions visible before the declaration containing the invocation, or, when
a file's `let macro` precedes it, that binding or its `path`. The
macro definition should express an effectful, local source rewrite. It reads
source elements to its right, writes replacement elements, and disappears
before ordinary expression parsing.

Original invocations within one declaration expand once from right to left.
Generated source cannot contain another macro invocation or comment. Macros
preserve balanced `()`, `[]`, `{}`, and anchored layout structure, but do not
receive an AST, raw whitespace, comments, source paths, or local binding
access.

Primary macro parameters are via effectfully reading source tokens. But macros
also support implicit parameters via `_module.meta.macro.env`.

See [Source Macros](Macros.md) for details.

## Annotations

We'll express annotations as a builtin function.

        import 'anno
        anno : Annotation -> Term -> Term

Annotations are not observable within the computation, but may guide performance, debugging, and other use cases. To avoid silent degradation of performance or reasoning, the assembler shall warn about unrecognized annotations. 

Three annotations provide and transform sealed associated-metadata carriers:

```g
carrier = anno 'meta_init ()
pureUpdated = anno meta_pure:UpdateFn [carrier, otherCarrier]
reflectionUpdated = anno meta_refl:EffectfulUpdate [carrier, otherCarrier]
```

The first form creates the initial carrier. Each update returns one carrier
for each input carrier. `meta_pure` applies an ordinary function;
`meta_refl` runs a reflection task when its hidden result is demanded.
Ordinary expressions may transport these values but cannot inspect them as
unit or as metadata. Reflection uses `.meta.inspect` to test and inspect a
carrier. See [Associated Metadata](Design.md#associated-metadata) for the
boundary, demand, and update semantics.

## Local Definitions

We'll support Haskell-style locals. 

        # basic let-in form for one-liner
        let Name = Def in Body

        # the Body itself may continue a one-liner 
        # but Name and Def must fit inline
        let Name = Def in this is a large Body expr
          and requires multiple lines

        # common multi-line form does not use 'in'
        # Body indentation must align with 'let'
        let Name = Def
        Body

        # continue large Def by indentation past Name
        let Name = This is a very long definition and it
              continues on the next line past the name
        Body

        # braced form for semicolon-separated names inline
        # each braced or layout group is mutually recursive
        let { Name1 = Def1; Name2 = Def2 } in Body

        # braces may have one leading/trailing semicolon
        let {; Name1 = Def1; Name2 = Def2; } in Body

        # explicit empty group; equivalent to Body
        let {} in Body

        # An inline first binding establishes a hanging sibling anchor.
        # The body still aligns with `let`.
        let Name1 = Def1
            Name2 = Def2
        Body

        # A next-line first binding freely chooses an anchor above `let`.
        let
          Name1 = Def1
          Name2 = Def2
        Body

        # the 'where' form is essentially a post-hoc 'let'
        Body where Name = Def

        # semicolon-separated groups require braces
        Body where { Name1 = Def1; Name2 = Def2 }

        # explicit empty group; equivalent to Body
        Body where {}

        # multi-line version
        Body where 
          Name1 = Def1
          Name2 = Def2
          Name3 = This is a very long definition and it
            continues on the next line past the name

        # An inline first binding may establish the same hanging layout.
        Body where Name1 = Def1
                   Name2 = Def2

A local binding takes parameters as a module definition does, and the
bindings of one group may name each other in any order, themselves
included:

        # parameters are lambda sugar
        let add a b = a + b in add 1 2
        # equivalent to:
        let add = \a b -> a + b in add 1 2

        # a binding may name a later binding of its group
        let { y = x; x = 42 } in y

        # or itself, for a local recursive helper
        go 5 where go n = if n == 0 then 0 else go (n - 1)

        # or bindings that name it back
        parity 7 where
          even n = if n == 0 then "even" else odd (n - 1)
          odd n = if n == 0 then "odd" else even (n - 1)
          parity n = even n

A `where` scopes the whole definition, statement or arm it ends (see
*Layout Bodies and Expression Resumption*). Each takes one `where`, whose
one mutually recursive group holds all its bindings, so a second `where`
after that group is an error. A `where` binding is a definition too, so a
`where` after its value scopes that value:

        Body where x = y where y = 1
        # equivalent to:
        Body where x = (y where y = 1)

        Body
          where x = y
          where y = 1     # ERROR: a second `where` for Body
        Body where { x = y; y = 1 }

Naked semicolons do not delimit `let` or `where` bindings. They are reserved
for an enclosing braced construct such as `do { ... }`; use a braced binding
group when bindings need semicolon separators. Within a braced binding group,
one leading and one trailing semicolon are permitted, while an empty member
between semicolons is an error.

Aside from `let` and `where`, locals can be introduced by pattern matching. See *Conditionals*.

## Tagged Data

Tagged data is modeled as singleton dictionaries, built by the tag prefix
`path:`, which reads alike everywhere, in braces or out:

        tag:Data            # joint: tags one atom; same as { tag:Data }
        tag: f x            # spaced: tags the rest of its item, tag:(f x)
        (tag:)              # its section, the constructor: (\ Data -> tag:Data)
        :tag                # a pun on one bare name: tag:tag

Joint to its operand, a tag binds tighter than juxtaposition: `g tag:f x y`
is `g (tag:f) x y`, and `g tag:(f x y)` couples the arguments. Followed by a
space or line break, its operand is the rest of its item, as a lambda's body
is: `tag: a + b` is `tag:(a + b)`, and it ends where tails do, as at a `do`
statement's `->`. Like a lambda, a spaced tag is never an argument:
`g (tag: f x)` or `g <| tag: f x`. An operator cannot begin a spaced tag's
operand, so `tag: |> g` is an error. A tag always takes an operand. Its constructor is its section, `(tag:)`, as `(+ 1)` is an
operator's: `map (tag:) xs`, `[(foo:), (bar:)]`, `x |> (tag:)`. Elsewhere a
tag with nothing after it is an error, so a line break never turns a tag
into its constructor. Joint tags that end with none, `a:b:`, may mean a tag
of a constructor, `a:(b:)`, or one path's constructor, `(a.b:)`, and the
error names both.

Every non-empty dictionary path takes part. Multi-component paths construct
one hierarchical dictionary.

        foo.bar:Data        # same as { foo.bar:Data }
        (foo.bar:)          # same as (\ Data -> foo.bar:Data)

        [KeyExpr]:Data      # one computed path component
        [KeyA,KeyB]:Data    # two computed path components
        ([KeyA,KeyB]:)      # same as (\ Data -> [KeyA,KeyB]:Data)

        (PathExpr):Data     # splice a computed list-valued path
        ((PathExpr):)       # corresponding constructor

For example, `[a,b]:Data` constructs `{[a]:{[b]:Data}}`. To use a list as one
dictionary key instead, nest the brackets: `[[a,b]]:Data`.

A pun, `:name`, is `name:name` everywhere, and a pattern reads it as
capturing `name` under the key `name`. It takes one bare name: puns on paths,
`:a.b`, wait for a consistent approach to capturing patterns, and perhaps
locals generally, into structured dicts.

## Atoms

Atoms are data where the only useful observation is equality.

The unit value `()` is a built-in atom. `'name` is sugar for a tagged unit value, `["name"]:()`. Tagged unit data effectively serves as an atom because we cannot observe `"name"`, we can only test whether it is present. Note that `'tag` and `tag:()` are distinct: the latter is equivalent to `['tag]:()`. Atoms of the `'eax` form are convenient for expressing small enums.

Scope-unique atoms are useful for the ephemeron performance pattern. To support this pattern, we can introduce a term annotation, `anno 'scope_unique`, that wraps a given atom with unique metadata. If ever we compare the same atom with different metadata, we diverge instead, thus never observing the violation of scope uniqueness. When used as dict keys, we associate data to a weakref of that metadata.

For access control and conflict avoidance, we can leverage the namespace as a stable source of unique atoms. A viable approach is `Foo = anno 'scope_unique (abstract_global_path Foo)`. Toplevel-only declaration `unique Foo, Bar, Baz` introduce such definitions, resisting accidental reuse. This leverages the module system namespace as a source of identity.

## Dicts

In expression contexts, `{}` is the empty dictionary, a value like any other.
A dictionary literal introduces its members in written order, the inverse of
a dictionary pattern's takes (`dict-literals-introduce-in-order`):

- A tag member, `path:Value`, introduces `Value` at `path`, which must not yet
  be defined. The dictionaries on the way are created where absent.
- An optional member, `path?:List`, introduces `V` where `List` is `[V]`, and
  nothing where it is `[]`, as a pattern's `path?:` matches `[V]` or `[]`.
- Any other member, a parenthesized one included, is a dictionary whose keys
  join the root, each new: `{ D1, D2 }` is the shallow union of two
  dictionaries.

So `{a.b:1, a.c:2}` is `{a:{b:1, c:2}}`, and `{a:{b:1}, a.c:2}` adds `c` to
what `a` introduced. Computed paths, list literals or parenthetical
expressions of lists, fuse alike: `{ [0]:A, [1,2]:B, ([1] ++ [3,4]):C }`.
Order is the writer's. A literal writes shallower paths first, as a pattern
takes deeper ones first; where the written order makes a conflict the
compiler can see, it reports it and suggests the order. Even `{foo:1, foo:1}`
is an error: there is no generalized unification. So is
`{ {foo:{bar:0}}, {foo:{baz:1}} }`, since union is shallow; a deep merge is a
function's to write. An error is reported where it is noticed, by the
compiler where it sees it, never by forcing a value sooner to notice it.

Braces have no member grammar of their own beyond this, so a tag, a pun and a
constructor read in them as anywhere:

        {:value}             # {value:value}
        {foo: f x, D}        # foo:(f x), then D's keys
        {(foo:) Value, D}    # a dict member, foo:Value, then D's keys
        {:foo Value}         # applies the pun foo:foo to Value
        {opt?:[x], none?:[]} # {opt:x}

Reading a missing key is an error rather than `{}` (`dict-empty-is-a-value`).
`dict.defines Path D` asks whether a path is defined, never reading its value,
and answers as a comparison does, `.r ()` or `.fail`. `dict.default Path
Fallback D` reads with a fallback, `dict.lookup Path D` reads, failing where
undefined, `dict.insert Path Value D` writes, replacing what is there, and
`dict.remove Path D` removes, pruning the dictionaries the removal empties, as
insertion adds them. Dictionary equality is structural, the same keys and then
equal values, and an atom equals the dictionary it stands for, `'tag` and
`{tag:()}`.

Multi-line literal dictionaries accept a leading comma for convenient line-editing, consistent with lists:

        {
        , name1:Expr1
        , name2:Expr2
        ...
        }

They may instead keep initial members on the opening line. The first later
member line selects the content anchor:

        { name1:Expr1,
          name2:Expr2,
          name3:Expr3 }

As a special rule, the usual syntax for dictionaries (literals, with notation) does not enable users to directly touch `spec`. The name `spec` is used by the compiler when modeling objects upon dictionaries. Escape hatches are provided via built-in functions, but I don't want people accidentally mismatching `spec` with object definitions. 

Dictionaries and objects have access to a `with` syntax for definition-style updates. This supports explicit overrides. 

        {name1:Expr1a} with
            name1 := Expr1b
            name2 = Expr2

        # An inline first definition establishes a hanging sibling anchor.
        {name1:Expr1a} with name1 := Expr1b
                            name2 = Expr2

        # equivalent semicolon-delimited form
        {name1:Expr1a} with { name1 := Expr1b; name2 = Expr2 }

        # explicit no-op update
        {name1:Expr1a} with {}

In this notation, a '.' prefix is required when first path element is expression-indexed.

        {[0]:0, [1]:1} with
            .[0] := 1
            .[1] := 0

Users may also name the dictionary with an `alias Name` declaration as the
body's first member, or even support object scope via `alias self`. The
default for an update is `alias _`, which names nothing, so `self` there is
the host's. As with objects, users can reference prior definitions via
`_name` prefix, and final definitions via `name`. But, for dictionaries,
'final' extends only to the current update because there is no specification
to rebuild the dictionary.

        Dict with
            alias d
            x := _d.x + 1   # prior d.x
            y = d.x + a     # result d.x

        Dict with
            alias self
            x := _x + 1
            y = x + ^a      # access 'a' in host scope

Pattern matching on dictionaries generally has the form
`{Path1:Pattern1, Path2:Pattern2, RemainingPattern}`. There is at most one
remaining pattern, default `{}` thus requiring a full match. Patterns read as
expressions do: `foo:p` and `foo: p` are the same tag pattern, and `:x`
captures `x` under the key `x`, so `{:x,:y,:z}` is `{x:x, y:y, z:z}`. A
constructor, `(foo:)`, is not a pattern. A spaced payload is a complete
position: its item's comma or brace bounds it, and it runs to the end of its
item, pattern operators included, so `{x: Nat n}` tags a predicate,
`{x: f -> p}` a view, and `{x: a as b}` both captures. A joint payload stays
nested, as in expressions, so `x:Nat n` would apply the predicate `x:Nat`
to `n`. A tag builds data, so it is never a predicate, which passes or
fails, nor a view, which yields `.r View`, and the compiler rejects one in
either place: `x:Nat n`, `(foo:) p` and `((foo:) -> p)` are errors.

## Embedded Texts

Syntax:

        "inline text"

        """
        " first line
        " "quotes are permitted"
        # source-only comment and blank lines are erased

        " line with # retained as text
        """ |> postprocessing

The opening delimiter is followed by a newline. Each content line begins with
`"` and either a newline (an empty content line) or one separator space, which
is not part of the text. Source indentation before these prefixes may vary.
Source-only blank and comment lines are erased rather than producing content
lines. Content lines are joined with `LF`, regardless of source line endings,
and no final `LF` is added implicitly. Quotes, `#`, and trailing spaces after
the prefix are raw text.

Texts concretely translate to binaries, using ASCII encoding (or utf8 under some extensions). There are no escape characters, i.e. texts are raw and postprocessing is explicit. If users want to embed a binary, that might be expressed as something like:

        """
        " 74686572 65206973 206E6F20 68696464 
        " 656E206D 65737361 67652C20 6A757374
        " 20612073 696C6C79 20657861 6D706C65
        """ |> hex2bin

In practice, it is terribly inconvenient to maintain large embedded texts, much less embedded binaries. Instead, leverage the module system to import file binaries:

        import "MyFile.md" binary as my_file

This enables users to use conventional tools to edit and maintain the text.

## Numbers

Number literals are using the same characters as names, albeit in such a way that they don't overlap names. 

        0
        1
        _42
        1.234
        1.23e_7

        1e6
        1000000
        1_000_000

We use a prefix underscore to indicate negative numbers. This is part of the number literal, not a separate operator. Internal underscores between digits (i.e. digit on both sides) existing only to enhance legibility for humans. Decimal floating point or scientific notation can be encoded directly using an 'e' separator for the exponent.

        0xc0de
        0b10010_00110100_11111110_11011100

We'll support hexadecimal (0x) and binary (0b) number literals, too. We can feasibly provide some 'bitwise' operators or accelerated functions on natural numbers. Although numbers don't have a built-in notion of word size or encoding, it isn't difficult to impose one.

The compiler will provide a few useful operators - `+ * / -` and a prelude to work with numbers.

Numbers are modeled as exact rationals with no bound on size or precision. Thus, any loss of precision is under user control. This has severe performance implications. If users ever need high-performance assembly-time number crunching, they'll be relying on accelerated evaluation of CPU or GPGPU DSLs instead of built-in arithmetic.

*Aside:* We should model any non-trivial math libraries via embedded DSLs. This enables us to evaluate at assembly-time, interpret abstractly, or generate machine code.

## Lists

I propose to use square brackets and commas for inline lists.

        []
        [1]
        [1,2,3]

Multi-line lists admit a leading comma for consistent line editing.

        [
        , 1
        , 2
        , 3
        ]

Opening-line items need not determine later indentation. Both a trailing
separator and a leading separator can introduce the content anchor:

        [1,2,3,4,
          5,6,7,8]

        [1,2
          ,3,4
          ,5,6]

We'll use `++` to compose lists by appending them. In contrast to Haskell's `x:xs`, there is no dedicated 'cons' operator, though we can define `cons x xs = [x]++xs`. One motive for this is symmetry: lists are typically implemented as finger-tree ropes, so we can work efficiently at either end (and split or append in log-time). We may generally use `++` in pattern matching, limited to one variable-length list, e.g. `[x]++xs` or `xs++[x]` or `[x0,x1]++xs++[xn]`. 

We can introduce a few term annotations to manage representations, e.g. flattening a list into an array. We'll rely on accelerated functions on lists, too.

*Notes:* 
- optional values are represented as `[A]` vs. `[]`
- favor effects to construct big lists, never literals

## Tuples

        (,)         tuple:[]
        (a,)        tuple:[a]
        (,a)        tuple:[a]
        (a,b)       tuple:[a,b]
        (,a,b,)     tuple:[a,b]
        (x,y,z)     tuple:[x,y,z]

A comma inside parentheses distinguishes a tuple from unit or grouping: `()` is
unit and `(a)` is simply `a`, while `(,)` is an empty tuple and `(a,)` is a
singleton tuple. Like lists and dictionaries, tuples accept one leading and one
trailing comma for consistent multiline editing:

        value = (
          , first
          , second
          )

As with every delimited group, an opening-line element does not fix the later
content anchor:

        value = (first,
          second,
          third)

Missing internal elements remain invalid, so `(a,,b)` is not a tuple. Commas
are literal separators rather than Haskell-style tuple-section operators; write
an explicit lambda such as `\b -> (a,b)` for partial construction.

A tuple is essentially list with different connotations. Lists tend to be variable-size but homogeneous. Tuples tend to be fixed-size but non-homogeneous. We append lists, but we tend to simply construct or match on tuples inline.

Tuples are concise, but they negatively impact extensibility and scalability. This is mitigated by ad hoc polymorphism, e.g. we can easily match both `(X,Y,Z)` and `{x:X, y:Y, z:Z}` within a context. But, in practice, it's best to use tuples only for private intermediate representations or stable public interfaces.

## Tables and Databases

One way to maintain tables is to simply import from a database in a file:

        # assuming env.lang.["db"].compile
        import "MyData.db" as my_db

        # alternatively, postprocess
        import "MyData.sqlite" binary as my_sql
        my_sql ::= lazy_sqlite  # in place rewrite

Consequently, we don't need embedded tables for embedded data. 

Embedded tables are still necessary when elements include functions or objects, e.g. a dispatch table for multimethods. In general, I highly recommend modeling table objects within database objects. Each table object may have its own metadata, indexed views, reflection tasks to check 'foreign keys' in other tables. The database object ensures we can update (extend) or fork (inherit) whole databases cohesively. We can also model some tables as computed views within the database object.

We can update such tables manually.

        extend my_db with
          extend my_table with
            data ::= \ prior -> prior ++ using ^^self in
              [ # (col1, col2, col3)
              ,   (  42,   53,   f1)
              ,   (  54,   72,   f2)
              ]

The above form is awkward, tedious, and error prone. Tables would benefit from dedicated syntax, but I'm unwilling to commit to any at this time. Instead, I recommend developing embedded DSLs. Perhaps something closer to:

        @table.insert my_db/my_table <| do
          .h 'col1 'col2 'col3
          .r    42    53    f1
          .r    54    72    f2

We'd also need DSLs to 'query' tables, e.g. based on a relational algebra or Datalog.

## Functions

There are two ways to express functions: lambdas and interaction nets.

### Lambdas

We'll adopt Haskell's use of `\` for lambdas.

        \ x y z -> Expr
        \ x -> \ y -> \ z -> Expr

We'll also support Haskell-style `name args = ...` as a syntactic sugar. 

        name = \ x y z -> Expr
        name x y z = Expr
        name x y z := Expr
        name x y z ::= Update       # name := \ x y z -> (Update) _name

Unlike Haskell, there is no support for pattern matching on lambda or definition arguments. 

### Interaction Nets

Interaction nets are expressed effectfully and constructed via builtin
`interaction_net`. The result is an opaque net value, already in weak-head
normal form, rather than an ordinary function. Ordinary application of a raw
net is an error. Inside another interaction net, a raw net embedded as data is
called when it meets a `Bind`; the runtime loads it lazily through its exposed
port.

An ordinary source function embedded with `.data` follows ordinary value
application instead. When it meets a `Bind`, that bind supplies exactly one
argument. If the function remains partial, the result is another ordinary
function in `Data`; supplying its next argument requires another explicit
`Bind`. The runtime fuses the intermediate bind-bind application bridge, but
does not expose or load the function's internal staged net. Thus calling a
function and loading a raw net remain observably distinct even though both
begin with `Bind` meeting `Data`.

The provisional `net_arity N Net` builtin presents a raw net to the ordinary
lambda-calculus layer. At arity zero it is a lazy computation that expects the
exposed interface to produce data. At positive arity it is an ordinary
function that attaches `N` arguments before demanding data. Partial application
does not inspect the staged net. A residual bind or non-data normal form after
saturation is an error; data produced before saturation is left to ordinary
interaction rules and may become stuck. Constructing either `interaction_net`
or `net_arity` does not itself demand the net.

`interaction_net` and `net_arity` are ordinary builtins provided by
`import 'std`. The construction program receives `.bind`, `.copy`, `.data`,
and `.wire` plus the standard task-local effects. Construction requires exactly
one successful branch; use `.cut` when search could otherwise return several.
The current bootstrap can express construction programs either explicitly with
`>>=` and `=>>` or with the implemented name-only layout `do` form described
above. Eventually, we may also want a macro DSL or user-defined syntax for
direct expression of nets.

## Errors

Annotations can raise pure evaluation errors and add structured context:

        anno 'error ErrorMessage
        anno context:Context Expr

Demanding the first form evaluates `ErrorMessage` to weak-head normal form,
then raises it as a permanent error. A text message is accepted as shorthand
for a conventional `msg.text` diagnostic; diagnostic objects retain their
other fields. If evaluating `ErrorMessage` itself fails, that prior failure
receives the context `eval:{op:'error_message}`; the successfully
constructed error does not.

The second form is transparent when `Expr` succeeds. If demand on `Expr`
instead reaches a permanent error, it prepends `Context` to the ordered
`msg.context` list. Nested contexts therefore run from outermost to innermost.
Successful evaluation does not demand `Context`. Effect failure, scheduler
blocking, and unresolved promises are not converted into errors.

An explicitly supplied context frame with a defined `msg` interface is itself
a recursive diagnostic-style message. It may be either a dictionary or a full
object with `spec`, arbitrary fields, and viewer-dependent definitions:

        anno context:{
          msg:{
            text:"while encoding instruction",
            severity:'info,
            context:[eval:{op:'binary_extraction}]
          }
        } Expr

The default terminal observer enriches this object with the same terminal
viewer snapshot as the outer diagnostic, but with indentation anchored at the
nested message. A defined standard `msg.severity` supplies its line header;
without one, the neutral header is `msg:`. This presentation choice is exposed
as the complete textual `viewer.header`, including location, wording,
punctuation, trailing spacing, and terminal color escapes as applicable,
without inventing or replacing semantic `msg.severity`. It has no terminating
newline. Recursive context messages are views, not separately emitted
diagnostics, and therefore do not affect diagnostic counts. Malformed or
divergent custom views remain the author's responsibility, though failure to
render one does not suppress the primary diagnostic.

Automatic evaluator context uses the tagged form
`eval:{op:Operation, args?:Arguments}`, where `Operation` is an atom and
defined `Arguments` is a dictionary of named fields. Argument-free frames omit
`args`. Current operations identify annotation, reflection-annotation,
log-message, log-severity, list-index/count, net-arity, and interaction-net
construction. Most frames decorate a nested failure that the operation
forced; the operation's own validation errors remain self-describing. The
public `net_construction` frame instead covers the complete construction
pipeline, including selection and topology validation errors.

The built-in `g0` compiler also places a shallow context around each source
definition while its value is first demanded:

        {g:{origin:OpaqueOrigin, line:LineNumber, definition:Name}}

The origin is one assembler-created opaque value for the source-input context;
the line and definition fields belong to this front end's location model. The
frame covers definition initialization only. In particular, initializing a
function does not capture its arguments or make the frame follow later calls
to that function. A compiler may add finer static frames where they do not
capture runtime values.

Source code may use the `module_origin` special reference to place the same
opaque value in manual frames.

Opaque origins are not implicitly rendered or searched for inside
`msg.context`. A reflection observer that recognizes this convention may
select the value while iterating the context dictionaries and explicitly
apply the capability returned by `.env '.glam.origin.inspect`. That projection
produces ordinary assembler provenance for the observer's own view.

Other conventional annotations include `anno 'TBD Expr` for incomplete
definitions and `anno 'deprecated Expr` for valid transitional code that
should produce a warning.

`anno 'compile Expr` evaluates `Expr` and, when it is a function or a
net, reduces that net in place as far as it goes without arguments or
demand, then compiles it if it can: every step that only builds a value,
under fuel in proportion to the net, so it never diverges. Nothing is
forced, so a call inside the net still waits to be demanded. The result
is `Expr` itself; only its later applications get cheaper. Applying a
function evaluates only what the application demands, so this is the way
to ask for more. Like any expression, the annotation is lazy: wrapped
around a function, it does its work when the function is first demanded,
usually by its first application.

`anno 'jit Expr` evaluates `Expr` and asks the same of it, deferred: when it
is a function, its net is reduced and compiled at its first application,
partial or not, rather than when the annotation is demanded. The result is
`Expr` itself, marked; a function value demanded but never applied costs
nothing more. A front end may inject either annotation into the code it
generates. g0 lowering marks each function it builds this way. A closure
of a marked lambda and a partial application of a marked function are
marked too, so each is specialized to what it holds when it is first
applied.

`anno 'normalize Expr` asks for the same without fuel: each step is
charged to the evaluation budget, so it may run long, or forever, as an
annotation may.

## Pipes

Borrowing F#'s syntax here:

        f <| arg = f arg
        arg |> f = f arg

I propose to also support directional function composition:

        f >> g = \ h -> g (f h)     
        g << f = f >> g

Ideally, we arrange precedences such that we can write stuff like:

        .op1 >>= f >> g >> .op2 >>= h >> .r
        .op1 >>= (f >> g >> .op2) >>= (h >> .r)

We'll generally forbid mixing right-pipes and left-pipes without explicit parentheses.

## Modules

Modules are loaded through toplevel-only declarations:

        import LocalRef ((as|at) Name)?
        import ((as|at) Name)? from RemoteRef

This structure is intended to resist accidental mixing of local and remote refs in metaprogramming.

### Local

Local filepaths are relative to the current file.

        import "Foo.g"          # integrate with current namespace 
        import "Bar.g" as b     # 'as' for default introduction 
        import "Baz.g" at b     # 'at' to extend the existing 'b'
        import "A/B/C.g"        # access to subfolders

*Note:* Parent-relative (`"../"`) and absolute paths are not permitted. Nor are files or subfolders whose names start with ".". 

### Remote

Remote modules include a reference, a folder revision hash, and search hints for where to find that folder (with optional backups). 

        import as q from {
            , ref:"Qux.q"      
            , rev:Text          # hash of folder content or revision history
            , search:[
                , tag:Text      # to help filter downloads
                , url:Text      # main search
                , url:Text      # backups
                ] 
            }

### Binary Mode

Sometimes we just want the raw data. 

        # local binary
        import ModulePath binary as Name

        # remote binary
        import as Name from {
            , ref:binary:ModuleRef
            , rev:...
            , ...
            }

Name is introduced, and the binary data is lazily loaded, or perhaps loaded on demand (no need to cache in memory). 

### Builtins

Built-in definitions are provided via built-in modules. These are treated as local modules except the naming convention uses atoms instead of filenames. Instead of standard libraries, we might have some built-in libraries.

        import 'prelude
        import 'trig as t

For reproducibility, built-in definitions shall be stable. Like keywords, they should vary only with language version declarations. After import, built-in definitions are normal user definitions, e.g. subject to override. 

### Access Control

There is no notion of export control. That concept conflicts with my extensibility goals and with modules-as-mixins. However, we can easily invert this to explicitly distinguish public interfaces. For example, libraries may define a public `api.*` intended for integration into `env.*`. 

        import "MyFooLib.g" as libfoo
        env.foo = libfoo.api
        libfoo.internal_method := ...

Controlling what subprograms observe is useful for local reasoning. It starts with hiding a few definitions, but 

## Objects

An object is modeled as a dictionary that contains a specification, `spec`. A specification is itself a dict of three items:

- name - specification name is a unique ID in linearization scope
- defs - a mixin, logically of form `\ prior instance -> prior with ...`.
- deps - a list of specifications for multiple inheritance 

Object syntax can and should be compact by default. I propose:

        # declaration
        object foo extends bar, baz with
            def1 = ...
            def2 := ...

        # desugars as expression
        foo = object (abstract_global_path foo) extends bar, baz with 
            def1 = ...
            def2 := ...
        
        # roughly evaluates as
        foo = object_instance {
            , name:(anno 'scope_unique (abstract_global_path foo))
            , deps:[bar.spec, baz.spec]
            , defs: \prior instance -> prior with
                def1 = ...
                def2 := ...
            }

        # declaration
        (abstract)? object Name (extends ExpressionList)? (with Body)?

        # expression
        (abstract)? object (NameExpr|_) (extends ExpressionList)? (with Body)?

The `extends` and `with` sections are optional, with `spec.deps` and
`spec.defs` respectively defaulting to the empty list and const function
(`\x _ -> x`). If `extends` is provided, it cannot be empty. A layout `with`
must contain at least one definition, while `with {}` is an explicit empty
body. In general, `spec.name` may be any value with equality, e.g. `"foo"`.
Toplevel object declarations use `abstract_global_path` to ensure globally
unique names, but it's sufficient that we don't reuse a name for two different
specs across transitive deps.

Every object, `extend`, and dictionary/object-update `with` body accepts either
the next-line layout form shown above, a hanging layout whose first definition
follows `with`, or a brace-delimited form:

        object hanging with value = 1
                            other = 2
        object child with {
          value = 1;
          object nested with { other = 2 };
        }
        extend child with { value := 2 }
        updated = child with { added = 3 }

Braced bodies use the same recursive definition vocabulary and source ordering
as layout bodies. Members are separated by semicolons, and one leading or
trailing semicolon is permitted. Empty braces are an explicit no-op body;
omitting the body after `with` remains an error. In hanging layout, the first
definition's token column is the sibling anchor; deeper lines continue the
preceding definition and a dedent returns to the enclosing expression.

`ExpressionList` is one or more ordinary expressions separated by top-level
commas. Each expression is resolved in the scope surrounding the object and
must evaluate lazily to a parent object with a defined `spec`. Plain
dictionaries are not implicitly accepted as parents; use `object_from_dict`
when that conversion is intended. Parent expressions that contain a top-level
comma must put that comma inside a delimiter group. The declared object's
target remains a static path so its namespace and `abstract_global_path` are
known during compilation.

To instantiate the object, the compiler applies a linearization algorithm (C3?) to deduplicate and merge components. The compiler uses `spec.name` to distinguish specifications, and asserts (via reflective term annotation) that `spec.name` is not used for two different specs in linearization scope. After specifications are ordered, we apply `spec.defs` to an empty base `{}` then finally introduce `spec` as an implicit final mixin. 

For consistency and convenience, the compiler may expose the linearization and instantiation functions as builtins, e.g. `object_instance`. It should be something users can define.

        extend foo with
            def1 := ...

        extend Name with Body                   # declaration

We also have syntax `extend Object with ...`, which updates the specification then re-instantiates the object, preserving name and deps. This is declaration-only because it's usually a bad idea to preserve name while forking identity. Note that `_spec` is always undefined when extending objects, only the final `spec` is visible.

### Object Namespaces

To improve concision, expressions within objects are localized by default. That is, we bind `foo` as `self.foo` and `_foo` as `_self.foo`, where `self` is a keyword referencing the local object namespace, analogous to `module`. Users instead pay a small syntactic tax to access the host scope via `^name`, `^(Expr)`, or use of `module`. The host scope includes its locals: a parameter, `let` binding or body `let` bound outside the body is `^name` within it, while the body's own locals need no escape. A localized body, like a `using` scope, is thus a controlled form of shadowing: a bare `name` reads the body's own `name` even beside a local `name` outside it. 

        a = 1

        object foo with
            bar = ^a
            baz = bar + ^a
            qux = baz + ^a

Use of `^` composes, e.g. `^^^method` escapes three lexical levels. But it's best to keep syntax shallow.

In every scope, a bare `foo` is a visible local `foo` or `self.foo`, and each
`^` steps out of the innermost `self` to its lexical parent. A body that binds
no `self`, under a named alias or `alias _`, is transparent: there `foo` is
its host's `self.foo`, and `^foo` its host's parent's. The module's `self` has
no parent, so `^name` at module scope, even within a transparent body there,
is an error. An escape sees the locals lexically visible where it lands,
together with those it binds itself, as in `^(\w -> w ++ v)`.

For cases that require too many escapes, a body may begin with an `alias
Name` declaration, which names the object locally instead; names then resolve
in the host scope. The default for objects is `alias self`, which is why we
have the default local names and `^` escapes, while `alias _` names nothing
and also resolves names in the host scope. `self` is simply the name that
`alias self` binds: under a named alias or `alias _`, `self` and `_self` keep
their host's meaning, so `alias _` leaves no way to reference the body. In
some contexts, it is more
convenient to use a local name so we don't need escapes. An alias may only be
the first member of an object, `extend` or update body, and only once; the
same declaration serves all three. A module, too, has `alias self`, so
`self.foo` reaches the module namespace from the module's own scope, while
`module` is a reserved alias for it from any scope.

        with
            alias Name      # or `alias self`, or `alias _`
            Definitions

For example:

        a = 1
        b = 2 

        object bar with 
            C = 3

        object foo extends bar with
            alias f
            A = f.B + a
            B = f.C + b

In this context, `foo.A == 6`. Note that we do not need `^a` to reference the global `a`, but now we use `f.B` to reference the local `B`. To reference prior definitions, we'd use `_f.B` instead. 

### Body Locals

A `with` body may also hold `let` members: private bindings that serve as
utility definitions with convenient access to the alias, and that are not
entries of the result. A `let` member is one binding, a hanging layout group
whose first binding sets the column, or a braced group, with the bindings of a
`let` expression:

        Dict with
            alias d
            let tmp = _d.x + 1
            x := tmp + 2
            y = tmp * 2

        object counter with
            let { step = 1; next n = n + step }
            value = next 41

Each group is mutually recursive within itself and visible only to the
members after it, and a nested localized body reaches it as `^tmp`; it reads the definitions before it, so `_d.x` above is the
prior `x` where the group stands. Its names are locals: a member, a use in the
same file, or another local in scope may not share them, and `_tmp` is an
error rather than a prior definition.

### Anonymous Objects

An anonymous object has no name: its `spec.name` is `{}`, the empty dict, which names nothing. Users express anonymous objects via `_` in name position, e.g. `object _ extends foo, bar with ...`. To ensure anonymous objects are intentional, we raise an error if `NameExpr` evaluates as `{}`.

Anonymous objects do not fully participate in multiple inheritance: they are not deduplicated and have a simplistic merge order. They do support mixin inheritance,  always applying before named objects. To resist surprises, it's a linearization error for named objects to appear before anonymous objects in `spec.deps`. For example, in `object _ extends foo, bar`, `bar` may be anonymous only if `foo` is anonymous.

A named object may extend anonymous objects. Logically, `spec.defs` updates from transitive anonymous parents are fused into the named child. 

### Abstract Objects

An abstract object has a full specification but the instantiated members are
absent, i.e. its value is the singleton `spec:{:name, :defs, :deps}`. This is
expressed via `abstract object ...`, as either a declaration or expression.
The body still constructs `spec.defs`; those definitions are applied only when
the specification is later instantiated. Declaring abstract names or methods
inside an ordinary object does not make that object abstract.

An anonymous abstract object's spec names it `{}`. A spec without `name`
takes `{}` as its default, so the two instantiate and linearize alike, though
only the first defines `spec.name`. A spec's `deps` default to `[]` and its
`defs` to the default definitions, and an empty spec `{}` takes all three
defaults.

For `extend Object` this is expressed as `extend abstract Object ...`. It
composes the extension into `spec.defs` but leaves the resulting object
abstract regardless of the prior realization. Conversely, ordinary `extend`
instantiates its result even when the prior object was abstract.

### Lightweight Extension

The `with` syntax for dict updates also works for objects, with an update's
default alias:

        Object with Body
        object _ extends Object with { alias _; Body }

        Object with { alias Name; Body }
        object _ extends Object with { alias Name; Body }

Essentially, the `with` syntax for dictionary updates will recognize `spec` and and treat the `with` body as an anonymous mixin.

This supports lightweight extensions

        foo = op1 >>= op2 >>= op3 with 
            A := 42
            B := op4 >>= op5 >>= op6 with
                alias o
                C c = op7 ...
          where
            op1 = ...

        foo = op1 >>= op2 >>= op3a where
            op3a = op3 with
                ...

### Method Chaining

In OO languages, a common pattern is method chaining where each method linearly returns the 'next' object, and users select a method on that object. It's a convenient pattern. This can be almost directly expressed via piped functions and a helper function.

        # OO language idiom
        Obj.method(Arg1, Arg2)
           .method2(Arg3, Arg4)

        # translation to ".g" syntax
        Obj |> call 'method (Arg1, Arg2) 
            |> call 'method2 (Arg3, Arg4) 

The ".g" syntax isn't optimized for direct use of this idiom. Consequently, a direct translation is much less ergonomic and somewhat more typing. What the ".g" syntax is optimized for is running effects. We can easily use effects handlers as a basis for method chaining. 

        Obj.runWith s0 <| do
            .method Arg1 Arg2
            .method2 Arg3 Arg4

In many ways, this is more flexible than the method chaining idiom. Beyond basic chaining, effects enable users to easily capture intermediate results, integrate loops or conditions, invoke procedural abstractions, all without breaking the chain. Also, we can contextually distinguish 'pure' runners vs. effects transformers based on whether we're running in context of another effect. I encourage users of this syntax to favor this form. 

### Dictionary as Object

Objects and dictionaries serve distinct roles. In particular, an `extends`
expression must produce an object with a defined `spec`; a plain dictionary is
not silently treated as a parent. An undefined parent is an error to read,
and a plain dictionary is rejected rather than taken as a no-op mixin.

`object_from_dict Dict` explicitly constructs an anonymous object that unions 
`Dict` into the inherited base, a shallow union. For the empty dictionary,
this is equivalent to `object _`. It fails if `Dict` defines `spec`, whatever
its value: such a dictionary is an object already.

## Conditionals

We model conditional behavior as effectful and backtracking, i.e. in terms of `.alt/.fail/.cut`.

Boolean expressions become pass/fail effects, i.e. `.r ()` and `.fail`. This
impacts all boolean operators: `(3 > 4)` evaluates as `.fail`, `and` sequences
with `.seq`, and `X or Y` constructs the raw ordered choice `.alt X Y`.

`or` is not itself a Boolean value or an implicit cut point. Under a
branching handler, a continuation after `X or Y` may run once for each
successful alternative. Parentheses only group that choice:

```g
list.pure <| do
  X or Y
  .r Result
```

To select one result manually, place `.cut` around the complete computation
whose failure should permit reconsidering an alternative:

```g
.cut <| do
  X or Y
  LaterGuard
  .r Result
```

This differs from `(.cut (X or Y)) =>> LaterGuard`, which commits to `X` or
`Y` before learning whether `LaterGuard` succeeds. A raw top-level `or`
means what its handler makes of it: a branching handler such as `list.pure`
continues once per success, while machines and reflection tasks make its
first operand atomic and the choice final once that operand succeeds
(`alt-cuts-its-first-operand`).

Conditional forms such as `if` and `match` supply the correctly placed outer
cut around their entire generated choice tree. They therefore return at most
one result and do not expose branching merely because their guards contain
`or`.

Negation can be expressed via staged effect:

        not C = .alt (C =>> .r (.fail)) (.r (.r ())) >>= \ op -> op
        could C = not (not C)

In this case, `could` will run `C` to prove it works, backtrack, then continue running. With just `.alt/.cut/.fail` there is no way to exfiltrate details about the success, other than the observation that it would have passed. 

### If Then Else

I support `if/then/else` for reasons of familiarity and convenience. We'll desugar as a match form.

        # basic forms
        if C then A else B
        A if C else B

        # in general, desugars as
        match when
            C => A
            _ => B

The Rust bootstrap implements both forms and the explicit `match` syntax
below.

Note that conditions are not expressions. Instead, they're guard clauses, i.e. a sequence of `Guard (and Guard)*`. Relevantly, this admits pattern guards, which are often convenient, and effects guards, which can express branching conditions.

        if (a,b) = Expr and a > b then A else B

After an operand, an `if` is the postfix form, so a prefix `if` or `try` as
an argument is parenthesized: `f (if C then A else B)`, while `f if C then A
else B` is an error. Elsewhere either form stands in tail position (see
*Tail Position*).

The postfix form has the same binding scope as the prefix form. Its guards
run first, and names introduced by those guards are visible in the
textually-earlier successful result:

```g
value if (value, rest) = decompose input else fallback
```

The captures are not visible in the `else` result or after the conditional.
This is an intentional case where semantic binding order differs from textual
order.

*Note:* Users are encouraged to switch to the `match` form rather than chaining `else if` many times.

### Try Variants

Pure `if/then/else` and `match` syntax uses a compiler-provided local effects
handler implementing the stateless subset of standard effects
(`.alt/.fail/.cut/.r/.seq/.fix`). Effectful `try/then/else`, `try_match`, and
guard-only `try_match when` instead return their root `.cut` operation to the
host environment. This gives them transactional backtracking and access to
host state or “would this work” conditions. `try` requires `else` and is
therefore total; an exhausted `try_match` remains `.fail` for the ambient
handler.

### Tentative Choice

Instead of confidently returning a result, we can extend the conditional into the result via `then?` (or `=>?` for `match`).

        if C then? .r A else B          # same as if C then A else B
        if C then? .fail else B         # always returns B

The motive for this is to support refactoring of conditional *structures*, factoring chunks from the middle of a conditional pattern.

        if C1 then E1
        else if C2 then E2
        else if C3 then E3
        else if C4 then E4
        else E5

        # snip chunk from middle

        if C1 then E1
        else if _ then?
            # can now move this
            if C2 then .r E2
            else if C3 then .r E3
            else .fail
        else if C4 then E4
        else E5

The `then?` branch has access to the same effects as guard conditions, and must explicitly return the branch result or fail. This isn't convenient: we manually wrap `E2` and `E3` (with `.r`) and add `.fail` on the `else` branch. But it is possible, and it generalizes. 

The effectful `try` and `try_match` variants use the same `then?` and `=>?`
markers. Their tentative operation runs in the ambient host handler, and a
failed operation rolls back before the next sibling is attempted under the
conditional's root `.cut`.

### Match

I borrow a lot of inspiration from Haskell's syntax for `match`. Common use cases are basically the same.

        match Expr as
            Pattern1 => Result1
            Pattern2 => Result2
            _ => Result3

The first ungrouped `as` after `match` ends the subject expression and begins
the arms; a match nested in the subject takes the first `as` after its own
head, as nested `if`s take their `then`s. After `as` come pattern arms, as
`P as Q` views a value through a pattern, and after `when` come guard arms.
An update in a subject needs no parentheses, since it names its base in its
body:

        match Dict with { value := 42 } as
            {value:x} => x
            _ => {}
        match Dict with { alias d; value := d.base + 1 } as
            {value:x} => x
            _ => {}

A `{` on the line of `as` delimits the semicolon-separated arms. A first
arm with a dictionary pattern therefore begins on the next line, where it is
an ordinary layout-arm head:

        match Value as
            {value:x} as whole => whole
            _ => {}

        match Value as { {value:x} => x; _ => {}; }

A match is a tail form (see *Tail Position*): an operator or an enclosing
form's keyword may follow its arms, but an argument may not. A match in an
arm's result may hang its arms on that arm's line, so a later `=>` there is
the nested match's, while a second `=>` of one arm is an error:

        match Expr as
            P1 => match R1 as Q => R2   # one nested arm
            _ => R3

We also support branching guard clauses. We use `when` to separate the pattern from the a branching clause. base case, we have only one branch. But we'll need another `when` for hierarchical branching.

        match Expr as
            P1 when C1 => R1        # basic
            P2 when                 # multiline
                C2_a => R2_a
                C2_b => R2_b
            P3 when
                C3_a when           # multi-level
                    C3_a_a => R3_a_a
                    C3_a_b => R3_a_b
                C3_b => R3_b

A `when` that ends its line heads nested arms, as does one that a `{`
follows on its line, so a guard that begins with a dictionary pattern is
parenthesized: `P when ({}) <- Op => R`. Any other `when` begins guards,
which may continue onto later lines before the arm's `=>`:

        match Expr as
            P1 when C1
                 and C2 => R1

Nested arms end their arm: nothing may follow them in it, on a shallower line or
after a braced block.

If users don't need the pattern, they may just write `match when` instead. 

        match when
            C1 => R1
            C2 when
                C2a => R2a
                C2b => R2b

Tentative choice is expressed using `=>?`.

### Guard Clauses

Several forms of guard clauses:

- `Effect` - evaluates to `{eff:(_),_}`, executes
  - reject on `.fail`
  - accept on `.r ()` 
  - error on `.r Other` (implicit data loss)
- `Effect -> Pattern` or `Pattern <- Effect` 
  - reject on `.fail`
  - accept `.r Result when Pattern = Result`
- `Pattern = Expr` - semantically equivalent to `Pattern <- .r Expr`, but
  lowers as a direct value binding without an actual `.r` request
- `_` - pass, eqv. to `.r ()`

Guard clauses compose sequentially via 'and': `Guard (and Guard)*`. This is essentially the same as boolean 'and', and guards are an `and` chain whose operands may bind: as in any `and` chain, an `or` among them is parenthesized, as in `if (a or b) and c then …`.

### Pattern Matching

Patterns offer a concise way of extracting data from similar structure. I'm borrowing or adapting a lot from Haskell here.

        Name                        # bind as local name 
        _Name                       # don't warn if Name unused
        _                           # drop unused data
        Pattern as Pattern          # many views of same element
        (Pattern)                   # scope control

        ()                          # unit

        {}                          # empty dict
        {d}                         # any dict
        {x:Pattern, y:Pattern, rem} # dict of at least x,y with matching data
        {x:Pattern, {y:Pattern}}    # match residual dict with another pattern
        {x?:Pattern, rem}           # Pattern matches [] where x is absent, [V] where present
        {x?:[]}                     # explicitly accept a missing x
        {x?:[v]}                    # x present, its value v
        {:x,:y,:z}                  # same as {x:x, y:y, z:z}
        {foo.bar.baz:Pattern, _}    # deep refs
        {selector:key, [key]:Pattern}
        {selector:key, root.[key]:Pattern}
        { (Expr):Pattern, _}        # eval list-path expr, extract, match Pattern

        tag:Pattern                 # same as {tag:Pattern}
        [KeyExpr]:Pattern           # same as {[KeyExpr]:Pattern}
        [KeyA,KeyB]:Pattern         # same as {[KeyA,KeyB]:Pattern}
        (PathExpr):Pattern          # same as {(PathExpr):Pattern}
        'name                       # a constant, same as ["name"]:()
        '.Path                      # a constant path, matches equal list
        '.foo.[Expr].(PathExpr)     # computed components; prior captures visible

        []                          # empty list
        [a,b,c]                     # list of three items
        [x]++xs                     # we can use append notation in patterns
        xs++[x]                      
        [x0]++xs++[xN]
        [x0]++([y]++ys)++[xN]       # middle slice runs an ordinary pattern
        # lhs++rhs                  # ILLEGAL - limit one variable sublist

        "foo"                       # match text
        "foo"++xs                   # texts are just lists

        (,)                         # empty tuple
        (P,)                        # singleton tuple
        (P1,P2,...,PN)              # same as tuple:[P1,P2,...,PN]

        42                          # match exact number
        _1.23
        1/6                         # exact rationals supported

        (View -> Pattern)           # parenthesized in ambiguous contexts
        (Pattern <- View)
        View -> Pattern => Result   # whole match-arm view may omit parens
        (Predicate Pattern)         # predicate patterns (special view)
        Predicate Pattern => Result # whole match-arm predicate may omit parens
        (Pattern when Guard)        # local guards

Within a pattern, `when` binds loosest, then one view arrow (`->` or `<-`),
then `as`. Below them a pattern reads as the same tokens would as an
expression: juxtaposition binds tighter than `++`, and a tag tighter than
juxtaposition. So `a:Nat n` would apply the predicate `a:Nat` to `n`, which
is an error since a tag is no predicate, while `a:(Nat n)` and `a: Nat n`
tag a predicate. Views, predicates and local guards fill a
parenthesized pattern or a whole match-arm head. There they may also follow
`as` or a view's arrow, or precede `when`, as in `(p as Nat n)`,
`(f -> Nat n)` and `(Nat n when n > 0)`.

### View Patterns

View patterns have an opportunity to filter, rewrite, and search (branch) on data before we match on it. In the effectful `try` variants, they may also inspect the environment, e.g. `(.get -> Pattern)` would view a 'key' in terms of associated state.

        (View -> Pattern)     # or equivalently
        (Pattern <- View)

Parentheses are required where the view arrow would be lexically or visually ambiguous with another `->` or `<-` owner, e.g. inside a `do` binding, or when chaining views. A view occupying the complete pattern head of a match arm may omit them because `=>` marks the distinct result boundary:

```g
match Subject as
  View -> Pattern => Result
```

The primary difference between a view pattern and effectful guard clause is that, in the pattern context, we have an input other than the effectful environment. The viewer has access to the *same* effects as the guard clauses and tentative choice.

As a rule, view patterns apply before the pattern is matched. If users need a different order, use a guard.
*Note:* View patterns are an approach to refactoring *patterns*. In contrast, tentative choice is supports refactoring *conditional structures*. In practice, it's usually more convenient to refactor patterns. 

### Predicate Pattern

Predicate patterns are a specialized case of view patterns. The predicate is pass/fail. The value captured is not a computed view, but the original input. In most cases, the pattern is a name.

        (Pred Pattern)          # recognized by whitespace as op

        # examples
        (Nat n)                 # check if Nat, capture n
        (Prime n)
        (UTF8 text)
        (Prefix "foo-" text)    # only last arg is pattern

        # complete match-arm patterns may omit the outer parentheses
        match Value as
            Prefix "foo-" text => text

        # as a view pattern, given p2v p x = do { p x; .r x }
        (p2v Pred -> Pattern)

Consistent with view patterns, we forward the argument to the inner pattern
only on pass, i.e. the predicate runs first. The `=>` boundary makes the
complete match-arm pattern unambiguous, so only `do` bindings and nested
pattern positions require the outer parentheses. If users want to run the
predicate after the match, use `(Pattern as tmp when Pred tmp)` instead.

### Exhaustion

An ordinary pure `match` commits to the first complete successful path. If no
path succeeds, observing the result reports `match exhausted on line N`, using
the source line where the enclosing `match` begins. A rejected component or
guard is ordinary `.fail`; the diagnostic does not select or rank a “closest”
failed arm.

An evaluator error raised while running a view or guard remains that evaluator
error. Likewise, an error in the selected result does not cause fallback to a
later arm.

### Open Matches

The starred match forms omit the complete match's implicit `.cut`.

* `match*` runs the open search through the stateless list handler and returns
  its lazy list of results, in source alternative order.
* `try_match*` returns the open search to the ambient effects handler, allowing
  an enclosing choice or cut to decide how many outcomes to observe.

Both subject and guard-only forms are supported:

        matches = match* Value as
            PatternA => ResultA
            PatternB => ResultB

        search = try_match* when
            GuardA => ResultA
            GuardB => ResultB

An exhausted `match*` is `[]`. It does not add an error result as another
alternative. An exhausted `try_match*` is `.fail`. Ordinary `match` and
`try_match` retain their one root `.cut`.

## Loops

As with Haskell, we don't need keywords to support loops. Using *objects*, we can do even better, states in the loop object as method objects that we 'wire' together by overriding 'continuations'. But simple loops should be normal functions. Examples:

        # loop until step failure, backtrack final step
        untilFail Action s0 = .cut (.alt RunLoop EndLoop) >>= \ op -> op where
            RunLoop = Action s0 >>= \ s1 -> .r (untilFail Action s1)
            EndLoop = .r (.r s0)

        # foreach [1,2,3] \item-> do Body
        foreach L Action = match L as
            [x]++xs => (Action x) >>= foreach xs Action
            [] => .r ()

        untilDone s0 Action = match s0 as
            done:R => .r R
            _ => Action s0 >>= \ s1 -> untilDone s1 Action

At least for now, I'll defer keywords for loops. But there are at least a few motives for syntax-supported loops: user familiarity, and tighter integration with pattern matching. Perhaps more if we could make loop objects syntactically convenient to work with. Will review later.

## Open Continuations

Assuming the *Lightweight Extension* syntax for objects, we can support continuation-passing style via extension of abstract method objects. This is another way of passing parameters, more extensible and flexible than function arguments. Moreover, it shifts some parameters from horizontal to vertical layout, and avoids some redundancy of reference. The resulting syntax might look a bit like this:

        foo x y = op1 x >>= op2 y >>= op3 with
            A a = op4 x >>=\_-> op5 a >>= op6 with
                alias op
                B := ... op.F ... 
                C c = ...
            D ::= \ prior -> prior + 42

I'm uncertain how useful this 'style' will be, but Koru language is essentially built around a restricted subset of this form of composition.

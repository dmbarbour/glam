# Source Macros

`language g0` source macros are bounded, effectful source rewrites. A macro
reads normalized source elements to its right, writes replacement elements,
and then lets the ordinary `.g` parser process the result.

Macros operate between lexical analysis and grammatical parsing. They do not
receive an AST, token tree, source path, physical indentation count, or raw
comments.

## Invocation and lookup

A macro head is `@` followed by a joint static name path:

```g
@format
@table.create
@outer @inner input
```

These select `_module.format`, `_module.table.create`, and so forth from the
module definitions visible before the declaration containing the invocation.
The `@`, names, and path dots must be joint. Consequently:

```g
@format.item    # selects _module.format.item
@format .item   # invokes _module.format; .item is macro input
```

Dynamic heads, computed path components, and `@(Expression)` are not part of
`g0`. Macro lookup is independent of lambda parameters, patterns, `let`,
`where`, and object-local names.

Each original declaration captures one prior-module snapshot. Its original
macro invocations run exactly once from right to left, so `@inner` above
finishes before `@outer` reads its replacement. Generated source cannot
introduce another macro invocation.

## A small macro

The selected value is interpreted as an effect. Source parameters are read effectfully:

```g
pair =
  .read.sep =>>
  .read.data >>= \left ->
  .read.sep =>>
  .read.data >>= \right ->
  .write.text "[" =>>
  .write.data left =>>
  .write.text "," =>>
  .write.data right =>>
  .write.text "]"

value = @pair 10 20
```

The final declaration parses as if it contained `value = [10,20]`. The macro
does not parse or render the numbers as text: `.read.data` and `.write.data`
transport ordinary Glam values atomically, and literals are presented as 
data.

A successful invocation has exactly one effect branch returning unit. Zero or
multiple successful branches, a non-unit result, a blocked lookup, or an
unsupported effect is a compilation error.

## Macro environment

A module parameterizes its macros through `meta.macro.env`:

```g
meta.macro.env = {
  style:"compact",
  features:["packet"]
}
```

The compiler constructs the immutable invocation environment as if it applied:

```g
_module.meta.macro.env with
  language = DeclaredLanguage
```

An undefined `meta.macro.env` behaves as `{}`. `DeclaredLanguage` reflects the
file's required declaration, including extension order. For example,
`language g0 with utf8, demo` supplies a value equivalent to:

```g
{base:'g0, extensions:['utf8, 'demo]}
```

Macros select values with `.env Path`:

```g
.env '.style
.env '.language.base
```

A missing path produces `{}` under ordinary dictionary access semantics.
Every original macro invocation in one declaration sees the same environment
snapshot. `meta.macro.env` may be an adapting object; the ordinary `with`
operation re-instantiates it after adding `language`.

## Reader effects

Readers advance one transactional, forward-only cursor:

| Effect | Result and behavior |
| --- | --- |
| `.read.text Text` | Read exactly `Text`; return unit or invoke `.fail` without consuming input |
| `.read.regex Pattern` | Match a text pattern at the current position; return `{span:Text}` |
| `.read.text_span` | Read the remaining nonempty current textual run; return `{span:Text}` |
| `.read.data` | Read one embedded Glam value without forcing it |
| `.read.sep` | Read logical separation within the current item |
| `.read.layout Parser` | Enter the attached child layout and run `Parser` |
| `.read.anchor` | Read the next sibling anchor inside `.read.layout` |
| `.read.end` | Succeed only at the end of the current root item or layout |

`.read.text` can read delimiters `()[]{}` explicitly. A committed macro must
leave these parentheses, brackets, and braces balanced. `.read.regex` and
`.read.text_span` are restricted to a nonstructural text run and cannot cross
embedded data, logical separation, delimiters, or layout anchors.

The root cursor begins immediately after the static macro head and cannot
consume a peer logical item. Root `.read.anchor` simply invokes `.fail`.
Inside `.read.layout`, the parser must precede each sibling with `.read.anchor`
and finish the layout; it never observes physical columns, newline spelling,
blank lines, or comments.

Logical separation is stretchy. It may originate from spaces or from a
newline indented beyond the current floor. An attached layout may begin on the
next line or as a hanging layout after remaining content on the invocation
line.

## Writer effects

Writers append to a separate transactional output:

| Effect | Behavior |
| --- | --- |
| `.write.text Text` | Write non-whitespace logical source text |
| `.write.data Value` | Embed an arbitrary Glam value atomically |
| `.write.sep` | Write logical separation within one item |
| `.write.layout Writer` | Write one attached child layout |
| `.write.anchor` | Begin the next layout or replacement sibling |

Within `.write.layout`, each nonempty item begins with `.write.anchor`.
An empty layout item, consecutive anchors, and a trailing anchor are invalid.
A leading root `.write.anchor` creates sibling output and is valid only when
the invocation replaces a complete logical item. Inline output cannot create
sibling boundaries.

`.write.text` rejects ASCII C0 controls, space, DEL, `@`, and `#`. Use
`.write.sep` and `.write.anchor` for logical whitespace. The last two
characters remain reserved for original macro invocations and source
comments. Values written by `.write.data` are opaque and may contain any of
those characters.

The accepted output replaces the macro head and exactly the input prefix the
reader consumed. Unread input remains. Writing nothing deletes that range;
writing fewer elements naturally shrinks it.

Commas and semicolons have no special macro meaning. They are ordinary text
until the expanded result reaches the tuple, collection, `do`, or braced-body
parser.

## Search and diagnostics

Macros use the standard deterministic effect search operations, including
`.r`, `>>=`, `.alt`, `.fail`, `.cut`, task-local state, and delimited control.
Their reads, writes, task-local state, `.case`, and direct `.log` calls
backtrack together.

```g
.case "a field declaration" FieldParser
.log 'warn Message
```

When every branch fails, the compiler reports the active `.case` explanations
at the furthest input position. Direct `.log` messages are published only
after the selected branch and its generated syntax are accepted.

The macro effect API does not expose the shared reflection heap, task
operations, files, clocks, process state, or source paths. A demanded
`anno refl:Task Target` inside a helper may still start ordinary reflection
reasoning in the compilation-private macro session. Such committed reflection
work is not rolled back when the surrounding macro alternative is abandoned.

## Text patterns

`.read.regex` uses the same language-owned, capture-free `g0` text-pattern
grammar as configured CLI token parsing:

```text
Pattern      ::= Alternative ("|" Alternative)*
Alternative  ::= Repetition*
Repetition   ::= Atom ("?" | "*" | "+")?
Atom         ::= Literal | "." | Class | "(" Pattern ")"
Class        ::= "[" ClassItem+ "]"
ClassItem    ::= ClassLiteral | ClassLiteral "-" ClassLiteral
```

Matching is anchored, alternation is ordered and leftmost-first, and
repetition is greedy. Parentheses group without capturing. Captures, flags,
lazy or counted repetition, class negation, anchors, lookaround,
backreferences, shorthand classes, Unicode-property classes, and the borrowed
`(?:...)` spelling are invalid.

A valid nonmatch invokes `.fail`; invalid pattern syntax is an effect error.

## Helper definitions and staging advice

Macro expansion occurs before the current module fixpoint is sealed. An
ordinary global reference normally denotes that future final module, even
when the referenced definition appears earlier in the file. A helper selected
that way may therefore wait on the very module compilation that is waiting
for the macro.

Prefer a named top-level object declaration for a reusable macro grammar:

- place the macro entry points and related helpers in the object;
- refer to recursive members through its object-local alias; and
- invoke the public entries through static paths such as `@words.expand`.

This gives the grammar one namespace that other modules can reuse, extend, or
override through ordinary object composition. When inheriting from a top-level 
macro object, always spell that parent `_name`, never `name`:

```g
object specialized_words extends _words with
  # additions and overrides
```

`_words` selects the prior definition. Unescaped `words` denotes the final
module binding and can recreate the module-fixpoint dependency that the object
boundary is meant to avoid.

For example:

```g
object words with
  read_all =
    .alt
      (.read.end =>> .r [])
      (
        .read.sep =>>
        .read.regex "[A-Za-z]+" >>= \word ->
        read_all >>= \rest ->
        .r ([word.span] ++ rest)
      )

  expand =
    read_all >>= \items ->
    .write.data items

  eff = expand

result = @words hello
```

The top-level object owns its recursive knot independently of the module-level
fixpoint while remaining a named, composable grammar. 

Use a local `let` or `where` group when a small helper group is intentionally 
private to one macro value. Use `.fix` or `do { abstract ... }` only if when
a fixpoint value genuinely depends on the result of some effectful observation.

An explicit prior-definition reference can also avoid the final module, but
top-level object declarations are the preferred reusable form.

## Non-hygienic macros

`g0` macros are deliberately non-hygienic. The normal `".g"` parser interprets
emitted texts as names, 'let', symbols such as '=', etc.. This keeps the protocol
small.

The language mitigates common hygienic accidents through:

- prohibition of local name shadowing;
- file-wide checks against introduced and actually used global roots;
- explicit introduction, override, and update definition operators;
- static macro lookup independent of local bindings;
- structural balance checks around macro input and output;
- macro composition at definition instead of application;
- `using` to deliberately shadow names in scope; and
- atomic `.write.data` for passing values and helper functions without naming
  or serializing them in generated source.

Macro authors remain responsible for names they intentionally generate. Favor
distinct naming conventions or rely on `using` to scope locals.

## Inspection and examples

`inspect_g_source` and `glam --parse` never evaluate macros. They retain
lexical and structural diagnostics, summarize ordinary declarations, and
report macro-bearing declarations as deferred. Full generated-syntax
diagnostics require compilation.

Executable examples are under
[`samples/contracts/macros/`](../samples/contracts/macros/):

- `logic.g` implements a small recursive logic DSL;
- `rewrite_rules.g` captures and replays balanced source and layouts; and
- `packet.g` generates a codec object from a packet description.

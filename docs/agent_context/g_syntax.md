# Built-in `.g` Front-End Invariants

These are regression-sensitive rules for the Rust bootstrap's built-in `.g`
compiler. Current control flow lives in
[`../architecture/front_end.md`](../architecture/front_end.md); user syntax
lives in [`../SyntaxCheatSheet.md`](../SyntaxCheatSheet.md). Parser tests and
valid/invalid samples are executable specifications when prose and behavior
disagree.

## Lexical and Layout Ownership

- `g_syntax/parser/lexical.rs` owns source-wide newline consistency, allowed
  whitespace, embedded text and numeric payloads, delimiter balance,
  indentation facts, and declaration sections. Fatal lexical errors stop
  grammatical parsing.
- `parser/input.rs` is the only adapter from the shared lexical result to token
  parsers. Production parsers receive an existing `TokenView`; do not re-lex
  source substrings or add another global structure scan.
- Source macro replacement is the narrow exception: it classifies each evolving
  owned logical declaration locally before the ordinary parser sees it.
- `LayoutView` owns both parser body inference and macro child-layout scopes.
  It returns rather than consumes the first dedent. Do not introduce an
  independent macro indentation algorithm.
- A layout owner establishes an exclusive floor. The first member chooses the
  sibling anchor; a hanging first member may establish that anchor on the
  owner's line. A final structural child inherits the owner's floor rather than
  drifting to the inline right-hand-side column.
- `ExpressionContext` decides whether a postfix or infix owner may resume after
  dedent. Only recognized syntax resumes; indentation is never an implicit
  expression separator.
- Delimited groups retain ordinary expression continuation. Commas or
  semicolons, not indentation, separate members. A later contributing line
  aligns with the first post-opening member or separator.
- A boundary-aligned line containing only closing delimiters may terminate a
  declaration, but cannot carry or precede a later expression suffix.

## Macro Staging

- One original declaration captures one prior namespace and environment, then
  expands only its original finite macro worklist, right to left. Generated
  text is never scanned for new macro invocations.
- Macro cursors reuse `LayoutView`; abstract anchors are interpreted relative
  to the invocation floor. A leading anchor after a layout-introducing keyword
  uses the first hanging member's inferred column, including inside delimiters.
- Macro input discovery never treats `,` or `;` as a boundary. Punctuation is
  ordinary text until the expanded source reaches the normal parser.
- `.write.text` excludes ASCII C0 controls, SP, and DEL. Only `.write.sep` and
  `.write.anchor` emit logical whitespace and sibling boundaries.
- Failed macro search reports the furthest cursor and active `.case` values.
  Never replay a macro merely to enrich diagnostics: demanded reflection
  annotations commit outside its branch journal and could run twice.
- A macro helper reached through the ordinary final module may depend on the
  unsealed module fixpoint. Prefer a named top-level object grammar and its
  object-local recursive alias. Inherit from `_name`, not final `name`.
  Mutually recursive `let`/`where` is appropriate for deliberately private
  helpers.
- Macro effect objects follow the ordinary `{eff:_, _}` convention. Helper
  members do not disqualify them; expose a primary operation with
  `eff = entry.eff`, not `eff = entry`.

## Representation Boundary

- Syntax, lexical names, scopes, captures, lambdas, and sugar belong entirely
  to `g_syntax`. `core` and evaluation have no syntax expression, local
  environment, lambda AST, or closure representation.
- `ResolvedExpr<Value>` is affine front-end IR. Move it through one lowering;
  cloning can lower and evaluate source work twice.
- A complete source function lowers to one bind spine, including leading
  capture binds. Maximal applications lower together where possible.
- Definition targets retain parsed `SyntaxKeyExpr` paths. Never reconstruct or
  re-lex a source target fragment.
- The production staged parser lowers declarations immediately. Whole-file
  `ParsedSource` parsing is retained only as a test oracle.
- Source inspection is non-evaluating. A macro-bearing declaration is
  `MacroDeferred`; inspection does not invent a macro namespace or parse a
  partial expression around the invocation.

## Compiler Capabilities

- `CompileContext` provides source-scoped authority: relative loads,
  `abstract_global_path`, prior/final definitions, canonical unit, diagnostic
  emission, and one opaque origin. It must not become a general expression or
  value-construction DSL.
- Source bytes and artifact metadata cross separate boundaries. The `.g`
  compiler validates UTF-8; the assembler retains source identity, digest,
  resolver, and import provenance.
- Import requests and `abstract_global_path` components are relative. Reject
  absolute paths, backslashes, empty or dot components, parent traversal, and
  other dot-prefixed components.
- In `g0`, `abstract_global_path` takes a static name path. A bare root is valid
  only when ordinary lexical resolution selects the module namespace. Locals,
  object scope, aliases, and `using` scope must reject it or require explicit
  `module.`. Do not lower its operand as a runtime expression.
- Built-in closed helpers and modules are cached per runtime. Paths,
  environments, promises, and reflection tasks remain per compilation or
  module.

## Names and Scope

- `g_syntax/keywords.rs` is the single `g0` reserved-word table. Enforce it for
  bare definition roots, locals, and bare references; do not grow parser-local
  keyword lists. Explicit member/key positions may use keyword spellings.
- A source local may not shadow another active local or a global introduced or
  actually selected by the same file through a visible namespace. The
  file-wide global check belongs in `name_analysis.rs`, not parser routing.
- `_name` has canonical name `name`. Bare `_` and compiler-generated bindings
  remain exempt from source-local shadow checks.
- Object scopes resolve through explicit prior/final self values. `module`,
  `self`, aliases, and `^` escapes have defined owners; do not implement object
  lookup as an implicit fallback chain.
- `using Dict in Expr` evaluates and shares `Dict` in the surrounding scope,
  then installs it as temporary final namespace and `self`; prior namespace is
  `{}` and `^` still escapes outward. It is front-end sugar, not a runtime
  scope agent or object constructor.

## Definitions and Collections

- `=`, `:=`, and `::=` remain introduction, override, and non-observing update.
- List literals preserve every comma-separated expression as one element.
  Only explicit `++` or `list.concat` flattens structure.
- Dot-leading effect paths in application-argument position require grouping:
  `foo (.bar)` is valid, while `foo .bar` is rejected. Head `.bar`, access
  `foo.bar`, and `foo <| .bar` remain distinct.
- Distinct arithmetic operators have no implicit precedence; neither do `and`
  and `or`. Homogeneous chains remain accepted, while mixed forms require
  parentheses.
- Multiline `let` and `where` bindings align under their first binding and do
  not accept `in`. Naked semicolons do not group them; braced forms do.
  Braced `let`/`where` and every `with` body permit leading/trailing separators
  and explicit empty `{}` bodies.

## Do, Patterns, and Conditionals

- Layout `do` disappears during g-syntax resolution. A bare intermediate
  statement uses `=>>` semantics and requires unit; the final expression is the
  continuation and is not implicitly wrapped in `.r`.
- Do patterns expand into one resolved primitive-do stream. A multi-capture
  `P as Q` binds one internal subject and then emits ordered aliases; do not
  reconstruct a surface `DoExpr` or lower the subject twice.
- Pattern mismatches invoke compiler-private `.fail`; evaluation failures still
  propagate. View and predicate patterns append their ordered effect steps to
  the same primitive stream.
- Fixed dictionary, tag, tuple, list, and quoted-path patterns use persistent
  decomposition without observing unrelated members. Required dict paths
  reject absent/undefined entries; `path?:Pattern` explicitly passes `{}`.
  Computed paths evaluate once at their ordered match step and may use earlier
  captures. A remainder is an ordinary, potentially refutable pattern.
- Pure `if` and `match` own one stateless root `.cut`. Host `try` variants
  return the equivalent cut to the ambient handler. `match*` and `try_match*`
  deliberately omit it; do not add a hidden exhaustion branch to `match*`.
- `match when` is a distinct guard-only form. Hierarchical child alternatives
  share progressive parent captures and may fall through to the next parent;
  only the complete match owns the cut.
- Ordinary `then`/`=>` results are staged with compiler-owned `.r`; tentative
  `then?`/`=>?` results are emitted as effects. Preserve this distinction in
  resolved IR rather than inferring it from an expression.
- Postfix `A if Guards else B` resolves guards before `A`. Guard captures are
  visible in `A`, never in `B` or the surrounding expression.
- Recursive do is explicit. Each `abstract` name has an independently
  completable `.fix` interval. The planner consumes the completed primitive-do
  stream, not surface patterns. Crossing intervals promote the later-ending
  fixpoint with a warning; missing, duplicate, or prematurely observed
  declarations are errors.

## Objects and Reflection Boundaries

- Object syntax and scope remain front-end-owned; core sees ordinary values,
  dictionaries, functions, and nets. See
  [`objects.md`](objects.md) for representation and linearization.
- Named module definitions and members of named declared objects receive one
  shared lazy reflection boundary for final `refl.*`. `refl`, `meta`, `spec`,
  computed roots, and expression-local objects remain inert.
- Object scanner identity derives from final `spec.name`, so inherited members
  use the derived object's overridable reflection namespace.

## Verification Anchors

- Parser unit tests live beside `g_syntax/parser/` modules.
- Cross-stage resolution/lowering tests live in `g_syntax/tests.rs`.
- Macro protocol integration lives in `g_syntax/macro_expansion/tests.rs` and
  `tests/macro_protocols.rs`.
- Valid and invalid source fixtures under `samples/` define user-visible
  acceptance and diagnostics.

When changing these rules, add or update a focused regression before changing
the implementation. Do not accept a broad full-suite pass as proof that a
specific layout, staging, or single-evaluation invariant was exercised.

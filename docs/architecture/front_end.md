# Built-in Front-End Architecture

This document follows one source artifact through the Rust bootstrap's built-in
`.g` compiler. It describes current implementation ownership, not the eventual
user-defined compiler contract. Regression-sensitive parser and lowering rules
live in [`../agent_context/g_syntax.md`](../agent_context/g_syntax.md); intended
syntax lives in [`../Syntax.md`](../Syntax.md).

## Compilation Boundary

`SourceSystem` is the assembler's source-discovery authority. A load returns an
immutable `SourceArtifact` containing the exact bytes, source identity, content
digest, and a relative resolver for imports. The front end receives the bytes
separately and performs its own UTF-8 validation. It cannot replace the source
system or infer host filesystem authority from a source name.

`CompileContext` is a capability context for one source invocation. It provides
relative imports, namespace qualification, prior and promised final module
definitions, diagnostic emission, canonical unit, `abstract_global_path`, and
one opaque source-origin token. It does not expose assembler provenance fields
or act as a general value-construction DSL. The compiler decides whether and
where to place the opaque origin in its own context frames.

One top-level module build allocates one `CompilationExecution`. Every ordered
input and recursive import in that build shares its import lookup context and
macro demand session. Macro tasks have their own demand ownership and
diagnostic counts, but enter the same `EvaluationRuntime` work coordinator,
reflection heap, protected-volume namespace, query domain, and executor as the
rest of the build.

## Built-in `.g` Pipeline

```text
SourceArtifact bytes
  -> UTF-8 validation
  -> one lexical token/group/declaration structure
  -> staged declaration stream
       -> expand original macro invocations, when present
       -> parse one declaration range
       -> resolve lexical and namespace names
       -> analyze source-wide name use
       -> lower immediately into semantic values and nets
  -> close the module final-definition promise
  -> drain compilation-session macro reasoning
  -> closed module Value
```

`g_syntax/parser/lexical.rs` performs the one source-wide lexical pass. Its
result owns normalized newline and whitespace facts, eagerly parsed numeric and
text payloads, delimiter groups, indentation facts, and declaration sections.
`parser/input.rs` is the checked adapter from that structure to Chumsky token
parsers. Production parsers receive `TokenView` ranges and do not re-lex source
substrings.

`parser/logical.rs` owns the declaration-scoped macro rewrite pipeline. It
discovers original invocations, constructs normalized macro input and layout
views, validates generated output with the authoritative lexer, preserves
embedded values while rendering, and replays the completed declaration into
ordinary parsing. It does not retain a second token or group representation of
the source or generated output.

`StagedSourceParser` and `ModuleLowerer` alternate parsing and lowering in
source order. This staging lets a declaration's macro resolve against the
correct prior namespace, while definitions produced by that declaration become
available to later declarations. The whole-file `ParsedSource` path remains a
test oracle rather than the production compilation path.

## Macro Expansion Seam

An original declaration containing macros captures its prior module namespace
and compiler environment. Expansion processes the finite set of macro calls
already present in that declaration from right to left. Generated text is
never scanned for further macro invocations.

Macro execution uses a private evaluation demand session and an isolated
all-results effect search. Branch journals backtrack normally; demanded
`anno refl:` work commits independently and therefore must not be replayed to
obtain better diagnostics. Failed searches retain the furthest cursor and its
active `.case` values. Successful expansion is parsed as an ordinary logical
declaration before direct macro diagnostics are published in source order.

Macro cursors reuse the parser's `LayoutView`. Abstract anchors are
materialized relative to the invocation floor, including hanging layout and
layout bounded by delimiters. Commas and semicolons remain ordinary macro text;
the later tuple, collection, or braced-body parser gives them meaning.

See [`../Macros.md`](../Macros.md) for the user protocol and
[`../agent_context/g_syntax.md`](../agent_context/g_syntax.md) for staging
hazards around helper definitions and final-module references.

## Syntax and Semantic Representations

The front end owns syntax, source names, scopes, captures, and language sugar.
Those representations do not enter `core` or evaluation.

```text
parser syntax nodes
  -> resolver-owned BindingId locals
  -> affine ResolvedExpr<Value>
  -> direct net/value lowering
  -> closed Value / FunctionCode / NetValue
```

`ResolvedExpr<Value>` is moved through one lowering. It is intentionally not
cloneable: duplicating it could lower and evaluate the same source work more
than once. Complete functions lower as one bind spine, including leading
capture binds, and application spines lower together where possible.

Definition targets retain parsed `SyntaxKeyExpr` components through lowering.
The compiler never reconstructs a target by slicing or re-parsing source text.
Computed path components remain expressions until their ordered semantic
lowering point.

## Module, Import, and Object Lowering

Module lowering owns declaration order, the open module fixpoint, prior versus
final definitions, imports, automatic reflection boundaries, and definition
assertions. Imports re-enter the same assembler and `CompilationExecution`
through the relative resolver carried by the imported artifact.

Import requests and compiler-supplied `abstract_global_path` components are
relative. The compiler rejects absolute paths, parent traversal, empty or dot
components, backslashes, and other dot-prefixed components. Host CLI paths are
a separate trust boundary.

Object syntax is also front-end-owned. Object lowering creates specification
values, definition mixins, dependency expressions, object-local scopes, and
named-member reflection boundaries using ordinary semantic values and nets.
Core has no object expression tree. Detailed representation and linearization
rules live in [`../agent_context/objects.md`](../agent_context/objects.md).

## Compiler-Owned Constructed Values

The runtime value cache stores one complete type-indexed bundle of closed
built-in compiler helpers, built-in modules, and the default diagnostic
formatter. A candidate bundle is built outside cache synchronization and one
completed winner is installed. Concurrent first users may do harmless duplicate
pure construction, but no partially built bundle is shared.

Closed helper construction uses a private closed evaluation context over the
selected runtime value factory. It can reduce genuine lazy helpers without
registering scheduler demand, work records, or reflection activity in the live
runtime coordinator. Module-specific paths, environments, promises, and
reflection tasks are not cached in the compiler bundle.

## Inspection Boundary

`inspect_g_source` is the narrow public non-evaluating inspection API used by
standalone `--parse`. It returns diagnostics and declaration summaries without
exposing the syntax AST, `CompileContext`, macro values, or lowering internals.
A macro-bearing declaration is reported as `MacroDeferred`; inspection does
not attempt a partial parse using an invented macro environment.

## Adjacent Owners

- [`assembly.md`](assembly.md): source selection, module construction, and CLI
  lifecycle.
- [`diagnostics.md`](diagnostics.md): compiler diagnostic envelopes, origins,
  and rendering.
- [`evaluation.md`](evaluation.md): runtime value cache, demand contexts, and
  compiled-value execution.
- [`../agent_context/g_syntax.md`](../agent_context/g_syntax.md): parser,
  resolver, macro, and lowering invariants.

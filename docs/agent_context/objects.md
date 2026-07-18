# Object Implementation Invariants

This note describes the object behavior implemented by the Rust bootstrap. The
broader target design remains in `docs/DistilledDesign.md` and `docs/Design.md`.

## Representation and Instantiation

An object is an ordinary dictionary with a `spec` member. A specification is a
dictionary with:

- `name`: a stable identity for named objects, or the empty dictionary for an
  anonymous object;
- `deps`: a list of parent specifications; and
- `defs`: a curried two-argument definition mixin, logically
  `prior_self -> final_self -> dictionary`.

`ObjectInstanceFromParts` constructs this specification and delegates to
`ObjectInstance`. Instantiation creates a pending `final_self`, applies each
linearized spec's `defs` to the accumulated base and that final self, then
inserts the most-derived `spec` and fulfills the pending self. Every mixin must
produce a dictionary.

Ordinary dictionaries participate as anonymous object specifications through
`ObjectSpec`: their dictionary content becomes a mixin, their name is empty,
and they have no dependencies. This is bootstrap compatibility, not a final
persistent-dictionary design.

## Dependency Order

The evaluator computes C3 linearization from `spec.deps`, then applies
definitions from least to most derived.

- Named specifications compare and deduplicate by the evaluated `name` key.
- Each anonymous specification receives a traversal-local identity and remains
  distinct even when its contents match another anonymous spec.
- Anonymous direct dependencies must precede named direct dependencies.
- An inconsistent C3 merge is an evaluation error.

Do not replace this with left-to-right parent application; that was an early
spike rule and is no longer current.

## Front-End Lowering

Object syntax is owned entirely by `g_syntax`:

- named top-level and nested `object` declarations;
- object expressions, including anonymous `_` names;
- `extends` dependencies and `extend` declarations;
- object aliases;
- `with` expressions; and
- lexical `^` escapes.

The parser produces object syntax nodes. Module/object lowering resolves their
bodies into the affine front-end IR and then emits ordinary values and
interaction nets. Core does not contain object syntax or an object expression
tree.

Top-level named declarations use `CompileContext::abstract_global_path` for
their specification name. Nested named objects derive a hierarchical name from
the enclosing object's specification. Anonymous object expressions use the
empty dictionary name.

`extend target with ...` preserves the target's name and dependencies and
composes the new definition mixin after the prior one before re-instantiating
the object.

## Object Scope

An object body is never resolved as ordinary module scope. Lowering introduces
two stable locals for the prior and final self plus sequential bindings for the
currently visible body definitions.

Without an alias:

- `name` reads `final_self.name`;
- `_name` reads the prior/previously visible definitions; and
- `self` denotes the complete final object.

With `as alias`:

- unqualified ordinary names continue to resolve in the parent scope;
- `alias` denotes the object's final self;
- `_alias` denotes its prior self; and
- explicit `self` still denotes the current object self.

Lexical locals shadow scope lookups. `module` explicitly names module final
definitions. `^expr` moves resolution outward by the requested number of
lexical object scopes; do not implement host access as an implicit fallback.

The synthetic `spec` member is removed from intermediate visible-definition
dictionaries while an object body is assembled. This prevents ordinary body
updates from accidentally treating runtime metadata as a user definition.

## Current Boundaries

- Object builtins are internal protocol adapters, not a public user-facing
  object API.
- Direct manipulation of `spec` is not yet protected as a full language-level
  abstraction.
- Dictionary/object compatibility uses the current eager dictionary
  representation and will need review when persistent lazy dictionaries land.
- Final-self uses a computed fixpoint cell. Its first evaluator owns production;
  recursive self-demand fails, while concurrent observation waits if that
  producer has suspended on other work. Module final definitions still use a
  separate fail-fast `Promised` assignment hole.

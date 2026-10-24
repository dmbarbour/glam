# Dict Semantics — 2026-10-24

A design note on revising what a dict is: `{}` as the empty dict rather
than "undefined", shallow union, and paths fused by their prefixes. Asked
for by the maintainer (2026-10-24), after `api-comparables` met equality
that had to force values to tell an undefined field from an absent one.
It comes before `api-comparables`, whose definedness checks move to the
primitives this note introduces.

## Today

- `{}` is the empty dict and the "undefined" value: `{foo:{}}` ≡ `{}`, and
  reading a missing key or path yields `{}` (Syntax.md, "Dicts").
- A dict literal is the union of its items, and union is hierarchical: at
  a key both sides define, two dicts merge recursively, an undefined side
  gives way, and two other values are an error, lazily, when the key is
  read (`eval/dict_machine.rs`, `MergeDuplicate`).
- Dotted and computed paths in literals build nested singletons, which
  union merges (`dict-literals-compose`).
- Definedness is a comparison with `{}`: `x == {}`.

What that costs:
- **Whether a key is defined depends on its value.** `{foo: f x}` defines
  `foo` only if `f x` is not `{}`, so equality, union and every
  definedness check force values.
- **An empty dict cannot be a value** apart from "absent".
- **Merging works at every shared key**, a lazy merge per key, forcing
  both sides, recursively.
- **Definedness is spelled as a comparison**, which the comparables rule
  rejects: `{} == \x -> x` is incomparable, an error (`api-comparables`).

## The model (maintainer, 2026-10-24)

- **`{}` is the empty dict, a value like any other.** `{foo:{}}` defines
  `foo`. Removing a key is an operation of its own, never an assignment
  of `{}`.
- **Union is shallow.** Any key both operands define is an error, whatever
  the values. Overlap is known from the keys alone: no merge, no forcing
  of values. Eager or lazy reporting is evaluation strategy, chosen by
  annotation, not semantics: an error is reported when it is noticed.
- **Paths are fused, not merged.** A literal's static paths are grouped by
  the front end, so `{a.b:1, a.c:2}` is `{a:{b:1, c:2}}`. Computed paths
  cannot be expressed as merging dicts: lowering fuses them at run time by
  their shared prefixes, so `{[1,2]:B, [1,3,4]:C}` is `{1:{2:B, 3:{4:C}}}`.
  A path that is a prefix of another, or appears twice, is an error.
- **Reading what is undefined is an error.** A missing key, or an undefined
  heap, local or volume path, reads as an error value, not `{}`: `.get`
  answers the error value rather than diverging, so the effect goes on and
  only an observer of the value fails (maintainer, 2026-10-24). New
  operations:
  - query whether a path is defined without observing its value;
  - `.try_get Path`, the value where defined, and `.fail` where not, so it
    composes with `.alt` and `.try`; optional values do not suit every
    context (maintainer, 2026-10-24);
  - `.del Path`, removing a definition.
- **Optional patterns use the same convention.** `path?:Pattern` matches
  `Pattern` against `[]` where `path` is absent and `[V]` where defined.
- **Dict equality is structural**: the same keys, and equal values. An
  atom equals the dict it stands for, `[key]:()`; unit is a primitive
  atom, never constructed, and stands for no dict (`api-comparables`).
- **Mixins and objects do not rely on hierarchical union.** `dict_as_object`
  is the rare case, where a shallow union is adequate and users may
  specialize.

Rejected (maintainer, 2026-10-24): key presence by structure while keeping
`{}` as undefined, inconsistent; keeping the semantics and optimizing it,
which keeps the surprises.

## Survey (2026-10-24)

Two surveys, of the Rust implementation and of the Glam sources and
snippets, found that much depends on `{}` as undefined. Deep union, though,
has almost no users.

**Two notions of undefined.** The code already disagrees with itself:
- a shallow check, a dict that is empty (`is_undefined_dict_value`,
  `eval/value.rs`, 12 sites, and local copies in the annotation, journal
  and object machines);
- a deep one, `RegionalSemanticUndefined` (`eval/tagged_machine.rs`),
  which forces every member and counts `{foo:{}}`, or `{foo: <lazy {}>}`,
  as undefined.
Constructors such as `Values::record`, `Machine::dict_pack` and pattern
results never normalize, so a dict may hold `{}` members, and which check a
site uses changes its answer.

**Deep union.**
- Runtime: `merge_dicts_in` and `MergeDuplicate` (`eval/dict_machine.rs`),
  also behind `object_from_dict`/`dict_as_object`
  (`eval/object_builtin_machine.rs`).
- Literals: `{a.b:1, a.c:2}` lowers to a union of nested singletons that
  only deep merge reconciles (`resolve/expression.rs`).
- Users: one live deep union in Glam code, a test
  (`api/tests.rs`, `{left, right}` sharing `nested`). No static dotted
  paths share a prefix anywhere. Computed paths share one in a test
  (`g_syntax/tests.rs`, `{[1,2]:B, (path):C}`) and in Syntax.md's example.
- The compiler's records built by `DictUnion` (9 sites) join distinct
  names; module, object and `with` definitions are ordered updates.

**`{}` normalized away.** `DictSingleton`, `DictFromEntries`, `DictUpdate`
(which also prunes parents left empty), `closed_dict`, dict keys
(`key_from_value`: `{key:{}}` as a key is `{}`), equality (a missing key
pairs with `{}`), and pattern remainders (pruned parents).

**Reads of what is missing give `{}`.** Static and computed access
(`whnf.rs`, `access_machine.rs`); an unbound name, which resolves to an
access on `final_defs`; `.get`, `.heap.get`, `.vol.get`, `.env`, store
reads and query state; the pure handler's, macros', tokens' and the net
builder's `get`; object `name`/`spec` defaults; the turn API's
`dict_get`, `dict_unpack`, `dict_extract`, `heap_get`, `volume_get`,
`local_get`; the public API's `access` ("returning `{}` for a missing
key"); the CLI's optional configuration, `conf.env`, `conf.log`, `conf.cli`
and `conf.completion_script`, through `std.default`.

**Definedness tested by `{}`.**
- The compiler: `=`/`:=` lower to `assert_undefined`/`assert_defined`
  annotations; `Default` over `meta.refl` and journals; the reflection
  scanner's `== {}` guards; `defined_or`/`require_defined`/`std.default`
  (`Builtin::Default`, deep check); macro selection.
- Effect recognition: `{eff:P, …}` is an effect only where every other
  member is deeply undefined (`whnf.rs`), and a tuple's extra members must
  be undefined to order.
- Glam code: 26 `== {}` sites in 6 files, mostly tests reading `.try`'s
  `r.err`/`r.wait`/`r.conflict`, state after a rollback, or a missing env
  var; one in `samples/config/direct_assembly.g`.
- The **wait idiom**, about 23 sites: a cut reads a heap key not yet set,
  gets `{}`, fails or compares, and parks until the key is written.

**Deletion by `{}`.** `.set Path {}`, heap, volume and environment writes,
query retirement, the turn API's `dict_insert*`/`dict_update_in`/
`env_mixin_clear`/`clear`, the pure handler's and net builder's `set`, and
the front end's `x := {}`, `refl := {}` and object `spec` removal.
`direct_assembly.g` deletes `handler.trace` through an update returning
`{}`, and uses `{}` as "absent" in literals (`cursors:{}`, `symbols:{}`).

**Patterns.** `path?:P` passes `{}` where absent (`pattern_machine.rs`,
`finish_absent`); exact path patterns such as
`{selector:key, root.[key]:value}` leave `{root:{}}` behind, which matches
only because it is `{}`.

**Tests.** About 80 Rust tests encode the old semantics, most in
`eval/tests.rs`, `g_syntax/tests.rs`, `reflection/machine/tests.rs`,
`tests/runtime_effects.rs` and `tests/runtime_cuts.rs` (whose helper
`heap_undefined` reads "a missing key reads `{}`").

**Docs** stating the old semantics: Syntax.md ("Dicts", override checks,
`_self`, `?:`, objects' `_spec`/`spec.name`, `object_from_dict`),
SyntaxCheatSheet.md, DistilledDesign.md, Design.md (`.get` defaults,
"set `{}` to erase", `conf.*`), Macros.md, CLI.md, the agent context's
g_syntax, reflection and objects notes, and `dict-literals-compose`.

## Answers (maintainer, 2026-10-24)

1. **Names.** Path-based builtins in `std.dict`, as needed: lookup,
   insert, remove, delete, defines, update and so on, and a shallow union.
   No syntax is optimized for them yet.
2. **Eager or lazy union.** Errors are found opportunistically, where
   evaluation meets them; no annotations to tune it yet.
3. **Override checks** become simpler definedness checks, on the prior
   namespace's path.
4. **Defaults** are for an undefined path, which raises the arity:
   `default Path DefaultVal Dict`, in that order, for pipelining and
   partial application.
5. **Exact path patterns.** The common `dict.remove` prunes the
   hierarchical components it empties, as insert adds them: the two are
   symmetric. Finer-grained functions can come where needed. Patterns
   take a path as `dict.remove` does, so
   `{selector:key, root.[key]:value}` leaves no `{root:{}}` behind.
6. **Effect and method recognition.** A handler recognizes
   `{eff:…, foo:1}` as an effect, which the `.g` compiler does not apply;
   `{apply:…, foo:1}` is applicable but no effect; `{eff:…}` alone is
   both, as is `{eff:…, apply:…, foo:1}`. That holds today too. Ideally
   `.op` would lower to an object defining both `eff` and `apply`, with no
   special case for `{eff:…}`, but performance does not allow it yet.
7. **The wait idiom.** `.try_get` failing within a cut, its read of the
   path's definedness observed, serves; waiting through divergence is
   valid in some cases too.

## Steps

Slices keep the old semantics working until the last flips them.

1. `dict-semantics-review` — the maintainer reviews this note.
2. `dict-presence-ops` — the new operations beside the old: the
   definedness query, for dicts and for heap, local and volume paths,
   observed as a read; `.try_get`, failing where undefined; `.del`; dict
   removal; their turn and public API counterparts.
3. `dict-presence-migration` — definedness checks move to them: the
   compiler's guards, override checks and defaults, `std.default` and the
   CLI's configuration, the wait idiom, `.try` outcome checks, tests,
   samples, `direct_assembly.g`.
4. `dict-path-fusion` — the front end groups static paths; a run-time
   builtin fuses computed paths by shared prefix, rejecting a prefix or a
   repeat; literals stop relying on deep union.
5. `dict-optional-patterns` — `path?:P` matches `[]` or `[V]`; the rule
   for exact path patterns.
6. `dict-empty-is-a-value` — the flip: `{}` is the empty dict, reads of
   what is undefined diverge, union is shallow, with eager and lazy forms
   by annotation, normalization and elision of `{}` go, dict equality is
   structural, and an atom equals the dict it stands for.
7. `dict-docs` — the syntax and design docs, the agent context, and an
   ADR revising `dict-literals-compose`.

Then `api-comparables` resumes, its definedness checks already moved.

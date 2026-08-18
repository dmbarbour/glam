# Value Facade Transition Plan

Status: complete (2026-08-18).

This plan resolves
[CCR-011](../reviews/CodeCleanup_2026-08-15.md#ccr-011--presence-oriented-assemblergetget_optional-remains-a-compatibility-interpreter)
by separating semantic value construction, semantic demand and host
extraction, and bootstrap-specific reflection. Its preliminary rationale was
developed in `docs/.tmp/TmpDiscussion.md`.

## Purpose

The Rust front end currently reaches ordinary Glam semantics through a mixture
of `Values`, methods directly on `Assembler`, immediate methods on `Value`, and
`ReflectionInspector`. Several of these APIs interpret the same concepts a
second time on the host side:

- `Assembler::{get,get_optional}` parse dotted paths and inspect dictionaries;
- `Assembler::apply` enters evaluation while constructing an application;
- `Assembler::binary_slice` separately traverses binary-compatible lists;
- `Assembler::{evaluate,to_binary}` combine client demand and extraction; and
- `ReflectionInspector` currently includes both ordinary evaluation/list
  observation and genuinely reflective inspection.

The target is a public API whose location makes the semantic boundary clear:

1. `Values` constructs runtime-local Glam values and computations without
   demand.
2. `ValueEvaluator` demands values and returns `EvaluatedValue`, whose immediate
   extractors expose the resulting outer semantic value.
3. `ReflectionInspector` exposes runtime-specific, unstable, and potentially
   nondeterministic observations unavailable to ordinary Glam evaluation.

`Assembler` remains a convenient owner and provider of these facades. It does
not remain a parallel semantic interpreter.

## Non-goals

This transition does not:

- stabilize the reflection API;
- introduce a general Rust embedding framework;
- make evaluator failures reproducible;
- add dictionary iteration to ordinary Glam semantics;
- expose compiler-private `Builtin` variants in the public Rust API;
- design streaming retention or assembly-namespace garbage collection;
- optimize list representations; or
- replace `Value` with a garbage-collected handle.

The API remains pre-release. Compatibility shims exist only to stage the
repository migration and may then be removed.

## Target Boundaries

| Facade | Responsibility | May demand? | Stability and reproducibility |
|---|---|---:|---|
| `Values` | Construct semantic values and computations | No | Stable semantic surface |
| `ValueEvaluator` | Reach WHNF and return an evaluated host view | Yes | Successful values are reproducible |
| `ReflectionInspector` | Inspect current runtime representation and privileged structure | Capability-specific | Unstable and outside reproducibility |

The intended acquisition shape is explicit:

```rust
let values = assembler.values();
let evaluator = assembler.evaluator();
let reflection = assembler.reflection();
```

Reflection is deliberately not obtained from `ValueEvaluator`. A call site
which crosses the reflection boundary should say so visibly.

`ValueEvaluator` is a capability-bearing handle rather than merely a namespace
for one `Assembler` method. This preserves a future shape such as:

```rust
assembler
    .evaluator()
    .with_tuning(parameters)
    .eval(&value)
```

Tuning is deferred and must not change successful semantic results. It may
select scheduling, normalization, resource, or diagnostic policy and may
therefore affect whether or how evaluation fails or diverges, consistent with
the successful-result reproducibility rule below.

### Successful-result reproducibility

The evaluator has an asymmetric guarantee. If two evaluations successfully
produce values, those values must be equivalent under Glam's confluent
semantics. Annotations and reflection gates may instead cause either
evaluation to fail or diverge. Failures need not be equivalent: diagnostic
data, context, or runtime observations may depend on scheduling, reflection
state, or bootstrap version.

Thus, successful results remain reproducible while failure behavior is not a
reproducibility promise.

## Semantic Invariants

1. Every public `Value` remains bound to one `EvaluationRuntime`; all three
   facades reject foreign-runtime values at their boundary.
2. A successful `Values` call does not claim a lazy value, subscribe to a
   promise, run a reflection gate, reduce an interaction net, or otherwise
   demand an input.
3. `Values` composition has the meaning of an ordinary `.g` expression, not a
   separate host interpretation of that expression.
4. `ValueEvaluator` reports structured evaluation failures with their context.
   It does not collapse them into ordinary Rust type mismatches.
5. A representation observed before demand is not presented as semantic WHNF
   information.
6. Ordinary dictionary selection and `:`/`?:` pattern behavior are semantic.
   Dictionary iteration is reflective.
7. Compact binary data remains a valid byte-valued list representation. List
   operations therefore apply to it without a binary-specific slicing path.
8. `ReflectionInspector` documentation identifies its observations as
   runtime-specific, unstable, and outside assembly reproducibility.

## Provisional Target Surface

Names are provisional until their implementation checkpoint. The ownership
and demand behavior are not.

### `Values`

```rust
impl Values {
    // Existing literal and aggregate constructors remain.

    pub fn bytes(&self, bytes: impl Into<Bytes>) -> Value;

    pub fn anno(
        &self,
        annotation: Value,
        target: Value,
    ) -> Result<Value, Error>;

    pub fn apply(
        &self,
        function: &Value,
        arguments: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error>;

    pub fn access_path(
        &self,
        root: &Value,
        keys: impl IntoIterator<Item = Value>,
    ) -> Result<Value, Error>;

    pub fn access_names<I, S>(
        &self,
        root: &Value,
        names: I,
    ) -> Result<Value, Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>;

    pub fn list_slice(
        &self,
        value: &Value,
        range: Range<usize>,
    ) -> Result<Value, Error>;

    pub fn anno_binary(&self, value: Value) -> Result<Value, Error>;

    pub fn anno_array(&self, value: Value) -> Result<Value, Error>;

    pub fn anno_deque(&self, value: Value) -> Result<Value, Error>;
}
```

`apply` constructs a left-associated application spine for all arguments
without evaluating the callee. `access_names` turns each complete name into an
atom key; it never reparses dots within a name. Empty paths and empty argument
lists return the original value after checking runtime provenance.

`list_slice` constructs the ordinary `slice Start End Value` computation using
the runtime's semantic builtin representation. It accepts compact binaries,
ordinary lists, concatenations, and lazy list computations uniformly when
later demanded. Range validity is checked by ordinary slice evaluation unless
the Rust range itself is structurally impossible to represent.

`bytes` injects host bytes as compact binary data. `anno` is the generic
no-demand counterpart of the source `anno` form. `anno_binary` constructs
`anno 'binary Value`; it does not validate or demand `Value`. The names keep
the Rust representation (`bytes`) distinct from the semantic assertion
(`binary`). Temporary `binary` and `annotate` compatibility names can delegate
during migration and then be removed.

`anno_array` and `anno_deque` are no-demand conveniences over the already
implemented `array` and `deque` annotations:

- demanding `anno 'array List` forces the complete list spine and returns one
  strict value-list representation; a compact binary becomes a list of small
  integer values;
- demanding `anno 'deque List` forces deferred list chunks and returns the
  balanced persistent finger-tree representation, preserving compact byte
  chunks; and
- neither annotation demands the values stored as list elements.

`array` is the canonical preparation for host iteration. `deque` is useful
when a client intends to preserve efficient end and split operations. Both
eliminate unresolved list-spine thunks before returning successfully.

Additional list conveniences such as `list_at`, `list_split`, and
`list_split_end` may be added in the same implementation style if actual
migrations need them. They are not prerequisites for deleting
`Assembler::binary_slice`.

### Basic dictionary composition

The initial public dictionary layer stays below pattern decomposition:

```rust
impl Values {
    pub fn empty_dict(&self) -> Value;

    pub fn dict_singleton(
        &self,
        key: Value,
        value: Value,
    ) -> Result<Value, Error>;

    pub fn dict_union(
        &self,
        left: Value,
        right: Value,
    ) -> Result<Value, Error>;

    pub fn dict_update(
        &self,
        dictionary: Value,
        path: Value,
        new_value: Value,
    ) -> Result<Value, Error>;
}
```

Together with `access_path`, these construct the ordinary semantics of `{}`,
dictionary literals, hierarchical union, and path updates. `empty_dict`
constructs the immediate empty dictionary, which is also Glam's undefined
value and the identity for union. Updating a path to `{}` performs semantic
removal. The other operations validate runtime provenance but do not evaluate
keys, paths, dictionaries, or values.

The existing `empty_record` name becomes a transitional delegate to
`empty_dict` and is removed after callers migrate. There is no distinct empty
record representation: records are merely dictionaries whose keys are names.

These operations deliberately do not return a combined `{value, rest}`
pattern-lowering record. Most callers need either lookup or update, while the
remainder exists only when lowering a larger dictionary pattern. A Glam helper
can combine access, matching, and update when it genuinely needs “take”
semantics, and can choose a result appropriate to its caller—an effect, tagged
result, fallback value, or something else.

Required and optional host-selection policy will initially be expressed by a
small user-expressible Glam helper and invoked through `Values::apply`. The
bootstrap may cache the lowered helper as an acceleration, just as it caches
other compiler functions, but its semantics must remain expressible in Glam.
The public facade does not standardize `Option<Value>`, a zero-or-one list, or
the compiler's internal pattern-step record without demonstrated broader use.

### `ValueEvaluator`

```rust
let evaluator = assembler.evaluator();
let binary = values.anno_binary(value.clone())?;
let evaluated = evaluator.eval(&binary)?;
let bytes = evaluated.as_bytes();
```

The evaluator owns:

- client demand to WHNF;
- propagation of structured `EvaluationHalt` data as public errors;
- construction of the `EvaluatedValue` view used for immediate scalar and
  structural extraction.

It does not own application, path composition, list slicing, annotation,
binary conversion policy, dictionary matching, or type-specific extraction.

`eval` returns `EvaluatedValue`, a Rust type-state witness around an ordinary
`Value`. Its private construction proves only that the outer value reached
WHNF; dictionary members, list elements, function results, and other nested
values may remain lazy. It is not a second semantic representation.

The witness is cheap to discard:

```rust
let value: Value = evaluated.into_value();
// Equivalently: Value::from(evaluated)
```

It also offers `as_value()` for borrowing. It should not initially implement
`Deref<Target = Value>` because an explicit conversion makes loss of the WHNF
witness visible.

Immediate extraction lives on `EvaluatedValue`:

- `as_bytes` for an already compact binary result;
- `array_items` for an already strict value array;
- `as_i64` and `as_rational_i64` for small exact numbers;
- `as_f64` for the existing documented lossy conversion;
- `number_text` for canonical, exact arbitrary-precision number text.

`EvaluatedValue` does not expose a general `kind` or `is_undefined` shortcut.
Current representation kind belongs to reflection. Logical undefined testing
may require recursively observing dictionary members, so it belongs to the
ordinary Glam matching/assertion helpers invoked through `Values`, not an
immediate outer-WHNF view. Crate-private immediate checks may remain where an
internal representation invariant already guarantees their meaning.

`ValueEvaluator::eval(&Value)` is the sole initial evaluator operation. The
name is intentionally shorter than `evaluate_whnf`, while its documentation
states that evaluation means demand to outer WHNF. Type-specific extraction
belongs to `EvaluatedValue`. A client which wants bytes composes
`Values::anno_binary`, evaluates the result, and calls `as_bytes`; a client
which wants a number evaluates and calls the corresponding number extractor.

Extraction which would demand additional nested structure is not an immediate
`EvaluatedValue` operation. Phase 2A adds:

```rust
pub fn array_items(&self) -> Option<Vec<Value>>;
```

The method recognizes the strict value-array representation produced by
`Values::anno_array` (including an empty array), clones its runtime-root
handles into one owned `Vec`, and does not demand the elements. It declines a
non-array list representation rather than traversing a deque, concatenation,
byte chunk, or unresolved list thunk. This keeps the immediate API simple and
makes the allocation visible in its return type.

Clients working with a large list or deque can construct an ordinary semantic
slice first, normalize only that slice with `anno_array`, evaluate it, and
extract the resulting small array. `anno_deque` remains useful for semantic
list composition but does not require a second host view abstraction in this
transition.

### `ReflectionInspector`

The reflective facade retains or gains operations such as:

- associated metadata inspection;
- dictionary entry iteration in canonical key order;
- atom identity/key inspection;
- current representation kind;
- lazy/promise status and ownership where safe to expose;
- eventual reference equality; and
- interaction-net inspection.

Detailed lazy-status and net-inspection APIs are deferred. This transition only
needs enough surface to move existing representation-sensitive methods to the
right boundary.

`ReflectionInspector::{evaluate,list_items}` are not inherently reflective:
WHNF demand moves to `ValueEvaluator`, while whole-list enumeration becomes
explicit array annotation, evaluation, and `array_items` extraction. A
reflective operation may still demand when its own
documented protocol requires it, but that does not make ordinary semantic
observation reflective.

## Current Compatibility Surface

| Current API | Target owner | Retirement intent |
|---|---|---|
| `Values::empty_record` | `Values::empty_dict` | Remove the false suggestion that records have a distinct empty representation |
| `Assembler::apply` | `Values::apply` | Compose lazily, then evaluate explicitly if needed |
| `Assembler::{get,get_optional}` | `Values` access plus caller-selected Glam helpers | Remove host dotted-path and presence interpreter |
| `Assembler::evaluate` | `ValueEvaluator::eval` | Compatibility delegate, then remove |
| `Assembler::to_binary` | `Values::anno_binary`, `ValueEvaluator::eval`, then `EvaluatedValue::as_bytes` | Keep one semantic binary assertion path |
| `Assembler::binary_slice` | `Values::list_slice`, binary annotation, evaluation, and `EvaluatedValue::as_bytes` | Delete separate list traversal |
| `ReflectionInspector::evaluate` | `ValueEvaluator::eval` | Move ordinary demand out of reflection |
| `ReflectionInspector::list_items` | `Values::anno_array`, `ValueEvaluator::eval`, then materialized-list iteration on `EvaluatedValue` | Make spine demand semantic and explicit |
| `Value::kind` | `ReflectionInspector` | Treat kind as current runtime representation |
| `Value::is_undefined` | User-expressible Glam matching/assertion helper | Do not confuse exact empty-dict representation with logical undefined semantics |
| `Value::as_*` | `EvaluatedValue` | Preserve scalar extraction functionality; retain crate-private fast paths |

`Value::runtime_id` remains an ordinary ownership property. It does not cross
the reflection boundary.

## Resolved API Questions

### Evaluated-result wrapper

`ValueEvaluator::eval` returns `EvaluatedValue`, not bare `Value`. The wrapper
is a type-state witness that prevents a lazy number from looking unlike the
same number after demand. It is freely convertible back to `Value`; conversion
discards only the static WHNF guarantee.

### Binary naming

- `Values::bytes` injects Rust bytes as compact binary data.
- `Values::anno` constructs a generic annotation.
- `Values::anno_binary` constructs the no-demand binary assertion.
- `Values::{anno_array,anno_deque}` construct no-demand list-representation
  assertions.
- `EvaluatedValue::as_bytes` extracts bytes after the binary assertion has
  evaluated successfully.

This is intentionally asymmetric: `bytes` names Rust input/output, while
`binary` names the Glam annotation. The evaluator itself has no binary-specific
operation.

### Immediate extraction scope

The pre-transition public surface provided `as_binary`, `as_i64`,
`as_rational_i64`, `as_f64`, and `as_number_text`, plus `is_undefined` and
`kind`. The implementation audit found:

- byte extraction is used broadly by `main`, CLI, logger, and tests;
- `i64` and canonical number text have production users;
- rational-pair and `f64` extraction are currently exercised only by public
  API tests; and
- no structured public `BigInt` or `BigRational` extractor exists. Arbitrary
  precision is retained internally and exposed losslessly as canonical text.

The transition therefore changes ownership, not numerical functionality.
Scalar extraction moves to `EvaluatedValue`; it is not removed merely for
cleanup. `kind` becomes reflective, while undefined testing uses semantic
Glam matching/assertion machinery. Adding public number-library types or
type-specific evaluator conveniences remains deferred.

## Pre-implementation Review

Reviewed on 2026-08-17 before Phase 0. The facade split and transition order
are sound, and no architectural blocker prevents beginning the inventory.
This review made four corrections:

1. A general value `kind` remains a representation observation and therefore
   belongs to reflection. `EvaluatedValue` exposes typed extractors, not a
   second public kind taxonomy.
2. Logical undefined testing belongs to ordinary Glam matching/assertion
   semantics. It is not equivalent to recognizing the current empty-dictionary
   representation after one WHNF step.
3. Verification must establish both positive results and absence of hidden
   demand. Broad end-to-end tests alone cannot prove that a constructor did not
   claim a lazy value, subscribe to a promise, or reduce a net.
4. Phase 3 introduces and documents the explicit reflection replacement, but
   compatibility methods cannot be removed before Phase 4 migrates production
   callers. Their deletion therefore belongs to the Phase 5 audit.

The prior open list-view question is resolved by `array_items`. The proposed
public dictionary-pattern result is withdrawn: Phase 1C instead establishes
basic dictionary composition and leaves higher-level match result shapes to
user-expressible helpers until repeated usage supports standardizing one.

## Transition Phases

Each checkpoint must leave the repository formatted, lint-clean, and passing
tests. Completion is recorded here as phases land.

### Phase 0 — Inventory and Contract Latching

Status: complete (2026-08-17).

#### Phase 0A — Classify callers

- Inventory every call to the six `Assembler` compatibility methods,
  `ReflectionInspector::{evaluate,list_items}`, and public immediate `Value`
  observations.
- Classify each as construction, semantic demand/extraction, or reflection.
- Record any caller which truly needs presence without final-member demand.
- Confirm that no production caller requires `Option<Value>` as a core API.

Verification:

- The inventory accounts separately for `src/main.rs`, public API tests,
  macro protocols, diagnostics/viewer code, and reflection-store tests.
- Any surprising semantics become an explicit amendment to this plan before
  implementation.

Phase 0A result (2026-08-17): complete.

The compiler-assisted inventory found 423 typed references across all Cargo
targets. The largest test cohorts are `tests/public_api.rs` (133), reflection
tests (102), `api` tests (72), macro-expansion tests (29), CLI tests (14), and
reflection-store tests (11). Production sources contain 43 typed references;
three are compatibility implementations delegating to another compatibility
method rather than independent policy callers.

The public surface totals are:

| Surface | Typed references | Classification |
| --- | ---: | --- |
| `Assembler::apply` | 22 | semantic composition followed by demand |
| `Assembler::evaluate` | 56 | semantic WHNF demand |
| `Assembler::{get,get_optional}` | 155 | semantic access plus caller-local required/fallback policy |
| `Assembler::to_binary` | 83 | semantic binary assertion and host extraction |
| `Assembler::binary_slice` | 4 | compatibility extraction; no production caller |
| `ReflectionInspector::{evaluate,list_items}` | 7 | diagnostic/viewer representation inspection |
| immediate `Value` observations | 96 | scalar extraction or representation inspection |

Production `get_optional` callers use absence only to select a default
configuration/CLI behavior or omit an optional diagnostic field. None needs
presence to be a first-class Glam result, and none depends on distinguishing
absence without later applying its own policy. Required lookups likewise use
ordinary access followed by an embedding-level assertion. The inventory
therefore confirms that no production caller needs `Option<Value>` as a core
semantic API.

The representation-sensitive production uses are confined to diagnostic
transport decoding and the default logger/viewer. Dictionary and atom
iteration there remain explicit reflection. Logger calls to
`ReflectionInspector::evaluate` and `list_items` mix ordinary WHNF/list
materialization with that reflection policy and should migrate to the semantic
evaluator/list facade while retaining reflection only for representation
inspection.

No inventory finding changes the target API. Direct `.g` equivalence fixtures
for constructors which do not exist yet will land with their Phase 1
implementation; Phase 0B latches the existing access/annotation and demand
boundaries first.

#### Phase 0B — Latch boundary behavior

- Add focused characterization tests showing that the existing
  `Values::{access,annotate}` constructors do not demand an unresolved lazy
  value or promise.
- Establish the crate-private claim/status test helpers used to apply the same
  negative assertion to the new Phase 1 constructors as each one lands.
- Preserve structured failure/context tests for WHNF and binary assertion.
- Preserve foreign-runtime rejection at construction and demand boundaries.
- Add direct `.g` comparison fixtures for access, slicing, dictionary union and
  update, and the small required/fallback helpers needed by current callers.

The tests should expose the current mismatch before changing behavior where
feasible, then be updated only when the intended new contract lands.

Phase 0B result (2026-08-17): complete.

The new characterization test observes both the producer state of an
unresolved lazy value and the exact subscription count of an unresolved
promise. `Values::{access,annotate}` leave both untouched, and their constructed
results remain unclaimed lazy values. Existing tests continue to cover
structured binary failures, nested evaluation context, path-demand context,
and foreign-runtime rejection at construction and demand boundaries. The full
format, lint, and test suite passes at this checkpoint.

### Phase 1 — No-demand `Values` Composition

Status: complete (2026-08-17).

#### Phase 1A — Batched application and paths

- Add lazy batched `Values::apply` using application `LazySource` construction,
  not `eval::apply_values`.
- Add `access_path` and `access_names` as folds over `Values::access`.
- Define empty-iterator behavior and reject foreign-runtime members before
  constructing any result.
- Migrate a small internal cohort to prove the API without removing shims.

Verification:

- Lazy/promised callees remain unclaimed until result demand.
- Argument order and application associativity match `.g` application.
- Names containing dots remain one name.
- Empty paths and argument lists preserve runtime identity.

Phase 1A result (2026-08-17): complete.

`Values::apply` now retains a whole application batch in one lazy semantic
application source. `access_path` validates all keys before constructing the
access chain, and `access_names` treats each supplied string as one complete
atom name. Tests compare batched and nested construction with direct `.g`
application, verify argument order, prove promised callees remain
unsubscribed, distinguish a dotted name from two path components, cover empty
identity behavior, and reject foreign-runtime roots and members. Assembly
result selection is the first migrated production cohort.

#### Phase 1B — List slicing and binary annotation

- Add no-demand list slicing through the ordinary `Slice` builtin.
- Add no-demand binary, array, and deque annotation helpers.
- Migrate existing binary-slice tests to construct the slice under `Values`
  and extract it through the temporary evaluator path.
- Retain the compatibility method only as a delegate if a staged migration
  still needs it; it must no longer own a list traversal implementation.

Verification covers compact binaries, value lists, lazy concatenations, empty
and boundary ranges, invalid ranges, structured item failures, array
materialization, deque balancing, preservation of lazy element values, and
proof that construction performs no demand.

Phase 1B result (2026-08-17): complete.

`Values` now distinguishes host `bytes` from semantic `anno`, with temporary
`binary` and `annotate` aliases, and provides lazy slice, binary, array, and
deque constructors. `Assembler::binary_slice` delegates through semantic
slice and binary annotation; its separate recursive list traversal and the
now-unused range-output helpers have been removed. Tests cover compact and
value-list slices, invalid ranges at demand time, strict array and balanced
deque materialization, preservation of lazy elements, exact no-demand promise
state, and foreign-runtime rejection. The public slicing test now exercises
the semantic composition path rather than the compatibility helper.

#### Phase 1C — Basic dictionary composition

- Add `empty_dict` plus no-demand semantic singleton, hierarchical-union, and
  path-update constructors without exposing compiler-private builtin
  identities.
- Migrate `empty_record` callers to `empty_dict`, retaining it only as a
  temporary delegate during the transition.
- Treat update-to-`{}` as the ordinary deletion operation rather than adding a
  separate Rust-only removal primitive.
- Define the current required/fallback lookup policies as small Glam helpers,
  invoked through `Values::apply`; cache their lowered values only as an
  implementation acceleration.
- Keep compiler pattern-step operations private and do not add an
  `Option<Value>` evaluator convenience.

Verification compares each constructor and helper with its `.g` definition.
Cases cover empty-dictionary identity, missing fields, explicit and recursively
logical `{}`, wrong intermediate kinds, lazy and failing fields, nested paths,
union conflicts, update introduction/replacement/removal, and proof that
construction performs no demand. No public result contains an unused
dictionary remainder.

Phase 1C result (2026-08-17): complete.

`empty_dict` is now the primary immediate constructor and all in-repository
callers have migrated from `empty_record`; the old name remains only as a
temporary delegate. Singleton, hierarchical union, and path update are lazy
semantic builtin calls hidden behind `Values`. Source-comparison tests cover
nested merge, ambiguity, introduction, replacement, and deletion by `{}`;
separate state assertions prove construction leaves promised operands
unsubscribed.

The cached `defined_or` and `require_defined` functions are lowered closed Glam
helpers. The first uses ordinary recursive equality with `{}`, so dictionaries
whose fields are themselves logically undefined select the fallback. The
second uses the existing semantic defined assertion. They remain crate-private
until Phase 4 caller migration demonstrates whether any broader convenience
surface is warranted; no `Option<Value>` or remainder record was introduced.

### Phase 2 — `ValueEvaluator`

Status: complete (2026-08-17).

#### Phase 2A — Introduce `EvaluatedValue`

- Add the private-field wrapper around an ordinary `Value`.
- Add `as_value`, `into_value`, `From<EvaluatedValue> for Value`, and `Clone`.
- Move the existing immediate scalar views onto the wrapper while retaining
  temporary `Value` delegates for staged migration.
- Add `array_items` as an owned `Vec<Value>` extraction for the strict
  value-array representation. It must decline every other list representation
  rather than traversing or forcing it.
- Document explicitly that only the outer layer is in WHNF.

Verification:

- Only `ValueEvaluator` and crate-private trusted paths can construct the
  wrapper; public conversions can only borrow it or discard the witness.
- Converting, borrowing, and cloning preserve runtime provenance and value
  identity, do not evaluate nested values, and remain valid after the
  short-lived evaluator handle is dropped.
- Scalar tests cover signed bounds, exact rational pairs, the documented lossy
  float conversion, and canonical arbitrary-precision text.
- Array extraction preserves order and leaves deliberately lazy elements
  unclaimed. The canonical empty list succeeds (an empty array and empty deque
  have the same representation); nonempty deques, compact byte chunks,
  concatenations, and unresolved list spines are declined without changing
  claim or subscription state.

Phase 2A result (2026-08-17): complete.

Public `EvaluatedValue` now witnesses outer WHNF while retaining an ordinary
runtime-rooted `Value`. It supports explicit borrow/conversion, byte and number
views, and owned extraction from exactly one strict value leaf. Tests cover
numeric bounds and arbitrary precision, witness lifetime after the assembler
handle is dropped, lazy array elements, compact bytes, concatenations,
nonempty deques, and deferred spines. Array inspection performs no demand. The
canonical empty list necessarily qualifies regardless of whether it was
produced by `array` or `deque`, since those empty representations coincide.

#### Phase 2B — Introduce evaluator facade

- Add `Assembler::evaluator()` backed by the existing client-demand session.
- Make `eval` demand WHNF, return `EvaluatedValue`, and preserve structured
  failure conversion.
- Keep the evaluator free of type-specific extraction and nested-structure
  traversal.
- Keep temporary `Assembler` delegates while migrations proceed.

Verification covers successful-result equivalence across worker schedules,
structured but not necessarily identical failures, foreign-runtime rejection,
and type-mismatch diagnostics after demand. Promise and lazy suspension tests
use barriers or explicit resolver steps to force wait, wake, success, and
failure orderings; sleeps and repetition are supplementary stress checks, not
the primary proof. An already-WHNF value completes without queueing work, and
repeated evaluation of one fulfilled lazy value does not recompute it.

Phase 2B result (2026-08-17): complete.

`Assembler::evaluator()` now provides the sole `ValueEvaluator::eval`
operation, backed by the existing client-demand context. The temporary
`Assembler::evaluate` method delegates through it. Tests cover immediate
WHNF, foreign-runtime rejection, a retained resolver-promise subscription
which wakes a later demand, lazy result caching, and structured failure
preservation. A separate worker-claimed lazy test proves that synchronous
evaluation waits when the runtime has an authoritative owner for future
progress. Resolver-owned promises deliberately may report stable blockage:
an arbitrary external resolver is not runtime-owned progress.

#### Phase 2C — Separate composition from extraction

- Migrate byte extraction to `Values::anno_binary`, `ValueEvaluator::eval`, and
  `EvaluatedValue::as_bytes`.
- Ensure slicing is never reintroduced into the evaluator.
- Make each migrated caller visibly compose under `Values` before extracting.

Verification compares the old and new byte-output paths before the old path is
removed. The cases include compact and value-list binaries, empty data, lazy
concatenation, an invalid byte, a non-list value, and an upstream structured
failure. A prefix slice with a deliberately poisoned unused tail proves that
the new path preserves slice demand rather than materializing the whole input.
The public evaluator surface is audited to contain only `eval`.

Phase 2C result (2026-08-17): complete.

Compatibility byte extraction now composes `anno_binary`, `eval`, and
`as_bytes`; it no longer performs its own semantic conversion. Slicing remains
entirely under `Values`. A poisoned-tail test proves that prefix slicing plus
binary assertion does not materialize an unused deferred suffix. Existing
structured failure, invalid byte, non-list, compact binary, and value-list
tests continue to exercise this single path.

### Phase 3 — Explicit Reflection Boundary

Status: complete (2026-08-17).

#### Phase 3A — Split the current inspector

- Introduce the explicit evaluator and materialized-list replacements for
  `ReflectionInspector::{evaluate,list_items}` and mark the old methods as
  transitional.
- Migrate a small inspector-specific caller cohort to explicit array
  annotation, evaluation, and `EvaluatedValue::array_items` extraction.
- Retain metadata, dictionary iteration, and atom-key inspection.
- Rewrite facade documentation to state instability and reproducibility limits.

Verification compares replacement list enumeration with the old inspector on
the same lists before deletion. It additionally proves that `array` expands
compact bytes into small integer values, a deque must be sliced or normalized
before host extraction, dictionary iteration order and atom identity remain
unchanged, and no ordinary caller acquires reflection merely to evaluate or
enumerate a semantic list.

Phase 3A result (2026-08-17): complete.

Reflection documentation now states its runtime-specific, unstable, and
non-reproducible boundary. Diagnostic, CLI, and public API callers enumerate
lists through `anno_array`, `eval`, and `array_items`; ordinary WHNF callers
use the evaluator. The transitional inspector methods remain only for direct
replacement comparison and are scheduled for Phase 5 deletion. Tests verify
equivalent enumeration, compact-byte expansion, and preservation of the
existing reflective dictionary and atom operations.

#### Phase 3B — Representation observation

- Move pre-demand `Value::kind` behavior to a clearly named reflection method.
- Establish ordinary Glam matching/assertion helpers as the replacement for
  public `is_undefined`; migrate production callers in Phase 4 rather than
  adding another representation check.
- Keep immediate `Value` observations as documented transitional delegates
  until Phase 4 migrates their production callers.
- Defer detailed lazy-state APIs unless needed to replace a real caller.

Verification shows that representation kind can report an unresolved lazy
value without forcing it, while ordinary semantic callers use evaluation,
extractors, and patterns rather than representation-kind branching.

Phase 3B result (2026-08-17): complete.

`ReflectionInspector::kind` now performs the runtime-provenance check and
reports current representation without demand. The default logger's kind
rendering uses that explicit boundary. An unresolved-promise test proves kind
inspection neither subscribes nor assigns it. Immediate `Value` delegates and
remaining undefined-policy callers are intentionally retained only until the
coordinated Phase 4 migration, avoiding a second temporary public selection
API.

### Phase 4 — Production Caller Migration

Status: complete (2026-08-18).

#### Phase 4A — Configuration and CLI

- Replace dotted-string `get` calls for `conf.env`, `conf.cli`, `conf.log`, and
  related configuration entries with semantic name-array paths.
- Apply the source-defined required/fallback helpers where configuration policy
  must distinguish a defined value from logical `{}`.
- Replace application-plus-extraction chains with explicit `Values` then
  `ValueEvaluator` calls.

Verification covers present, missing, logically undefined, failing, and
divergent configuration entries through the real CLI lifecycle. Path-demand
context remains an explicit host composition policy (for example, an
`anno context:...` wrapper at the configuration boundary), not hidden behavior
inside `Values::access_names`.

Phase 4A result (2026-08-18): complete. Configuration selection now composes
semantic name-array access, cached Glam defined/fallback policy, and explicit
evaluation. `conf.env`, `conf.cli`, `conf.log`, and completion-script selection
wrap the delayed candidate in an explicit `anno context:...` path frame; the
facade itself neither parses dotted host strings nor invents demand context.
Focused CLI library and end-to-end configuration tests pass for missing,
defined, failing, and fallback entries.

#### Phase 4B — Diagnostics, viewers, and logger

- Use semantic access for known diagnostic paths.
- Keep dictionary iteration or metadata inspection visibly under reflection.
- Preserve context and origin frames on evaluator failure.
- Do not turn the logger into a reason to restore host path interpretation.

Verification exercises plain and structured messages, origins, nested context
frames, associated metadata inspection, and logger fallback. It confirms that
reflection is used only for the privileged observations and that evaluator
failures retain context through final rendering.

Phase 4B result (2026-08-18): complete. The default logger uses semantic
access/evaluation for known message fields and reserves reflection for atom
identity, dictionary enumeration, and pre-demand representation kind. Context
arrays cross through `anno_array` and `EvaluatedValue::array_items`; scalar
text and number rendering crosses through evaluated extractors. The binary's
22 logger/supervisor tests and all 49 CLI integration tests pass.

#### Phase 4C — Assembly, macro, and reflection hosts

- Move assembly binary output to list composition, binary annotation,
  evaluation, and `EvaluatedValue` extraction.
- Migrate macro protocol extraction.
- Migrate volume-capability and reflection-task call construction to batched
  `Values::apply` and semantic paths.

Verification includes executable samples, binary imports, macro protocol
contracts, volume capabilities, reflection-task construction, and worker-zero
and worker-enabled assembly. The same successful assembly bytes must result
from both schedules.

Phase 4C result (2026-08-18): complete. Assembly output, configured completion
scripts, diagnostic formatting, and `.write_stderr` now explicitly construct
binary annotations, evaluate them, and extract from `EvaluatedValue`.
Transactional stderr output retains the already evaluated runtime value rather
than extracting and reinjecting it. Macro protocol extraction uses the same
composition. Macro protocols, executable samples, and hello assemblies pass.

#### Phase 4D — Public tests and examples

- Rewrite public API tests around the three explicit facades.
- Retain a small number of tests for temporary compatibility delegates until
  their deletion phase.
- Update examples and API documentation before removing old methods.

Verification treats public API examples as compile-tested clients. Each
example must make construction, evaluation, and reflection crossings visible;
none may import crate-private representations or depend on a compatibility
delegate not named as transitional.

Phase 4D result (2026-08-18): complete. `tests/public_api.rs` now acts as an
external facade client: construction goes through `Values`, demand and scalar
extraction through `ValueEvaluator`/`EvaluatedValue`, and representation
inspection through `ReflectionInspector`. Its local readability helpers are
compositions of those exported operations rather than compatibility delegates.
All 44 public API tests pass.

Phase 4 review: the production migration did not require a new facade. It
confirmed that path-demand context is caller policy and raised the question of
whether buffered output should require an evaluated runtime root. Phase 5B.2
resolves that question in favor of unrestricted `Value` transport and
callback-selected demand. Phase 5 has a larger internal-test migration than
its original two checkpoints suggested, so the removal work is split below;
this changes staging, not the target API.

### Phase 5 — Compatibility Removal and Audit

Status: complete (2026-08-18).

#### Phase 5A — Remove `Assembler` semantic helpers

##### Phase 5A.1 — Migrate internal test clients

- Add narrowly scoped test helpers which compose `Values`, `ValueEvaluator`,
  and `EvaluatedValue`; do not add production compatibility surface.
- Migrate unit tests in `api`, `reflection`, the reflection store, CLI, and the
  binary crate away from the retiring methods.
- Preserve tests whose purpose is the old/new differential only until their
  replacement assertion is established, then remove the old side.

##### Phase 5A.2 — Delete compatibility methods

- Remove `Assembler::{apply,get,get_optional,evaluate,to_binary,binary_slice}`.
- Remove transitional `ReflectionInspector::{evaluate,list_items}`.
- Remove the transitional `Values::empty_record` naming delegate.
- Remove the private binary-slice interpreter and now-unused path-context glue.
- Remove imports, tests, and comments which describe the transitional surface.

Verification includes a repository-wide symbol search for every removed
method and its private implementation helpers, followed by a public API build.

Phase 5A result (2026-08-18): complete. Internal tests now use a test-only
composition trait backed exclusively by `Values`, `ValueEvaluator`, and
`EvaluatedValue`; tests which intentionally retain an unresolved value use
`Values::access_path` directly. The `Assembler` semantic helpers, transitional
reflection methods, `Values::empty_record`, private binary-slice interpreter,
and obsolete path-context helper have been removed. `Values::empty_dict` is
the sole empty dictionary/undefined constructor. Repository-wide symbol
searches find no retired public method, and the 1164 library, 22 binary, 44
public API, and 5 macro-protocol tests pass.

One migration test exposed an incorrect timing assumption rather than a
runtime defect: a resolver-owned promise is not authoritative runtime-owned
future progress, so synchronous evaluation may report stable blockage before
an external resolver runs. The deterministic replacement verifies that the
blocked lazy producer retains exactly one promise subscription, resolution
wakes it, and a later demand succeeds. The separate worker-claimed lazy test
continues to prove that synchronous evaluation waits when progress has a
runtime-known owner.

#### Phase 5B — Audit `Value` and reflection

##### Phase 5B.1 — Migrate immediate observers

- Move public-client scalar assertions to `EvaluatedValue` and pre-demand kind
  assertions to reflection.
- Keep direct core matches inside trusted implementation modules and test them
  as implementation details, not through the public `Value` surface.

Phase 5B.1 result (2026-08-18): complete. Public clients retain the
`EvaluatedValue` witness through scalar and byte extraction, while pre-demand
kind and empty-dictionary inspection are explicit reflection. The CLI search
renderer no longer evaluates a value and immediately discards its witness;
binary-crate tests likewise use evaluated extraction. Immediate number,
undefined, and kind views on arbitrary `Value` are no longer public. Internal
core matches remain available for transport decoding and implementation tests.
Format, all-target clippy, the binary tests, and all 44 public facade tests pass
at this checkpoint.

##### Phase 5B.2 — Narrow `Value`

The design checkpoint is resolved as follows:

- Runtime input and output FIFOs continue to transport unrestricted `Value`.
- Journaling, commit, and delivery neither force values nor require outer
  WHNF. A lazy value may be delivered directly.
- Output callbacks choose whether to evaluate their payload and may capture an
  assembler/evaluator when semantic extraction is required.
- `EvaluatedValue` is a proof of completed outer demand, not an isolation or
  staging boundary. It neither prevents nested demand nor establishes a
  schema for dictionaries and lists.
- A future boundary that truly prohibits observation until another phase must
  be represented explicitly, for example by an opaque `ValueEnvelope` and a
  separate opening capability. This transition does not approximate that
  policy through incidental WHNF checks.
- Public scalar extraction belongs to `EvaluatedValue`. Immediate core
  matches remain crate-private implementation fast paths and trusted protocol
  decoders.

##### Phase 5B.2a — Migrate the remaining output decoder

- Give the logger stderr decoder explicit access to the assembler it already
  retains through `MainEffects`.
- Evaluate the delivered value in the callback, then extract bytes from its
  `EvaluatedValue` witness.
- Preserve the existing `.write_stderr` semantic check before transaction
  commit; the delivery-side evaluation is the host decoder independently
  enforcing its protocol, not a new primitive-layer requirement.
- Add a regression proving the generic output journal accepts a lazy payload
  without demanding it and that a callback may choose to evaluate it during
  delivery.

The logger specialization already retains the same `Assembler` in every task
machine. Capturing a clone in its host-side decoder therefore adds no new
ownership edge category; it only makes the decoder's existing protocol demand
explicit.

Phase 5B.2a result (2026-08-18): complete. The logger stderr decoder now
evaluates its delivered payload explicitly and extracts bytes from the
resulting witness. A runtime regression writes a deliberately lazy value,
proves that journal construction and transaction commit leave it unclaimed,
then proves that a decoder which captures an assembler may demand and deliver
it. The focused runtime test and all 22 binary tests pass.

##### Phase 5B.2b — Close the public `Value` surface

- Make the remaining public `Value::as_binary` view crate-private.
- Keep internal immediate matches where they are safe implementation fast
  paths.
- Verify that every public pre-demand representation observation is explicitly
  reflective.
- Verify that ordinary extraction is reachable without reflection.
- Search exported source for public `Value` scalar/kind/undefined observers
  and run the external public-API suite after the visibility change.

Verification checks public visibility as well as call sites: arbitrary
`Value` has no immediate scalar/representation inspection other than runtime
ownership, `EvaluatedValue` owns immediate scalar extraction, and pre-demand
kind and privileged iteration are available only through reflection.

Phase 5B.2b result (2026-08-18): complete. `Value::as_binary` is now
crate-private; `Value::runtime_id` is the only public method on the bare root.
All public scalar and strict-array extraction lives on `EvaluatedValue`, while
`ReflectionInspector` owns kind, atom-key, dictionary-entry, and associated
metadata inspection. An all-target build, all-target clippy, 44 external
public facade tests, and 5 macro-protocol tests pass after the visibility
change.

#### Phase 5C — Close CCR-011

The final phase is split because documentation closure and the plan-wide
completion audit prove different things.

##### Phase 5C.1 — Close review and architecture documentation

- Update the cleanup review with the final surface and verification results.
- Promote the enduring construction/demand/reflection boundary into current
  architecture/API documentation; leave transition-only detail here as
  history.
- Correct stale architecture text that assigns ordinary demand to reflection
  or treats `Value` itself as an accessor service.

Phase 5C.1 result (2026-08-18): complete. CCR-011 records the retired
compatibility surface, final facade ownership, unrestricted FIFO decision, and
verification evidence. Current assembly architecture now distinguishes
no-demand `Values`, semantic `ValueEvaluator`, and privileged
`ReflectionInspector`; diagnostic architecture explicitly states that event
transport does not force payloads and that decoder demand is host policy.
Transition chronology remains in this plan rather than the current-state
architecture. Documentation diff checks and stale-claim searches pass.

##### Phase 5C.2 — Plan-wide completion audit

- Audit every invariant, target-surface item, migration row, phase result, and
  verification-matrix entry against current source and tests.
- Search for every removed compatibility symbol and unintended public
  immediate observer.
- Run focused facade/lifecycle coverage followed by all required repository
  gates and end-to-end targets.
- Record final evidence, mark the plan complete only after every requirement
  has direct evidence, and close the active implementation goal.

Final verification runs focused facade and lifecycle tests first, then the
entire required check suite. Relevant end-to-end cases are run with zero and
multiple workers so successful-value equivalence is exercised across both
deterministic and concurrent scheduling.

Phase 5C.2 result (2026-08-18): complete. The final source audit found every
provisional target operation under its intended facade. `ValueEvaluator`
contains only `eval`; immediate byte, number, and strict-array extraction lives
on `EvaluatedValue`; and `Value::runtime_id` is the only public method on the
bare value root. Reflection retains pre-demand kind observation, canonical
dictionary iteration, atom-key inspection, and associated-metadata inspection
behind its explicitly unstable boundary.

The semantic invariants and verification matrix have direct coverage:

| Requirement | Final evidence |
| --- | --- |
| No-demand construction | `access_and_annotation_construction_do_not_demand_inputs`, `values_apply_is_lazy_and_matches_source_application_order`, `list_and_representation_constructors_are_lazy_semantic_operations`, `dictionary_composition_is_lazy_and_rejects_foreign_members`, the opaque-net public tests, and `output_journaling_preserves_lazy_payload_until_decoder_demand` inspect lazy claims, promise subscriptions, or delayed delivery demand directly. |
| Runtime provenance | `public_value_factories_reject_foreign_composite_members`, `value_evaluator_returns_a_runtime_rooted_whnf_witness`, `reflection_kind_observes_an_unresolved_promise_without_demand`, `assembler_boundaries_reject_foreign_values_before_evaluation_or_storage`, and the runtime-event boundary tests cover construction, demand, reflection, storage, and host events. |
| Semantic composition | Application, complete-name and computed paths, source dictionary literals/union/update/removal, semantic binary slicing, array/deque normalization, and defined/required lookup helpers are compared with direct `.g` behavior in facade and public tests. |
| Demand and extraction | Evaluator tests cover immediate and suspended WHNF demand, retained promise subscriptions, cached lazy success, structured failures, scalar bounds, arbitrary-precision number text, strict-array extraction, and witness lifetime after facade owners are dropped. |
| Reflection boundary | Public reflection tests cover pre-demand kind without subscription, canonical dictionary entries, atom identity, and sealed metadata; current architecture documentation marks the surface unstable and outside reproducibility. |
| Error context | Structured evaluator, binary assertion, path, configuration, import, diagnostic, and logger tests retain their Glam diagnostic data and contextual frames through final rendering. |
| Scheduling equivalence | `ordinary_result_is_identical_across_worker_counts` compares successful CLI output with zero, one, and four workers; the executable and hello sample targets exercise the migrated assembly path. |
| Compatibility removal | Repository-wide searches find no retired `Assembler` or `ReflectionInspector` symbol and no `Values::empty_record` use outside this historical plan/review. Exported-source inspection finds no public scalar, kind, undefined, or binary observer on arbitrary `Value`. |

Focused completion runs passed 105 facade/lifecycle unit tests, 44 external
public-API tests, 5 macro-protocol tests, 49 CLI tests, 5 executable-sample
tests, and the independently timed hello-assembly test. Final
`cargo fmt --check`, all-target/all-feature clippy with warnings denied, and
`cargo test -q` all pass; the full test run includes 1165 library tests and
all integration targets. No additional transition subphase is needed.

## Verification Strategy

Verification is layered so a broad green suite cannot conceal a boundary
mistake:

1. **Internal contract tests** exercise construction, demand, extraction, and
   reflection separately. These tests may use crate-private claim/status
   inspection solely to prove that a public `Values` operation took no action.
2. **Public facade tests** in `tests/public_api.rs` compile and run through only
   exported types. They prove ownership and visibility, not just runtime
   behavior.
3. **Source equivalence fixtures** express the corresponding `.g` operation
   for application, access, slicing, annotations, dictionary composition, and
   the lookup-policy helpers used by the host.
   The host-composed operation and source-composed operation must produce
   equivalent successful values. Tests compare the appropriate semantic
   observation—bytes, exact number text, selected members, or normalized list
   contents—not runtime-root identity or incidental internal representation.
4. **Lifecycle and scheduling tests** force important orderings with barriers,
   resolvers, explicit work pumping, or existing deterministic test hooks.
   Sleeping, high iteration counts, and many worker threads may supplement but
   never replace an ordering-controlled regression.
5. **End-to-end integration tests** cover the CLI, macro protocols, executable
   samples, assembly samples, diagnostics, and reflection hosts after their
   migration.

### Proving absence of demand

Each no-demand constructor is applied to unresolved lazy values, resolver-owned
promises, reflection gates, and nets where those inputs are meaningful. The
test records the relevant claim/subscription/reduction state, constructs the
result, and verifies that state is unchanged. It then demands the result and
verifies that the expected work occurs. Returning quickly is not sufficient
evidence: an implementation could subscribe or enqueue work and still return
quickly.

### Differential migration

Before a compatibility implementation is deleted, old and new paths run
side-by-side for their shared intended semantics. The old implementation is
not the oracle where this plan deliberately corrects it—most importantly
optional dictionary access and logical undefined handling. Those cases use
the `.g` helpers as the semantic oracle and retain a test showing the former
mismatch until the replacement lands.

The differential comparisons cover values and structured failure shape. They
do not require byte-for-byte identical failure messages or timing because the
evaluator only promises equivalence of successful results.

### Focused and final commands

Implementation checkpoints run the narrowest affected unit tests first, then
the relevant integration targets, such as:

```sh
cargo test -q --test public_api
cargo test -q --test macro_protocols
cargo test -q --test cli
cargo test -q --test executable_samples
cargo test -q --test hello_assemblies
```

The exact focused commands are recorded with each completed checkpoint. Every
Rust-edit checkpoint then runs the repository-required full checks shown below.

## Verification Matrix

| Concern | Required evidence |
|---|---|
| No-demand construction | unresolved lazy, promise, reflection gate, and net inputs remain unclaimed/unreduced |
| Runtime provenance | every composite and evaluator entry rejects foreign-runtime values |
| Application | zero, one, and multiple arguments match `.g` association and order |
| Paths | empty, nested, dotted-name, computed-key, missing, and failing-intermediate cases |
| Dictionaries | access, singleton, union, update/removal, and caller-defined required/fallback helpers match `.g` |
| Lists | compact binary, ordinary list, concatenation, lazy chunk, bounds, item failure, array materialization, and deque balancing |
| Binary | annotation is lazy; `eval` preserves structured semantic errors; evaluated extraction is immediate |
| Reproducibility | successful values agree across worker schedules; failures are only required to remain well-formed |
| Reflection | pre-demand kind/status is visibly reflective and documented unstable |
| Wrapper lifecycle | `EvaluatedValue` preserves its runtime root and remains usable after temporary facade handles are dropped |
| API boundary | public clients cannot construct the WHNF witness or reach immediate representation inspection through `Value` |
| Error context | path, configuration, binary assertion, and logger migrations retain structured context through rendering |
| Compatibility removal | no production call or public test uses removed `Assembler` helpers |

Every Rust-edit checkpoint runs:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Focused tests should run first, especially public API, evaluator, configuration,
diagnostics, macro protocol, and reflection-store groups.

## Risks and Controls

### Accidentally introducing demand in `Values`

Use direct lazy-source/builtin construction and tests with unresolved inputs.
Do not implement `Values::apply` through the existing evaluator helper.

### Changing missing-versus-undefined behavior silently

Compare all new dictionary operations to source pattern behavior. Do not use
the old `get_optional` result type as the oracle.

### Losing structured context

Preserve and extend tests for binary assertion, path/member demand, and
configuration entry contexts before deleting compatibility glue.

### Turning reflection into a miscellaneous utility bag

Require each reflection method to name an observation unavailable to ordinary
Glam semantics. Implementation access to `CoreValue` is not by itself a reason
for public reflection placement.

### Expanding scope into streaming and retention

This transition may expose semantic list split/slice/at constructors, but it
does not promise bounded retention. Streaming assembly output remains deferred
until namespace and list-root retention can be controlled precisely.

## Deferred Work

- A stable Rust embedding convenience library, including optional dictionary
  wrappers.
- Evaluator tuning for scheduling, normalization, resource, or diagnostic
  policy.
- Incremental/streaming extraction with explicit retention policy.
- Detailed lazy, promise, task, and interaction-net status reflection.
- Reference equality and richer net inspection.
- Public optimization annotations for list ropes or binary materialization.
- Arena or tracing-GC integration with the evaluator/WHNF wrapper.
- Stabilization or versioning of reflection APIs.

# Value Facade Transition Plan

Status: proposed on 2026-08-17. No implementation phases are complete.

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

The current public surface provides `as_binary`, `as_i64`,
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

Status: pending.

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

### Phase 1 — No-demand `Values` Composition

Status: pending.

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

### Phase 2 — `ValueEvaluator`

Status: pending.

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
  unclaimed. Empty arrays succeed; deques, compact byte chunks,
  concatenations, and unresolved list spines are declined without changing
  claim or subscription state.

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

### Phase 3 — Explicit Reflection Boundary

Status: pending.

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

#### Phase 3B — Representation observation

- Move pre-demand `Value::kind` behavior to a clearly named reflection method.
- Replace public `is_undefined` callers with ordinary Glam matching/assertion
  helpers rather than another representation check.
- Keep immediate `Value` observations as documented transitional delegates
  until Phase 4 migrates their production callers.
- Defer detailed lazy-state APIs unless needed to replace a real caller.

Verification shows that representation kind can report an unresolved lazy
value without forcing it, while ordinary semantic callers use evaluation,
extractors, and patterns rather than representation-kind branching.

### Phase 4 — Production Caller Migration

Status: pending.

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

#### Phase 4B — Diagnostics, viewers, and logger

- Use semantic access for known diagnostic paths.
- Keep dictionary iteration or metadata inspection visibly under reflection.
- Preserve context and origin frames on evaluator failure.
- Do not turn the logger into a reason to restore host path interpretation.

Verification exercises plain and structured messages, origins, nested context
frames, associated metadata inspection, and logger fallback. It confirms that
reflection is used only for the privileged observations and that evaluator
failures retain context through final rendering.

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

#### Phase 4D — Public tests and examples

- Rewrite public API tests around the three explicit facades.
- Retain a small number of tests for temporary compatibility delegates until
  their deletion phase.
- Update examples and API documentation before removing old methods.

Verification treats public API examples as compile-tested clients. Each
example must make construction, evaluation, and reflection crossings visible;
none may import crate-private representations or depend on a compatibility
delegate not named as transitional.

### Phase 5 — Compatibility Removal and Audit

Status: pending.

#### Phase 5A — Remove `Assembler` semantic helpers

- Remove `Assembler::{apply,get,get_optional,evaluate,to_binary,binary_slice}`.
- Remove transitional `ReflectionInspector::{evaluate,list_items}`.
- Remove the transitional `Values::empty_record` naming delegate.
- Remove the private binary-slice interpreter and now-unused path-context glue.
- Remove imports, tests, and comments which describe the transitional surface.

Verification includes a repository-wide symbol search for every removed
method and its private implementation helpers, followed by a public API build.

#### Phase 5B — Audit `Value` and reflection

- Remove public immediate observations which violate the selected facade
  boundary.
- Keep internal immediate matches where they are safe implementation fast
  paths.
- Verify that every public pre-demand representation observation is explicitly
  reflective.
- Verify that ordinary extraction is reachable without reflection.

Verification checks public visibility as well as call sites: arbitrary
`Value` has no immediate scalar/representation inspection other than runtime
ownership, `EvaluatedValue` owns immediate scalar extraction, and pre-demand
kind and privileged iteration are available only through reflection.

#### Phase 5C — Close CCR-011

- Update the cleanup review with the final surface and verification results.
- Mark all completed phases in this plan.
- Promote any enduring semantic explanation into architecture/API docs; leave
  transition-only detail here as history.

Final verification runs focused facade and lifecycle tests first, then the
entire required check suite. Relevant end-to-end cases are run with zero and
multiple workers so successful-value equivalence is exercised across both
deterministic and concurrent scheduling.

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

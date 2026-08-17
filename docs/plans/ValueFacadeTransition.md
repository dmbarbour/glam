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

### Dictionary pattern computations

`Values` will expose the smallest semantic constructors required to express
required (`:`) and optional (`?:`) dictionary pattern selection. They may wrap
the existing compiler-private pattern builtins internally, but those builtins
do not become public API.

The constructed operation retains Glam's effectful match result and remainder
semantics. The core API does **not** translate it to `Option<Value>`.
Embeddings which want that Rust policy may define a wrapper by running the
semantic computation through `ValueEvaluator`. A convenience utility can be
added later if real use warrants it.

Exact method names and whether the primitive exposes `{value, rest}` directly
will be chosen after the current callers are inventoried. The semantic tests,
not `get_optional` compatibility, define the contract.

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
- no-demand iteration of an already materialized list spine;
- `as_i64` and `as_rational_i64` for small exact numbers;
- `as_f64` for the existing documented lossy conversion;
- `number_text` for canonical, exact arbitrary-precision number text;
- `is_undefined`; and
- semantic `kind`, which has no `Lazy` or `Promised` case.

`ValueEvaluator::eval(&Value)` is the sole initial evaluator operation. The
name is intentionally shorter than `evaluate_whnf`, while its documentation
states that evaluation means demand to outer WHNF. Type-specific extraction
belongs to `EvaluatedValue`. A client which wants bytes composes
`Values::anno_binary`, evaluates the result, and calls `as_bytes`; a client
which wants a number evaluates and calls the corresponding number extractor.

Extraction which would demand additional nested structure is not an immediate
`EvaluatedValue` operation. An immediate list iterator succeeds only when no
unresolved list-spine thunk remains. Evaluating `Values::anno_array` or
`Values::anno_deque` guarantees that condition; arbitrary list WHNF does not.
The exact Rust iterator/view type is selected in Phase 2A, but iteration must
not perform hidden evaluation.

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
explicit array/deque annotation, evaluation, and immediate iteration of the
materialized result. A reflective operation may still demand when its own
documented protocol requires it, but that does not make ordinary semantic
observation reflective.

## Current Compatibility Surface

| Current API | Target owner | Retirement intent |
|---|---|---|
| `Assembler::apply` | `Values::apply` | Compose lazily, then evaluate explicitly if needed |
| `Assembler::{get,get_optional}` | `Values` access and pattern computations | Remove host dotted-path interpreter |
| `Assembler::evaluate` | `ValueEvaluator::eval` | Compatibility delegate, then remove |
| `Assembler::to_binary` | `Values::anno_binary`, `ValueEvaluator::eval`, then `EvaluatedValue::as_bytes` | Keep one semantic binary assertion path |
| `Assembler::binary_slice` | `Values::list_slice`, binary annotation, evaluation, and `EvaluatedValue::as_bytes` | Delete separate list traversal |
| `ReflectionInspector::evaluate` | `ValueEvaluator::eval` | Move ordinary demand out of reflection |
| `ReflectionInspector::list_items` | `Values::anno_array`, `ValueEvaluator::eval`, then materialized-list iteration on `EvaluatedValue` | Make spine demand semantic and explicit |
| `Value::kind` | Evaluator after WHNF or reflection before WHNF | Split semantic kind from representation kind |
| `Value::{is_undefined,as_*}` | `EvaluatedValue` | Preserve current extraction functionality; retain crate-private fast paths |

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
These immediate observations move to `EvaluatedValue`; they are not removed
merely for cleanup. Adding public number-library types or type-specific
evaluator conveniences remains deferred.

## Transition Phases

Each checkpoint must leave the repository formatted, lint-clean, and passing
tests. Completion is recorded here as phases land.

### Phase 0 — Inventory and Contract Latching

Status: pending.

#### Phase 0A — Classify callers

- Inventory every call to the five `Assembler` compatibility methods,
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

- Add focused tests showing that semantic composition does not demand an
  unresolved lazy value or promise.
- Preserve structured failure/context tests for WHNF and binary assertion.
- Preserve foreign-runtime rejection at construction and demand boundaries.
- Add direct `.g` comparison fixtures for access, slicing, and required versus
  optional dictionary patterns.

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

#### Phase 1C — Dictionary pattern computations

- Define public semantic constructors for required and optional dictionary
  pattern steps without exposing compiler-private builtin identities.
- Reuse the evaluator's existing pattern semantics rather than implementing
  host dictionary inspection.
- Do not add an `Option<Value>` evaluator convenience.

Verification compares the constructed computations with `.g` patterns for
missing fields, explicit/logical `{}`, wrong intermediate kinds, lazy fields,
failing fields, nested paths, and remainder preservation.

### Phase 2 — `ValueEvaluator`

Status: pending.

#### Phase 2A — Introduce `EvaluatedValue`

- Add the private-field wrapper around an ordinary `Value`.
- Add `as_value`, `into_value`, `From<EvaluatedValue> for Value`, and `Clone`.
- Move the existing immediate scalar views onto the wrapper while retaining
  temporary `Value` delegates for staged migration.
- Add a no-demand view/iterator for a materialized list spine. It must reject
  or decline an unresolved list representation rather than forcing it.
- Give the wrapper a semantic kind enum without lazy/promise variants.
- Document explicitly that only the outer layer is in WHNF.

#### Phase 2B — Introduce evaluator facade

- Add `Assembler::evaluator()` backed by the existing client-demand session.
- Make `eval` demand WHNF, return `EvaluatedValue`, and preserve structured
  failure conversion.
- Keep the evaluator free of type-specific extraction and nested-structure
  traversal.
- Keep temporary `Assembler` delegates while migrations proceed.

Verification covers successful-result equivalence across worker schedules,
structured but not necessarily identical failures, foreign-runtime rejection,
and type-mismatch diagnostics after demand.

#### Phase 2C — Separate composition from extraction

- Migrate byte extraction to `Values::anno_binary`, `ValueEvaluator::eval`, and
  `EvaluatedValue::as_bytes`.
- Ensure slicing is never reintroduced into the evaluator.
- Make each migrated caller visibly compose under `Values` before extracting.

### Phase 3 — Explicit Reflection Boundary

Status: pending.

#### Phase 3A — Split the current inspector

- Remove ordinary `evaluate` and list-enumeration responsibilities from
  `ReflectionInspector` after evaluator migration.
- Replace current `list_items` callers with explicit array/deque annotation,
  evaluation, and no-demand `EvaluatedValue` iteration.
- Retain metadata, dictionary iteration, and atom-key inspection.
- Rewrite facade documentation to state instability and reproducibility limits.

#### Phase 3B — Representation observation

- Move pre-demand `Value::kind` behavior to a clearly named reflection method.
- Provide semantic kind information only through `EvaluatedValue`.
- Remove the temporary immediate `Value` extraction delegates after callers
  migrate.
- Defer detailed lazy-state APIs unless needed to replace a real caller.

Verification distinguishes the representation kind of an unresolved lazy
value from the semantic kind obtained after demand, without making timing a
semantic assertion.

### Phase 4 — Production Caller Migration

Status: pending.

#### Phase 4A — Configuration and CLI

- Replace dotted-string `get` calls for `conf.env`, `conf.cli`, `conf.log`, and
  related configuration entries with semantic name-array paths.
- Use dictionary pattern computations where optional configuration is truly a
  match rather than raw access followed by undefined handling.
- Replace application-plus-extraction chains with explicit `Values` then
  `ValueEvaluator` calls.

#### Phase 4B — Diagnostics, viewers, and logger

- Use semantic access for known diagnostic paths.
- Keep dictionary iteration or metadata inspection visibly under reflection.
- Preserve context and origin frames on evaluator failure.
- Do not turn the logger into a reason to restore host path interpretation.

#### Phase 4C — Assembly, macro, and reflection hosts

- Move assembly binary output to list composition, binary annotation,
  evaluation, and `EvaluatedValue` extraction.
- Migrate macro protocol extraction.
- Migrate volume-capability and reflection-task call construction to batched
  `Values::apply` and semantic paths.

#### Phase 4D — Public tests and examples

- Rewrite public API tests around the three explicit facades.
- Retain a small number of tests for temporary compatibility delegates until
  their deletion phase.
- Update examples and API documentation before removing old methods.

### Phase 5 — Compatibility Removal and Audit

Status: pending.

#### Phase 5A — Remove `Assembler` semantic helpers

- Remove `Assembler::{apply,get,get_optional,evaluate,to_binary,binary_slice}`.
- Remove the private binary-slice interpreter and now-unused path-context glue.
- Remove imports, tests, and comments which describe the transitional surface.

#### Phase 5B — Audit `Value` and reflection

- Remove public immediate observations which violate the selected facade
  boundary.
- Keep internal immediate matches where they are safe implementation fast
  paths.
- Verify that every public pre-demand representation observation is explicitly
  reflective.
- Verify that ordinary extraction is reachable without reflection.

#### Phase 5C — Close CCR-011

- Update the cleanup review with the final surface and verification results.
- Mark all completed phases in this plan.
- Promote any enduring semantic explanation into architecture/API docs; leave
  transition-only detail here as history.

## Verification Matrix

| Concern | Required evidence |
|---|---|
| No-demand construction | unresolved lazy, promise, reflection gate, and net inputs remain unclaimed/unreduced |
| Runtime provenance | every composite and evaluator entry rejects foreign-runtime values |
| Application | zero, one, and multiple arguments match `.g` association and order |
| Paths | empty, nested, dotted-name, computed-key, missing, and failing-intermediate cases |
| Dictionary patterns | required/optional, logical undefined, nested path, wrong kind, and remainder cases match `.g` |
| Lists | compact binary, ordinary list, concatenation, lazy chunk, bounds, item failure, array materialization, and deque balancing |
| Binary | annotation is lazy; `eval` preserves structured semantic errors; evaluated extraction is immediate |
| Reproducibility | successful values agree across worker schedules; failures are only required to remain well-formed |
| Reflection | pre-demand kind/status is visibly reflective and documented unstable |
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

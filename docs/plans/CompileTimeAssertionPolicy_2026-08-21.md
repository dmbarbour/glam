# Compile-Time Assertion Policy

Date: 2026-08-21
Status: complete

## Purpose

Express hard type-layout and generic-representation requirements as compiler
errors instead of runtime assertions or unit-test failures. Keep ordinary tests
for behavior and for performance budgets that are useful regressions but are
not required for correctness.

This is an independent cleanup. It does not change the C2 collector design or
add a `static_assertions` dependency.

## Policy

- Put unconditional layout requirements beside the represented type as
  `const _: () = assert!(...)`.
- Use an inline `const { assert!(...) }` when the requirement depends on a
  generic parameter. Such a failure is reported when the invalid
  monomorphization is compiled.
- Use a compile-fail doctest to demonstrate an intentionally unsupported type
  at a public generic boundary.
- Retain named runtime tests for behavior and non-semantic size/performance
  budgets. Do not turn an optimization expectation into a compilation
  requirement accidentally.

## Implementation

1. Replace the runtime size assertions for these hard representation contracts
   with colocated unconditional const assertions:

   - `Option<RuntimeObservationEpoch>` occupies one `u64` word;
   - `NodeId`, `Option<NodeId>`, `Port`, and `Option<Port>` each occupy one
     `u64` word; and
   - `Gc<T>` remains one pointer wide. Add `repr(transparent)` to `Gc<T>` to
     state its existing single-field representation contract explicitly.

   Preserve the behavioral portion of the observation-epoch test and the
   compile-time `Send + Sync` bound check for `Gc<u64>`; only remove their
   redundant runtime layout assertions.

2. Replace `Mutator::alloc`'s runtime ZST assertion with an inline generic const
   assertion. Replace the `#[should_panic]` unit test with a compile-fail
   doctest showing that `mutator.alloc(())` is rejected during compilation.

3. Keep `boxed_reflection_computation_does_not_enlarge_lazy_source` as a named
   runtime test. It is a performance regression budget involving a comparison
   type local to the test, not a representation requirement needed for
   correctness.

4. Update the GC safety and verification ledgers to describe compile-time ZST
   rejection and the const pointer-width invariant rather than runtime tests.

## Verification

- Confirm the compile-fail ZST example fails for the intended const assertion.
- Confirm the remaining size-based runtime tests are only the deliberate
  `LazySource` performance regression.
- Run `crates/glam-gc/scripts/check.sh` and its exact unsafe audit.
- Run `cargo fmt --check`.
- Run `cargo clippy --all-targets --all-features -- -D warnings`.
- Run `cargo test -q`.
- Run `git diff --check`.

## Completion Criterion

Hard layout violations and GC ZST allocation attempts fail compilation, while
behavioral and performance-only checks retain useful named test coverage.

## Completion

Completed on 2026-08-21:

- `Gc<T>` is transparent over its pointer and has a colocated pointer-width
  const assertion.
- Observation epochs and packed interaction-net IDs and ports have colocated
  one-word and option-niche const assertions.
- `Mutator::alloc` rejects zero-sized `T` through inline const evaluation; its
  `compile_fail,E0080` doctest replaces the former panic test.
- The `LazySource` comparison remains the only runtime size regression because
  it monitors an optimization rather than a correctness requirement.
- The focused collector checks, unsafe audit, formatting, clippy, complete
  repository test suite, and whitespace validation pass.

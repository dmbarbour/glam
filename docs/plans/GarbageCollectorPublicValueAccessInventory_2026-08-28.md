# GC Public-Value Compatibility Access Inventory — 2026-08-28

Status: Phase I2C baseline and I3 scoped-access migration complete. I4F.1a now
provides the complementary durable-owner baseline and I4F.1b.1 has converted
the canonical bundle. I4F.1b.2 has closed type-erased cache admission and
compiler-cache storage; the remaining durable-owner conversion and production
root switch remain pending in I4F.1c-I4F.2.

## Purpose

Production still represents public and durable runtime values through
`RuntimeValueRoot` containing a bare `core::Value`. Internal constructors and
consumers consequently use temporary compatibility operations such as
`as_core`, `into_core`, `Value::from_core`, `Value::from_runtime`, and the two
`RuntimeValueRoot` constructors. Those operations cannot remain freely
available after public values become opaque managed roots.

The executable inventory in
[`src/api/value/access_inventory.rs`](../../src/api/value/access_inventory.rs)
records every such occurrence at source-module granularity. It scans the source
tree during tests, detects new modules and changed counts, and requires every
entry to name both its role and later migration owner. Exact line numbers are
deliberately omitted because ordinary edits would make them noisy without
improving coverage.

The inventory excludes the isolated Phase I2 prototype, its own scanner,
binary-crate code that cannot access private library conversions, named
`tests.rs` files, and `tests/` directories. Colocated `#[cfg(test)]` fixtures
remain included with their production module, making the baseline slightly
more conservative than the production build alone.

## Baseline

The 24 inventoried modules contain 198 compatibility occurrences after
I3B.1d.1 moved public construction behind `ScopedValues` and I3B.1d.2 moved
public evaluated-value extraction behind matching runtime authority:

| Operation family | Count |
| --- | ---: |
| borrowed `as_core()` projection | 68 |
| owned `into_core()` projection | 34 |
| `Value`/`PublicValue::from_core` construction | 79 |
| `Value::from_runtime` construction, including facade-local `Self` delegation | 6 |
| `RuntimeValueRoot::new` construction | 8 |
| `RuntimeValueRoot::from_runtime` construction | 3 |

| Area | Modules | Occurrences | Migration owner |
| --- | ---: | ---: | --- |
| Public API, assembly, diagnostics, errors, evaluation, readiness | 6 | 60 | I3B.1 and I3E.1-I3E.3 scoped operations; I4F.1 durable root surfaces; I4F.2 registration/facade switch |
| Core compatibility bridge | 1 | 2 | I4A-I4E exact shell, then I4F.2 |
| Core interaction-net construction | 1 | 10 | I3D.3-I3D.4 scoped net access; I4F.1 outcomes; I8 managed net |
| Evaluation access, sessions, pump, executor, coordinator task/spark | 6 | 12 | I3A.3-I3A.4, I3B.2, and I3C.1-I3C.2; I4F.1 durable outcome surfaces |
| Built-in `.g` compiler, macro expansion, logical/source parsing | 4 | 16 | I3E.2 compiler/macro regions; I4F.1 retained root surfaces |
| Reflection lifecycle, machine, protocol, requests, search, store | 6 | 98 | I3D.1-I3D.4 phase boundaries; I4F.1 machine/store root surfaces |

The executable table supplies the exact per-module counts, role descriptions,
and checkpoint assignments behind this grouped summary.

This remains an operation inventory. I4F.1a's separate
`durable_value_owner_inventory_is_complete` source latch parses declaration
types across both the library and binary crate and assigns every retained or
potentially hidden value family to a semantic owner row. Neither inventory is
a substitute for the other: this one closes compatibility operations, while
the durable-owner inventory closes storage and lifetime classifications.

## Boundary Decisions

- The inventory is a migration ledger, not permission to retain bare-core
  access indefinitely.
- I3 replaces observations and construction with bounded same-runtime scoped
  access. Nested helpers reuse one recursive heap admission.
- I4F.1 converts durable fields to stable root-shaped surfaces before managed
  values can escape into them.
- I4F.2 removes or privatizes the compatibility projections while atomically
  switching those surfaces to registered roots and changing the production
  public wrapper.
- Constructors, composite validation, storage, evaluator/poll paths,
  reflection, diagnostics, binary/scalar extraction, compiler/macro paths, and
  net construction are all represented. A new occurrence must update this
  inventory and name its migration owner; an unexplained count change fails the
  regression.
- Production remains `NoAuto` and retains the compatibility representation
  until the later gates pass.

## Verification

- `prototype_value_access_nests_in_one_mutator` holds an outer managed borrow,
  performs recursively admitted runtime observation, and verifies that the
  outer scope remains valid afterward.
- `public_value_compatibility_access_inventory_is_complete` compares the live
  source tree with the per-module baseline and rejects missing, added, or moved
  compatibility access.
- The inventory fixture was latched by lowering one expected `as_core` count;
  it reported the intended `src/api/evaluator.rs` mismatch before the baseline
  was restored.
- The post-I2 audit extended the `from_runtime` family to facade-local
  `Self::from_runtime` delegation. Before its baseline was updated, the test
  reported the existing `src/api/value.rs` count as two rather than one.
- I3B.1d.1 deliberately tripped the inventory after routing public
  construction through `ScopedValues`; the relatched baseline removed four
  borrowed and eighteen owned projections from `src/api/value.rs`.
- I3B.1d.2 deliberately tripped the same inventory after moving evaluation
  and extraction through matching `Values` authority. The new baseline removes
  nine borrowed/construction escapes and two evaluator conversion escapes.
- I3B.1d.4 moved the matching-domain authority into an exact weak observer on
  `EvaluatedValue`. This restores direct `as_*()` ergonomics without adding a
  compatibility projection, retaining the runtime heap, or changing the
  198-entry baseline.
- I4F.1d.3b deliberately tripped the inventory while converting the reflection
  effect API and contextual wrapper into durable roots. The machine row adds
  two compatibility root constructions and their two bounded projections;
  I4F.2 retires those projections when the root facade becomes managed.
- I4F.1d.3c.1 deliberately tripped both inventories while converting branch
  effect/state into durable roots. Immediate root assignment removes five
  owned projections and the redundant fused branch phase roots; the remaining
  projections are bounded reads of those roots inside evaluator phases.
- The existing public-value suite remains the compatibility behavior oracle
  until I4F.2.

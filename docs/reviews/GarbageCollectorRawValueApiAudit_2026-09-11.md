# Raw Core-Value API Audit

Date: 2026-09-11  
Checkpoint: GCI11R-002D.1a  
Status: complete; D.1b narrowed the orchestration seam, D.2a assigns every
remaining violation, and production repairs belong to GCI11R-002D.2b-D.2g

## Outcome

The production source tree contains 590 declarations whose signature directly
or through a local alias carries the private `core::Value` representation. The
new syntax-backed inventory classifies and fingerprints every declaration.
D.1b removed the ambiguous `EvalContext::evaluate_whnf(&core::Value)` name.
Its remaining unmigrated callers are deliberately isolated behind
`evaluate_compatibility_whnf` and remain latched as a violation assigned to
D.2.

This is an audit checkpoint, not a claim that the regional-value invariant is
already satisfied. The inventory deliberately records the current 486
authority-free operations before their migration. Treating every one as an
immediate defect would obscure the transition: many are coherent families of
evaluator, front-end, diagnostic, or reflection helpers which must acquire one
shared regional boundary rather than independently opening hundreds of
mutators.

## Mechanical inventory

`src/core/managed/raw_value_api_inventory.rs` parses every production Rust
source with `syn`. It excludes test modules and inventory scaffolding, but
includes the library and binary crates. It:

- distinguishes the durable public `api::Value` / `glam::Value` facade from
  private `core::Value`;
- follows qualified imports and local type aliases through references,
  options, results, vectors, slices, arrays, `Arc`, tuples, and callback
  parameter/result types;
- discovers file-local carriers of `RuntimeValueAccess` or
  `EvaluationValueAccess`, so methods on types such as `ScopedValues`,
  `ManagedPromiseAccess`, and `ResolvedNetLowerer` count as access-qualified;
- records explicit methods plus the generated `Clone`, `PartialEq`, and `Eq`
  contracts of `core::Value`; and
- uses a sorted FNV fingerprint in addition to counts, so exchanging one
  signature for another cannot preserve the baseline accidentally.

The exact human-readable inventory is available without editing the latch:

```sh
GLAM_DUMP_RAW_VALUE_API_INVENTORY=1 \
  cargo test -q \
  core::managed::raw_value_api_inventory::raw_core_value_api_inventory_is_complete \
  -- --nocapture
```

The checked baseline is:

| Declaration kind | Current classification | Count |
| --- | --- | ---: |
| function or method | regional access API | 73 |
| function or method | collector-mandated primitive | 23 |
| function or method | violation | 483 |
| type alias | regional representation | 8 |
| derived operation | violation | 3 |
| **Total** |  | **590** |

No current declaration qualifies as safe scoped exposure. Because raw
`core::Value` remains cloneable and unbranded, a callback receiving it can
clone it into outer state even when the reference itself is higher-ranked.
Likewise, durable public boundaries are absent from this raw inventory by
construction: they transport `api::Value`, `RuntimeValueRoot`, or another
traced owner instead.

The eight alias declarations are recorded as regional representations rather
than executable API violations. They are `CompileDiagnosticEmitter`,
`core::EvaluatedValue::Error`, `Dict`, `List`, `PromiseAssignment`,
`SemanticOperation`, `ManagedPromiseAssignment`, and
`CoreSpecialization::Data`. Every signature using one is expanded into the
same mechanical audit.

## Caller families and dispositions

| Family | Access | Collector | Regional aliases | Violations | Intended disposition |
| --- | ---: | ---: | ---: | ---: | --- |
| public API internals | 3 | 0 | 0 | 11 | Keep rooted public signatures; move private projection/construction inside an existing access or a scoped observer. |
| compiler, diagnostics, and source support | 3 | 0 | 1 | 29 | Give each compilation/diagnostic construction region one access; root values at durable diagnostic and source boundaries. |
| core values, managed cells, nets, and runtime roots | 32 | 23 | 6 | 49 | Keep access-qualified primitives; retire compatibility walkers with their shells; require access for raw construction, projection, comparison, and formatting. |
| evaluator operations and builtins | 1 | 0 | 1 | 201 | Thread `EvaluationValueAccess` through each callback-free quantum and fuse helpers under that region rather than opening per-helper mutators. |
| evaluation orchestration | 2 | 0 | 0 | 14 | Preserve mutator-free poll/wait orchestration; project or construct raw values only inside `EvaluatorStepContext::with_value_access`, then publish into traced ownership before leaving it. |
| built-in `.g` front end | 29 | 0 | 0 | 134 | Run semantic lowering and embedded-data manipulation under a shared `RuntimeValueAccess`; keep syntax AST operations separate from semantic values. |
| reflection machine and store | 3 | 0 | 0 | 48 | Open access only in callback-free reflection/evaluator steps; retain roots across transaction, wait, and host-callback boundaries. |
| **Total** | **73** | **23** | **8** | **486** |  |

D.2b.1a-D.2b.1b added four regional operations:
`RuntimeValueAccess::duplicate_value`, non-demanding diagnostic-kind
inspection, value-to-key conversion, and key reification. These are not
repaired violation counts: they are the explicit destinations for later
call-site migrations, and the 486 authority-free operations remain latched
until those callers move.

The collector allowlist is intentionally narrow: the
`core/managed/payload_edges` compatibility visitor family and
`trace_promise_assignment`. These functions run under collector phase
authority where an ordinary mutator is structurally unavailable. Adding a new
source file does not acquire collector status automatically.

## Highest-value repair seams

1. D.1b removed the ordinary raw `EvalContext::evaluate_whnf` orchestration
   path. Existing `api::Value` and `RuntimeValueRoot` callers now reuse their
   registered root, and completed public client demand remains rooted instead
   of being projected and wrapped again. D.2 owns the explicitly named raw
   compatibility callers.
2. Evaluator and builtin helpers should be migrated as families beneath
   `EvaluationValueAccess`. Adding an access independently to every leaf would
   preserve correctness but create needless mutator churn.
3. Front-end parsing/resolution must not be confused with semantic-value
   ownership. Source-embedded Glam data and lowered expressions need a shared
   runtime access or durable root; syntax nodes themselves remain ordinary
   Rust syntax data.
4. Diagnostics and reflection must retain traced owners across callbacks,
   waits, transactions, and logging. Formatting or comparing raw values is
   not an acceptable authority-free diagnostic shortcut.
5. The raw standard operations (`Clone`, `PartialEq`, `Eq`, and explicit
   `Debug`) require deliberate treatment. They may remain private
   representation machinery only when invoked under access or collector
   authority; their current unrestricted signatures are not grandfathered.

## Coverage boundary

This scanner inventories signatures, aliases, and the raw value's standard
operations. D.2 remains responsible for fields which durably store raw values
and for methods whose only syntactic connection to a raw value is such a
receiver field. D.1b-D.2 must join this API inventory with the existing durable
owner inventory before claiming closure.

The expanded D.2 plan partitions all 486 recorded violations by
implementation family and requires a zero-violation closure mode before Gate
G3. In particular, the derived `Clone`, `PartialEq`, and `Eq` contracts and the
custom `Debug` surface must receive an explicit regional representation
decision; they cannot disappear into a broad allowlist merely because their
present callers often happen to hold access.

## D.2a occurrence assignment

Completed 2026-09-12. The source-backed inventory now assigns each of the 486
violations to exactly one D.2b-D.2g checkpoint and one intended replacement
shape. The checked partition is:

| Owner | Replacement shape | Count |
| --- | --- | ---: |
| D.2b | access-qualified core structural operation | 40 |
| D.2b | managed-cell access | 6 |
| D.2b | bounded runtime-root projection | 3 |
| D.2c | one shared evaluator quantum | 201 |
| D.2d | rooted orchestration plus bounded projection | 14 |
| D.2e | one shared front-end region | 134 |
| D.2f | reflection region or durable root | 48 |
| D.2g | durable public boundary | 11 |
| D.2g | compiler/diagnostic region | 29 |
| **Total** |  | **486** |

The assignment is deliberately path-exact at family boundaries and is paired
with the existing occurrence count and signature fingerprint. A new or moved
declaration therefore cannot silently acquire an owner merely because its
spelling resembles an existing operation. The durable-owner, containment,
active-owner, recursive-identity, and persistent-edge inventories remain the
complementary storage/capture side of the closure; D.2h reconciles all of
those independently latched ledgers after the operation count reaches zero.

Test-only constructors and fixtures are excluded intentionally and remain
assigned to GCI11R-002E. The binary crate is included, but its ordinary
`glam::Value` handles do not appear because they are already durable public
roots.

# Interaction-Net Function Calls Plan — 2026-08-31

Status: proposed; not started.

This plan restores ordinary Glam function calls through interaction-net
construction while preserving the distinction between an opaque raw `Net`
and an ordinary `Function` value. It is intentionally a bounded semantic and
topology repair, not a redesign of function staging, raw-net application, or
the construction effects API.

## Decision Summary

When a principal `Bind` meets `Data(Function)`, the call must perform exactly
one ordinary Glam application and return the resulting value as `Data`:

```text
Bind >< Data(Function)
  -> fused installation of Operator::Applicable(Function)
  -> Operator::Applicable(Function) >< Data(argument)
  -> Data(apply_value(Function, argument))
```

If the function still needs arguments, `apply_value` naturally produces a new
`Value::Function` with one fewer remaining argument. The net therefore sees a
normal data value representing the partial function. Another application
requires another `Bind`; the runtime must not infer or synthesize that
application from the function's remaining arity.

The implementation must **not** load `FunctionValue::stage()` as though it
were a raw `Net`. That would expose the residual binder topology rather than
produce a value-level partial function, and would make a one-argument net call
silently consume more of the function's curried interface.

The implementation also need not materialize the existing intermediate
`Bind >< Bind` bridge. Its annihilation is statically inevitable. Callable
completion can perform the same positional splice directly:

```text
original application Bind       installed Operator
  aux1 -> argument neighbor  =>    principal -> argument neighbor
  aux2 -> result neighbor          auxiliary1 -> result neighbor
```

This fused rewrite removes the original application `Bind` and callable
`Data`, installs one `Operator`, and connects it to the two former auxiliary
neighbors in one net mutation. It preserves the generic `Bind >< Bind` rule
for graphs which actually contain two binds; it merely avoids constructing
that pair as an implementation bridge.

This preserves three separate operations:

| Encountered callable | Meaning of `Bind >< Data(callable)` |
| --- | --- |
| raw `Value::Net` | Load one logical copy at the net's exposed interface. |
| `Value::Function` | Apply one ordinary value argument through `CoreOperator::Applicable`. |
| builtin or applicable dictionary | Retain their existing operator lowering. |

`net_arity` remains the explicit one-way bridge from an opaque raw net to an
ordinary computation or function. This repair does not add an inverse
Function-to-Net conversion.

## Current Mismatch

The construction API can embed functions through `.data Expr`, and the design
documentation says applicable data may be called by a `Bind`. The evaluator,
however, currently rejects `Value::Function` in
`eval::net::lower_core_callable_in` while accepting builtins and applicable
dictionaries. As a result, a graph assembled entirely through the documented
`.bind`, `.data`, and `.wire` effects cannot call an ordinary source function.

Compiler-lowered source application is unaffected because it uses private
core operators such as `CoreOperator::ApplyArity`. That private route does not
repair the construction API: clients cannot emit it, and exposing it would
couple the public interaction-net vocabulary to compiler lowering details.

The existing generic runtime already expresses the required topology as two
steps. `resume_claimed_call_with_operator` replaces callable data with a unary
operator behind a fresh `Bind`; ordinary `Bind >< Bind` annihilation then
connects the original argument and result ports to the operator. This plan
fuses those steps inside callable completion. The operator path already calls
`apply_value` for `CoreOperator::Applicable`, including partial source-function
application. No generic reduction rule or public net API needs to change.

## Semantic Invariants

1. **One application node means one value application.** One original `Bind`
   supplies one argument to a `Value::Function`, independent of the function's
   remaining arity.
2. **Partial application remains a value.** Applying one argument to a
   multi-argument function yields `Data(Value::Function(...))`; it does not
   leave a structurally exposed residual binder.
3. **Saturation keeps ordinary evaluator semantics.** A saturated function
   may yield lazy computation data, wait on a promise or reflection gate, or
   fail exactly as it would through normal application.
4. **Raw nets remain opaque values.** `Value::Net` continues to take the
   logical-copy cursor path. Ordinary `apply_value` must not begin treating a
   raw net as a function.
5. **There is no directional shortcut in the generic graph.** The bridge uses
   the existing positional `Bind` wiring and `Operator >< Data` interaction;
   it does not inspect a port and guess which value is the argument.
6. **The temporary bind is not semantic.** Applicable callable completion
   directly applies the positional splice of the inevitable `Bind >< Bind`
   annihilation. No observable work item or revision may depend on that
   transient pair existing.
7. **Claims remain bounded.** Forcing a lazy or promised callable may block
   using the exact callable-claim protocol completed by GC integration phase
   I3D.3d. The claim must not survive an evaluator callback or wait.
8. **Failures remain structural.** A permanently non-callable value leaves the
   original call stuck. Once callable lowering succeeds, any later application
   failure belongs to the explicit operator pair, as it already does for
   applicable dictionaries and builtins.
9. **No new managed edge family is introduced.** `CoreOperator::Applicable`
   already owns a `Value`. Admitting `Value::Function` broadens a tested payload
   case but does not create a new ownership topology for GC integration.

## Non-Goals

- Do not expose `CoreOperator`, `ApplyArity`, or compiler-generated application
  graphs through the public construction effects API.
- Do not translate a `FunctionValue` by inspecting its stage or
  `remaining_arity`.
- Do not change `net_arity`, raw `Value::Net` WHNF behavior, or the number and
  meaning of `.bind` ports.
- Do not add eager function saturation, argument aggregation, or a new
  multi-argument interaction agent.
- Do not remove or specialize ordinary `Bind >< Bind` annihilation. Only the
  implementation-created pair in callable-to-operator lowering is fused.
- Do not fuse the separate `OperatorYield::Operator` representation. That
  `Bind` carries a partially applied operator as a callable result awaiting a
  future application; unlike the callable-lowering bridge, its annihilation is
  not yet inevitable.
- Do not redesign diagnostics for stuck interaction-net pairs.
- Do not fold this repair into the later managed-net representation work in GC
  integration phase I8.

## Phase F0 — Latch the Missing Semantics

Add the desired regressions before changing production classification and
confirm that at least the direct ordinary-function call fails for the current
reason: `Value::Function` is rejected as non-callable.

### F0.1 — Evaluator-level classification

Add a focused test beside the existing callable-lowering tests which verifies
that an ordinary closed `Value::Function` is intended to lower to:

```rust
CoreCallable::Operator(CoreOperator::Applicable(function))
```

The assertion should distinguish this from `CoreCallable::Net`; accepting the
function by loading its internal stage is specifically a failure.

Because these enums do not need broad public equality merely for this test,
prefer pattern matching plus an identity or behavior assertion over adding
new production comparison traits.

### F0.2 — Executable source regression

Construct a net through `.bind`, `.data function`, and `.wire`, then observe
its result through `net_arity 0`. Use a source function whose result visibly
depends on the supplied argument. This proves the public construction path,
not merely the private operator helper.

Record the pre-fix failure in the phase completion note, then update the same
test to require success after F1. Do not retain an ignored or expected-failure
test as the permanent oracle.

### F0.3 — Fused-topology regression

Replace the current generic-runtime test contract which explicitly expects
`resume_claimed_call_with_operator` to produce a `BindJoin`. First make the
test require the target topology and confirm that it fails against the current
implementation:

- callable completion removes the original `Bind` and callable `Data`;
- the installed `Operator` principal is connected to the original bind's
  first auxiliary neighbor;
- its single auxiliary is connected to the original bind's second auxiliary
  neighbor;
- a data argument is immediately visible as `OperatorCall`, without an
  intervening `BindJoin`; and
- non-data neighbors remain connected without being forced or classified.

The test should inspect topology or the next exact reduction, not merely count
nodes. Node allocation identities and revision counts are implementation
details unless another runtime invariant already promises them.

## Phase F1 — Admit Functions Through the Applicable Operator Path

### Phase F1.1 — Classify ordinary functions as applicable

Make the minimal production change in `src/eval/net.rs`:

```rust
value @ (Value::Function(_) | Value::Dict(_)) =>
    Ok(CoreCallable::Operator(applicable_operator(value)))
```

The exact match layout may follow formatter output. The significant decision
is that functions and applicable dictionaries share the existing value-level
`Applicable` behavior, while raw nets retain the copy path.

Do not change `FunctionValue`, `apply_function_values_in`, generic reduction
rules, or the construction builtins in this checkpoint.

### Phase F1.2 — Fuse callable-to-operator installation

Change `resume_claimed_call_with_operator` so it performs the composition of
its current rewrite and the immediately following `BindJoin` directly. Under
the still-owned callable claim:

1. remove the claimed active-pair record and disconnect the original
   principal-principal wire;
2. take the two auxiliary neighbors from the original application `Bind` in
   their existing positional order;
3. remove the application `Bind` and callable `Data`;
4. allocate the resulting `Operator` only;
5. connect `Operator.principal` to the former first auxiliary neighbor; and
6. connect `Operator.auxiliary(1)` to the former second auxiliary neighbor.

These actions must be one guarded net mutation. Normal `connect` bookkeeping
must publish any resulting active pair—for example, an already-present data
argument—without a separate artificial disturbance for `BindJoin`.

This generic completion path is also used after classifying builtins and
applicable dictionaries. They should receive the same fused topology and must
retain their existing value semantics. The optimization is therefore broader
than the new `Value::Function` match arm, although it remains confined to one
call-completion rewrite.

Reuse the existing auxiliary-taking/splicing primitives where they preserve
the same assertions as `join`; do not open-code adjacency-map edits. The
method may retain its name and return type unless the old return value is found
to promise the identity of the temporary bind. Callers currently interested
only in completion should not gain such a promise.

If the direct splice cannot be expressed without weakening topology checks or
claim atomicity, stop F1.2 for review. Do not retain both paths conditionally
as a performance heuristic: they are intended to be semantically identical,
and one canonical rewrite is easier to verify.

### F1 verification matrix

Use behavior tests rather than only enum-shape tests:

1. **Unary call:** an embedded unary function consumes one net-supplied value
   and returns the expected data.
2. **Partial application:** one `Bind` applies one argument to a binary or
   ternary function. `net_arity 0` extracts an ordinary `Value::Function`, and
   subsequent ordinary source application supplies the remaining argument(s).
   This is the primary regression against loading `function.stage()`.
3. **In-net continuation:** two explicit `Bind` applications may consume both
   arguments of a binary function and return its saturated result.
4. **Captured function:** a function closing over source data remains callable
   and preserves its capture.
5. **Suspended callable:** a lazy or promised value which resolves to a
   function follows the existing block/retry protocol, then succeeds. Prefer
   extending a forced-schedule evaluator fixture over probabilistic repetition.
6. **Unchanged callable families:** raw nets still install logical-copy
   cursors; builtins and applicable dictionaries still lower to their existing
   operators.
7. **Permanent mismatch:** numbers, lists, metadata carriers, and other
   non-callable data still produce the established structured failure.
8. **Fused work shape:** successful callable classification produces no
   temporary `Bind`, no `BindJoin` work item, and exactly the same argument and
   result connectivity as performing the old two reductions.
9. **Non-ready operand:** when the argument-side neighbor is not yet `Data`,
   the installed operator remains connected and waits for ordinary interaction
   rather than forcing or classifying that neighbor during the splice.

The source-level partial-application fixture is the acceptance test. A rough
shape is:

```g
pair x y = [x, y]

partial_net = interaction_net do
  .bind -> [call, argument, result]
  .data pair -> [function]
  .data 1 -> [one]
  .wire call function
  .wire argument one
  .r result

partial = net_arity 0 partial_net
asm.result = partial 2
```

Adapt the final fixture to the parser's established port/list helpers rather
than adding syntax for the test.

## Phase F2 — Operator-Claim Integration Checkpoint

GC integration phase I3D.3e is about replacing manual `Operator >< Data`
claims with the bounded claim protocol. Function calls restored by F1 pass
through that exact operator path directly, without first publishing synthetic
`BindJoin` work, so they are a useful non-synthetic client of I3D.3e.

If this plan is executed now:

- complete F0 and F1 before I3D.3e;
- retain at least unary, partial, suspended, and failing function cases while
  I3D.3e changes operator claims; and
- use explicit barriers/latches for the blocked/resumed ordering. Repetition
  is not evidence for a concurrency contract.

If this plan is deferred until after the current GC integration stretch:

- add a pending cross-reference to I3D.3e before implementing that checkpoint;
- do not claim that I3D.3e has covered all production operator clients solely
  from dictionary or directly constructed operator fixtures; and
- execute F0-F1 before phase I8 changes the outer core-net representation, so
  the semantic repair and managed-representation migration do not become one
  debugging problem.

F2 should not itself add production behavior. It is a verification handoff
between this repair and the claim-lifecycle migration.

## Phase F3 — Documentation Alignment

After the executable behavior is green, update current and target documents:

- `docs/agent_context/interaction_nets.md`: record the three-way callable
  distinction and explicitly prohibit opening `FunctionValue::stage()` for a
  net call.
- `docs/Design.md`: clarify that `.data Function` is ordinary applicable data;
  each `Bind` performs one value application, whereas `.data Net` loads a
  logical copy.
- `docs/Syntax.md` and `docs/SyntaxCheatSheet.md`: keep examples concise but
  make function application and raw-net loading visibly distinct.

Documentation should describe implemented semantics after F1, not announce
the proposed behavior while the mismatch still exists. Until then, this plan
is the only authoritative statement of the intended repair.

## Validation

Run focused tests after each checkpoint, then the repository checks after the
Rust implementation and documentation settle:

```sh
cargo test -q interaction_net
cargo test -q core_callable
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -q
```

Test filters are illustrative; use the final test names if they differ. New
concurrency-sensitive coverage must force the relevant ordering with barriers
or hooks. Do not accept a transient failure followed by repeated passes as
verification.

## Size, Risk, and Sequencing Recommendation

Expected implementation size:

- one small production classification edit;
- one bounded generic-runtime rewrite which removes a temporary allocation and
  reduction;
- roughly seven to ten focused/unit/source regression cases, mostly extending
  existing builders and fixtures;
- three or four short documentation corrections; and
- no public Rust API, Glam syntax, generic reduction rule, or construction-
  effect change.

Risk is **moderate but localized**. The classification edit is low risk and
reuses ordinary application semantics already reached by applicable
dictionaries. The fused rewrite changes topology bookkeeping, but it is the
composition of two existing deterministic rewrites and removes work rather
than introducing a new interaction. The meaningful risks are reversing the
two auxiliary neighbors, missing active-pair publication, accidentally opening
a function stage, consuming more than one curried argument, or losing an exact
blocked-call or operator-call obligation. The verification matrix targets
those failures directly.

Recommendation: implement F0-F1 now, before I3D.3e. The repair is bounded, and
it gives the operator-claim migration a real source-function client while
removing an artificial active pair, reducing rather than increasing the risk
of that checkpoint. F3 can follow immediately or after I3D.3e. Defer the whole
plan only if the current priority is to avoid all semantic edits during I3; in
that case, take the explicit F2 deferral path and complete it before I8.

## Completion Record

| Phase | Status | Outcome |
| --- | --- | --- |
| F0 | pending | Semantic mismatch and unfused topology latched before repair. |
| F1.1 | pending | Functions classify as ordinary unary applicable values. |
| F1.2 | pending | Callable completion directly splices the resulting operator. |
| F2 | pending | Function cases carried through operator-claim migration. |
| F3 | pending | Current and target documentation aligned with implementation. |

# Net Polarity Checker Plan — 2026-10-05

Status: planned. This plan comes before N8's random closed-net generator and
before any fuzzing of nets. It follows the crossed `Bind >< Bind` join that
landed on 2026-10-05.

## Goal

Every interaction net is *polarized*:
- each port carries a sign, `+` (provides a value) or `−` (consumes one);
- every wire joins a `+` port to a `−` port;
- a net's exposed output edge is `+` by fiat, so the exposed interface
  provides a value exactly like a `Data` node does.

A linear-time checker enforces this when a net is built. It is perhaps the
lightest type checker possible: two types, no annotations, and unification by
parity.

Polarity is a construction-time contract, not new semantics. Nets remain the
project's scalability mechanism, not a semantic experiment. Reduction rules do
not change. The checker rejects miswired nets early, with a precise
diagnostic. It also gives N8 and later fuzzing a well-defined input space.

## Decisions

- **Output edge is `+` by fiat — maintainer, 2026-10-05.** The alternative was
  to support both polarities at the exposed port. Fixing it is simpler, is
  checker-enforced, and makes `Data` consistent: `Data(Net)` is `+`, and the
  net it stands for exposes `+`. Every current observation path already
  assumes this: `Bind >< Data(Net)` splices the copied net at an application,
  and `net_arity` applies an application spine.
- **Build the checker early — maintainer, 2026-10-05.** It comes before N8's
  generator and before fuzzing nets, so both generate and accept only
  polarized nets, with a separate negative mode.
- **Crossed bind join — landed 2026-10-05.** It matches Lafont's γγ rule. It
  makes a bind's auxiliaries carry fixed, opposite signs, so polarity is
  locally determined through every bind.
- **Disconnected components are errors — maintainer, 2026-10-05.** A
  constructed net whose component is unreachable from the exposed port is
  rejected. Such a component should never appear in practice, so reporting it
  catches miswiring. This applies to construction only: reduction
  legitimately leaves disconnected garbage, such as closed loops and erased
  subnets. So the runtime invariant checks each component's polarity without
  requiring connectivity.
- **User merge fans are allowed — maintainer, 2026-10-05.** Superposition is
  a large part of why interaction nets help scalability. A `.copy` whose input
  is wired to a consumer is a polarized merge.
- **Eraser signs are free — maintainer, 2026-10-05.** In `−` position an
  eraser discards a value. In `+` position it is essentially an error value,
  the analog of `void`: a consumer that demands it observes an error, not
  data. Erasers never cause a polarity conflict, and conflict diagnostics skip
  them when explaining a cycle.

## Polarity table

Signs below are listed in each node's own port order, principal first.

| Node | Ports | Signs | Notes |
| --- | --- | --- | --- |
| `Data` | `[data]` | `[+]` | Provides its payload. |
| `Operator` | `[input, result]` | `[−, +]` | Consumes data, provides its result. |
| `Bind`, application role | `[ap, arg, result]` | `[−, −, +]` | Consumes the function, receives the argument value, provides the result. |
| `Bind`, function role | `[fn, result, arg]` | `[+, −, +]` | Provides the function, receives the body's result, provides the argument to the body. |
| `Bind`, either role | `[p, aux1, aux2]` | `[s, −, +]` | `aux1` always consumes and `aux2` always provides. The role is the principal's sign. |
| `Fan`, copy | `[input, left, right]` | `[−, +, +]` | Duplicates a value. |
| `Fan`, merge | `[input, left, right]` | `[+, −, −]` | Superposes consumers. |
| `Fan`, either | `[p, l, r]` | `[¬s, s, s]` | One variable per fan. |
| `Erase` | `[input]` | `[s]` | Free: `−` discards a value, `+` is an error value (`void`). |
| Builder tunnel (`.copy 1`) | `[input, output]` | `[¬s, s]` | A wire splice. |
| Exposed interface | the exposed port's peer | `+` | Fixed by fiat. |

Every rewrite preserves polarity on a polarized active pair. This includes the
crossed bind join, the positional fan join, fan commutation, fan duplication
of `Bind`, `Data` and `Operator`, the operator splice, and erasure. The checker
should also verify this, as an N8 invariant.

## Checker

**Algorithm.**
- Give each port a sign variable.
- Each node contributes constraints:
  - a fixed sign is an equation with a constant `+` anchor;
  - "equal" and "opposite" relations come from the table above.
- Each wire contributes `sign(a) = ¬sign(b)`, and the exposed port
  contributes `= +`.
- Solve with union-find over variables, storing each one's parity relative to
  its root.
- A conflict is an odd cycle. Report the wire that closes it, together with
  the path that forced the conflicting parity.

The checker is linear in the size of the net. It allocates only the
union-find arrays.

**Where it runs:**
1. **`NetBuilder::try_finish`,** for every template. Rust-built nets fail
   with a new `NetBuildError::Polarity` variant. User nets built with the
   `interaction_net` effect API surface it at the netlist replay boundary as
   an ordinary construction diagnostic. Branded port handles let the
   diagnostic name the user's ports.
2. **`RuntimeNet` in test builds,** after every transition, as part of N8's
   invariant checker. That needs signs for the evaluator-only nodes:
   - interface anchors are `−`, because they consume the net's output;
   - a remote cursor takes the sign of the remote port it stands for;
   - a callable checkpoint takes the sign of the application it replaces.

## Slices

1. **Document the table.** Put it in `docs/agent_context/interaction_nets.md`.
   In `Design.md`, replace "symmetric instead of directional" and "no
   arg-result distinction" with "undirected wiring, polarized ports". Fix the
   stale `Node::Bind` doc comment in `model.rs`, which still says
   `[ap*, arg, result]` for both roles.
2. **Read-only checker, test-only.** Run it over every lowered g net, every
   core and test template, and the sample nets. Expect all to be polarizable
   today. Fix any glam constructor that is not; the constant-effect template
   is a reminder that hand-built function binds can drift.
3. **Enforce at `try_finish`.** Add the error variant, user-facing
   diagnostics, and invalid samples for each rejection shape:
   - a same-sign wire, such as data wired to data;
   - a function bind wired in application order;
   - a `−` exposed port;
   - a component disconnected from the exposed port.
4. **Runtime invariant.** Check polarity preservation after every rewrite in
   test builds, alongside N8's link-symmetry and active-pair checks.
5. **N8 generator.** Random polarized closed nets, plus a negative mode that
   must be rejected at construction. Compare readback across random pair
   orders.
6. **Fuzzing,** per the panic-safety plan's discovery policy, over polarized
   nets only.

## Background: polarity, GAL, and Lafont

An earlier design discussion explored adapting GAL level tracking to nets not
built from λ-calculus. GAL is an optimization of Lamping's brackets and
croissants. That discussion is deferred: no work toward levels, boxes,
brackets, or croissants is planned. It is recorded here because the polarity
checker is its first, independently useful step.

- **Polarity and roles.** The discussion's polarity is "+ provides, −
  consumes", and a net's root is `+` by fiat. It inferred bind roles by
  union-find with parity. With the crossed join, roles need no inference for
  checking: a bind's auxiliaries are always `[−, +]`, and only its principal
  varies.
- **Lafont 1997.** γγ annihilates crossed (`γ[x, y] ⋈ γ[y, x]`) and δδ
  annihilates straight. glam's `Bind` is γ and its fan is δ. Lafont's
  *directed combinators* `γ, γ*, δ, δ*, ε, ε*` polarize this system:
  - "a + must always be plugged into a −";
  - the polarization separates forward and backward execution;
  - the annihilation orientation "is just a matter of taste".

  The polarity checker effectively verifies that a net lifts to its directed
  reading.
- **Mazza.** The symmetric interaction combinators annihilate everything
  straight. They are universal only for polarized systems, and their polarized
  form is Lafont's directed combinators. Mazza also notes that the crossed γγ
  rule prevents his relational semantics.
- **Deferred beyond this plan:**
  - GAL levels for fans, in place of history identities;
  - box and level inference;
  - bracket and croissant nodes;
  - levels on `Data` and `Operator`;
  - a label-equality debug assertion.

  Each would change reduction semantics or runtime representation, and needs
  its own design discussion first.

# Delimited Control — 2026-10-23

An exploration the maintainer asked for (2026-10-23), for the near future:
whether the standard API's delimited control can separate its
responsibilities more clearly. Not a priority unless it would simplify
something at once.

## The question

The reset stack lives in the user's state, under a key no source can name
(Design.md, "Flexible Monoliths"; `effect-families-pass-through`). The
effect machine keeps it there, and since `pure-handler-control` so does the
pure handler's core. One placement serves several ends:
- where a running computation finds its resets;
- a thread's checkpoint: `.get []` captures the control state with the data,
  and `.set []` restores both, for coroutines and effectful backtracking;
- clearing every reset in scope, by replacing the state with one that lacks
  the key.

The maintainer has second thoughts (2026-10-23): the placement conflates
too many responsibilities, and where the stack lives is somewhat arbitrary.
Restoring the abstract reset context is tangled with returning to a
continuation, the corner the four-primitive frameworks address.

## Symptoms so far

- A data write, `.set []`, can drop or install control frames as a side
  effect.
- In the pure handler, a frame restored after the cut it lies within has
  ended returns through a mark no walk awaits. The run fails with a
  diagnostic; the semantics are unsettled.
- `.fix` hides the resets beneath it with a barrier frame in the same
  stack.
- The effect machine keeps an empty stack's key; the pure handler removes
  it. Neither is specified.
- The `.with` sketch (`EffectWith_2026-10-22.md`) uses resets as dynamic
  bindings, one more responsibility.

## References

Both are cited in Design.md:
- "A Monadic Framework for Delimited Continuations" (Dybvig, Peyton Jones
  and Sabry) builds control from four primitives: `newPrompt`,
  `pushPrompt`, `withSubCont` and `pushSubCont`.
- Kiselyov's delimcc for OCaml, "Delimited Control in OCaml, Abstractly and
  Concretely", implements the same four as `new_prompt`, `push_prompt`,
  `take_subcont` and `push_subcont`.

Their separation is what this exploration is about:
- prompts are fresh values, not names;
- a captured subcontinuation is a value apart from the stack it came from;
- reinstating a subcontinuation onto the current stack is an operation of
  its own, not a side effect of returning or of restoring state.

## Questions

1. Which responsibilities are there, and which primitive should own each:
   delimiting, capture, reinstatement, a thread's checkpoint, and clearing
   scope?
2. Should a thread's checkpoint be an explicit control effect, capturing and
   restoring the control context as an abstract value, so that the user's
   state is plain data and `.get []`/`.set []` touch only data?
3. Prompts: keys, as now, or fresh prompt values? Fresh prompts cannot be
   captured by accident across abstractions. Keys are convenient; could they
   stay as sugar?
4. Which shift is ours? `.shift` removes its matching reset, and its function
   may reinsert one (Design.md). That is closer to `shift0` or to
   `withSubCont` than to classic `shift`. Name and specify the variant.
5. How does each option work with `.fix` (hiding scope), `.cut` (marks in
   the continuation, a shift that leaves cuts), `.alt` (every branch in pure
   handlers, a cut first operand in machines) and `.with` (dynamic
   bindings)? How does it keep the effect machine and the pure handler
   alike, and continuations valid only within their task?
6. What gets simpler? A control context apart from the user's state would
   drop the pure handler's reads of the user's state on return, and its flag
   for whether resets may exist. It would also drop the effect machine's
   encoding of frames into the state.

## Steps

1. `control-survey` — list every use of the reset stack in the user's
   state, in the effect machine, the pure handler, the interaction-net
   builder, samples, tests and docs, with what each relies on. Summarize the
   four-primitive frameworks' semantics against today's `.reset` and
   `.shift`.
2. `control-options` — a short design note with two or three options, each
   showing how `.fix`, `.cut`, `.alt` and `.with` work, for the maintainer
   to choose from. Candidates:
   - the status quo, specified;
   - a control context apart from the user's state, with explicit effects
     to checkpoint and restore a thread;
   - the four primitives, with keyed `.reset`/`.shift` as sugar.
3. `control-build` — if adopted, build in the pure handler's core first, as
   the smallest; then the effect machine; then the interaction-net builder,
   or with `net-builder-handler`. An ADR revises the stack's placement in
   Design.md and `effect-families-pass-through`.

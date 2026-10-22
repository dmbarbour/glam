# Effect API Extension, `.with` — 2026-10-22

`.with Mixin Op` (Design.md, "Standard Effects", tentative) runs `Op`
against the effect API extended by `Mixin`: a monad transformer stack built
from API objects, which lets users model an extended effects API without
introducing primitives. The maintainer (2026-10-22) asked for its interplay
with the other effects, `.reset`/`.shift` above all, and a concrete
approach, before any build. It is not to come ahead of the runtime API
transition without an immediate use case. The list handler (`list.pure`)
stays specialized.

## Today

- An effect is `{eff:f}`. The effect machine applies `f` to one API dict
  per task (`EffectTask::api`, built by `effect_api`) and selects the
  request by its private tag. A member that yields anything but a tagged
  request is an error.
- A branch's control is a continuation `sequence` with `delimiters`:
  `Resume` around a resumed continuation, `Restore` around `.fix`. Reset
  frames live in the state under the private `continuation_state` key, so
  `.get []` captures them and `.set [] checkpoint` restores them.
  Captured continuations are handles local to their task.
- `.fix` hides the reset stack while its function runs. A cut frame holds
  the outer branch, and its alternatives are whole branches.

## Semantics, proposed

1. **Scope.** In `.with m op >>= k`, `op` runs against `api ⊕ m` and `k`
   against `api`.
2. **Composition is the object model's.** `api ⊕ m` instantiates `m` over
   the API as its prior. Within `m`, `prior.op` is the API outside this
   `.with`, and a member's dot-sugar `.op` dispatches through the API in
   scope when it runs, as `self` does: open recursion.
3. **A member may answer with an effect.** Where `api.op args` yields
   `{eff:_}` rather than a tagged request, the machine runs it in place,
   against the API in scope (Design.md: handlers recognize `{eff:_}`).
   Tagged requests from `prior` reach the machine as they are.
   - So an inner `.with` intercepts the dot-sugar requests in an outer
     mixin's effects, as an override does in any object. A layer that must
     not be intercepted calls `prior.op`.
   - Considered instead: handler-style dispatch, where a member's effects
     run against the layers outside the one that defined it (deep handlers,
     transformer semantics). Composed objects do not say which layer
     defined a member, so this needs a layer per definition and dispatch
     by search. Heavier, and its encapsulation is had by `prior.op`.
4. **Layers are delimited dynamic bindings** (Kiselyov, Shan and Sabry,
   "Delimited Dynamic Binding", 2006). They belong to the control context
   and are delimited with it.
   - `.reset k op` records, in its reset frame, the layers in scope at the
     reset.
   - `.shift k f` runs `f` with the target frame's layers. The captured
     continuation holds the layers pushed since that reset, each with its
     pop frame in the captured sequence.
   - Resuming the continuation pushes those layers over the layers where
     it resumes. A `.with` within the delimited segment travels with the
     continuation; one outside it comes from wherever it resumes.
   - The reset frames record their layers, so a checkpoint taken under one
     `.with` scope and restored under another stays consistent: returning
     through, or shifting to, a restored frame restores that frame's
     layers. The layers themselves are control, not state, balanced by
     their pop frames as the sequence is.
5. **`.fix` keeps the layers visible**, so a recursive `do` under `.with`
   sees the mixin's operations. `.fix` hides the reset stack because a
   continuation may run many times against its one fixpoint value, which
   does not apply to a view of the API.
6. **Search needs nothing more.** Branches carry their layers through
   `.alt`, `.fail` and a provider's `Alternatives(n)`. A cut's body is
   balanced, so the cut continues with the layers it entered with.
7. **Host requests.** A mixin may intercept a host request,
   `log s m := prior.log s (decorate m)`, or hide one, `heap := {}`.
   Hiding is advisory: a tagged request obtained elsewhere still runs,
   since tags are unforgeable but transferable.
8. **Pure handlers.** `list.pure` keeps its fixed API. A pure runner for
   the whole monolith, state, resets and `.with`, would be a separate
   builtin.

| Effect | With layers |
|---|---|
| `.r`, `.seq` | none; `.with` pushes a pop frame like any continuation |
| `.get`, `.set` | none; layers are not state, and a mixin keeps its own state under its own key |
| `.reset`, `.shift` | frames record layers; continuations carry those pushed since their reset |
| `.fix` | layers stay visible |
| `.alt`, `.fail`, `.cut` | branches carry layers |
| host requests, providers | interceptable and hideable through `prior` |

## Concrete approach

- `.with mixin op` is a pure request that every effect machine serves,
  reflection tasks and turns alike, with arity 2.
- A branch holds its layers, a persistent stack of `(mixin, api)` with each
  API composed once. The decoder applies an effect to the top layer's API;
  `EffectTask::api` becomes the bottom layer.
- Entering composes `api ⊕ mixin` lazily, through the object machinery,
  with the base API as an anonymous object. It pushes the layer and a
  `Continuation::PopLayer`, then drives `op`. Delivering through
  `PopLayer` pops the layer.
- Request selection accepts `{eff:_}` from a member and drives it as the
  branch's effect.
- A reset frame encodes its layers' mixins beside its key, continuation,
  depth and order. A shift gives the handler the target frame's layers and
  records the suffix in `CapturedContinuation`. `install_captured` pushes
  the suffix again, composing each API over the resuming site's.
- `.fix` is unchanged: the layers pass `Delimiter::Restore` untouched.
- Cost: one lazy instantiation per `.with` entered. Dispatch stays one
  member access, and a branch clone copies a short stack.

## Steps

1. `effect-with-semantics` — an ADR from this plan, once the maintainer has
   answered its open questions.
2. `effect-with-scope` — the request, layers, pop frames and members answering
   with effects. Tests: scope, override through `prior`, open recursion,
   effects answered in place, and search.
3. `effect-with-delimited` — reset frames record layers, and shift and resume
   delimit them. Tests: each placement of `.with` against `.reset` and
   `.shift`, a continuation resumed outside its `.with`, and checkpoints
   restored across scopes.
4. `effect-with-fix` — `.fix` under `.with`. Test: a recursive `do` sees the
   mixin's operations.
5. `effect-with-syntax` — only if wanted: sugar beyond `.with m op`, since the
   keyword `with` belongs to objects.

## Use cases and priority

- A library of functions returning effects needs no `.with`: a glam
  tokenizer could define `tok.text "x"` as `.get ['pos] >>= …` today.
  `.with` adds dot-sugar for new operations, interception and hiding.
- Internal candidates: `MacroEffects` hides the heap
  (`exposes_shared_heap`), direct effect profiles hide `.exit`, and the
  specializations shape their APIs in Rust generally. Each could become a
  `.with` layer in glam rather than a Rust flag.
- Decorating `.log`, as `conf.log` might.
- None blocks anything. Recommended: after the runtime API transition,
  unless one of its steps finds a use.

## Open questions

1. Open recursion, as proposed, or handler-style dispatch for the effects
   that members answer with?
2. Should `.fix` keep the layers visible?
3. Which mixins: objects only, or dicts too, by `object_from_dict`? A
   dict can add members but not override them.
4. Should hiding be enforceable, for instance by layering tags?

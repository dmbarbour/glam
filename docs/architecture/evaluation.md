# Evaluation Architecture

This document follows ordinary value evaluation through sessions, lazy work,
interaction nets, and background workers. Detailed hazards live in
[`../agent_context/evaluation.md`](../agent_context/evaluation.md) and
[`../agent_context/interaction_nets.md`](../agent_context/interaction_nets.md).

## Context and Session

Every production evaluator entry receives an `EvalContext` borrowed from an
`EvaluationSession`. An `Assembler` and its clones share one internal
`ReasoningSession`, which owns that evaluation session and the assembler's
reflection host. `EvaluationSession` owns reflection and lazy task records,
wait lookup, the reflection launcher, and its connection to a shared
`EvaluationExecutor`. The immutable reflection environment belongs to the
active task host rather than the scheduler.

Lazy values retain computation and a stable identity, not a captured evaluator
session. The observing `EvalContext` supplies host and scheduling behavior when
the value is forced.

When a lazy task blocks on another lazy producer, the session records one
strict dependency edge. The graph has at most one outgoing edge per unresolved
lazy task, so an edge insertion can find a cycle with a successor walk. A pure
lazy cycle receives one canonical structured failure shared by all members;
an edge through reflection or another external producer is not poisoned.

## Value Observation

```text
ordinary value demand
  -> non-lazy data, FunctionValue, or Value::Net is already WHNF
  -> LazyValue work is claimed, computed, and memoized through its session task

arity bridge
  -> arity 0: LazySource::NetComputation expects exposed Data
  -> arity n: FunctionValue expects n staged Bind interfaces, then Data

apply(function, arguments)
  -> builtin or partial-builtin staging
  -> shared FunctionValue curried stage
  -> temporary dictionary-applicability compatibility

interaction-net call
  -> Bind >< Data(Value::Net)
  -> logical-copy cursor attached to the opaque net's exposed interface
```

An undersaturated `FunctionValue` shares a curried runtime stage; saturation
produces memoized work. A raw `Value::Net` is an opaque value already in WHNF,
not an ordinary callable. Only the interaction-net call reduction opens it by
attaching a cursor. `LazySource::NetComputation` is the internal zero-arity
bridge: forcing it must expose data, and an exposed bind or non-data normal
form is an error. `FunctionValue` provides the corresponding positive-arity
bridge and checks its bind spine during staged application.

The source language does not yet expose `interaction_net` or the provisional
`net_arity` bridge. There is also one deliberate bootstrap mismatch:
`eval_value(Value::Net)` still calls `observe_net` and may project exposed data.
Do not build new behavior on that path. Removing raw-net projection and the
shallow evaluator is deferred until the lazy-cycle transition plan is
complete.

Compact persistent lists live in `list.rs`. Their lazy holes are opaque; range
and binary observation in `eval/sequence.rs` forces only the pieces required by
the caller.

## Lazy Producers

Computed fixpoint cells track the lazy task currently responsible for
production. Strict recursive observation is diagnosed by the common lazy
dependency graph, while guarded recursion can finish at a constructor. Other
tasks wait on a stable token if production suspended. Task-owned reflection
fixpoints retain their direct owner check. Assignment-style `Promised` cells
are a separate fail-fast bootstrap mechanism.

Reflection annotations are also lazy producers. Constructing a gate demands
neither its effect nor its target. Demand on the gate registers or resumes the
effect task; after checking that it returned unit, the same demand continues
into the target. Blocking remains session task state rather than a cached lazy
error.

## Interaction-Net Handoff

`NetBuilder` validates an immutable template. Instantiation creates a shared
runtime with a stable interface. Evaluation repeatedly claims one exact
principal-principal active pair. Pure topology rules rewrite under the runtime
lock; core callable, operator, or cursor work runs after releasing it and then
updates the same pair.

Logical copies use target-owned one-way cursors into stable source frontiers.
A source active pair reduces in the source and never crosses a cursor boundary.
See the focused interaction-net note for fan identity, frontier, and locking
rules.

## Shared Executor

Related assembler, logger, and future IDE sessions register with one
`EvaluationExecutor`. Its fixed worker pool alternates between ready reflection
sessions and optional spark work. The serial pump remains available for exact
foreground dependencies and explicit batch draining.

Demand on `seq A B` forces the outer semantic value of `A` before transferring
that demand to `B`. Demand on `spark A B` submits `A` and transfers foreground
demand to `B`; merely constructing either expression demands neither target.
Their annotation forms use the same paths. Only workers consume sparks, so a
zero-worker executor discards them immediately.

Sparks are performance hints outside reflection transactions and reasoning
completion. They do not keep sessions alive or report independent failure. A
divergent spark can occupy a worker forever; the bootstrap currently provides
neither evaluator fuel nor cooperative cancellation.

# Evaluation Architecture

This document follows ordinary value evaluation through sessions, lazy work,
interaction nets, and background workers. Detailed hazards live in
[`../agent_context/evaluation.md`](../agent_context/evaluation.md) and
[`../agent_context/interaction_nets.md`](../agent_context/interaction_nets.md).

## Context and Session

Every production evaluator entry receives an `EvalContext` borrowed from an
`EvaluationSession`. An `Assembler` and its clones share one internal
`ReasoningSession`, which owns that evaluation session and the assembler's
reflection host. `EvaluationSession` owns task records, lazy single-flight
claims, wait lookup, the reflection launcher, and its connection to a shared
`EvaluationExecutor`. The immutable reflection environment belongs to the
active task host rather than the scheduler.

Lazy values retain computation and a stable identity, not a captured evaluator
session. The observing `EvalContext` supplies host and scheduling behavior when
the value is forced.

## Value Observation

```text
Value + EvalContext
  -> eval_value / force_value_shell
       -> return already observable data
       -> claim, compute, and memoize LazyValue work
       -> drive a net computation until its interface exposes Data

apply(function, arguments)
  -> builtin or partial-builtin staging
  -> shared FunctionValue curried stage
  -> explicit Value::Net cursor attachment
  -> temporary dictionary-applicability compatibility
```

An undersaturated `FunctionValue` shares a curried runtime stage; saturation
produces memoized work. An explicit `Value::Net` may leave a residual net after
application. A net-backed lazy computation has a narrower contract: forcing it
must expose data.

Compact persistent lists live in `list.rs`. Their lazy holes are opaque; range
and binary observation in `eval/sequence.rs` forces only the pieces required by
the caller.

## Lazy Producers

Computed fixpoint cells track the task currently responsible for production.
The owner detects recursive self-observation, while other tasks wait on a
stable token if production suspended. Assignment-style `Promised` cells are a
separate fail-fast bootstrap mechanism.

Reflection annotations are also lazy producers. Their first observer registers
an effect task in that session. Completion returns the untouched target after
checking the effect returned unit; blocking remains session task state rather
than a cached lazy error.

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

`seq A B` forces the outer semantic value of `A` before returning `B`.
`spark A B` submits `A` and immediately returns `B`; its annotation forms use
the same paths. Only workers consume sparks, so a zero-worker executor discards
them immediately.

Sparks are performance hints outside reflection transactions and reasoning
completion. They do not keep sessions alive or report independent failure. A
divergent spark can occupy a worker forever; the bootstrap currently provides
neither evaluator fuel nor cooperative cancellation.

# Diagnostic Invariants

These rules protect structured errors, committed publication, provenance, and
client-owned rendering. Current flow lives in
[`../architecture/diagnostics.md`](../architecture/diagnostics.md).

## Failure Representation

- Permanent evaluation failures retain one Glam diagnostic emission plus an
  ordered context stack through lazy caches, task results, public `Error`, and
  runtime reports. Do not stringify at scheduler or host boundaries.
- Retryable waits and unassigned promises are control states, not errors. A
  cached terminal lazy result must never contain either.
- `anno 'error Message` evaluates `Message` to WHNF before raising it. Failure
  during that demand gains `eval:{op:'error_message}` context.
- `anno context:Frame Expr` decorates only a permanent failure reached while
  demanding `Expr`; it does not turn a wait into an error or publish a message.
- Public `Error::Display` is a Rust-facing summary. The primary structured
  diagnostic and additional emitted diagnostics remain separately available.

## Context Frames

- Automatic evaluator frames use `eval:{op:Atom, args?:Dict}`. Operations
  without arguments omit `args`; do not use a unit/dictionary union or embed
  renderer prose.
- Binary extraction adds `eval:{op:'binary_extraction}` only when a deferred
  piece fails while being demanded. Immediate validation already identifies
  the invalid value and receives no redundant frame.
- Failed intermediate path demand adds
  `eval:{op:'path_lookup, args:{path:Text}}`; a merely absent member does not.
- Configuration entry failures use `{conf:{entry:Text}}`.
- `.task.join` adds `{task:{operation:'join, id:Number}}` only when propagating
  a child failure. Status and error inspection observe the failure as data and
  add no frame.
- Explicit net computation adds `eval:{op:'net_computation}`. Raw `Value::Net`
  is already WHNF and receives none.
- Built-in source definition initialization uses the shallow static frame
  `{g:{origin:OpaqueOrigin, line:Number, definition:Text}}`. It must not capture
  function arguments or follow later calls.
- Import failures use the `import` tag. Reserve `g` for built-in front-end
  syntax and definition context.

## Origins and Privilege

- Front ends receive one opaque source-origin token and choose how to place it.
  They do not observe its fields.
- Evaluation and rendering never recursively search messages for opaque
  origins. A reflection task explicitly invokes
  `.env '.glam.origin.inspect`; Rust clients use `Assembler::reflection`.
- Assembler-authored import frames may carry ordinary projected provenance
  because they are constructed outside the compiler capability boundary.
- Origin records must not retain module values, compilation environments, or
  another runtime's opaque values.

## Publication and Counts

- Severity is an argument to publication, not inferred by evaluating the
  message.
- Transactional diagnostics publish only after commit. Abandoned alternatives
  affect neither subscribers nor severity counts.
- A bus owns sequence numbers and coherent severity counts, never retention.
  Queue consumption, dropping, rendering, and subscriber failure cannot undo a
  committed count.
- Assembler, macro, and logger buses remain distinct even when their tasks share
  one runtime.
- Runtime ingress prepares its transport root before mutation admission. A
  valid bus publication remains counted if ingress preparation fails; that
  transport failure remains separately reportable.

## Logger and Rendering Boundaries

- The library does not render or print diagnostics. Executable and IDE clients
  perform privileged inspection through the public reflection facade.
- Logger input and logger-session output buses are separate. Logger `.log`
  cannot feed its own `.read_log` stream.
- Logger input claims, heap edits, and output intents commit atomically. Host
  decoding and callbacks happen after all runtime locks and mutation admission
  are released.
- Diagnostic consumer route activation and logger-root activation occur under
  one exclusive settlement guard. Never expose a live route without its root.
- Nested context frames with a `msg` interface are enriched and rendered as
  diagnostic-style views, not republished events. They do not receive new
  sequence numbers or increment counts.
- `viewer.header` is complete presentation text, including spacing and terminal
  policy. It is not another semantic severity.
- The cached Glam formatter arranges client-supplied viewer data. It must not
  gain privileged origin inspection or presentation-policy inference.
- Retained task, delivery, exit, and killed-work failures make batch execution
  fail independently of successful diagnostic rendering. Fallback failure
  cannot turn a failed batch into success.
- Settled report obligations are one-shot. Do not retain growing
  supervisor-side identity sets to suppress repeated rendering.

## Locking and Destruction

- Authoritative component updates occur under their one component mutex and
  runtime mutation admission. Advance the observation epoch before releasing
  mutation admission, not by nesting the epoch mutex with another component
  lock.
- Subscriber callbacks, diagnostic enrichment, value destruction, wake
  delivery, output decoding, and host callbacks occur outside runtime locks.
- Endpoint and ingress back-references remain weak where a strong link would
  create a bus/runtime ownership cycle.

Tests around diagnostic ingress, logger activation, fallback races, structured
context rendering, and failed delivery are ordering specifications. When
changing these paths, use barriers to force the suspected race rather than
accepting a transiently passing stress test.

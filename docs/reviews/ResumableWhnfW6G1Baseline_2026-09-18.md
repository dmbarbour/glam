# Resumable WHNF W6G.1 Baseline Review

Date: 2026-09-18

Scope: the W6G.1 pump-ownership and lazy-producer boundary in
[`ResumableWhnfEvaluation_2026-09-12.md`](../plans/ResumableWhnfEvaluation_2026-09-12.md).
This is an implementation inventory, not the post-W6G.1 review.

## Current selector topology

| Entry point | Current claim surface | Current owner between polls | Target role |
| --- | --- | --- | --- |
| executor `select` | client demand, spark, reflection, any promoted deferred producer | one combined coordinator `WorkRecord` registry | spark/reflection roots plus exact descendants only |
| runtime `select_runtime_pump` | client demand, reflection, any promoted deferred producer | combined coordinator registry | explicitly selected reflection roots plus exact descendants only |
| synchronous `drive_client_demand` | exact client record only when there are no workers; exact dependency, then runtime-wide work, while blocked | client operation in the combined coordinator registry | exact client record plus exact descendants on the calling thread |
| `pump_demand` | deepest exact task, then unrelated ready work in the same demand session | combined coordinator registry | exact dependency chain only |
| session `run_until_quiescent` | reflection-preferred, then any ready work in the same demand session | combined coordinator registry | selected reflection roots plus exact descendants only |
| exact `claim_task` | one named reflection or deferred task | combined coordinator registry | causal descendant claim primitive |
| quiescence/readiness observation | no direct claim; runtime stabilization currently invokes the runtime-wide pump separately | combined coordinator registry | observation remains non-pumping |

The current worker and runtime-pump selectors therefore treat foreground
client records as globally claimable. Promoted deferred producers are also a
global queue rather than descendants rediscovered from a selected root. The
one-ordinary-machine-per-session scan limits those claims, but does not encode
causal ownership.

## Current record ownership

| Work kind | Machine owner | Session/profile provenance | Target disposition |
| --- | --- | --- | --- |
| `ClientDemand` | coordinator `ClientDemandWork.operation` | requesting client's demand session and default profile | client-only registry; never worker selected |
| `Spark` | coordinator `SparkWork.demand` | spark creator's demand session | background root |
| `Reflection` | coordinator `ReflectionWork.machine` | declared task session and task profile | background root |
| `Deferred` lazy producer | coordinator `DeferredWork.machine` | first discovering observer's session, originating task, and profile | managed lazy owns source/checkpoint/result; coordinator route is transient |
| `Deferred` task-owned promise producer | coordinator `DeferredWork.machine` | declared producer task lifecycle | retain producer-task ownership; observers share completion only |

`DeferredIndexes::by_value` already makes lazy admission canonical. It does
not make the resulting producer session-neutral: `reserve_deferred` installs
the first candidate's `demand_session`, wait token, `EvalContext`, and machine.
Closing that session currently abandons the producer even when another role
still observes the same lazy.

## Lazy producer checkpoint families

Every `LazyTaskWork` variant may survive a returned poll. "Reconstructable"
below means reconstruction would not change Glam's pure result; it does not
mean replay is operationally acceptable.

| Family | Durable semantic state today | External/one-shot boundary | Reconstructable without observable replay? | Managed migration requirement |
| --- | --- | --- | --- | --- |
| `Produce` | managed lazy source plus machine's `ManagedLazyRoot` | none yet | yes, before source classification | source remains in managed lazy |
| `Whnf` | `WhnfComputation`, including runtime roots or managed WHNF root | none | pure, but restarting loses partial work | edge-owned WHNF checkpoint |
| `NetWhnf` | normalization request, driver frontier, failure context | contested net claims | restarting loses net progress and may repeat contention | traced managed net/request edges plus scalar driver state |
| `Access` | path, rooted arguments/current value, key-conversion stacks, nested WHNF work | none | pure, but restart repeats conversions and demand | replace runtime roots with traced edges throughout access state |
| `Builtin` | family-specific rooted operands and nested machines | may publish a spark request | result is pure, but replay may duplicate scheduling observations | migrate every builtin submachine or keep a reviewed explicit root owner |
| `ObjectFixpoint` | rooted spec/self/base/definitions and nested conversion/WHNF stacks | promise/lazy dependencies only | pure, but restart loses traversal and mix progress | traced object/checkpoint state |
| `ListEffect` | rooted continuations/front machines/WHNF work; fix state owns a managed promise root | promise creation/assignment in fix handling | replay can manufacture a distinct promise and repeat effect traversal | managed promise edge and traced list-effect state |
| `HostCall` | before/invoking/after/consumed state and callback result root | opaque Rust callback, exactly once | no after `Before -> Invoking` | edge-free callback token plus managed captures/result/failure edges |
| `Reflection` | computation plus optional task reservation | task reservation, activation, and autonomous terminal execution, exactly once | no after reservation | task-owned managed completion promise followed through the ordinary WHNF checkpoint path; no reflection checkpoint |
| `NetConstruction` | isolated effect search, branch journals, exposed-value WHNF work | effect-search task lifecycle and journal progression | no: replay repeats search/task observations | split traceable search state from edge-free host/orchestration state |

The last four rows prevent a narrow `Whnf`-only ownership change from being a
sound last-subscriber policy, but they do not all require checkpoints. In
particular, `RuntimeValueRoot`, `ManagedLazyRoot`, `ManagedPromiseRoot`, and
`ManagedWhnfRoot` cannot be hidden inside a managed lazy checkpoint: their
registered roots would create an untraced backedge and retain cycles.
Demand-driven families must expose their semantic `Value`/`Gc` edges directly
to tracing. A started reflection task instead remains an autonomous
background root and fulfills a managed promise; its temporary registered
roots are task obligations outside the value graph and retire terminally.

## Existing deterministic coverage

The current suite already forces these orderings rather than relying on test
repetition:

- worker-owned foreground demand and the retirement/publication handoff;
- subscription before and after canonical lazy completion;
- two foreground consumers of one canonical lazy;
- a client following a lazy producer first owned by another session;
- exact reflection and spark wakeups through promise/wait dependencies;
- owner-session close while deferred, reflection, or spark work is queued,
  claimed, or blocked;
- terminal publication before coordinator retirement; and
- serialization of ordinary machine polls within one demand session.

W6G.1 still needs new forced fixtures for the *target* policy: workers and
runtime drains rejecting foreground records, workers reaching deferred work
only through a selected background root, three-role contention for one lazy,
session-neutral survival after first-observer close, and last-subscriber route
retirement with checkpoint preservation. Those fixtures belong beside the
mechanism that introduces each target behavior; this baseline records the
current mismatch without treating repeated parallel runs as evidence.

## Implementation order after inventory

1. Make foreground client selection explicit and remove it from worker and
   runtime-drain selectors while retaining the temporary per-session admission
   guard.
2. Separate root queues from the deferred producer index, but retain the
   current coordinator-owned producer machine until the managed checkpoint is
   ready.
3. Introduce the lazy source/checkpoint/result protocol and migrate producer
   families as one exhaustively inventoried boundary. Demand-driven work uses
   checkpoints; autonomous reflection work uses a task-owned managed
   completion promise. A discriminated transitional state is acceptable; two
   simultaneous authoritative producer states are not.
4. Change deferred selection from a global ready queue to exact traversal from
   the selected client, spark, or reflection root.
5. Retire the temporary session-wide machine scan only after every selector
   expresses its role directly.

The producer-family migration is substantially larger than a `Whnf` cell
change. It should be partitioned by ownership family, with a compile-exhaustive
inventory and a forced no-replay test at every host/reflection boundary.

## 2026-09-19 design correction

The baseline's implementation inventory remains accurate, but its original
target classification treated reflection as if it were resumable lazy work.
That was incorrect. Once activated, a reflection task runs to a terminal
disposition independently of continued demand for the lazy which launched it.
The corrected W6G.1f.3b design gives the task a managed completion promise and
lets the lazy follow that promise through its ordinary WHNF checkpoint. No
task handle, task continuation, or registered root is stored in the managed
value graph.

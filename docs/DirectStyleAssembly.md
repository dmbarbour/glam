# Direct-Style Assembly

Direct-style assembly presents machine instructions and assembly bookkeeping as
effects. A program writes an intermediate assembly representation without
repeatedly reading and returning the representation itself. Higher-order Glam
functions serve the role traditionally filled by assembly macros.

This document describes the current working model and the small x86-64 Linux
sample under [`samples/executable/hello_x86_64_linux/`](../samples/executable/hello_x86_64_linux/).
It is an executable design exercise, not yet a stable assembler API.

## Effects and Handler State

The direct-assembly handler separates its public effect API from protected
handler state. A program receives operations such as instruction emission,
cursor allocation, and label capture. It cannot directly access the cursor
graph, emitted streams, symbol table, or identity allocators.

The current configuration implements only deterministic return, sequencing,
and task-local user state from the standard effects. Choice, fixpoints, and
delimited control remain future exercises.

## Preliminary API

The current API is supplied by `samples/config/direct_assembly.g`. A program
constructs ordinary freer-effect values using the operations below, then
`env.linux_x86_64.executable Program` interprets the program and returns the
ELF bytes. `env.x86_64` exposes the same public effect API for explicit
composition; handler implementation details are not exported.

The signatures in this section are descriptive Glam pseudotypes. `Cursor` and
`Label` are opaque values.

### Standard effects

```g
.r Value                         # -> Value
.seq Operation Continuation     # -> continuation result
.get Path                        # -> task-local user value
.set Path Value                  # -> ()
```

Layout `do` notation lowers through `.r` and `.seq`. Public `.get` and `.set`
are confined beneath the task's `user_state`; they cannot access cursor,
symbol, or encoder state.

### Regions and cursor selection

```g
.section.root Kind               # -> Cursor
.section.split Kind              # -> Cursor
.cursor.on Cursor Operation      # -> operation result
```

`.section.root Kind` allocates an independent root region and appends it to the
root layout order. It returns the new cursor without selecting it.

`.section.split Kind` requires a selected cursor. It inserts a new region
immediately after the selected region, gives the new region the selected
region's former continuation, and returns its cursor. The selected cursor
continues to receive subsequent writes. Repeated splits of one selected cursor
are valid.

`.cursor.on Cursor Operation` selects `Cursor` while interpreting `Operation`,
then restores the previously selected cursor. Its result is the result of
`Operation`. To split a retained cursor that is not currently selected, select
it with `.cursor.on` and perform `.section.split` inside that scope.

`Kind` is currently retained as metadata. The sample convention uses `'text`
and `'rodata`, but the bootstrap does not yet validate kinds or map them to
separate physical ELF sections.

### Labels and publication

```g
.cursor.label Cursor             # -> Label
.label                           # -> Label
.publish Name Label              # -> ()
.global Name                     # -> Label
```

`.cursor.label Cursor` captures a label at the explicit cursor's current
logical offset without selecting that cursor. `.label` does the same at the
selected cursor. Both immediately append an immutable zero-width label marker;
they never return an unbound label.

`.publish Name Label` associates a text name with an existing label. The same
label may be published under several names, but publishing one name twice is
an error. `.global Name` is the convenience form: it captures a label at the
selected cursor, publishes it, and returns it. The Linux wrapper currently
requires a published `"_start"` label and uses it for the ELF entry address.

### Current x86-64 writer operations

```g
.mov_u32 Register Immediate      # -> ()
.mov_label_u32 Register Label    # -> ()
.xor_u32 Destination Source      # -> ()
.bytes Binary                    # -> ()
.syscall                         # -> ()
```

These operations append to the selected cursor. The provisional register set
is `'eax`, `'ecx`, `'edx`, `'ebx`, `'esp`, `'ebp`, `'esi`, and `'edi`.
`mov_label_u32` resolves the label to an absolute address after logical layout.
The sample still emits one executable load segment, so `.bytes` can currently
place either code or data in any logical region.

### Associated trace metadata

This is one application of the general
[associated-metadata boundary](Design.md#associated-metadata): assembly
evaluation constructs the trace, but only reflection may inspect it.

```g
env.linux_x86_64.executable Program
env.linux_x86_64.executable_with_trace TracePolicy Program

env.linux_x86_64.trace.drop
env.linux_x86_64.trace.full
env.linux_x86_64.trace.summary
```

`executable` uses the `drop` policy and emits no trace diagnostic.
`executable_with_trace` carries one sealed metadata token through protected
handler state. Successful root-section allocation, region splitting, label
capture, symbol publication, and instruction emission derive a new token.
The program cannot inspect this token or the metadata associated with it.

After the handler has selected its final state, the policy's reflection
reporter receives that state's token. The bundled `full` policy inspects one
ordered event list and emits an informational diagnostic. The `summary` policy
retains only counters for roots, splits, labels, publications, and
instructions, then emits those counters. Both diagnostics contain compact
`msg.text` for the default logger and retain the inspected value under
`direct_assembly.trace` for a configured logger or IDE.

The policy is an ordinary object with two operations:

```g
{
  update:\Event PriorMetadata -> NextMetadata,
  report:\Carrier -> ReflectionTask
}
```

This is deliberately extensible. A project may replace the representation and
reporter without exposing metadata to the assembly program. `report` must
return unit. A dropping policy can ignore both inputs and return `.r ()`
without inspecting the carrier.

The trace records logical history carried by the final handler state, not
worker scheduling or evaluator demand order. The current handler has no choice
effects yet. Once it gains `.alt`, metadata derived in rejected state branches
must become unreachable, and only the carrier from the selected state is
reported.

### Example

```g
program = do
  .section.root 'text -> entry
  .cursor.on entry do
    .global "_start" -> _

    # The second split is placed before the first split.
    .section.split 'rodata -> message
    .section.split 'text -> exit
    .cursor.label message -> message_label

    # Construction order is independent of layout order.
    .cursor.on message (.bytes ("Hello!" ++ [10]))

    .mov_label_u32 'esi message_label
    .cursor.on exit do
      .mov_u32 'eax 60
      .xor_u32 'edi 'edi
      .syscall

asm.result = env.linux_x86_64.executable program
```

The resulting logical order is `entry`, `exit`, then `message`, even though
`message` is populated first.

## Logical Sections and Cursors

A physical executable section is not represented by one global write cursor.
Instead, a program may allocate any number of logical section fragments. Each
fragment has:

- an opaque cursor handle;
- a section class such as `text` or `rodata`;
- an append-only stream of instructions or data; and
- a continuation to the next logical region.

A cursor is therefore a retained write continuation. Selecting a cursor makes
subsequent writer effects append to that fragment. Leaving the selection
restores the prior cursor.

New continuations are created by splitting the selected region. The selected
cursor continues writing the first half; the operation returns a cursor for a
new second half. The new region inherits the selected region's previous
continuation, so it is inserted immediately after the selected region rather
than appended to the end of an existing chain.

Splitting one cursor repeatedly is therefore well-defined. If `message` is
split off first and `exit` second, the final order is:

```text
selected region -> exit -> message -> previous continuation
```

This relation covers completed logical fragments, not their lengths at the
time of the split. A program can retain several cursors, populate them in any
convenient evaluation order, and still establish a different final byte order.

For example, a conditional branch can retain one cursor for its fallthrough
path and another for its taken path. Either path may be constructed first.
Their immutable layout links determine the eventual byte order.

The bootstrap handler supports root regions, appended to the root layout order,
and repeated splits of the selected region. Because each split allocates a
fresh region and only inserts it into an existing continuation, this operation
cannot introduce a layout cycle. Richer constraints, alignment,
physical-section grouping, and a general layout solver remain future work.

## Labels

An internal label is never allocated in an unbound state and later defined.
Instead, a program asks a cursor for a label at its current logical offset.
That operation:

1. allocates a globally unique label identity within the assembly;
2. immediately inserts an immutable boundary marker at the cursor's tail; and
3. returns an opaque handle to that marker.

Later appends do not move the marker. Multiple label requests at one boundary
produce distinct labels for the same logical position. Resolution to a byte
offset or address is deferred until fragments have been laid out and encoded.

This construction prevents internal labels from being undefined, multiply
defined, or rebound. Forward references are expressed structurally: allocate
the destination cursor, capture its initial label, emit a reference to that
label elsewhere, and populate the destination fragment whenever convenient.

## Published Symbols

Published symbols are separate from labels:

```text
label identity -> logical cursor boundary
symbol name    -> label identity plus linkage metadata
```

Publishing an existing label supports aliases and future linkage or visibility
options. A convenience `global` operation may capture the current cursor
boundary and publish it in one step. A published name such as `_start` is not
the identity or location of the label itself.

External symbol imports are also distinct from local labels. Their resolution
will belong to a later linking layer.

## Bootstrap Representation

The sample handler currently uses tagged, monotonically allocated numeric
identities for cursor and label handles. Programs must treat these values as
opaque. Their representation, numbering, and allocation order are not part of
direct-assembly semantics.

The handler records label markers in logical streams. After cursor layout is
flattened, encoding scans those markers to resolve label references and
published entry points. This is intentionally simple and favors inspectability
over performance.

The active tooling investigations exposed by this model are consolidated in
[`docs/.tmp/AuthoringTools.md`](.tmp/AuthoringTools.md).

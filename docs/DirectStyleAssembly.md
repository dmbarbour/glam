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

## Logical Sections and Cursors

A physical executable section is not represented by one global write cursor.
Instead, a program may allocate any number of logical section fragments. Each
fragment has:

- an opaque cursor handle;
- a section class such as `text` or `rodata`;
- an append-only stream of instructions or data;
- one layout relation selected when the cursor is created.

A cursor is therefore a retained write continuation. Selecting a cursor makes
subsequent writer effects append to that fragment. Leaving the selection
restores the prior cursor.

The common layout relation is `after`: the new fragment linearly follows the
completed contents of another cursor. This relates whole logical fragments,
not their lengths at the moment the relation is declared. A program can
therefore retain several cursors, populate them in any convenient evaluation
order, and still establish a different final layout order.

For example, a conditional branch can retain one cursor for its fallthrough
path and another for its taken path. Either path may be constructed first.
Their immutable layout links determine the eventual byte order.

The bootstrap handler currently supports:

- a root cursor, appended to the root layout order; and
- a cursor that linearly follows another cursor.

The intended model rejects contradictory links and layout cycles. Richer
constraints, alignment, physical-section grouping, and a general layout solver
remain future work.

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

## Missing Authoring Tools

Writing the executable sample has exposed several useful tooling targets:

- A handler-level structured failure operation for invalid cursor handles,
  contradictory layout links, duplicate symbol publication, and unresolved
  references. The sample currently piggybacks on `assert_unit`, which preserves
  its context but adds an irrelevant “unit expected” suffix.
- An opt-in effect trace showing public operation dispatch and summarized
  protected-state changes without exposing the state to the program.
- A cursor-layout view showing fragment classes, links, stream lengths, label
  boundaries, and published symbols before encoding.
- A reflection view for object specifications and linearization that does not
  force unrelated definitions.
- `anno context:Context Expr` or an equivalent pure-evaluation diagnostic
  context.
- Lazy-dependency diagnostics that identify the effect and protected state
  path involved. Storing a shared lazy payload in handler state and observing
  another property of it later initially surfaced only as a host stack
  overflow; the sample currently makes the intended demand explicit with
  `seq`.
- Better object-parent diagnostics that report the selected expression, value
  kind, and undefined values.
- Parser recovery for an empty-dictionary `match` pattern inside a layout
  object body. The attempted handler validation was initially misreported as
  “`with` must end an object declaration header”; an equivalent `if` works.
- A referential-equality assertion before implementing shared singleton
  sections and content-addressed constants.
- A reusable assembly contract harness that can inspect logical fragments and
  resolved labels without first wrapping them in an ELF file.
- An encoder/disassembler comparison view for generated machine code.

These are deliberately tracked alongside the sample: exercising the language
as an assembly author is expected to guide the diagnostic and reflection APIs.

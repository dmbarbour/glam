# Obligations — 2026-10-25

A design note for `api-refl-obligations` in the
[runtime API plan](RuntimeApi_2026-10-17.md), the second step of the
[Quiescence](Quiescence_2026-10-25.md) transition: accepted promises, as
`obligations-are-accepted-promises` decides, which the analysis reports as
pending obligations. Drafted 2026-10-25 for the maintainer's review before
building.

## The model

From the decision (maintainer, 2026-10-21) and its neighbours:

- `.promise.accept Resolver Diagnostic` adds the promise to the machine's
  accepted set, with a diagnostic saying what is owed, and commits with
  the cut. Several machines may accept one promise.
- Resolving the promise, by anyone, discharges it for every acceptor.
  `.promise.abandon Resolver` removes it without breaking it.
  `.promise.accepted Resolver` reads whether any machine accepted it, a
  tracked read within a cut.
- Acceptance holds the resolver, so an accepted promise is never lost. A
  machine in error keeps what it accepted, for recovery; clearing a
  machine abandons it all. A child accepts its queued tasks' promises.
- Turns get `m.accept_promise`, `m.abandon_promise` and
  `m.promise_is_accepted`.
- At quiescence, an accepted promise still pending is unresolved, with its
  diagnostic and its acceptor's state. A forgotten promise is not a
  forgotten obligation: the status lives with the resolver's breaker,
  whether or not anyone can still await the promise
  (`promise-cells-are-weakly-resolved`).

## Today (survey, 2026-10-25)

- **Promises.** The store holds each pending promise's cell weakly by its
  ID; a resolver is an edge to a breaker that keeps the status. A
  resolution is staged in a transaction's store journal and settles with
  its commit, under the runtime's transaction lock, the same lock under
  which the event state commits.
- **Machine state is in two places.** Ports, shares, reflection children's
  flags and states live in the transactional event state, by machine
  path, for turns and tasks alike. A Rust machine's stack, local state and
  error flag live in the turn runtime's tree, which the analysis cannot
  see.
- **Nothing accepts.** No accepted set exists, and `.todo.*` never shipped.

## Proposed design

### An accepted record in the event state

Acceptance is machine state that both drivers write and the analysis
reads, so it lives in the event state beside the flags: a record keyed by
promise ID, each entry the resolver and its acceptors, each acceptor a
machine path with its diagnostic. A journal stages acceptances and
abandonments and records the reads of `.promise.accepted`, versioned per
promise, for validation.

- **Discharge.** A commit that resolves a promise drops its entry in the
  same commit, under the same lock: the store journal names what it
  settles. Nothing else keeps a resolved promise's entry.
- **Clearing** a machine drops it from every entry, and an entry with no
  acceptor left goes; a flag keeps them, for recovery.
- **Reclamation** keeps a machine that accepted a pending promise, as it
  keeps a flagged one, so an anonymous root's task that ends owing is
  reported rather than reclaimed (see question 2).
- **The machine** that accepts is the current branch's, as `.child.*`
  reads it: within a call, the callee; for a task with no machine, its
  anonymous root. For a turn, its machine's path.

### The effects and the turn operations

- `.promise.accept Resolver Diagnostic`, `.promise.abandon Resolver`, and
  `.promise.accepted Resolver`, an observed read, answering `.r ()` or
  `.fail` as `dict.defines` does. Within a cut they stage with it; outside
  one, each commits at once, as `.child.*` edits do.
- `m.accept_promise`, `m.abandon_promise`, `m.promise_is_accepted` on a
  resolver from the stack, through the cut's event journal or a
  transaction of their own.
- A queued task's reply is not accepted implicitly (maintainer,
  2026-10-25; see the decisions below). It counts as a wait: the waiting
  task is listed with its own family, and an evaluation awaiting the reply
  is a promise wait.

### The analysis

`RuntimeAnalysis` lists each pending accepted promise, with its acceptors
and their diagnostics, as a new issue family, `PendingObligation`, by
default unresolved.

### Slices

1. `obligations-record` — the record and journal, discharge at
   resolution, clearing and reclamation, the effects, and the analysis's
   family. Tests: accept, abandon and the tracked read; discharge by
   another's resolution; several acceptors; a cut's failure undoing an
   acceptance; clearing abandons, a flag keeps; an ended root owing is
   kept and reported.
   Built 2026-10-25: `AcceptedRecord` and `AcceptedJournal`
   (`api/runtime/accepted.rs`) in the event state, each entry holding the
   root on its resolver's breaker; `.promise.accept`, `.promise.abandon`
   and `.promise.accepted` as reflection requests on the branch's machine,
   edits staged within a cut and committed at once outside one, through
   the child edits' path. Both commit paths, the combined one and the
   store's own, discharge what their store journal resolves, under the
   transaction lock. An acceptance whose promise another resolved before
   it committed owes nothing, since the acceptance reads nothing that the
   resolution would conflict with: its commit skips it. Reclamation keeps
   what is not clear, a machine in error or one that owes. The analysis
   lists `PendingObligation`s, with each acceptor's path and diagnostic, a
   diagnostic being a record such as `{msg:{text:"..."}}`. A test-only
   `.TEST_ONLY.accept_now` accepts at once within a cut, as another server
   would, to force the tracked read's conflict. Mutation-checked: the
   skip of a resolved promise, discharge on both paths, abandonment, the
   journal's view, the read's validation, several acceptors, clearing,
   reclamation and the read's answer.
   Left open: a machine kept for owing stays after its obligation is
   discharged, until a host clears it, since reclamation runs again only
   as its root or a run ends; `quiescence-options` takes it up.
2. `obligations-turns` — the turn operations.
   Built 2026-10-25: `m.accept_promise` (a diagnostic over a resolver),
   `m.abandon_promise` and `m.promise_is_accepted` (the resolver stays),
   for the turn's machine, through the cut's event journal or a
   transaction of their own, the read observed within a cut; `m.clear`
   abandons what the cleared machines accepted. Test: accept, read,
   abandon, a failed atomic transaction undoing an abandonment that it
   read, a child's clear, and a resolution's discharge; each operation
   and the cut's read mutation-checked.

A slice for children accepting their tasks' replies was dropped by
decision 1.

## Decisions (maintainer, 2026-10-25)

1. **No implicit acceptance.** A child accepting an obligation just for
   being tasked is implicit, and would count every waiting task; a task's
   pending reply should count differently. So acceptance is explicit only,
   and a reply counts through the waits. This amends
   `obligations-are-accepted-promises`, which had a child accept its
   queued tasks' promises.
2. **What is not clear stays visible.** A machine that owes, as one in
   error, keeps its machine through reclamation, for the analysis to
   report and a host to clear.

## Questions for the maintainer, as drafted

1. **A waiting task owes its reply.** A child accepts its queued tasks'
   replies, and a forgotten promise is not a forgotten obligation, so a
   child task left waiting at quiescence, on a heap value or on input,
   leaves its reply pending: an unresolved obligation, failing a batch by
   default, whether or not anyone holds the reply. Meanwhile an
   annotation task left waiting owes nothing, having no reply. Is that
   the intent? The alternatives: a task's reply counts only while its
   promise can still be awaited, which would amend the forgotten-promise
   clause for replies; or children do not accept replies, leaving it to
   code that wants the obligation. My lean: the decision as it stands,
   since a queued task is work its parent asked for, with a client mask
   for batches that leave servers waiting.
2. **Owing keeps a machine.** An anonymous root's task that ends with a
   pending acceptance keeps its machine, as a flag does, for the analysis
   to report and a host to clear. My lean: yes.

The slices go in the order above: the turn operations come last, as turns
are not yet in the analysis's counting either.

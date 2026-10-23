# Pure Handlers — 2026-10-23

A sketch, for the maintainer to settle before any build, of specialized pure
handlers to replace the three effect specializations that run under the
isolated, all-results search: the CLI's argument parser (`CliEffects`), its
token parser (`TokenEffects`), and source macros (`MacroEffects`). Asked for
by the maintainer (2026-10-23), who chose pure handlers over an all-results
`effect_run` (option (a) of the `api-effect-providers` discussion), expecting
a specialized handler per use case, as `list.pure` is one.

## What the three are

All three search a fixed input for every parse, keep per-branch evidence,
and read failed branches as well as successful ones. Facts from a survey of
the code, 2026-10-23:

| | Token parser | Argument parser | Macros |
|---|---|---|---|
| Input | one argument's text, a completion offset | the arguments, a completion point | macro input elements and layouts, an environment |
| Per branch | cursor, expectations, candidates | cursor, edits, expectations, candidates, case stacks, diagnostics | structural cursor, output, case stack, diagnostics |
| Operations | `.token.text`, `.regex`, `.text_span`, `.any`, `.end` | `.read.keyword`, `.text`, `.token`, `.path`, `.end`; `.write.*`; `.case`; `.env`, `.log` | `.read.text`, `.regex`, `.text_span`, `.data`, `.sep`, `.layout`, `.anchor`, `.end`; `.write.*`; `.case`; `.env`, `.log` |
| Host | none | `.read.path` probes the filesystem, per branch; `.env` reads the process | none; `.env` reads the input's environment |
| Results read | values of complete branches; evidence pooled at the furthest offset | one plan among the successes, or ambiguity; the furthest failure's labels and cases; completion at the furthest frontier, verified by running again | exactly one success, else the furthest failure's position and cases |
| Rust | about 495 lines | about 1,700, plus shared completion and plan types | about 1,730 |

Scoped operations, `.case`, `.read.layout` and `.write.layout`, run an
operation and then a closing request that only success reaches, so a failed
branch keeps its case stack as evidence. Within a user's `.cut`, the
isolated search keeps only the last failed alternative's evidence.

`list.pure` is the precedent: a builtin whose API is a fixed dict of
builtins, which reduces within the evaluator, under its budget and lazily,
to an ordered list of results. It has no effect machine, journal, host or
search object: about 700 lines in all.

## The proposal

1. **One core, a table of operations per handler.**
   - `pure-handler-core`: a reducer like `list.pure`'s that threads a
     handler state through each branch and keeps every branch. It reduces
     `.r`, `.seq`, `.alt`, `.fail`, `.cut` and `.fix` as `list.pure` does,
     plus `.get` and `.set` on the user's own state, kept apart from the
     handler's (maintainer, 2026-10-23: a provider's private state stays
     apart from the user's).
   - A handler's operations are Rust functions of the handler's state and
     their arguments, each answering with results and the states to
     continue from, or a failure with the state to keep as evidence. The
     API is the core's builtins plus the operations, fixed per handler.
   - Scoped operations are compositions, not machinery: `.case e p` pushes
     `e`, runs `p`, and pops on success, so a failure keeps the push.
2. **Every branch is a result, in two lists.** A run reduces to two lazy
   lists, each in search order, from one search: successes, each a value
   with its states, and failures, each with its states as evidence. Order
   across the two is not needed, so a consumer of successes never touches a
   failure (maintainer, 2026-10-23); the search still runs a failed path to
   learn it fails, and keeps it for the other list. The Rust callers read
   them as they read branches now: ambiguity and the unique success from
   the first, the furthest frontier from the second.
3. **The input is the handler's argument,** so a handler is pure: the token
   parser over a text and a completion offset; the argument parser over the
   arguments and their host facts; macros over their input and environment.
4. **File metadata is a pseudo-import** (maintainer, 2026-10-23). Nothing
   is gathered ahead: the input carries, for each argument, a lazy value
   that, once forced, requests the argument's metadata as an import
   requests its source (`imports-are-source-requests`), and awaits the
   host's reply: what the argument names, a file, a folder or nothing (with
   its parent then), and whether it could be read or written. In
   completion, one more such value lists the folder being completed. As an
   import's binary is, the reply is data, fixed within the assembly step,
   so the parser stays a pure function of its input. A branch's
   `.read.path` forces only its argument's facts, and the run keeps them,
   so the least is done: one request for each argument some branch reads
   as a path, where today each branch probes for itself, and a run made to
   verify a completion can share the replies. The host answers from
   metadata, rather than opening a file for writing to test it, as now.
5. **The order of building.**
   - `pure-handler-core`, with `list.pure`'s tests as its starting cases.
   - `pure-handler-token`, the smallest and free of the host, run by the
     argument parser's `.read.token`.
   - `pure-handler-cli`, with the host facts.
   - `pure-handler-macros`, the largest, run within compilation's macro
     session as it is now, without its own search object.
   - `pure-handler-retire-search`: the isolated search and
     `IsolatedTaskHost` leave the public API (`effect-families-pass-through`).

## Answers (maintainer, 2026-10-23)

1. Generalize to a shared core where it fits; where that proves awkward,
   separate handlers.
2. File metadata is a pseudo-import: lazy input the host serves as it
   serves imports.
3. Within a user's `.cut`, aggregate the evidence of the failures observed
   before its first success, for diagnostics, and keep that success.
4. Aggregation may live in Rust or in glam: the hard part is interpreting
   an arbitrary user program, done by then.
5. The core does not reduce `.reset` and `.shift`.

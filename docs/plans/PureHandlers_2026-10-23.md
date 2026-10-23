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
2. **Every branch is a result.** A run reduces to a lazy list of outcomes in
   search order: `ok` with a value and its states, or `failed` with its
   states. The Rust callers read that list as they read branches now: the
   furthest frontier, ambiguity, the unique success.
3. **The input is the handler's argument,** so a handler is pure: the token
   parser over a text and a completion offset; the argument parser over the
   arguments and their host facts; macros over their input and environment.
4. **Host facts are gathered before the parse.** For each argument, the CLI
   stats it once, to see whether it names a file or a folder, and whether
   it could be read or written, from metadata alone. In completion, it also
   lists the folder being completed. The facts go in as input, so a branch's
   `.read.path` is pure, and a run made to verify a completion probes
   nothing new. Today a branch probes for itself, and checking write access
   opens the file for writing.
5. **The order of building.**
   - `pure-handler-core`, with `list.pure`'s tests as its starting cases.
   - `pure-handler-token`, the smallest and free of the host, run by the
     argument parser's `.read.token`.
   - `pure-handler-cli`, with the host facts.
   - `pure-handler-macros`, the largest, run within compilation's macro
     session as it is now, without its own search object.
   - `pure-handler-retire-search`: the isolated search and
     `IsolatedTaskHost` leave the public API (`effect-families-pass-through`).

## Open questions

1. A shared core with a Rust table of operations per handler, as proposed;
   or glam libraries over a generic stateful runner, which would make the
   CLI's and macros' operations glam code at some cost per operation; or
   handlers fully separate, as `list.pure` is.
2. Host facts gathered beforehand, from metadata alone, as proposed; or
   `.read.path` kept as a request to a provider, so the argument parser is
   not pure.
3. Within a user's `.cut`, keep the evidence of every failed alternative,
   for better diagnostics, or only the last, as now.
4. Where aggregation lives: in Rust, reading the outcomes as now, or in
   glam, with the handlers' results.
5. Whether the core reduces `.reset` and `.shift`, which none of the three
   parsers' tests and samples use.

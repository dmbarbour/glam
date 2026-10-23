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
   - `pure-handler-control`: the core reduces `.fix`, `.reset` and
     `.shift`, which macros promise.
   - Macros, the largest, run within compilation's macro session as now,
     without a search object of their own, before the CLI, which waits on
     `api-cli-sources` (maintainer, 2026-10-23):
     - `pure-handler-macro-ops`: an opaque context holds the input's
       structure, its text runs, delimiters, separation and layouts; the
       handler's state holds the embedded data and the environment as
       plain values, and per branch the reader's position, the output,
       the active cases and the logs. The readers, writers, `.env` and
       `.log` are private builtins. `.case`, `.read.layout` and
       `.write.layout` are compositions over `.seq` of hidden operations
       that open and close them, so a failure within keeps what was
       opened as evidence.
     - `pure-handler-macro-run`: the runner reads the successes, at most
       two, and otherwise the failures' furthest frontier and cases, then
       checks and splices as now; `MacroEffects` and `MacroHost` retire.
     - `pure-handler-macro-resume`: a macro's run, a lazy value, stays in
       the compile's replay cache, so a compile replayed after a wait on
       an import resumes the macro where it waited rather than running it
       again; only while that stays small.
     - Imports stay evaluation dependencies, not macro effects: a macro is
       a pure function of its input and environment. Were macros ever to
       make the front end's requests, the shape is a pass-through to the
       compiler's handler, with `.with` or `compile-resumable`.
   - `pure-handler-cli`, with the host facts.
   - `pure-handler-retire-search`: the isolated search and
     `IsolatedTaskHost` leave the public API (`effect-families-pass-through`).

## Built

- `pure-handler-core` (2026-10-23), in `list.pure`'s family of lazy
  computations, so it shares its reducer, checkpoints and tracing.
  - `Handler ops user handler effect` runs `effect` from the state `{user,
    handler}` against an API, the core's requests united with `ops`, which
    the run holds: fixed for the run, it need not travel in the state
    (maintainer, 2026-10-23). Each request, the core's or a handler's, is a
    function of the state, as `list.pure`'s are tagless, answering with an
    ordered list of outcomes: `{value, state}` or `{failed: state}`. The run
    answers `.seq`, `.alt` and `.cut` itself, since they run effects of
    their own against its API.
  - A request answers with outcomes, never with an effect to run in its
    place: a composition of operations is an ordinary effect, built where
    it is used (maintainer, 2026-10-23).
  - The core reduces `.r`, `.seq`, `.alt` (every branch, in order),
    `.fail`, `.cut` (its first success and the failures before it), and
    `.get`/`.set` on `user`, the empty path naming all of it. It did not
    reduce `.fix`, `.reset` or `.shift` until `pure-handler-control`.
  - The outcomes stay ordered within a run, since a cut keeps only the
    failures before its first success. A run's results are two lazy views
    of them: `ok`, each `{value, user, handler}`, and `failed`, each `{user,
    handler}`. A view passes over the other kind without a list cell of its
    own, and reads only as far as it is read, so `ok`'s first success
    forces no later branch.
  - An operation receives the whole state, `{user, handler}`, which it
    returns with its `handler` changed, rather than the handler's state
    alone, as the sketch had it: one convention for every request.
  - Bound as `list.search` only in tests until `pure-handler-token` gives it
    a caller; its name, if glam is to have it, is the maintainer's.

- `pure-handler-token` (2026-10-23): the CLI's token parser runs by the
  pure handler, from the library, `run_token_parser`, which the CLI's
  `.read.token` calls in place of a nested isolated search; the CLI's
  token specialization, about 450 lines, is gone.
  - Its operations, `.token.text`, `.regex`, `.text_span`, `.any` and
    `.end`, are private builtins, each a function of its argument, if any,
    and the state (maintainer, 2026-10-23: private builtins for now, to be
    eliminated gradually as performance and accelerators allow).
  - The handler's state holds an opaque context made once a parse, the
    token's text and the offset being completed, and per branch plain
    data: the cursor and the evidence, expectations and candidates, each
    `{offset, label}` or `{offset, replacement}`.
  - The run reads the successes' values and every branch's evidence, as
    the isolated search's branches were read.
  - Within a user's `.cut`, a token parse now keeps the evidence of the
    failures before its first success, where the isolated search kept the
    last failure's alone. No test depended on the difference.
  - `list.search` stays bound in tests only.

- `pure-handler-control` (2026-10-23): the core reduces every standard
  effect, `.fix`, `.reset` and `.shift` too, so the token parser, macros
  and any later handler have them (maintainer, 2026-10-23; `.with` joins
  when it becomes a standard). The private interaction-net builder was the
  precedent: a pure handler in the same family with all three.
  - The run carries what it returns to: the continuation up to its
    innermost `.reset`, its frames innermost first. `.seq` pushes onto it
    in place, and `.r` returns to it. Before, the continuation lived only
    in nested lazy lists, where `.reset` could not take it.
  - A `.reset` pushes that continuation, with its key, onto a stack of
    frames in the user's state, under a key no source can name, as
    Design.md's reference handler and the effect machine keep it
    (`effect-families-pass-through`): `.get []` captures the stack and
    `.set []` restores it, and a state without it has no reset in scope.
    The key leaves the state when the stack empties, so a reset whose
    operation returns leaves the state as it found it. A run with no reset
    never reads the user's state to return.
  - A `.shift` takes the continuation and the frames above its reset as a
    continuation value; resuming it pushes them back above a frame for the
    caller, whose continuation the resumed one returns to.
  - A cut marks the continuation it runs within, and the first outcome to
    return to the mark is its success: the cut's walk passes the outcomes
    before it, failures and others, then continues that one. A `.shift`
    that leaves cuts within its reset sends its outcomes past their walks,
    so its branch is no longer theirs, and resuming a continuation that
    holds cut marks walks them anew.
  - `.fix` reuses `list.pure`'s fixpoint, one future per alternative, and
    hides the frames beneath it behind a barrier frame, so a `.shift`
    within it reaches only resets within it (Design.md: "`fn` receives
    result as input but hides `.reset` scope").
  - Not settled: a frame restored by `.set []` after the cut it lies within
    has ended returns through a mark no walk awaits; the run fails with a
    diagnostic rather than guess. The effect machine orders its delimiters
    to decide this; the pure handler has no such order yet.

## Answers (maintainer, 2026-10-23)

1. Generalize to a shared core where it fits; where that proves awkward,
   separate handlers.
2. File metadata is a pseudo-import: lazy input the host serves as it
   serves imports.
3. Within a user's `.cut`, aggregate the evidence of the failures observed
   before its first success, for diagnostics, and keep that success.
4. Aggregation may live in Rust or in glam: the hard part is interpreting
   an arbitrary user program, done by then.
5. The core reduces every standard effect, `.reset`, `.shift` and `.fix`
   included, for every handler; `.with` joins when it becomes a standard
   (revised the same day; the first answer left delimited control out).
6. Macros keep their run across a compile's replays only if that stays
   small; if it interacts badly with lazy fixpoints shifting under the
   compile's replay, it waits for `compile-resumable`.

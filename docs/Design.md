# Glam Design

Rough approach:

- design language for assembling binaries in general
- libraries for CPU targets, executable formats, etc.
- syntactic sugars similar to conventional assembly

This document is both detailed overview and brainstorming.

## Desiderata

Regarding binary specification, relevant system-level goals include:

- reproducibility: easily share binary by sharing code
- extensibility: easy variants without invasive edits
- adaptability: outcome depends on a system definition
- modularity: share definitions, incremental computing

These goals entangle: System definition must also be reproducible, extensible, and modular. A system definition can provide modules to the binary specification for adaptability. Transitive dependencies must be reproducible. Incremental computing should share work (local verification, partial evaluation, etc.) across similar but non-identical system definitions.

Regarding the programmer experience:

- a simple and locally comprehensible semantics
- looks and feels like well-documented assembly code
- automated verification of assumptions and reasoning
- easily visualize assembly, interactive debug views
- easy live coding, continuous feedback during change 
- flexible metaprogramming with macros and DSLs

These present significant design challenges. I'll generally prioritize system-level features over experiential properties, but some difficult design decisions are required.

## Overview of Semantics

Toplevel semantics is a lazy, untyped lambda calculus. For performance, users
may implement computations and functions behind explicit lambda-style
interfaces using [interaction nets](https://en.wikipedia.org/wiki/Interaction_nets).
A few data types - lists, numbers, dicts - have built-in representations and
operators.

We'll model object-oriented programming in terms of [open fixpoints](http://fare.tunes.org/files/cs/poof.pdf), and monadic effects in terms of a [freer monad](https://okmij.org/ftp/Haskell/extensible/more.pdf). The toplevel namespace is modeled in an object-oriented style, thus supports overrides as a foundation for extensibility.

Aside from behavioral semantics, we introduce a notion of *annotations* as an 'active' comment. Annotations shall not observably influence evaluation, but may block evaluation: divergence is not observable. Annotations support performance, debugging, reasoning, editing, and other tooling.

## Performance

This section describes our approach to assembly-time performance: speed and scalability of builds and tests. Performance of assembled machine code is left entirely to the user, as is the nature of an assembler.

* *Laziness* - Default evaluation strategy is laziness, i.e. evaluate only what contributes. Users can express a huge namespace but we'll download and extrat only what is needed. Annotations may tune the strategy, e.g. to sequence or spark (parallelize) evaluation.

* *Caching*: annotations ask assembler to memorize some expensive computations. When encountered again, substitute result instead of replaying full computations. Persistent caching provides our basis for incremental assembly. A remote proxy cache (with a little PKI) can potentially share work within a community.

* *Parallelism* - Interaction nets naturally express fine-grained parallel dataflows. Users may also use annotations to 'spark' computations in anticipation of future need. But SIMD, GPGPU, etc. parallelism depend on *Acceleration*.

* *Acceleration*: annotations ask assembler to substitute a performance-critical reference function with a built-in. For example, a function representing an abstract CPU, TPU, or GPGPU emulator can be substituted by a built-in that compiles code for actual hardware. Acceleration is high-risk, high-reward. Risks are correctness, maintenance, portability, bloat. Rewards are extending assembly or effective testing (e.g. hardware emulation) to new problem domains.

* *Ephemerons*: use annotations to mark atoms as scope-unique. Equality checks diverge upon observing identical atoms with different marks. When used as dict keys, we can use a weakref, enabling garbage collection of a dict modeling a 'heap'.

## Data

The built-in data types are numbers, lists, dicts, and functions. All data is immutable, i.e. you cannot update a list or dict, but you can construct a new list or dict in terms of updating an existing one.

- Numbers are exact rationals. Any loss from rounding numbers is under explicit control of the user.  
- Lists are a one-size-fits-all sequential data structure. Concretely represented by finger-tree ropes under the hood, supporting append at either, compact binaries, array-like flattening. 
- Dicts are finite key-value associative structures. Keys must be equatable, i.e. excluding functions. There is no iteration over keys. Tagged data is modeled as a singleton dict.
- Functions are frequently expressed as lambdas, but are closed-term inets under-the-hood.

The `interaction_net` escape hatch additionally produces opaque net values.
They are not functions or ordinarily applicable data; the Interaction Nets
section defines their composition and explicit `net_arity` bridge.

All basic data types are implicitly tagged, i.e. users can distinguish a function from a number in context of pattern matching. Users can further tag data to support a dynamic type feel.

## Objects

Objects are most useful for extension in context of mutually recursive structures. For example, a grammar can be modeled as an object where each 'rule' is a parser combinator, enabling override of specific rules. We'll model objects at the module layer to support extension of the namespace.

Pure functions can model stateless objects in terms of open recursion via latent fixpoint. A basic object model with mixin composition is `Dict -> Dict -> Dict` in roles `Base -> Self -> Instance`. Here, 'Base' is a parent object, initially empty, and 'Self' is a future fixpoint.

        mix child parent = λbase. λself.
           let base' = parent base self in
           child base' self
        fix f = -- lazy built-in fixpoint
            let x = f x in x        
        new obj = fix (obj Dict.empty) 

It's best to design a syntax for constructing objects that avoids observing 'base' or 'self' prior to instantiation. Otherwise, there's a good chance of datalock on 'fix'.

### Singleton Instance

Instead of a `Dict -> Dict -> Dict` structure, we can model objects as a `Dict` that defines `spec` and other fields. The `spec` would include the mixin and perhaps some metadata - a unique ID and inheritance list - to support multiple inheritance. Users can access the instantiated version of the object and still support extension and override via `spec`.

With singleton instance, we might use annotations to 'freeze' the Dict to prevent accidental update, because updates should use the extension mechanism instead.

### Multiple Inheritance

We can model multiple inheritance, where an object inherits from several others that may share ancestors. We apply a linearization algorithm (such as C3) to ensure each shared ancestor is mixed-in only once and in a consistent order.

Linearization requires an identifier to distinguish when two specifications are the same. In practice, we can use arbitrary values in this role, e.g. "A" and "B", but also assert (with reflection and referential equality) that no identifier is shared by two different specifications in scope of linearization.

The metadata required for multiple inheritance is easily included in object `spec` from singleton instance. 

*Note:* I do not recommend use of spec names for 'is-a' purposes. The alignment between inheritance and types is weak.

### Explicit Override

To resist ambiguity conflicts, it is useful to distinguish introducing and overriding a name. We can report an error if we override a name that is undefined or if we introduce a name that is already defined. The implicit assumption is that a name is introduced with some meaning or purpose, while overrides presumably preserve meaning and purpose. Syntactically, this distinction may be lightweight, e.g. '=' vs. ':='.

### Structured Namespaces

A flat object namespace easily grows cluttered. It is not difficult to organize names hierarchically. For example, we can easily apply a mixin at an index. 

        apply_at idx mixin = λbase.λself. 
            base with { 
                (idx) = mixin base.(idx) self.(idx) 
            }

More sophisticated translations are possible, e.g. translating individual names. However, it is awkward to extend translations like this to multiple inheritance. It is feasible to develop a few specialized variants, assuming adequate developer control over the 'spec' interface.

### Stateful Specification

Mixins can model state-like updates, treating Base as a previous state, Instance as next state, and Self as final state.

        λbase. λself. Base with { 
            v = base.next, 
            next = 1 + base.next 
        }

This can provide a foundation for allocating unique identifiers or building tables. However, this pattern easily interferes with lazy loading of modules if we aren't careful about integration. Ideally, a front-end compiler would provide dedicated syntax for safe patterns.

### Stateful Objects

We can model conventional heap-based objects. Objects include an allocated reference into a shared 'heap'. Methods are modeled effectfully, with implicit access to the heap. Objects may be freely shared in scope of the heap. The heap may simply be a dict value with a 'next' allocator. Thus, we can also model an 'image': a collection of 'public' objects paired with a heap. We can copy or fork images. 

I don't have a strong use case for stateful objects in this project, but they're easy to model.

### Method Objects

        {apply:f,_} x = f x

With a little support from assembler or front-end compiler, users can transparently apply dicts or objects as functions. The object form supports extensible functions, i.e. with overrides influencing 'apply' behavior. For example, users could expose tuning parameters or even deep implementation details.

It is convenient to organize mutually-interdependent methods into a host object. This is a clear use case for hierarchical objects.

Abstract method objects require clients to introduce some definitions via override. This pattern is useful for static integration, analogous to boxes-and-wires programming styles. The 'wires' are definitions representing continuations, callbacks, channels, etc.. The 'boxes' become hierarchical objects that inherit from abstract methods. The resulting effect may *also* be an abstract 'box', with some 'wires' left undefined. This enables a robust form of program composition.

Multimethods are also a use case for method objects. A multimethod inherits from a generic template that defines heuristic dispatch functions. Clients extend the multimethod, adding dispatch cases to a table with suitable metadata. Those cases may be abstract or tunable method objects to support a final integration with the multimethod.

## Effects

Even without a runtime, effects are convenient for implicit dataflow, backtracking and error handling, flexible composition, extensible behavior, etc.. I'll use Haskell(-ish) syntax to describe these, but it should be easy to translate.

        type Eff rq a =
            | Yield (rq x) (x -> Eff rq a)
            | Return a

We can model a free-er effects monads as either *yielding* a request, continuation pair or *returning* a final result. In case of yield, the expected response type depends on the request.

We can easily introduce some syntactic sugar:

        (sugar)         (monadic ops)
        a <- op1        op1 >>= \ a ->
        op2             op2 >>= \ () ->
        op3 a           op3 a

There are no typeclasses. We can specialize the monadic operators for our only monad. 

        (Yield rq k1) >>= k2 = Yield rq (k1 >=> k2)
        (Return a) >>= k = k a
        k1 >=> k2 = (>>= k2) . k1

Effectively, '>>=' captures the continuation into 'Yield'. A relevant concern is left-associative structures such as `((((k1 >=> k2) >=> k3) >=> k4) >=> k5)` would tend to rebuild the 'stack' on every step. Right-associative `(k1 >=> (k2 >=> (k3 >=> (k4 >=> k5))))` performance is superior. Ideally, this is resolved at evaluation time, which benefits from a stack-like representation of the continuation (instead of immediate composition of functions).

Behavior is embodied in the runner or handler. Almost any effect can be modeled, the primary exception being race conditions. Not all effects are fully 'compatible'. A few examples:

        -- a reader monad passes an implicit environment
        runEnv e (Yield Env k) = runEnv e (k e)
        runEnv e (Return r) = r

        -- a state monad supports update of a state var
        -- returns final value of state
        runState s (Yield Get k) = runState s (k s)
        runState _ (Yield (Put s) k) = runState s (k ())
        runState s (Return r) = (r,s)

        -- a list monad models lazy, ordered, non-deterministic choice
        runChoice (Yield (Choice xs) k) = List.flatMap (runChoice . k) xs
        runChoice (Return r) = List.singleton r

        -- delimited continuations
        runCont (Yield (Shift fn) k) = runCont (fn k)
        runCont (Yield (Reset op) k) = runCont op >>= runCont . k
        runCont (Return r) = r

It feasible to compose some effects via stack of 'monad transformers'. We can implicitly pass unrecognized effects up the stack.

        -- environment transformer with implicit lift
        runEnvT e (Yield Env k) = runEnvT e (k e)
        runEnvT e (Yield rq k) = Yield rq (runEnvT e . k)
        runEnvT _ r@(Return _) = r

        -- state transformer with explicit Lift
        runStateT s (Yield Get k) = runStateT s (k s)
        runStateT _ (Yield (Put s) k) = runStateT s (k ())
        runStateT s (Yield (Lift rq) k) = Yield rq (runStateT s . k)
        runStateT s (Return r) = Return (r,s)

Unfortunately, monad transformers do not compose nicely in context of higher-order effects. For example, 'runStateT' would not know that it should thread state into 'fn' or 'op' in case of '(Shift fn)' or '(Reset op)' effects for delimited continuations. The best solution I've found is to support *Extensible Monoliths* (see below).

### Monadic Fixpoint

Haskell has *MonadFix* and a *RecursiveDo* syntactic sugar, enabling a result to be used before it is defined. In context of assembly, this would be convenient because it enables users to reference forward to labels for branches or jumps. We might encode this as `(Yield (Fix f) k)` where `f : a -> Eff rq a`.

To evaluate a Fix request requires lazy handling of a future Return value, passing the main result back into 'f' and handling state correctly. Ultimately, 'Fix' must be passed up the handler stack and correctly handled at every step until closed by a 'runPure' or equivalent.

        runStateT s (Yield (Fix f) k) = Yield (Fix f') k' where
            f' = runStateT s . f . fst
            k' (r, s') = runStateT s' (k r)

        runPure (Yield (Fix f) k) = runPure (k (fix (runPure . f)))

Fixpoint is not compatible with all effects. But it is feasible to restrict some effects ins scope of 'Fix'.

### Effectful Pattern Matching

I propose to desugar syntax for conditional behavior, such as pattern matching, into a choice effect. The choice effect supports deferred branching for cases sharing a common prefix, and empty choice expresses match failure or backtracking. A conventional `Pattern -> Outcome` syntax may desugar to `PatternEffect >>= \ vars -> Return Outcome` such that Pattern binds variables in scope of Outcome, and Return serves as a stage separator for further effects. We can introduce an alternative separator where the Return is explicit, perhaps `Pattern >> Return Outcome`, such that the RHS may be an effectful expression expanding to multiple Outcomes. 

We should carefully distinguish global non-deterministic choice from local pattern branching. They have distinct intentions, connotations, use cases. With patterns, we predictably 'commit' to the first match, and choice is scoped syntactically. Fortunately, we don't need fully indexed choice because pattern matching is hierarchically structured. It seems adequate to support global 'Choice' and local 'Alt' as distinct constructors.

An intriguing opportunity is to mix choice with other effects. For example, a pattern reads from a queue and matches only some read values. Ideally, such operations are reverted when the match fails, implicitly checkpointing state. Generic integration is feasible by introducing a `(Cut altOps)` effect as a standard delimiter for backtracking of Alt branches.

        -- a pure pattern matching handler
        runAlt (Yield rq k) = match rq with
            Fail -> []
            (Alt a b) -> List.flatMap (runAlt . k) [a,b]
            (Cut op) -> runAlt (k (runCut op))
            (Fix f) -> runAlt (k (fixListFn (runAlt . f)))
            _ -> error "unrecognized effect in pure runAlt"
        runAlt (Return r) = [r]

        runCut = head . runAlt

        fixListFn f = match (fix (f . head)) with
            x:_ -> x:(fixListFn (tail . f))
            [] -> []

We could use 'runCut' as an evaluator for pure pattern matching. A front-end compiler could easily support this as a built-in. But when we don't know whether a pattern is effectful, we could use the 'Cut' effect for  

*Aside:* It is feasible to further extend choice with search. By introducing effects to manage anticipated 'score' for a choice, a handler can heuristically reduce priority of a choice before fully computing it. 

### Flexible Monoliths

We can express extarbitrary data flow via indexed state, and arbitrary control flow via indexed, delimited continuations. Between these we can model almost any effect. But we'll also need a few 'generic' effects, e.g. Fix, Cut, and Alt for generic desugaring of do notation and effectful pattern matching. A viable one-size-fits-most handler for pure computations:

        unique CC, BT  

        -- A monolith of four generic effects: 
        --   Continuations (Shift, Reset)
        --   Alternative (Cut, Fail, Alt)
        --   State (Get, Set)
        --   Fixpoint (Fix)
        -- Result is list of outcomes as `(r,s')` pairs.
        run s (Yield rq k) = match rq with
            Get -> run s (k s)
            (Set s') -> run s' (k ())
            (Alt a b) -> a' ++ b'
                a' = run s (a >>= k)
                b' = run s (b >>= k)
            (Cut op) -> match (run s op) with
                (r,s'):_ -> run s' (k r)
                [] -> [] -- promote failure
            Fail -> []
            (Reset ix op) -> run s' op where
                s' = s with { .[CC] = ((ix,k):(s.[CC])) }
            (Shift ix fn) -> match s.[CC] with
                (ix',k'):cc' -> 
                    -- 'Shift' pops matched 'Reset', 'fn' may reinsert
                    let s' = s with { .[CC] := cc' }
                    if (ix is ix') then run s' ((fn k) >>= k') else
                    run s' (Yield rq (\r -> Yield (Reset ix' (k r)) k'))
                [] -> error "shift index not in scope"
            (Fix f) -> List.flatMap cont rs' where
                rs' = fixListFn (run (s with { .[CC] := [] }) . f) 
                cont (r,s') = run (s' with { .[CC] := s.[CC] }) (k r)
        run s (Return r) = match s.[CC] with
            ((_,k):cc') -> run (s with { .[CC] := cc' }) (k r)
            [] -> [(r,s)]

Unfortunately, fixpoint is not fully compatible with continuations. The essential issue is the continuation may be invoked any number of times, but we're only permitted exactly one fixpoint value. We can shift where reset is scoped within the fixpoint. We can support Alt and Fix together, i.e. exactly one fixpoint future per alt choice.

*Note:* I might review [Delimited Control in OCaml, Abstractly and Concretely](https://okmij.org/ftp/continuations/caml-shift.pdf) or [A Monadic Framework for Delimited Continuations](https://www.microsoft.com/en-us/research/wp-content/uploads/2005/01/jfp-revised.pdf) for a better alternative to shift-reset. A `pushSubCont` variation requires abstracting continuations as something more structured than functions.

*Note:* A transformer variation of 'run' is feasible. We'd likely want to lift Alt, Cut, Fail, and Fix, while rewriting the higher-order effects. But *Extensible Effects* offers a better direction.

### Abstract Effects API

Instead of pattern-matching request constructors like 'Get' and 'Set', we might express requests as `eff:(\api -> api.get)` and `eff:(\api -> api.set s)`. This supports any number of effects and abstracts which effects are 'primitive' in the sense of yielding to a handler. To complete this, we further abstract `Return` and `>>=` into our API, e.g. `op >>= k = eff:(\api -> api.seq op k)`.

For concision, a front-end compiler could desugar `.op` to `eff:(\api -> api.op)` and apply effects via `(eff:f) x = eff:(\api -> f api x)`. This supports concise, lightweight effects without tediously wrapping them in tags, functions, or intermediate definitions, e.g. `op >>= k = .seq op k`.

*Notes:* Not every value in `api` needs to be an effect, i.e. we ca and there is no guarantee that `api` is stable from step to step. But users may assume monad laws are respected, e.g. `(return r >>= k) = (k r)`.

### Standard Effects

It is very convenient to assume a standard effects API for generic extensions. Proposed:

- `.r Result` - pass result to next step
- `.seq op k` - equivalent to `op >>= k`
- `.alt A B` - run `A`; on fail, backtrack then run `B`
  - invalid outside `.cut` scope in general usage
  - ordered and deterministic: if `A` blocks on a lazy value, suspend rather
    than allowing readiness to select `B`
- `.fail` - failure for `.alt`, does not continue
- `.cut op` - scope for `.alt`, selects first success
- `.fix fn` - `fn` receives result as input but hides `.reset` scope
- `.get Path` - copy data from state; default is empty dict `{}`
- `.set Path Val` - overwrite data in state; set `{}` to erase key
- `.reset Key op` - scope for delimited continuations
- `.shift Key fn` - exits corresponding `.reset` with continuation 
  - continuation invalid outside current task in general usage

The `Path` type for `.get/.set` is a list of keys, assuming state is a hierarchical dictionary. The empty list represents the toplevel dictionary. Some contexts may recognize other `Path` types, e.g. lenses or opaque keys.

Tentative, deferred:

- `.env Path` - like `.get` but controlled externally
- `.scope Mixin Op` - apply mixin to `api` in scope of `Op`
- `.score Value` - for soft searches of `.alt` paths, preferences
- `.commit` - drop `.alt` paths except this one, scoped by `.cut`
- *constraints* - for reasoning and search across problem domains 

### Extensible Effects

For effects that accept arguments, we can generally leverage method objects to extend behavior, i.e. `{apply:f, _}`. Unfortunately, that doesn't work for nullary effects, and I'd hate to force a `()` argument. Instead, for purpose of running effects, handlers shall recognize `{eff:_, _}`, not limited to singletons. (More generally, handlers may support or favor alternatives to the `eff` calling convention.) We won't curry arguments for anything but the singleton `eff:f` case, but users may combine `{eff:_, apply:_, _}` to express an effect that is runnable as-is yet optionally accepts more arguments (a form of var-args).

*Note:* We'll generally treat the RHS of `eff:_` as opaque.

## Interaction Nets

In contrast to lambda calculus, interaction nets are graph-structured instead of tree-structured, and symmetric instead of directional. This simplfies fine-grained, flexible dataflow and supports backpropagation without fixpoint. In this project, interaction nets are scoped to expressions, exposing only one port.

    # identity function as inet
    interaction_net do
        .bind -> [ap, arg, result]
        .wire arg result
        .r ap

The construction effect produces a closed, opaque net value:

    interaction_net :: Eff [NetBuilder, Standard] Port -> Net

`Net` is already in weak-head normal form. It can be stored, copied, erased,
or embedded as data, but it is not an ordinary lambda-calculus function and
cannot be applied directly. Within another interaction net, connecting a
`Bind` to `Data net` loads a logical copy of `net` through its exposed port.
If that interface eventually presents `Data d`, the loaded net behaves as `d`
in its caller. Ordinary evaluation does not project `d` from the opaque net.

An explicit, arity-directed bridge gives a net a lambda-style interface. The
provisional name is `net_arity`:

    net_arity        :: Nat -> Net -> Value
    net_arity 0      :: Net -> a
    net_arity n      :: Net -> a1 -> ... -> an -> result  -- n > 0

Constructing this bridge does not inspect the net. At arity zero, demand
expects `Data` at the exposed interface and continues into its payload; `Bind`
or another normal form is an error. At positive arity, the bridge constructs
an ordinary function that attaches `n` arguments before demanding a result.
Partial application does not inspect or normalize the staged interface. After
the last argument, the result must expose `Data`; a remaining `Bind` or another
normal form is an interface error. If the net produces `Data` early, subsequent
argument wiring is governed by ordinary interaction rules and may become
stuck; the bridge does not add a separate early-result check.

A raw net boundary is therefore opened only by interaction-net loading or by
`net_arity`, never by ordinary evaluation or application.

Effects API: 

- Node constructors introduce ports. Principal port is head.
  - `.bind -> [ap, arg, result]` - constructor of functions
  - `.copy N -> [x0, x1, x2, ..., xN]` - dataflow, distinct logical instances
    - `.copy 0 -> [e]` - explicitly drops data
    - `.copy 1 -> [lhs,rhs]` - tunnel for non-local composition 
    - lambda lowering may normalize these to erasers, direct wires, and trees of
      binary sharing fans
  - `.data Expr -> [d]` - functions, lists, dicts, numbers
    - `Expr` is copied logically (refct or GC)
- Wires consume ports. Each port must be wired exactly once.
  - `.wire A B` - commutative (`.wire B A` is equivalent)
  - return port wired implicitly
- *Standard Effects* to support bookkeeping and backtracking

Nodes interact only when principal ports connect.
- bind-bind: join
- bind-copy: dup
- bind-data: call applicable data; a net is loaded by logical copy, other
  non-callable data is stuck
- copy-data: dup
- copy-copy: join paired residuals of one duplication process, dup otherwise
  - pairing follows complete dynamic duplication identity, not equality of one
    permanent node UID
  - lowered templates use local fan sites; runtime instantiation supplies one
    namespace for the whole template
  - dynamic duplication history distinguishes residual fans within a namespace
- data-data: stuck
- rules are commutative, e.g. copy-bind is bind-copy
- assembler may use intermediate nodes under-the-hood

Rules:
- join: 
  - annihilate nodes
  - connect auxilliaries positionally
- dup: 
  - copy node to each auxilliary opposite
  - wire auxilliaries to copies positionally
- call: 
  - make the called inet available in the caller inet
    - ideal: retain one shared function graph and push duplication through it
      lazily instead of eagerly relabeling or copying its body
  - connect the bind-bind principal ports 
- stuck: a type error! report and debug

Lambda calculus becomes a design pattern within interaction nets:
- lambda as `.bind` that copies and wires `arg` *into* `result`
- application as `.bind` that provides `arg`, extracts `result`

For interaction nets in general, there is no arg-result distinction. Data
flows in both directions similar to session types. `net_arity` presents only
the selected prefix of `Bind` stages and final `Data` as an ordinary function;
the raw net retains its more general interface. Initially, we'll mostly use
interaction nets as a performance tool for difficult dataflows behind explicit
lambda-style interfaces.

*Aside:* These aren't necessarily the nodes the assembler uses under-the-hood, just initial constructors for them.

## Modules

Each module is represented by a file that represents a mixin and extends an implicit, anonymous module object. The assembler provides a built-in front-end compiler for ".g" files, but *User-Defined Syntax* is supported, with users defining a monadic front-end compilers aligned to file extensions, and the assembler bootstrapping upon override.

To simplify architecture, file dependencies are constrained: a file may reference only local files and subfolders or transitively immutable remote files. We enforce immutability by requiring a DVCS revision hash for remote references. Because parent-relative and absolute filepaths are forbidden, every folder serves as a stand-alone package, easily shared and edited. 

A module is integrated by 'including' its definitions as a mixin. Any prior definitions or inclusions effectively model prior mixins. We can translate inclusions to a hierarchical element. Thus, I propose a few import forms:

- `import ...` - include-like mixins; module rewrites host `Base`, shares `Self`.
- `import ... as m` - introduces `m` with defaults then applies `import ... at m` 
- `import ... at m` - mixin applied to `m`, binds to host `Base.m`, and `Self.m`.
- `import ... binary as b`, introduces a raw file binary, does not compile 

Hierarchical imports are compatible with lazy loading.

### Configuration

The assembler implicitly loads a configuration module based on the `GLAM_CONF` environment variable or an OS-specific default, i.e. `"~/.config/glam/conf.g"` in Linux or `"%AppData%\glam\conf.g"` in Windows. A small, local user configuration typically extends a large, remote community or company configuration.

The initial configuration namespace consists of an empty `env` object. The configuration is expected to define options under `conf.*`, especially `conf.env` which is passed to the assembly. Other than `conf.env`, which impacts reproducibility, other options might be expressed effectfully with ad hoc assembler-provided effects. 

- *assembly environment*: `conf.env : Object` - determines `env` parameter to assembly. It is convenient to model this as an *object* for flexible extension by the assembly. Default is empty object.
- *command-line macros*: `conf.cli : Eff [Refl, ParseArgs, WriteArg] ()` - rewrites command-line arguments if (and only if) the first command-line argument does not start with '-'. The reader uses parser-combinators designed to simplify tab completion.
- *logger*: `conf.log : Eff [Refl, Log] ()` - can read log and write standard error, active in batch mode only. If undefined or terminates early, default logger takes over.
- *interactive development*: `conf.ide : Eff [Refl, Log, TTY, Net, File, GUI] ()` - runs in interactive mode, supports user interaction via TTY, limited network access, lightweight GUI. Limited ability to edit files and rebuild assembly. 
- *resource management*: as needed - the assembler may require ad hoc configuration for JIT and GC tuning, access to processors for acceleration, data persistence and constraint solver for reflection, remote proxies for distributed compilation or shared work caching, PKI certs and keys access DVCS or to sign works, etc.. 

For flexibility, `GLAM_CONF` may list several files using the OS-specific `PATH` separator. These files are logically applied as mixins, such that files listed earlier may override those listed later, left to right. We can feasibly split the configuration between OS-layer, project layer, and user layer. We may later extend this list to support remote URLs.

### Assembly

An assembly is expressed as a list of file and scripts on the command line. These are logically imported into an anonymous assembly module, such that first in list has final overrides. Often, it's just one file.

Command line options:

- `(-f|--file) FileName` - list a file to include; files earlier in list override those later. 
- `(-s|--script).FileExt Text` - as remote file with given extension and text. Scripts cannot import local files, hence are location-independent. 
- `--manifest FileName` - record the secure content hash of every local file used by configuration or assembly.
- `--refl Arg` - append an argument visible to reflection tasks but not to assembly as `asm.args`.
- `-- List Of Args` - the assembler defines `asm.args` as a list of strings prior to including files.

Inputs are `asm.args` from the command line and `env` from the user configuration. Depending on the configured environment, i.e. presence of `env.lang.[FileExt].compile`, assembly isn't limited to ".g" files (see *User-Defined Syntax*). The primary output is `asm.result`, which should represent a binary or filesystem folder. See *Assembler* below. 

### Remotes

Remote files are content-addressed, typically at DVCS scope. This might be expressed as a dict or object with:

- DVCS protocol (git, hg, darcs)
- DVCS repo revision hash
- filename within repo
- list of repo URLs (backups!)
- tag or branch name(s)

The file is uniquely identified and authenticated by revision hash and filename. The repo URLs support multiple backup search locations; this list may be rewritten by the user configuration. A tag or branch name is used only as a hint for efficient download, such as: `git clone -b Branch --single-branch URL`.

Remote files are a little awkward for syntax and maintenance. We'll need a good multi-line syntax for remote imports, and the ability to use expressions or abstract remotes into a separate index file. Perhaps we model each import as a miniature object? 

*Aside:* We aren't restricted to DVCS. Viable alternatives include download of secure-hashed zipfiles, or even individual source files. But my intuition is that DVCS will offer the best development and maintenance experience for content-addressed structure.

### Access Control

There is no 'export' control on modules. Everything defined in a module is exported, accessible for override. This simplifies extension, but increases risk of accidental name conflicts. The latter is mitigated by explicit overrides and hierarchical import structures. We do control the Base argument to a hierarchical module, i.e. 'env.\*', which can be leveraged with some idioms and patterns. For example, a module that defines a library may emit 'api.\*' as the primary public interface. We may then restrict 'env.\*' to share the public interface. 

        import "foo.g" as libfoo
        env.foo = libfoo.api
        override libfoo.*
        libfoo.settings.x = 42

In theory, we could use *Stateful Specification* or a secure hash to allocate anonymous namespaces, but doing so complicates things for users more than I'd prefer.

## Assembler

Primary behavior and inputs are detailed in *Modularity*. Roughly:

- load a configuration (`GLAM_CONF`) 
- construct an assembly (`-f -s --`)
- evaluate then extract `asm.result`

By default, we expect a binary result and extract to standard output. However, based on command-line arguments, we may expect and extract other result types. 

- *Expectation:* data type of result
  - `--binary` (default) - result is binary 
    - list of integers in 0..255
  - `--folder` - result models folder as dict 
    - text keys as file and folder names
    - binary or recursive folder values 
    - associated keys for permissions
  - `--auto` - decide result type heuristically
- *Extraction:* where to put result
  - `--stdout` (default) - write binary to standard output
    - incompatible with folders and interactive development
  - `(-o|--out) Destination` - output to named file or folder
  - `--discard` - no output, result ignored

Machine-code mnemonics are left to libraries. Effectively, we have a generic 'assembler' of binaries or filesystems.

### Reflection

Reflection is expressed effectfully, i.e. `eff:(\api -> ...)`. To keep implementation small and simple, the reflection API is version-specific beyond the *Standard Effects*, specialized to the assembler executable's representations and capabilities. Most bloat of portability and policy is pushed to user-defined adapters.

Reflection may be triggered by compilers (`refl.*`), configurations (`conf.log` and `conf.ide`), or anonymous term annotations (`anno refl:(.log Msg) Term`). In the general case, reflection tasks run concurrently and interact through shared state. An annotation runs its reflection task to completion before exposing `Expr`, and may observe `Expr` (triggering evaluation) or further annotate `Expr`, but does not alter the observable value of `Expr`. 

To control concurrency, the reflection API shall support software-transactional memory (STM), with transaction per scoped 'cut'. A failed computation is retried when it observed state that may have changed; failure without observations is permanent. The `cut` establishes choice and transaction scope, but is not itself an observation or a source of retryability.

Reflection is not reproducible. Between resource and scheduling variability, caching, timestamps, user interactions, etc. it's infeasible to reproduce the exact same log messages. It's left to users to ensure a reflection-based typechecker is confluent and doesn't depend on timestamps. A critical constraint on the reflection API is that it shall not observably influence pure computations. Thus, assembly 'result' remains robustly reproducible.

*Note:* Depending on API, reflection can control reflection. For example, we could provide methods to lazily rewrite annotations in a term, or disable reflection for a hierarchical namespace.

### Logging

Reflection emits diagnostics through the effect:

```glam
.log Severity Message
```

`Severity` is a separate effect argument rather than a field discovered by
evaluating `Message`. This allows queues and observers to classify a diagnostic
without first interpreting an arbitrary object. The conventional severities are
`'info`, `'warn`, and `'error`.

`Message` is normally an object. Its controlled public interface is the `msg`
member; fields outside `msg.*` are open for application-specific data and object
implementation details. The simplest useful message is:

```glam
{msg:{text:"something happened"}}
```

The conventional `msg` interface includes:

- `msg.text`: a plain-text view suitable for simple loggers;
- `msg.severity`: the authoritative severity supplied separately to `.log`;
- `msg.origin`: provenance the assembler or reflection host can attest;
- `msg.event`: an optional, user-supplied semantic identity for the event.

`msg.text` is a conventional fallback view, not the only possible rendering.
A message object may use its `spec` to derive text, structured terminal output,
SVG, interactive views, or other representations. More elaborate observers
may ignore `msg.text` entirely.

`msg.event` belongs to the emitter, not the assembler. Reusable libraries are
encouraged to use an abstract-global path or similarly stable value for this
field. Such an identity is often more useful for filtering, suppression,
documentation, and tests than a dynamic call location.

The raw queue entry is an envelope containing the original `Message`, the
separate `Severity`, and hidden host metadata. Emission does not mutate the
message. Before observation, the assembler may enrich a fresh view of the
object by authoritatively mixing `msg.severity` and `msg.origin` into it.
A logger or IDE may then apply another independent mixin, conventionally under
`viewer.*`, for properties such as terminal capabilities, language, display
width, or automatic indentation. Each observer may construct its own view;
the raw emission and views constructed by other observers remain unchanged.

`msg.origin` is structured provenance, not necessarily a single source
location. Depending on how a diagnostic was produced, it may describe a source
identity, compilation invocation, namespace, import chain, annotation emitter,
or reflection task. Consumers must not assume that it contains a meaningful
line number or a unique dynamic caller. Reusable logging functions, lazy
sharing, and tail calls can make any single “call site” misleading.

An annotation-launched reflection task has tacit access to the pure evaluator
continuation suspended behind that annotation. Reflection may query this
context and receive ordinary data—for example, a nearest definition or a
bounded source-level frame summary—but it cannot obtain, retain, compare, or
resume a first-class reference to the continuation. A logging library may
include selected context data in its message when dynamic context is useful.

Fine-grained provenance for result data is a separate concern. It should be
provided by an explicitly traced evaluation mode rather than by permanently
attaching histories to ordinary values.

Logging participates in reflection transactions. A `.log` inside `.cut` becomes
visible only when the surrounding transaction commits; failed alternatives
discard it. Queue reads observe committed host state only, never writes from
their own transaction, and fail immediately when no input is available.
Ordering between concurrently produced messages is host policy and must not
influence pure assembly results.

The assembler library does not render diagnostics. In batch mode, the
executable provides a small default terminal logger when no configured observer
handles a message. `conf.log` or `conf.ide` may instead enrich, filter, group,
store, or render messages according to user policy.

### Interaction

Interactive mode is enabled by command-line switch:

- `--batch` (default) - evaluate and extract result then return
- `(-i|--interactive)` - configurable user interface, maintains result
  - `--discard` by default, but compatible with `-o`

When interactive mode is enabled, the assembler will run `conf.ide`, and continues running until `conf.ide` terminates. Aside from the reflection API, the IDE has access to logging (downstream of `conf.log`) and user interaction via TTY (REPL, TUI) and network (listen on TCP or unix domain sockets). A lightweight GUI is feasible, e.g. Rust's egui or Dear ImGUI. Just enough filesystem access to update sources.

### Integration

The assembler does not directly support execution of assembled code, but we should at least support atomic updates to results in the filesystem. I.e. use Linux rename or Windows ReplaceFile and FILE_SHARE_DELETE. Staging might be controlled via environment variable. Atomicity of network access via `conf.ide` is encouraged.

### File Metadata

Aside from primary outputs, we might want the assembler to automatically set permissions on files in case of `-o` destinations. It doesn't seem difficult to support `asm.file_meta` to declare permissions for a subset of generated files.

## User-Defined Syntax

When loading a module, we'll first search the provided environment for a compiler: `_env.lang.[FileExt].compile`. If defined, we'll use this compiler, falling back to an assembler built-in or reporting an error. The assembler shall provide a built-in at least for file extension ".g", albeit not necessarily the oldest or newest versions. 

The 'compile' method shall be expressed effectfully. Effects include:

- parser combinators to read source binary
  - designed for tracing, isolation, laziness
  - i.e. multi-phase and scoped parsing
- import integration, files as modules or binaries
  - modules are always pathed in toplevel namespace
  - this simplifies override of modules
- uniqueness - abstract global paths
- access to the past and future namespace
- *Standard Effects* for convenience

The assembler should not privilege the built-in ".g" compiler or others. It is best to build upon the same API that will be provided for user-defined syntax.

For reasons of reproducibility and location-independence, the front-end compiler cannot see what file it's compiling or where a module is loaded within the global namespace. But such data implicitly annotates parse results and is visible through the reflection API.

*Note:* File extensions are normalized as lower-cased for A-Z, e.g. `"foo.Tar.Gz"` is processed by `env.lang.["tar.gz"].compile`.

### Compiler Bootstrapping

The assembler shall check whether the final `env.lang.[FileExt].compile` is different from the initial compiler. If so, the assembler will perform a bootstrap process: recompile using the returned compiler, repeat until the compiler stabilizes. Pseudocode:

        bootstrap fileExt binary base compile =
            let result = runCompiler (Yield (Fix (compile binary base)) Return)
            let compile' = result.env.lang.(fileExt).compile if defined
                           otherwise builtin for fileExt
            if(compile is compile') then result else
            bootstrap(fileExt, binary, base, compile')

The main motive for bootstrapping is reproducibility: stabilize the compiler, make it less dependent on context. There is also a role for extensibility. It can be difficult to integrate syntax extensions in scope of bootstrapping, but extensions to macro effects APIs are easier to integrate.

*Note:* Above bootstraps only front-end compilers. I hope to eventually bootstrap the assembler executable, i.e. with a portable definition of the executable in the module system. But that's a long term concern.

### Editable Projections

It is possible to express editable views of source texts via auxilliary methods in the language object, e.g. `env.lang.[FileExt].view`. This is a good starting point, at least. But it's coarse grained and very limiting.

To support fine-grained editable projections, it's useful to trace terms back to contributing sources. For example, a parsed integer may carry hidden metadata regarding the original source location, and even a codec. This metadata is visible via the reflection API. In theory, `conf.ide` can render a widget that ties back to original sources and supports editing thereof. 

But effective support for editable projections will benefit from careful design of our parser combinators.

### Macros

Compilers may support macros. Macros enable metaprogramming at the syntax layer in terms of rewriting text, tokens, ASTs, etc.. It is convenient to express macros effectfully, i.e. with 'read' and 'write' effects at flexible levels of abstraction. To simplify local reasoning, the compiler may restrict the scope of macros, e.g. ensuring balanced reads and writes of parentheses.

## Reasoning

What can we feasibly implement to support developers in reasoning about the assembly process and product?

* *Types*: We can describe assumptions about programs and data in a composable, machine-checkable way. This is directly useful for discovering and resisting bugs in the assembly metaprogramming. It's more difficult to specify anything meaningful about the assembly output, i.e. the generated program, though clever use of dependent types, phantom types, GADTs may help.

* *Tests*: We can sample subprogram behavior under various conditions. We can simulate or emulate execution of machine code. With acceleration, emulation may even perform adequately. With non-deterministic choice, it is feasible to fuzz test indefinitely, simulate race conditions, check a wide variety of conditions. Heuristic non-deterministic choice together with abstract interpretation can effectively lift tests into constraint models.

* *Contracts*: We can describe a monadic subprogram's stateful preconditions, postconditions, invariant assumptions. It is feasible to check these conditions, either directly (via handler) or indirectly, by integration with a type system. This isn't especially useful for verifying correct output, but it's a simple and direct approach to verifying programmer assumptions and isolating errors.

* *Proofs*: Under Curry-Howard, types can be understood as theorems, and programs as proofs. But for sophisticated types, verifying types may involve expensive searches. Ideally, we can provide some hints to reduce the verification overheads, separate from the program itself but perhaps as part of a declaration.

* *Visualization*: We can support developers in viewing and understanding the assembly process, auxilliary processes such as typechecking, testing, or theorem proving, and the outcomes of these various processes. We can draw user attention where it's needed. Ideally, we can support interactive visualization, with progressive disclosure and support the user in understanding problems. Bonus points if interactive visualization integrates smoothly with editable projection.

* *Tracing*: An assembler can maintain some metadata to trace outputs back to sources. However, there's a rather severe tradeoff between precision and performance. This can be mitigated by replay with greater precision or with an intersection of different mappings. Ideally, developers may also guide tracing via annotations.

* *Profiling*: Feedback about where an assembler is spending its time is useful for identifying infinite loops and improving performance of the assembly process. It won't help much with output modulo profiling of emulations.

* *Abstract Interpretation*: We can interpret machine code against an abstract representation of machine state. With reflection, we can do similarly for functions. We can include assumptions and test for contradiction or consistency. It is feasible to integrate a constraint model to perform the actual checks.

### Reflection

The logic required for reasoning is non-trivial, and I'd prefer to keep it separate from the assembly executable. The proposed alternative is that the assembler provides a low-level reflection API and runs ad hoc, user-defined reflection tasks. The reflection API should bypass abstraction barriers, such that users can inspect functions and dictionaries and perform ad hoc proofs. Reflection can also support visualization by interacting with a logger or IDE.

Front-end compilers may further contribute to reflection by exporting intermediate representations.

### Constraint Solver

Many forms of reasoning benefit from a high-performance constraint solver. To support this, we can attach the assembler's reflection API to an external SMT solver such as Z3 or cvc5. Although reflection API cannot directly modify code, it can provide recommended edits for an IDE to apply. Thus, we can leverage the constraint solver for assisted theorem proving or programming.

Long term, with acceleration, it should be feasible to implement constraint solvers fully within the assembly. This would enable the assembler itself to perform more of the search.

## Programming

### Direct-Style Assembly

To support direct-style assembly, we express assembly mnemonics as writer effects with a concise API. Logically, we're writing an assembly representation, and higher-order effects serve the role of conventional assembly macros. For aesthetics, assume a do notation that accepts `action -> result` to capture return values.

        writeln msg = using x86 do
            .rodata (msg ++ [10])       -> msg_loc
            movl 'rdi 1
            movl 'rsi msg_loc
            movl 'rdx (1 + len msg)
            syscall

        exit = using x86 do
            movl 'rax 60
            xor 'rdi 'rdi
            syscall

        main = do
            .global "_start"
            writeln "Hello, World!"
            exit

        asm.result = mkelf main

A characteristic of 'direct-style' assembly, the heart of its vibe IMO, is that it's locally write-only. Users aren't reading contexts to make decisions. Insofar as we pursue direct-style as our foundation, we should build a set of effects that returns unit values or opaque references. Extensions befitting direct-style assembly:

- *singletons*: Declare that some resources are written only once, e.g. based on a shared name or content addressing. This allows us to write singletons on demand.
- *write cursors*: Track references to multiple write 'sections' with layout constraints (e.g. B starts where A ends). Grow and logically link sections across multiple steps. Heterogeneous cursors for bss, rodata, stack frames, etc..
- *abstract interpretation*: Maintain an abstract representation of machine state and user assumptions, so we can detect conflicts. Carry this metadata with each label, so we can ensure consistent contexts on branches or jumps. 
- *program search*: When conflicts are detected between conditions and assumptions, have alternatives as backups. Potential extensions to weighted search, integrating preferences. 
- *obligations*: Write down what you're planning to do, some constraints on order of events, and write when you're done. Likely per cursor. These may be opaque to assembler, but support human discipline.
- *constraint models*: lightweight constraint system as a form of global agreements within an assembly. We do not reading solver values while writing the assembly, but we can arrange for them to influence the next stage of assembly.

There's a reasonable case to be made for some coordination effects, e.g. first-class queues for communication between subtasks shouldn't severely detract from the direct-style experience. Ultimately, direct-style is a stylistic choice, not a mandate, and users fully control effects.

*Aside:* Above, I use `using x86` to avoid polluting the toplevel namespace, but a viable alternative is to leverage lightweight effects, e.g. `.movl 'rax 60`, pushing the assembly mnemonics directly into the effects API. Of course, this implies something like a `mkelf_x86` variant to provide the API.

### Structured Assembly

Direct-style assembly with most CPU machine code naturally covers all procedural structures (while/then, if/then/else, etc.). Those are trivial to model. But I'm especially interested in approaches to making assembly-level programming scale more directly to asynchronous and concurrent interactions, and eventually to distributed and heterogeneous systems.

- Coroutines
- Kahn process networks
- Temporal-spatial logics
- Harel state charts
- Optimistic transactions
- Incremental computing

Transaction loops, where we repeatedly run an atomic, isolated transaction - optimized via replication on choice, incremental computing, and distribution - is of special interest to me. It's easy to replace the transaction we're running, providing a robust foundation for live systems. 

### Multi-Level Languages (Policy)

When expressing domain knowledge, we should aim to do so in an extensible and substrate-independent manner: objects for extensibility, DSLs for substrate-independence. For example, a system of equations should be expressed as an object with definitions in an calculus DSL. Users can tweak the equations and extract the knowledge. Combat choreography for video-game cinematics should be an object with an animations DSL, such that users can override parts of the animation and extract to a system of equations or other model.

Effectful DSLs, i.e. DSL as effects API, are convenient. While extracting code, we can rely on generic effects for bookkeeping and backtracking. Constraint systems enable flexible program search across problem domains. We can easily introduce intermediate representations with *singletons* and *write cursors* to mitigate scatter-gather challenges.

Essentially, we can model each language as an assembly target instead of a syntax. Separately, we can develop user-defined syntax or macro DSLs to conveniently express behavior.

### Proof-Carrying Code

It is possible to express theorems on assembly or machine code, e.g. in terms of preconditions, postconditions, invariants. Useful properties to examine included bounded-time, bounded-space, memory safety. It should be possible to build proofs of these theorems that are much easier to check than they were to discover, then bundle them with the code, perhaps as a separate file.

Although we cannot rely on reflection to emit proofs, we can use reflection and the IDE to help discover and inject proofs (or proof tactics). Most relevantly, the reflection API may provide access to an SMT solver, and the IDE may support edit suggestions from the reflection API. Thus, assisted theorem proving is possible. As we accelerate constraint solvers, proof tactics may become more adaptive.

I use the word 'possible' because I'm not confident to say 'feasible'. The VALE project (Verified Assembly Language for Everest) demonstrates theorem proving at the assembly level, but not at great scale, and not with extensibility or adaptability. There is significant risk of proofs becoming an anchor.

## Concurrent Assembly

We can model multi-threading in terms of shift-reset, transferring ownership of a 'heap' between threads upon each context switch. When a threads update different parts of the heap, it is feasible to evaluate multiple threads in parallel. Explicit use of interaction nets can feasibly model futures and promises more directly.

Although outcomes are deterministic, we'll also want them to be *stable* across most changes in code. Ideally, the assembly outcome should be entirely independent of heuristic thread scheduling decisions. This suggests designing around CALM and confluence: futures and promises, linear channels, broadcast channels, constraint systems, CRDTs.

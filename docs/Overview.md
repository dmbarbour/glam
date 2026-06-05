# Project Overview

My goal for this project is to lift assembly-level programming into something I feel comfortable directly using as a primary language, and also a suitable medium for directly distributing software.

Rough approach:

- design language for specifying binaries in general
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

## Why Another Language?

Some existing languages align with some of my desiderata. For example, F\* or Vale support reasoning, and Unison supports reproducible modularity. But none attend the whole range, much less offer the programmer experience I want. Historically, assembly languages haven't received a lot of love from programming language designers.

## Overview of Semantics

I propose to build upon a pure, untyped lambda calculus with lazy evaluation and annotations.

A few data types - lists, numbers, dicts - receive optimized representations and accelerated operations. Through syntax, we'll support object-oriented inheritance in terms of [open fixpoint](http://fare.tunes.org/files/cs/poof.pdf), and monadic effects in terms of a [freer monad](https://okmij.org/ftp/Haskell/extensible/more.pdf). The toplevel namespace is modeled in an object-oriented style, thus supports overrides as a foundation for extensibility.

Annotations are essentially active comments. They do not influence evaluation, but may affect performance, reasoning, visualization, projectional editing, and other tooling. 

## Performance

Performance of lambda calculus is mediocre by default, and bignum arithmetic won't help. Performance can be significantly enhanced with a little guidance. Some relevant patterns:

* *Acceleration*: Substitute a definition with a built-in. Choose built-ins that enable flexible computation, e.g. an accelerated abstract machine that can be recompiled to run on a CPU or GPGPU.

* *Parallelism*: Trigger a lazy computation early and evaluate in a background worker thread. This pattern is called 'sparks' in Haskell. We can also obtain some parallelism from acceleration.

* *Caching*: Remember expensive computations to avoid rework. Persistent caching can support incremental compilation. A shared remote cache with PKI infrastructure can support direct downloads of binaries. 

* *Ephemerons*: Mark some atoms as scope-unique. Diverge upon comparison if the assertion proves untrue. Use marked atoms as dict keys. Garbage collect associated data when the marked atom is collected.
  
These patterns should be supported via annotations or built-ins functions.

## Data

The built-in data types are numbers, lists, dicts, and functions. All data is immutable, i.e. you cannot update a list or dict, but you can construct a new list or dict in terms of updating an existing one.

- Numbers are bignum integers and exact rationals. Rounding of numbers is left to users, but the underlying representation may optimize for floating-point style usage (e.g. base 2 or 10).
- Lists are the one-size-fits-all sequential data structure. Lists may be concretely represented by finger-tree ropes under the hood. Binaries are modeled as lists of small integers (0..255) and heavily optimized.
- Dicts are finite key-value associative structures. Keys must be comparable with equality, i.e. no functions. Relevant:
  - Dicts do not support iteration over keys. Enables key hash, data abstraction, ephemerons.
  - Tagged data is modeled as a singleton dictionary and annotated to keep it as a singleton.
  - Unique atoms are modeled as tagged data with abstract tags. Only observation is equality.
- Functions are expressed in the lambda calculus.

These basic data types are implicitly tagged, i.e. users may express conditional behavior based on whether a value is a number or a function. But in many cases, users will want to further tag data to distinguish purpose. The assembler should optimize for common cases, such as use of atoms as keys in dicts or tagged data.

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

For stateless objects, we don't need more than one object instance. Instead of presenting a `Dict -> Dict -> Dict` function, we can directly instantiate the dictionary while preserving the mixin under a special interface. To support further overrides, we define a 'Spec' interface that includes the mixin.

### Multiple Inheritance

We can feasibly model multiple inheritance, where an object inherits from several others that may share ancestors. We can apply a linearization algorithm, ensuring each shared ancestor is mixed in only once and in a consistent order. 

Linearization requires an identifier to distinguish whether two specifications are the same. This should be paired with an assertion that specifications with same identifier are equivalent, such that the assumption can be verified based on structural or referential equality. In context of singleton instantiations, all the necessary information could be held under the 'Spec' interface within a dictionary.

### Explicit Override

To resist ambiguity conflicts, it is useful to distinguish introducing and overriding a name. We can report an error if we override a name that is undefined or if we introduce a name that is already defined. The implicit assumption is that a name is introduced with some meaning or purpose, while overrides presumably preserve meaning and purpose. Syntactically, this distinction may be lightweight, e.g. '=' vs. ':='.

### Structured Namespaces

A flat object namespace easily grows cluttered. It is not difficult to organize names hierarchically. For example, we can easily apply a mixin at an index. 

        apply_at idx mixin = λbase.λself. 
            base with { 
                (idx) = mixin base.(idx) self.(idx) 
            }

More sophisticated translations are possible, e.g. translating individual names. However, it is awkward to extend translations like this to multiple inheritance. It is feasible to develop a few specialized variants, assuming adequate developer control over the 'Spec' interface.

### Stateful Specification

Mixins can model state-like updates, treating Base as a previous state, Instance as next state, and Self as final state.

        λbase. λself. Base with { 
            v = base.next, 
            next = 1 + base.next 
        }

This can provide a foundation for allocating unique identifiers or building tables. However, this pattern easily interferes with lazy loading of modules if we aren't careful about integration. Ideally, a front-end compiler would provide dedicated syntax for safe patterns.

### Stateful Objects

It is expensive to maintain object state in terms of stateful specifications. Each state update would extend a 'chain' of specifications, an append-only log of every object's history. For efficient state, we must flatten the log, i.e. eliminate intermediate states. To resolve this, we must maintain state separately from the mixin structure.

One option is pass around something like a `(Spec, State)` pair. 

Alternatively, we can support pass-by-ref: design objects to operate in an effectful context, allocate and assign `obj.HeapRef` upon instantiation, interact with the object only within context. We can freely mix stateful and stateless objects. We can leverage the *Ephemerons* pattern for automatic garbage collection of heap state. Essentially, we can support the popular OO programming style.

## Effects

Even without a runtime, effects are convenient for implicit dataflow, backtracking and error handling, flexible composition, extensible behavior, etc.. I'll use Haskell(-ish) syntax to describe these, but it should be easy to translate.

        type Eff rq a =
            | Yield (rq x) (x -> Eff rq a)
            | Return a

We can model a free-er effects monads as either *yielding* a request, continuation pair or *returning* a final result. In case of yield, the expected response type depends on the request. Of course, in context of untyped lambda calculus, enforcement may be limited.

We can easily introduce some syntactic sugar:

        (sugar)         (monadic ops)
        a <- op1        op1 >>= \ a ->
        op2             op2 >>= \ () ->
        op3 a           op3 a

We can specialize the monadic operators for our only monad. Our untyped lambda calculus doesn't offer a direct solution to type-indexed behavior, such as typeclasses, so this is convenient:

        (Yield rq k1) >>= k2 = Yield rq (k1 >=> k2)
        (Return a) >>= k = k a
        k1 >=> k2 = (>>= k2) . k1

Effectively, '>>=' captures the continuation into 'Yield'. Unfortunately, the Kleisli composition `>=>` is left-associative, i.e. `((((k1 >=> k2) >=> k3) >=> k4) >=> k5)`, but this rebuilds the entire 'stack' on every step. Right-associative `(k1 >=> (k2 >=> (k3 >=> (k4 >=> k5))))` performance is superior. Ideally, the assembler optimizes this, logically recognizing and rewriting `>=>`. We can also rewrite `(k >=> Return)` and `(Return >=> k)` to `k` as a form of tail-call optimization (TCO). We'll want TCO in general for long-running loops.

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

We can express extarbitrary data flow via indexed state, and arbitrary control flow via indexed, delimited continuations. Between these we can model almost any effect. But we'll also need a few 'generic' effects, e.g. Fix, Cut, and Alt for generic desugaring of do notation and effectful pattern matching. A viable one-size-fits-most handler:

        run s op = runChoice (runCST s (Yield (Cut op) Return)) 

        unique CC, CutScope, BT  

        -- implements delimited continuations and state
        runCST s (Yield rq k) = match rq with
            Get -> runCST s (k s)
            (Set s') -> runCST s' (k [])
            (Reset ix op) -> runCST s' op where
                s' = s with { (CC) = ((ix,k):(s.(CC))) }
            (Shift ix fn) -> match s.(CC) with
                (ix',k'):cc' -> 
                    -- 'Shift' pops matched 'Reset', 'fn' may reinsert
                    let s' = s with { (CC) = cc' }
                    if (ix is ix') then runCST s' ((fn k) >>= k') else
                    runCST s' (Yield rq (\r -> Yield (Reset ix' (k r)) k'))
                [] -> error "shift index not in scope"
            Fail -> runCST s (onFail >>= k)
            (Alt a b) -> runCST s ((onAlt a b) >>= k)
            (Cut op) -> runCST s ((onCut op) >>= k)
            (Fix f) -> Yield (Fix f') k' where
                f' ~(r',_) = runCST (s with { (CC) = [] }) (f r')
                k' (r',s') = runCST (s' with { (CC) = s.(CC) }) (k r')
            _ -> Yield rq (runStateT s . k)
        runCST s (Return r) = match s.(CC) with
            ((_,k):cc') -> runCST (s with { (CC) = cc' }) (k r)
            [] -> Return (r,s)

        yield rq = Yield rq Return
        return = Return
        shift ix fn = yield (Shift ix fn)
        reset ix op = yield (Reset ix op)
        shift_reset ix fn = shift ix (reset ix . fn)

        onCut op = do
            prev <- getref BT
            setref BT (error "failed without alternative")
            outcome <- reset CutScope op
            setref BT prev
            return outcome
        onFail = shift_reset CutScope <| \_-> getref BT >>= id
        onAlt a b = 
            shift_reset CutScope <| \k-> do
                cp <- get 
                setref BT (set cp >> k b)
                k a

        -- external 'non-deterministic' choice
        runChoice (Return r) = [r]
        runChoice (Yield rq k) = List.flatMap (runChoice . k) <| match rq with
            (Choice xs) -> xs
            (Fix f) -> fixListFn (runChoice . f)

Unfortunately, fixpoint are not fully compatible with continuations. We resolve this above by forbidding Shift across Fix. Users must be aware of this limitation if they're using fixpoints a lot, or at least they'll become aware swiftly. Backtracking conditional effects are more flexible, being modeled entirely in terms of state and continuations. The implicit Cut (in 'run') simplifies further composition.

### Extensible Effects

With flexible monoliths, we can define almost any effect. But there's an awkward distinction between defined and assumed effects. This separation hinders generic programming. 

To eliminate this distinction, we can abstract the effects environment, e.g. threading an 'effect' API, such that we're invoking 'effect.Op' instead of calling a separate definition that assumes a specific set of primitives. The caller never needs to know whether Op is a primitive or a definition. We can also abstract the monad structure, i.e. the 'Return' and '>>=' (Seq) constructors.

Modeling 'effect' as an object with inheritance enables extensions as mixins. Compared to monad transformers, this also simplifies extension with higher-order effects because we can capture context - including API 'self' - at point of invocation instead of first unwinding a handler stack.

In context of multiple inheritance, a linearization algorithm will deduplicate and merge extensions. This relaxes constraints on stack order and local knowledge of usage contexts. Structurally-incompatible extensions are detected as linearization conflicts or ambiguity errors (via explicit overrides). This simplifies debugging and improves user confidence. 

### Optimistic Concurrency

We can model cooperative threads in terms of continuations and state. Assume our cooperative threads run in isolation between explicit checkpoints. We can view this as transactional steps, with each checkpoint 'committing' any updates. It is possible to evaluate multiple threads in parallel, analyze for read-write conflicts, commit a non-conflicting subset of checkpoints then replay the remainder.

A failed transaction is logically replayed until it succeeds, waiting for conditions to change, a basis for concurrency control. We may also introduce 'atomic' sections where checkpoints are suppressed.  

A few concerns:
- *Rework* - too much replay. A scheduler can mitigate rework heuristically based on conflict history. Users can avoid rework via design patterns, e.g. favoring queues or CRDTs. It is feasible to support partial rollbacks. Rework is easy to report and debug, yet something to design around.
- *Starvation* - need to spread replays across threads, not kill the same ones every time. Even better if weighted by effort, i.e. better to replay inexpensive operations. Mitigated by metadata, e.g. counters and priorities.
- *Determinism* - modulo reflection and race conditions, the scheduler awaits the slowest thread in each optimistic batch. This is an opportunity cost to keep cores busy. Users can mitigate via sparking 'will need' pure computations to keep cores busy between checkpoints. 
- *Local Reasoning* - it's difficult to understand and debug interactions that are distributed across multiple threads. This is mitigated by explicit checkpoints and atomic sections. We can do better by designing for confluence, eventual consistency, such that the final outcome is independent of the schedule.

I'm not convinced that modeling assembly as a deterministic multi-threaded process is a great idea. The local reasoning issue, especially, is a concern. Single-threaded with sparks is much easier to control and debug, and can still utilize all the CPUs. Nonetheless, I do want the option to be available.

But optimistic concurrency will serve as a foundation for reflection tasks and the configured IDE.

## Modules

A module is represented by a file, and represents a mixin object. The assembler provides a built-in front-end compiler for ".g" files, but *User-Defined Syntax* is supported, with users defining a monadic front-end compilers aligned to file extensions, and the assembler bootstrapping upon override.

To simplify architecture, file dependencies are constrained: a file may reference only local files within the same folder or subfolders (no parent-relative ("../") or absolute paths), or content-addressed remote files (by DVCS revision hash and filename). File dependencies must form a directed acyclic graph. Files and subfolders whose names start with "." are also hidden from the module system.

A module is integrated by 'including' it as a mixin. Any prior definitions or inclusions effectively model prior mixins. We can translate inclusions to a hierarchical element. Thus, I propose a few import forms:

* *include Module* - bind included module's Base to host's current Base namespace, sharing Self.
* *include Module at m* - apply module to override component dictionary 'm', binds Base.m, Self.m 
* *import Module as m* - introduce 'm' with `{ "env": Self.env }` by default, then apply 'include Module at m'.
  - This treats 'env' as an implicit, read-only environment at the module layer, supporting adaptability.

The hierarchical 'include at' and 'import as' forms simplify lazy loading. In contrast, with toplevel 'include', it is often difficult to determine which modules introduce or override a definition without loading everything. Ultimately, there is only one 'Self'. This simplifies deep overrides and extensions, analogous to mutable definitions without actual mutation.

### Configuration

The assembler implicitly loads a configuration module based on the `GLAM_CONF` environment variable or an OS-specific default, i.e. `"~/.config/glam/conf.g"` in Linux or `"%AppData%\glam\conf.g"` in Windows. A small, local user configuration typically extends a large, remote community or company configuration.

The configuration defines various options under 'conf.\*' to guide the assembler. As a rule, configuration is expressed effectfully to simplify extension and composition. 

- *assembly environment*: `conf.env : Eff [Compile] ()` - determines initial (Base) environment for assembly. Modeled as compiling an empty, anonymous source file (per *User-Defined Syntax*). Default is to forward toplevel 'env'.
- *command-line macros*: `conf.cli : Eff [Parse, Write] ()` - rewrites command-line arguments if (and only if) the first command-line argument does not start with '-'. Expressed via parser combinators to support tab completion.
- *interactive development environment*: `conf.ide : Eff [Refl, TTY, Net, File, GUI] ()` - see *Interaction* 
- *batch-mode logger*: `conf.log : Eff [Refl, Log] ()` - potential user configuration for batch-mode visualization, may write to stderr and potentially local log files.
- *resource management*: GPGPUs for acceleration, remote proxies for shared cache or compilation, constraint solvers, GC and JIT tuning, etc.. Figure things out we go.

For flexibility, `GLAM_CONF` may list several files using the OS-specific `PATH` separator. These files are logically 'included' as mixins, such that files listed earlier may override those listed later, left to right. We can feasibly split the configuration between OS-layer, project layer, and user layer. We may later extend this list to support remote URLs.

For reasons of reproducibility, we're careful about effects in 'conf.env' and 'conf.cli', as those may influence the assembly result. Most other configuration features will receive ad hoc access to the reflection API.

### Assembly

The assembler receives command-line arguments that express an assembly module as a list of mixins. Though, in practice, it's usually just one file or script. Relevant arguments:

- `(-f|--file) FileName` - list a file to include; files earlier in list override those later (left to right). Depending on the configured environment, assembly isn't limited to ".g" files (see *User-Defined Syntax*).
- `(-s|--script).FileExt Text` - as remote file with given extension and text. Scripts cannot import local files, hence are location-independent. 
- `-- List Of Args` - the assembler shall define `asm.args` as a list of strings prior to including files.

Typically, the namespace for an assembly starts with `asm.args` and `env.*` from command line and configuration respectively. The primary output is `asm.result` for extraction (see *Assembler* below). 

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

### Reflection Tasks

A module may define effectful 'task.\*' operations to perform upon loading. The assembler runs these tasks concurrently upon loading a module, providing a reflection API. Use cases include testing, typechecking, visualization, and cache management. Reflection tasks do not directly influence assembly output, but they may raise warnings or errors.

To keep implementation small and simple, the provided reflection API is version-specific, specialized to an executable's representations and capabilities. The bloat of portability and policy is pushed to user-defined adapters.

### Interaction

Instead of repeatedly asking an assembler to evaluate a result, we can ask an assembler to repeatedly evaluate a result, i.e. external versus internal loops. Although this simplifies in-memory caching, the primary benefit is that the process remains available for interrogation. This provides a foundation for interactive debugging and development.

The assembler executable shall support interactive mode via simple command-line switch:

- `--batch` (default) - evaluate and extract result then return
- `(-i|--interactive)` - configurable user interface, maintains result
  - `--discard` by default, but compatible with `-o`

To avoid bloat, the assembler leaves definition to the user configuration, 'conf.ide'. The assembler provides an effects API supporting reflection, TTY, listen on TCP ports or unix sockets, write access to local source files, and perhaps a lightweight GUI. Interactive mode terminates upon return. 

The reflection API does not provide direct access to the user. But, indirectly, reflection tasks may check for interactive mode, publish notifications where 'conf.ide' will find them, and receive feedback based on user input.

### Integration

The assembler does not directly support execution of assembled code, but we should at least support atomic updates to results in the filesystem. I.e. use Linux rename or Windows ReplaceFile and FILE_SHARE_DELETE. Staging might be controlled via environment variable.

## User-Defined Syntax

When loading a module, we'll first search the provided environment for a compiler: `Base.env.lang.[FileExt].compile`. If defined, we'll use this compiler, falling back to an assembler built-in or reporting an error. The assembler shall provide a built-in at least for file extension ".g", albeit not necessarily the oldest or newest versions. 

The 'compile' method shall be expressed effectfully. Effects shall include:

- generic state, shift/reset, fix, cut, alt
- parser combinators to read source binary
  - design for tracing, isolation, laziness
  - i.e. multi-phase and scoped parsing
  - simplicity is also a goal here
- load files as modules or binaries
  - modules are always bound to namespace
- generate unique atoms
- access and update 'Base' namespace in state
  - final 'Base' becomes 'Instance' namespace
  - no-op module thus has identity behavior
  - enforces explicit overrides
- access final 'Self' namespace 
- access built-in functions 
- warnings, errors, recommendations

I accept a fair bit of complexity in the compiler effects API because it's inevitable: the assembler must implement the ".g" syntax and support adequate debugging thereof, thus there is some arbitrary division between 'effects API' and 'the rest of the compiler'. By sharing common logic - especially parser combinators - we can support more-consistent debugging, projectional editing, and other tooling across front-end languages.

For reasons of reproducibility and location-independence, the front-end compiler does not know what file it's compiling or where the module is loaded within the global namespace. But such metadata is available through reflection APIs.

*More Notes:*
- Built-in languages should use the same API internally.
- Normalize file extensions: lower-case A-Z, drop initial '.'
  - e.g. "foo.TaR.gZ" via `env.lang.["tar.gz"].compile`
- The language object may define more methods for tooling.

### Compiler Bootstrapping

The assembler shall check whether `Self.env.lang.[FileExt].compile` is different from the initial compiler. If so, the assembler will perform a bootstrap process: recompile using the returned compiler, repeat until the compiler stabilizes. Pseudocode:

        bootstrap fileExt binary base compile =
            let result = runCompiler (Yield (Fix (compile binary base)) Return)
            let compile' = result.env.lang.(fileExt).compile if defined
                           otherwise builtin for fileExt
            if(compile is compile') then result else
            bootstrap(fileExt, binary, base, compile')

The main motive for bootstrapping is reproducibility: stabilize the compiler, make it less dependent on context. There is also a role for extensibility. It can be difficult to integrate syntax extensions in scope of bootstrapping, but extensions to macro effects APIs are easier to integrate.

*Note:* Above bootstraps only front-end compilers. I hope to eventually bootstrap the assembler executable, i.e. with a portable definition of the executable in the module system. But that's a long term concern.

### Macros

Macros support metaprogramming at the syntax layer, often in terms of rewriting text, tokens, etc.. Macros may be defined within the normal program namespace for modularity. However, in context of lazy loading, a compiler must not peek at definitions to determine whether they are macros. Thus, macro invocations must be distinguished syntactically, i.e. by operator or special naming convention.

It is convenient to express macros effectfully, especially regarding inputs and outputs. With compiler support, this enables each macro to operate at an appropriate level of abstraction, whether it's texts, tokens, or AST structures. Further, it ensures an opportunity for extension and adaptation: compilers may gradually (across versions) extend supported effects, and macros may query compiler versions or feature sets to adapt behavior.

*Note:* Many conventional use cases for macros evaporate in context of higher-order programming, first-class effects, lazy evaluation, extensible modules, user-defined syntax, and staged assembly. But there are still use cases for embedded DSLs and namespace boilerplate.

### Editable Projections

It is possible to express editable views of source texts via auxilliary methods in the language object, e.g. `.env.lang.[FileExt].view`. This is a good starting point, at least. But it's coarse grained and very limiting.

To support fine-grained editable projections, the front-end compiler will support annotation of terms. For example, a parsed integer might maintain enough metadata to both locate it in the original source file and edit it, with an associated codec translating an updated number into source text. This allows for editors to integrate where the term is used instead of only where defined. A subset of standard term annotations may be implicit, built into the parser combinator.

In general, we should support 'views' on individual terms that may be interactive, e.g. to view large graphs or tables we'll want progressive disclosure. It isn't feasible to predict all the demands for such up front, but we can make a best effort with ad hoc user values as annotations, examining and rendering annotated terms via reflection API.

## Reasoning

What can we feasibly implement to support developers in reasoning about the assembly process and product?

* *Types*: We can describe assumptions about programs and data in a composable, machine-checkable way. This is directly useful for discovering and resisting bugs in the assembly metaprogramming. It's more difficult to specify anything meaningful about the assembly output, i.e. the generated program, though clever use of dependent types, phantom types, GADTs may help.

* *Tests*: We can sample subprogram behavior under various conditions. We can simulate or emulate execution of machine code. With acceleration, emulation may even perform adequately. With non-deterministic choice, it is feasible to fuzz test indefinitely, simulate race conditions, check a wide variety of conditions. Heuristic non-deterministic choice together with abstract interpretation can effectively lift tests into constraint models.

* *Contracts*: We can describe a monadic subprogram's stateful preconditions, postconditions, invariant assumptions. It is feasible to check these conditions, either directly (via handler) or indirectly, by integration with a type system. This isn't especially useful for verifying correct output, but it's a simple and direct approach to verifying programmer assumptions and isolating errors.

* *Proofs*: Under Curry-Howard, types can be understood as theorems, and programs as proofs. But for sophisticated types, verifying types may involve expensive searches. Ideally, we can provide some hints to reduce the verification overheads, separate from the program itself but perhaps as part of a declaration.

* *Visualization*: We can support developers in viewing the assembly process, obtaining useful feedback. This includes text logs, interactive debuggers, and graphical outputs including tests and simulations. User-defined visualizations should be possible both within an assembly and generically via user configuration of interaction mode. Bonus points if visualization integrates smoothly with editable projections of source.

* *Tracing*: An assembler can maintain some metadata to trace outputs back to sources. However, there's a rather severe tradeoff between precision and performance. This can be mitigated by replay with greater precision or with an intersection of different mappings. Ideally, developers may also guide tracing via annotations.

* *Profiling*: Feedback about where an assembler is spending its time is useful for identifying infinite loops and improving performance of the assembly process. It won't help much with output modulo profiling of emulations.

* *Abstract Interpretation*: We can interpret machine code against an abstract representation of machine state. With reflection, we can do similarly for lambdas. We can include assumptions and test for contradiction or consistency. It is feasible to integrate a constraint model to perform the actual checks.

### Reflection

The logic required for reasoning is non-trivial, and I'd prefer to keep it separate from the assembly executable. The proposed alternative is that the assembler provides a low-level reflection API and runs user-defined reflection tasks. Front-end compilers may further contribute, e.g. by exporting intermediate representations.

### Constraint Solver

Many forms of reasoning benefit from a high-performance constraint solver. I'd prefer to keep this separate from the assembler executable, but we could feasibly configure access to a constraint or SMT solver, whether remote or via dynamic linking. Access to this solver can be provided through the reflection API.

### Rendering

Most logic for rendering is tossed over the wall to 'conf.ide' or a similar 

Reflection tasks may 'emit' views to be rendered as logs or presented to a user by the interaction loop. Depending on the reflection API, it may be necessary to explicitly deprecate and replace old views as sources are updated or other observed conditions change.

The reflection API does not provide effects to edit sources. That capability is left to the user interaction loop. But emitted visualizations may include interactive methods intended for execution within the user interaction loop. Thus, we can bridge the gap between visualization and projectional editing.

## Monadic Assembly

It is very convenient to express assembly effectfully. Each mnemonic becomes an operation that writes machine code to implicit state. Assembly macros become simple procedures. The state may also aggregate data declarations, e.g. '.bss' or '.data' sections. We can further extend state with compiler-like features, such as tracking which registers are in use. This could help with detecting conflicts or adapting assembly in context.

In context of *Extensible Effects*, we can improve the aesthetic by expanding '%name' to the relevant `effect.name`. With a suitable setup and monadic do notation, users will be writing columns of concise mnemonics such as '%mov'. Aside from aesthetic benefits, abstraction of mnemonic operations provides an opportunity for flexible processing, e.g. we could build some extra metrics about the assembly, or automatically maintain an abstract interpretation of machine state. 

## Syntax

This section proposes an initial syntax for ".g" files. Relevant goals include a syntax that I find pleasant to work with, and supporting the assembly programming vibe. 

As a design principle, this isn't a minimalist syntax, but it is generalist. I'll introduce operators and keywords where it offers significant benefits (improved clarity, reduced boilerplate, etc.) without becoming overly specialized to a problem domain. Macro DSLs and user-defined syntax should cover the rest. 

### Character Set

We'll limit to printable ASCII and whitespace (0x21-0x7E, SP, CR, LF) for assembler-provided front-end compilers. We could easily extend to UTF-8 post-bootstrap.

### Keywords and Operators

Keywords and operators are compiler-defined. Their meaning should be stable for entire subcommunities. User-defined syntax, where the user provides or overrides the front-end compiler, offers a mechanism to extend keywords and operators. But, I imagine most user projects will favor macro DSLs when they need a few local syntactic tweak.

Keywords are distinguished from user definitions by prefix '.', e.g. `.keyword`. This fits the assembly aesthetic, simplifies backwards compatible language extension, and is clear to readers without syntax highlighting. It has grown on me since I started using it. Keywords are handled much the same as operators.

To avoid unnecessary parentheses, the compiler shall have a sophisticated model of precedence and associativity for operators. This is most useful for associative constructs like `f >> g >> h` or `x + y + z`. Not every pair of operators will have a precedence, however. For example, if we use both '>>' and '<<', parentheses are required. 

We'll support Haskell-style closure of binary operators, e.g. such that `((>>= k) op)` and `(op >>= k)` are equivalent. This does not extend to keywords. In general, for operators both arguments are expressions. Keywords are a mixed bag, but frequently involve special syntactic forms.

*Note:* There are no implicit conversions. Users are free to leverage ad hoc polymorphism, but the built-in keywords and operators will expect one type and report errors.

### Names and Namespaces

Names use a fairly conventional alphanumeric encoding, e.g. regex `'_'?[a-zA-Z][a-zA-Z0-9_]*`. 

Namespaces are concretely represented as dictionaries. Names desugar into atoms for purpose of indexing dictionaries, e.g. `name` becomes `.["name"]:()`. In context of hierarchical namespaces, we use dotted paths, e.g. `foo.bar.baz`.

In the general case, we support expression-indexed names. This is expressed as `.(ListExpr)` or `.[...]` for a literal list. These names are interpreted such that `.([1, .["two"]:()] ++ [3])` is equivalent to `.[1].two.[3]`. The empty path is permitted, e.g. `foo.[]` is equivalent to `foo`. Users are not encouraged to use expression-indexed names in the module toplevel.

#### Introductions and Overrides

We'll syntactically distinguish introductions vs. overrides. It's an error to introduce a name that already has a definition, or to override a name that does not have a prior definition. We use `name = Expr` for introductions and `name := Expr` for overrides. In the latter case, `.prior` implicitly refers to the previous definition. 

By default, module-scope names evaluate to final definitions, subject to overrides. But there are a few cases where we'll want to reference prior definitions. I propose `(.module_prior Expr)` and `(.module_final Expr)` to control bindings in scope. We essentially default to the `.module_final` scope.

#### Forbid Name Shadowing

Name shadowing occurs when a name masks access to another name in scope. This usually happens with generic local names like `\map -> ...` where 'map' may mean many different things (e.g. to apply a function over a list, an associative data structure, a game world map). Unfortunately, this easily results in bugs that humans easily miss when reading code: contextual usage is obvious to humans, but the compiler's interpretation of context is not.

In context of open recursion for inheritance and override, name shadowing would be even more problematic. Masking names hinders extension, and it becomes confusing what the final definition for any given use of a name refers to. 

Thus, as a rule, we'll forbid name shadowing. But only for static contexts: we forbid shadowing of names from an included module, but not for shadowing of names introduced by *including* a module. This may involve threading metadata through includes via the namespace. It isn't difficult to avoid name conflicts.

#### Abstract Definitions

In context of modules as mixins, we may assume a name is introduced without defining it locally. However, to resist errors, it is convenient to report 'undefined' errors closer to the code that leaves names undefined. To support these cases, I propose a toplevel declaration:

        .abstract Name(, Name)*

This declaration is not required for names brought into scope (or declared abstract) via 'include'. The intuition we want for include is that we're including all relevant definitions and declarations, including `.abstract`. Hierarchical names are captured implicitly, i.e. if we `.abstract foo` we don't need `.abstract foo.bar`. For convenience, `.abstract env` is implicit.

The tracking of abstract definitions across includes is essentially just maintaining a set of dotted-path prefixes that we *don't* report 'undefined' errors about. 

#### Aliasing (Tentative, Low Priority)

We can write `baz = foo.bar.baz` as a definition. However, a definition isn't an alias. In context of macros, we'd need to write `baz = .module_prior foo.bar.baz` instead. We must be aware of the distinction between overriding `baz` and `foo.bar.baz`. 

We could support aliasing in terms of rewriting names. But we should be very careful about name shadowing and stability of names. As a minimum viable aliasing model, we could support declarations of form:

        .alias baz = foo.bar.baz

This introduces a rewrite rule such that future references to `baz` expand to `foo.bar.baz`. To resist ambiguity errors, it's useful to also introduce a definition for `baz`. This could be a placeholder like `(.error "aliased")` or `foo.bar.baz` as a backup.

When basic aliasing is working properly, we could feasibly introduce a syntax for bulk aliasing. Something like:

        .using foo: bar.baz, q = qux
            # logically expands to
        .alias baz = foo.bar.baz
        .alias q = foo.qux

There are remaining design challenges, such as how aliasing should interact with includes and imports. 

### Effects

We'll almost directly adopt Haskell's do notation, `.do ...`. 

Instead of desugaring to monadic operators, we'll desugar to a tagged function of form `eff:(\__api -> Body)`, where `__api` is guaranteed to not shadow anything. The `eff` tag helps distinguish effects from pure functions and doubles as a calling convention. Within the '.do' context, we'll desugar `%name` to `__api.name`. This generalizes, e.g. `%[Expr].op` to `__api.[Expr].op`, or `%[]` to capture the effects API. The compiler will essentially use `%Seq` to compose within Body.

We'll define several operators in these terms, e.g. `op >>= k = eff:(\api -> api.Seq op k)` and `>>>` if op returns unit. For convenience, I propose `.ret` as a built-in for `\x -> eff:(\api -> api.Return x)`. Other operators and keywords may support applicative styles, e.g. `<*>` and `<|>`, `.fail` and `.cut`, etc.. 

I intend to diverge from Haskell regarding RecursiveDo, requiring explicit forward declaration of locals whose values are determined later. This improves visibility and mitigates issues like the conflict between fixpoint and continuations.

### Macros

In context of lazy loading, macro invocations must be distinct from normal evaluation. Proposed syntactic forms:

        @(Expr)
        @macro_name         short for @(.module_prior macro_name)

The compiler lazily evaluates and interprets `Expr` at compile-time. If this evaluates to a function, the compiler parses an argument `Expr`, applies the function, then repeats. Thus, macros may be parameterized as normal functions of any arity. After all arguments are read, the macro should evaluate to a `eff:(\api -> Body)` effect. The compiler provides a localized effects API then runs the effect. 

The effects API provides parser combinators to read code, supporting macro DSLs, and emitters to write code. Reads and writes both have flexible levels of abstraction, e.g. we can work with raw text, ASTs, abstract expressions, etc.. To isolate errors and simplify local reasoning, macros cannot escape their scope, and balance of brackets, braces, parentheses, etc. are preserved by both parsers and emitters. Without looking at its definition, we know `(@foo ...)` will read and write within those parentheses. Also, macros may also only read from their right-hand side.

A relevant concern is how macros interact, e.g. in context of `(@foo @bar ...)`. To keep it simple, I propose transactional semantics with a predictable schedule: each macro evaluates to completion in one step, and we always favor the earliest (i.e. leftmost, topmost) macro. This design still admits sophisticated interactions insofar as macros emit more macro invocations, but it ensures syntactic locality of such interactions.

Aside from reading and writing code, macros may provide access to other compiler-provided effects, e.g. access to built-in functions or writing messages to a log.

### Tagged Data

        tag:Data

Tagged data is modeled as a singleton dictionary. But the compiler implicitly annotates tagged data raise an error upon update (via `.with`). Thus, `{ tag:Data }` is distinct from `tag:Data` regarding opportunity for future updates. The tag generalizes to dotted-path names. The primary use case is `.[TagExpr]:Data` for a computed tag with a single-level index.

Pattern matching, in the general case of `.(TagList):Pattern`, would evaluate `TagList`, extract the indexed element while verifying that a singleton dictionary at each level, then match the given pattern. 

### Atoms

Atoms are data where the only useful observation is equality.

Constructed atoms are useful for structured data, flags, and names. The unit value is a built-in atom, expressed and matched as `()`. Tagged unit data, i.e. `tag:()` or `.[TagExpr]:()`, serves as a constructed atom. Names are indexed as constructed atoms, e.g. `.["name"]:()`, thus `tag:()` is technically shorthand for `.[.["tag"]:()]:()`. The assembler should recognize and optimize these atoms. 

Unique atoms are useful for access control and conflict avoidance. I propose a `.unique Foo, Bar, Baz` declaration at the module toplevel. These atoms derive uniqueness from the implicit path through a hierarchical module namespace, with just a little support from the assembler. Use cases include access control and conflict avoidance. 

Scope-unique atoms are useful for the ephemeron performance pattern. To support this, `.scope_unique : Atom -> Atom` returns the same atom annotated with unique metadata. When matching or comparing atoms with different metadata, we diverge with error. Thus, we never observe scope-uniqueness to be violated. When used as dict keys, we can collect associated data when metadata becomes unreachable.

### Dicts

For simple, literal dictionaries, I propose syntactic form `{ name1:Expr1, name2:Expr2, ... }`. This desugars to `{} .with { name1 = Expr1, name2 = Expr2, ... }`, where `{}` is the empty dictionary and `=` represents namespace introduction. It also generalizes to dotted-path and expression-indexed names, e.g. `{ .[1]:"hello", foo.[2]:"world" }`.

Dictionary updates are generally expressed using `.with` and `.without` special forms. These are applied much like infix operators, but the RHS isn't an expression:

        Dict .with 
            x := prior + 42
            y := 10
            .[1] = "this is new"

        Dict .without x, y, z

The `.with` syntax enforces explicit overrides, i.e. it's an error to introduce a name that already exists or override a name that does not exist. The `.without` form removes listed names if they exist, but is not an error if the name does not exist. In case of dotted-path names, it also removes empty hierarchical dictionaries in the removed path prefix. Thus, in case of `{foo:{bar:42}} .without foo.bar` the result is `{}` instead of empty directory `{foo:{}}`. 

Pattern matching on dictionaries uses the literal form with an optional remaining pattern, e.g. `{ .(Expr):(a,b,c), x:42, Pattern }`. We *evaluate* key expressions within the pattern, and we remove matched keys (via `.without`) before matching on the remaining pattern. The default remaining pattern is `{}`, requiring a complete match.

In the general case, users may want conditional behavior based on whether a dictionary contains a given field. This can be expressed in terms of pattern matching.

### Embedded Texts

Proposed syntax:

        "inline text"

        """
        " multi-line texts may include "quotes"
        " each line starts with " followed by SP
        " lines are separated by LF (no final LF)
        "   (even when host file uses CR or CRLF)
        """

There are no escape characters. Texts are always 'raw', and postprocessing is left to the user. If users want to embed a binary, that might be expressed as something like:

        """
        " 74686572 65206973 206E6F20 68696464 
        " 656E206D 65737361 67652C20 6A757374
        " 20612073 696C6C79 20657861 6D706C65
        """ |> hex2bin

That said, it is awkward to maintain embedded binaries or large texts. Users are encouraged to move large texts or binaries into separate files then load them through the module system.

### Numbers

Number literals are using the same characters as names, albeit in such a way that they don't overlap names. 

        0
        1
        _42
        1.234
        1.23e_7

        1e6
        1000000
        1_000_000

We use a prefix underscore to indicate negative numbers. This is part of the number literal, not a separate unary negation operator. Internal underscores (i.e. between digits) are ignored by the parser but may enhance legibility for humans. Decimal floating point or scientific notation can be encoded directly using an 'e' separator (or 'E', not case sensitive) for the exponent.

        0xc0de
        0b10010_00110100_11111110_11011100

We'll support hexadecimal (0x) and binary (0b) number literals, too (not case sensitive). These may be negative (e.g. `_0xff` is `_255`) though conventionally we'd only use this for natural numbers. 

We'll provide some arithmetic operators for numbers, e.g. `+ * / -`. Divide by zero will diverge lazily. We'll also support some comparisons, e.g. `> >= == =< <`. We might provide a few built-ins or accelerators for other common use cases.

Numbers are modeled as exact rationals with no bound on size or precision. Any loss of precision is under user control. This has severe performance implications, but they won't impact most assembly use cases. Where assembly-time number crunching performance is an issue, we'll develop accelerators. 

### Lists

I propose to use square brackets and commas for literal lists.

        []
        [1]
        [1,2,3]

We'll use `++` to compose lists by appending them. There is no dedicated 'cons' operator in syntax, but we can express `cons x xs = [x]++xs`. We may generally use `++` in pattern matching, limited to one variable-length list, e.g. `[x]+xs` or `xs+[x]` or `[x0,x1]+xs+[xn]`. 

Currently, there is no syntax for list length, slicing lists, etc.. We'll need accelerated functions in those roles.

### Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially a list with different connotations - fixed size, non-homogeneous - and distinct pattern matching. In practice, we'll almost always access tuples via pattern matching, e.g. `(Pattern, Pattern, Pattern)` for a triple. We can feasibly accelerate short tuples to reduce the number of allocations. There is no dedicated syntax for tuples smaller than a pair, though users are free to manually write `tuple:[a]`. 

Tuples are sometimes more convenient than small dictionaries. A relevant cost is extensibility. Tuples should mostly be used for either very stable structures or local intermediate representations.

### Functions

I propose to adopt Haskell's use of `\` for lambdas.

        \ x y z -> Expr
        \ x -> \ y -> \ z -> Expr

We'll also support Haskell-style `name args = ...` as a syntactic sugar. This extends to `:=`.

        name = \ x y z -> Expr
        name x y z = Expr
        name x y z := Expr

Pattern matching is entirely separated from argument bindings. It gets very awkward in context of overrides.

### Partial Functions

Functions may diverge in general, e.g. entering an infinite loop or dividing by zero. In practice, users will also experiment with incomplete implementations. We'll provide a little syntax for the cases where a user 

        .error Expr         recognized errors
        .tbd Expr           incomplete definitions

The expressions are visible to the reflection API, along with context. The should be something an IDE or logger can render. A string is adequate, but I suggest a dictionary or object that provides multiple interfaces for flexible filtering and views. Of course, one of those interfaces could be 'text'. 

### Pipes

Borrowing F#'s syntax here:

        f <| arg = f arg
        arg |> f = f arg

I propose to also support directional function composition:

        f >> g = \ h -> g (f h)     
        g << f = f >> g

### Booleans

Booleans need special attention in context of effectful conditional expressions.

A first take is to model booleans as atoms, i.e. `true:() | false:()`. Although this works, it does not make it convenient to mix effectful operations into conditional expressions. We can feasibly extend to `true:() | false:() | eff:(...)` where the effect returns a boolean, but I'd prefer to not mix layers like this. It may prove more convenient to directly use effectful `.ret ()` and `.fail` as booleans.

In any case, I propose `.t` and `.f` to abstract booleans and support pattern matching on booleans. 

### Modules

Need a syntax for `.import`, `.include` 

### Rejected Conditional Code

I've contemplated support for toplevel `.ifdef` and such, but I feel that conditional definitions overly complicate the namespace even before I contemplate how it should interact with overrides. Better to stick with aggregate definitions via `name := .prior ++ [...]` or similar.

### Object Specs

### Annotations



### Comments

- potential `.nb Expr`
- line comments
- disabling sections of code (`.DISABLE_START` and `.DISABLE_END` perhaps? or just leave to IDE).

### Laziness and Sparks

### Accelerators

### Language Version Declaration (Tentative)

A language version declaration enables a compiler to adapt to programs written in older versions of a language, or to detect early whether a program uses a more advanced version of the language than the compiler recognizes. But it seems much less necessary with keywords separated from user definitions.

### Pattern Matching

View patterns permit more than one match, however.

I want to desugar all pattern matching to monadic expressions, and I also want to support transactional backtracking conditionals by default. Support for 'what-if' pattern matching is simply very convenient.


Although we could support Haskell-style `match Expr with (Pattern -> Outcome)+` syntax, providing the pure handler, it's a little awkward to extend this syntax for effectful patterns, and it may be better to integrate the 'Expr' into the Pattern, allowing for more than one (e.g. as guards). I'm contemplating alternative syntax, e.g. based on unification or `Pattern = Expr` structures. We could feasibly integrate pattern matching into monads in general.

### User-Defined Types?

I would like to support lightweight declarations of type constructors and matching patterns.

### Data Embeddings

Some design constraints and desiderata:

- specialized monad for writing lists, multi-line texts? Tentative. 
- vertical structure, avoids 'deep' indentation
- user-defined types and object interfaces
  - possible type-indexed behavior bound to named types?


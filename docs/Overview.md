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

I propose to build upon a pure, untyped lambda calculus with lazy evaluation and annotations. A few data types - lists, numbers, dicts - receive optimized representations and accelerated operations. Through syntax, we'll support object-oriented inheritance in terms of [open fixpoint](http://fare.tunes.org/files/cs/poof.pdf), and monadic effects in terms of a [freer monad](https://okmij.org/ftp/Haskell/extensible/more.pdf).

The toplevel namespace is modeled in an object-oriented style, supporting inheritance and override of definitions and treating module 'include' like a mixin. This provides a foundation for open extension of assemblies and configurations.

Annotations do not influence evaluation but may influence performance, reasoning, visualization, projectional editing, and other tooling. Annotations are typically observed via effectful reflection APIs in context of annotated 'reflection tasks'.

## Performance

Performance of lambda calculus is mediocre by default, and bignum arithmetic won't help. Performance can be significantly enhanced with a little guidance. Some relevant patterns:

* *Acceleration*: Substitute a definition with a built-in. Choose built-ins that enable flexible computation, e.g. an accelerated abstract machine that can be recompiled to run on a CPU or GPGPU.

* *Parallelism*: Trigger a lazy computation early and evaluate in a background worker thread. This pattern is called 'sparks' in Haskell. We can also obtain some parallelism from acceleration.

* *Caching*: Remember expensive computations to avoid rework. Persistent caching can support incremental compilation. A shared remote cache with PKI infrastructure can support direct downloads of binaries.

These performance features may be guided by annotations or built-ins functions.

## Data

The built-in data types are numbers, lists, dicts, atoms, and functions. All data is immutable, i.e. you cannot update a list or dict, but you can construct a new list or dict in terms of updating an existing one.

- Numbers are bignum integers and exact rationals. Rounding of numbers is left to users, but the underlying representation may optimize for floating-point style usage (e.g. base 2 or 10).
- Lists are the one-size-fits-all sequential structure. Lists may be concretely represented by finger-tree ropes under the hood. Binaries are modeled as lists of small integers (0..255) and heavily optimized.
- Dicts are key-value associative structures. Keys must be comparable with equality, i.e. excluding functions. Dictionaries do not directly support iteration over keys, though it isn't difficult to maintain lists of keys. Variants are encoded as singleton dictionaries.
- Atoms are abstract data with equality. Modules may introduce guaranteed-unique atoms, and we can construct atoms from any data with equality (excluding functions). The underlying representation of atoms is hashed or interned enabling fast equality and dict lookups. 
- Functions are expressed in the lambda calculus.

Reflection APIs can bypass abstractions, e.g. to iterate a dictionary or render a function. But reflection APIs are not available for computation of the assembly result. 

## Objects

Objects are most useful for extension in context of mutually recursive structures. For example, a grammar can be modeled as an object where each 'rule' is a parser combinator, enabling override of specific rules. We'll model objects at the module layer to support extension of the namespace.

Pure functions can model stateless objects in terms of open recursion via latent fixpoint. A basic object model with mixin composition is `Dict -> Dict -> Dict` in roles `Base -> Self -> Instance`. Here, 'Base' is a parent class, initially empty, and 'Self' is a future fixpoint.

        mix child parent = λbase. λself.
           let base' = parent base self in
           child base' self
        fix f = -- lazy built-in fixpoint
            let x = f x in x        
        new obj = fix (obj Dict.empty) 

It's best to design a syntax for constructing objects that avoids observing 'base' or 'self' prior to instantiation. Otherwise, there's a good chance of datalock on 'fix'.

### Singleton Instantiation

For stateless objects, we don't need more than one object instance. Instead of presenting a `Dict -> Dict -> Dict` function, we can directly instantiate the dictionary while preserving the mixin under a special interface. To support further overrides, we may implicitly define a 'class' interface that provides the original mixin.

### Multiple Inheritance

We can feasibly model multiple inheritance, where an object inherits from several others that may share ancestors. We can apply a linearization algorithm, ensuring each shared ancestor is mixed in only once and in a consistent order. 

Linearization requires an identifier to distinguish whether two specifications are the same. We could use a class name in this role, then assert the name is always used with the same meaning within a given inheritance graph. In context of singleton instantiations, all the necessary information could be held under the 'class' interface within a dictionary.

### Explicit Override

To resist accidents, it's very useful to syntactically distinguish between introducing and overriding a name. We could aim for a lightweight syntax like '=' vs. ':=', or something more visible and obvious like '(override) name = ...', or just 'override' or '@override'. 

### Stateful Specification

Mixins can model state-like updates, treating Base as a previous state, Instance as next state, and Self as final state.

        λbase. λself. Base with { 
            v = base.next, 
            next = 1 + base.next 
        }

This can provide a foundation for allocating unique identifiers or building tables. However, this pattern easily interferes with lazy loading of modules if we aren't careful about integration. Ideally, a front-end compiler would provide dedicated syntax for safe patterns.

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
            (Alt xs) -> List.flatMap (runAlt . k) xs
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
                s' = s with { [CC] = ((ix,k):(s.[CC])) }
            (Shift ix fn) -> match s.[CC] with
                (ix',k'):cc' -> 
                    -- 'Shift' pops matched 'Reset', 'fn' may reinsert
                    let s' = s with { [CC] = cc' }
                    if (ix == ix') then runCST s' ((fn k) >>= k') else
                    runCST s' (Yield rq (\r -> Yield (Reset ix' (k r)) k'))
                [] -> error "shift index not in scope"
            (Alt xs) -> runCST s ((onAlt xs) >>= k)
            (Cut op) -> runCST s ((onCut op) >>= k)
            (Fix f) -> Yield (Fix f') k' where
                f' ~(r',_) = runCST (s with { [CC] = [] }) (f r')
                k' (r',s') = runCST (s' with { [CC] = s.[CC] }) (k r')
            _ -> Yield rq (runStateT s . k)
        runCST s (Return r) = match s.[CC] with
            ((_,k):cc') -> runCST (s with { [CC] = cc' }) (k r)
            [] -> Return (r,s)

        yield rq = Yield rq Return
        return = Return
        shift ix fn = yield (Shift ix fn)
        reset ix op = yield (Reset ix op)
        shift_reset ix fn = shift ix (reset ix . fn)

        onCut op = do
            prev <- get [BT]
            set [BT] (error "failed without alternative")
            outcome <- reset CutScope op
            set [BT] prev
            return outcome
        onAlt xs = match xs with
            [] -> shift_reset CutScope <| \_-> get [BT] >>= id
            (x:xs') -> match xs' with
                [] -> return x
                _ -> shift_reset CutScope <| \ k -> do
                    saved_state <- get [] 
                    set [BT] <| do 
                        set [] saved_state 
                        (onAlt xs') >>= k
                    k x -- current branch

        -- external 'non-deterministic' choice
        runChoice (Return r) = [r]
        runChoice (Yield rq k) = List.flatMap (runChoice . k) <| match rq with
            (Choice xs) -> xs
            (Fix f) -> fixListFn (runChoice . f)

Unfortunately, fixpoint are not fully compatible with continuations. We resolve this above by forbidding Shift across Fix. Users must be aware of this limitation if they're using fixpoints a lot, or at least they'll become aware swiftly. Backtracking conditional effects are more flexible, being modeled entirely in terms of state and continuations. The implicit Cut (in 'run') simplifies further composition.

### Extensible Monoliths

Flexible monoliths enable users to define almost any effect in terms of Get, Set, Shift, and Reset. However, the effects themselves - the recognized requests - are not very extensible. We introduced three 'generic' effects above: Fix, Cond, Alt. Where there's three, we'll eventually, inevitably want another. But it's awkward to add an effect without redefining the handler. It's essentially a closed recursive loop with mutual recursion between higher-order effects.

As described previously, *Objects* are the solution for extension in context of mutually recursive structures. 

Instead of concrete requests - Get, Set, Shift, Reset, Fix, Cut, Alt, etc. - we can desugar requests as calls to object methods, desugar monadic expressions to a lambda (`\eff -> ...`) such that 'eff' represents a threaded environment of effects APIs. Further, 'eff' may provide methods abstracting 'Return' and '>>='. 

Intriguingly, multiple inheritance gives us the extensible, stackable monad transformers we wanted in the first place. We assume 'eff.class.\*' provides access to a `Dict -> Dict -> Dict` mixin, plus class name and inheritance list for linearization and deduplication. We can easily apply a mixin to 'eff' in scope of a subprogram. Redundant mixins can be deduplicated, and most incompatible stacks can be detected early (based on violations of linearizability or explicit override).  

We can start with the foundation above, defining things like `eff.set s = Yield (Set s) Return`. But desugaring may assume access to `eff.alt` and `eff.cut` so we don't need to awkwardly wrap `(Cut op)` then unwrap to invoke `onCut` and similar. We can directly bind the effect definitions. Later, we might eliminate 'Yield' for most requests, instead threading state directly. This would significantly improve performance in context of JIT.

Ultimately, desugaring monads to object invocations is expressive, extensible, composable, and simple. Well, modulo the linearization algorithm. We could simply treat 'eff' as a keyword within do notations, analogous to treating 'self' as a keyword in class definitions. 

### Dynamic State

We can easily model a shared heap and memory allocator within indexed state. Unfortunately, we cannot easily model automatic garbage collection at this layer. Thus, users must explicitly 'free' refs. Dangling refs are mitigated because there is no need to recycle addresses, and we can easily recognize a freed ref. For performance and robust memory management, it may be useful to organize refs into hierarchical arenas that can be freed collectively.

### Concurrency

It is feasible to model cooperative threads in terms of continuation per thread and context switching state, e.g. sharing only 's.heap'. We can even model mutexes and semaphores (and deadlocks). However, modulo reflection APIs (e.g. to observe status of thunks), we cannot observe race conditions. Thus, although concurrency may be useful for decomposing large problems into interacting subtasks, it is not reliable as a basis for peformance. At best, we can spark some computations per thread.

Concurrency tends to hinder *local reasoning* about behavior. Even with a deterministic outcome, predicting that outcome may require expansive knowledge of other threads and their schedule. This can be mitigated with careful design, e.g. communicating via queues or channels, promise pipelining instead of shared state, confluence to guarantee outcome is independent of schedule.

## Modules

A module is represented by a file, and represents a mixin object. The assembler provides a built-in front-end compiler for ".g" files, but *User-Defined Syntax* is supported, with users defining a monadic front-end compilers aligned to file extensions, and the assembler bootstrapping upon override.

To simplify architecture, file dependencies are constrained: a file may reference only local files within the same folder or subfolders (no parent-relative ("../") or absolute paths), or content-addressed remote files (by DVCS revision hash and filename). File dependencies must form a directed acyclic graph. Files and subfolders whose names start with "." are also hidden from the module system.

A module is integrated by 'including' it as a mixin. Any prior definitions or inclusions effectively model prior mixins. We can translate inclusion to a dictionary defined within the host environment. Thus, we could have a few import forms:

* *include Module* - bind included module's Base to host's current Base namespace, sharing Self.
* *include Module at m* - apply module to override component dictionary 'm', i.e. binds Base->Base.m, Self->Self.m, 
* *import Module as m* - introduces 'm' with `{ "env": Self.env }` (by default), then applies 'include Module at m'.
  - This treats 'env' as an implicit, read-only environment at the module layer, supporting adaptability. Extensions to 'env' apply only to hierarchical imports.

The hierarchical 'include at' and 'import as' forms simplify lazy loading. In contrast, with toplevel 'include', it is often difficult to determine which modules introduce or override a definition without loading everything. Ultimately, there is only one 'Self'. This simplifies deep overrides and extensions, analogous to mutable definitions without actual mutation.

### Configuration

The assembler implicitly loads a configuration module based on the `GLAM_CONF` environment variable or an OS-specific default, i.e. `"~/.config/glam/conf.g"` in Linux or `"%AppData%\glam\conf.g"` in Windows. A small, local user configuration typically extends a large, remote community or company configuration.

The configuration serves several roles:

- *Assembly environment*: define 'env' as the Base argument for assembly modules. This environment can provide default target information, system includes and shared libraries, etc. for adaptation.
- *Command-line macros*: define a rewrite for command-line arguments. Applied if (and only if) the first command-line argument does not start with '-'. Supports extensible CLI 'language'.
- *Development environment*: Define the loop for interactive mode. Define an adapter for reflection tasks. Filter and rewrite log messages for standard error in batch mode. Other user-experience tuning. 
- *Resource management*: may specify GPGPUs available for acceleration, cache locations and replacement heuristcs, history management, shared proxy compilation and cache, alternative search locations for content-addressed remotes, tune assembler JIT or GC heuristics, quotas for testing, etc..

For flexibility, `GLAM_CONF` may list multiple files (same OS-specific separator as the `PATH` variable). These files are applied as mixins, i.e. files earlier in the list override those later, left to right. If there is need, we could further extend this to 'inline' or 'remote' files via special URLs. A motivating use cases for listing multiple files are to separate resource management, project-specific, and user-specific tuning.

Other environment variables do not directly influence configuration, but may be accessible in context of reflection and may influence assembler behavior (e.g. tuning JIT or GC). For portability reasons, the user configuration should have an opportunity to reflect and intervene on any features configured through environment variables.

### Assembly

The assembler receives command-line arguments that express an assembly module as a list of mixins. Though, in practice, it's usually just one file or script. Relevant arguments:

- `(-f|--file) FileName` - list a file to include; first file is included last, overriding those listed later. Depending on the configured environment, assembly isn't limited to ".g" files (see *User-Defined Syntax*).
- `(-s|--script).FileExt Text` - as remote file with given extension and text. Scripts cannot import local files, hence are location-independent. 
- `-- List Of Args` - assembler defines 'args' before including files or scripts. Default is empty list, but caller may override with elements following the '--' separator.

The namespace for an assembly starts with 'args' and 'env' from command line and configuration respectively. An assembly module shall define 'result', representing the assembled product, i.e. a binary or folder.

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
- evaluate then extract the 'result'

By default, we expect a binary result and extract to standard output. However, the assembler supports a few other filesystem-aligned options for extraction:

- *Expectation:* data type of result
  - `--binary` (default) - result is binary 
    - list of integers in 0..255
  - `--folder` - result models folder as dict 
    - dict keys are file and folder names
    - dict values are binaries or folders
- *Extraction:* where to put result
  - `--stdout` (default) - write binary to standard output
    - incompatible with folders and interactive development
  - `(-o|--out) Destination` - output to named file or folder
  - `--discard` - no output, result is ignored

Machine-code mnemonics are left to libraries. Assuming accelerators and user-defined syntax, we can adapt this 'binary' assembler to many targets: ray tracing, typesetting, websites, simulations, blueprints, etc..

### Interaction

Instead of repeatedly asking an assembler to evaluate a result, we can ask an assembler to repeatedly evaluate a result, i.e. external versus internal loops. The internal loop enables some optimizations, e.g. for incremental computing. But the main benefit is that the assembler process sticks around and is available for interrogation. Some useful possibilities: language server protocol, REPL, graphical debug views, progressive disclosure, editable projections, integrated development, etc..

The assembler shall support interactive mode via simple command-line switch:

- `--batch` (default) - evaluate and extract result once, return
- `(-i|--interactive)` - maintain result, configurable interface
  - discards result by default, but compatible with `-o`

To avoid cluttering command-line arguments, and to keep the executable small, the assembler asks the user configuration to define an interaction loop. The loop may observe environment variables, assembler capabilities, and assembly definitions. Thus, with a few conventions, we can specialize the loop to an assembly or task.

Interaction limits effects:

- *Filesystem:* Limited to files that contribute to assembly or configuration, plus associated files under ".glam/". Respects read-only restrictions. Remote files and scripts are always treated as read-only.
- *Network:* Cannot initiate connections. Listens on configured TCP ports or Unix Domain Sockets. May introduce specialized operations to synchronize local filesystem sources with DVCS repos.
- *TTY*: A standard input and output stream, modeled as an implicit network connection. Standard error is disabled.
- *Env*: access to OS environment, runtime version info, and similar features.
- *GUI*: tentative support for native GUI. Even without this, we can support GUI via Network or TTY.

The interaction loop may be expressed as a transactional 'step' method. This step runs repeatedly, subject to transaction-loop optimizations: optimistic concurrency on choice, incremental computing, await relevant update after abort. Updates to the user configuration may influence future steps. Effects are abstracted: effectful operations use constructors linked via object Base. 

### Debugging

When developing a library, it is often convenient to test entire volumes of definitions instead of just 'result' and its transitive dependencies. I propose `--test Name` that may be listed more than once. We can name entire dictionaries of definitions, such as `--test env` or `--test api`, for bulk testing. Testing includes best-effort typechecking and transitive dependencies. We'll often `--discard` the result during testing, perhaps adding `--test result`.

Tests may use non-deterministic choice to support fuzzing, property checking, and flexible analysis. In batch mode, we'll rely on configurable quotas and heuristics to determine whether we've done 'enough' testing. In interactive mode, tests may run indefinitely or based on user attention. These `--test` flags then determine an initial set of tests and user focus.

In batch mode, visualizations are filtered and rendered by a configurable method, then written to standard error. In interactive mode, we can potentially support interactive visualizations with progressive disclosure, dynamic views and queries, etc.. 

With effective acceleration, tests may emulate hardware for assembly targets. But external testing of an assembly doesn't easily feedback into debugging. It may be possible to trace a coredump to the associated sources. Perhaps we can extract a 'folder' that includes  generated together with the executable. See *Reasoning* for some patterns for debugging.

### Live Programming

Another process may continuously "run" an assembly result, watching for changes and integrating them. In context of executable machine code, this requires non-trivial setup, or at least restrictions on the function expressed by the machine code.

Although the assembler does not implement live programming directly, it should at least ensure atomic updates. That is, instead of replacing files, it first writes a temporary file then uses 'rename' in Linux or 'ReplaceFile' in Windows. Readers in Windows should open the file in FILE_SHARE_DELETE mode to avoid blocking the writer.

The interactive mode assembler may also provide 'result' via HTTP requests, perhaps with an ETAG based on contributing sources.

### History

Deterministic functions, location-independent folders, and content-addressed remote modules all contribute to reproducibility. But we still cannot reproduce the output if we cannot reproduce the initial conditions. To improve practical reproducibility, the assembler should automatically maintain a sufficient history to reproduce prior assemblies, ideally with structure sharing and effective pruning. 

In practice, this may require copying local files and maintaining a local DVCS repo to represent the history. Though, we'll also need a little attention on merging history for concurrent assembler processes. (I wonder if darcs would be a good fit here.)

## User-Defined Syntax

When loading a module, a front-end compiler is selected from the provided environment based on file extension: `Base.env.lang.[FileExt].compile` should define an effectful method to process the program. Other methods within the 'language object' may support syntax highlighting, autoformatting, linting, language server protocol, docs and tutorials, and other ad hoc tooling. But the primary method is:

        compile : Binary -> Dict -> Dict -> Eff CT Dict
            # in roles: Source -> Base -> Self -> Eff CT Instance

The compile-time (CT) effects are restricted for reasons of modularity, cacheability, and reproducibility. Also, this keeps the executable small, minimizing built-in logic. Developers of compilers are encouraged to build parser combinators and other expressive design patterns upon this foundation. CT effects include:

- loading modules, sources
- allocating unique atoms 
- emitting term annotations
- a few generics (fixpoint)

Essentially, a front-end compiler effectfully expresses an anonymous namespace mixin (`Base -> Self -> Instance`) given source code.

The assembler executable shall recognize a subset of file extensions, especially ".g", and provide built-in compilers. This is applied if (and only if) `Base.env.lang.[FileExt].compile` is undefined. The assembler further allows users to *bootstrap* a provided or built-in compiler by overriding `env.lang.[FileExt].compile` (see *Syntactic Bootstraps* below).

*Aside:* Not all syntax represents a proper namespace. However, namespace mixins maintain the opportunity for extension and adaptation. In case of compiling a ".json" or ".txt" file, a simple convention may be to define 'result' as the primary output.

*Note:* We'll moderately normalize file extensions: lower-case 'A-Z' and drop initial '.'. Multi-part file extensions are not decomposed. For example, to compile file "foo.TaR.gZ" we'll look for `Base.env.lang.["tar.gz"].compile`. 

### Syntactic Bootstraps

If the final `Self.env.lang.[FileExt].compile` method is different from the Base version, the assembler attempts bootstrap. This involves recompiling with the override version, repeating until fixpoint is reached (or a configured quota is exhausted). Pseudocode:

        bootstrap fileExt binary base compile =
            let result = runCompiler (Yield (Fix (compile binary base)) Return)
            let compiler' = result.env.lang.[fileExt].compile <|> builtin for fileExt
            if(compiler == compiler') then result else
            bootstrap(fileExt, binary, base, compile')

A built-in compiler is simply treated as one compiler in the bootstrap cycle, equivalent only to itself. 

### Editable Projections

It is possible to express editable views of source texts via something like `Base.eng.lang.[FileExt].view`. This is a good starting point, at least. But it's coarse grained and very limiting.

To support fine-grained editable projections, the front-end compiler will support annotation of terms. For example, a parsed integer might maintain enough metadata to both locate it in the original source file and edit it, with an associated codec translating an updated number into source text. This allows for editors to integrate where the term is used instead of only where defined. A subset of standard term annotations may be implicit, built into the parser combinator.

In general, we should support 'views' on individual terms that may be interactive, e.g. to view large graphs or tables we'll want progressive disclosure. It isn't feasible to predict all the demands for such up front, but we can make a best effort with ad hoc user values as annotations, examining and rendering annotated terms via reflection API.

### Macros

Macros support metaprogramming at the syntax layer. This may be expressed in terms of writing text, tokens, or AST structures. Syntax and front-end compilers may support macros via special operator for macro invocations and keywords for macro definitions. We must be careful to avoid accidental dependency on module 'Self' because macros are invoked before Self is determined.

However, many motives for macros are weakened in context of untyped higher-order programming, monadic effects, and lazy evaluation. User-defined syntax further covers potential use cases. Meanwhile, many costs of macros - added complexity and training, challenges for debugging and tooling, limited extensibility, etc. - are undiminished. The cost-benefit argument for macros becomes dubious. We'll support macros, but they won't be a primary focus of this project.

It is convenient to express macro definitions effectfully. This enables gradual extension and deprecation of compiler-supported effects, flexible integration from writing texts to terms, de facto standardization of macro effects across compiler.

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

### Reflection API

The logic required for reasoning is non-trivial. I would prefer to not maintain this logic within the assembler executable. A viable alternative is to represent this logic within the module system. The assembler, then, needs only provide a reflection API to support reasoning, e.g. to peek at function representations, manage cache, observe dataflows, write logs. The logic for 'reflection tasks', such as typechecking, may be expressed as namespace annotations within each assembly or user configuration.

It is convenient to express reflection effectfully, the assembler executable implementing a monadic reflection API. This API may be unstable, versioned with the assembler executable, albeit subject to de facto standardization. In general, reflection is non-deterministic and the API may provide shared state. Users must take responsibility for reproducibility or stability where it matters, such as typechecking. The reflection API shall not influence evaluation except through the reflection API. 

### Namespace Annotations

In general, we can express annotations at the namespace scope via simple naming conventions. For example, given a name like 'foo', we could describe its type as 'foo\_type' or 'type.foo' or another convention that an assembler or a reflection task can easily recognize and process. Of these, I prefer the 'type.foo' convention. This ensures it's relatively easy to separate type descriptions from the module (e.g. as an interface), and it mitigates the need to analyze otherwise atomic names.

A significant benefit of namespace-layer annotations is that they're available for overrides. If we override definition of 'foo', we might want to also override 'type.foo' and the types for some clients of 'foo'. In the general case, namespace extensions may represent breaking changes, thus it's very convenient that reasoning updates are also expressed in the same extension. We'll generally avoid term-level annotations for this reason.

Not every namespace annotation need be associative like types. It's ultimately just recognizing ad hoc naming conventions.

### Reflection Tasks

I propose to express reflection tasks as something like `refl.taskname = ReflTask` namespace annotations. In context of lazy loading, the assembler will 'run' reflection tasks when the module is loaded, providing the reflection API. Some tasks may install callbacks or yield, awaiting trigger conditions. For example, we might analyze the type of a method only if that method is used.

I imagine the assembler executable will ignore annotations *except* for reflection tasks. For example, although we might express type annotations, the actual type checker may be a reflection task defined in the user configuration. 

### Constraint Systems

In my vision, we attach the assembler executable to a constraint solver, such as cvc5 or Z3. We can adapt SMTLIB2 to a structured DSL for expressing constraints. It is feasible to access this constraint solver via acceleration or reflection. However, acceleration limits what we can observe: we can report sat/unsat, but not the discovered sat example or unsat kernel because discovery is implementation-dependent and non-deterministic. Reflection is a lot more flexible; the API may even support direct manipulation of the solver.

I imagine that, in practice, we'll mostly use reflection. We can defer the accelerator until we have a strong use case.

Access to a constraint solver will be very useful for analyzing properties of the assembly program or assembled product.

### Visualization

Reflection tasks may 'emit' views to be rendered as logs or presented to a user by the interaction loop. Depending on the reflection API, it may be necessary to explicitly deprecate and replace old views as sources are updated or other observed conditions change.

The reflection API does not provide effects to edit sources. That capability is left to the user interaction loop. But emitted visualizations may include interactive methods intended for execution within the user interaction loop. Thus, we can bridge the gap between visualization and projectional editing.

## Assembly Monad

It is convenient to express assembly effectfully. Relevantly, monadic expression enables us to implicitly thread extensible context as we write machine-code mnemonics or binary outputs. An extensible context is useful for both reasoning, e.g. representing abstract machine states and developer assumptions, and code generation, e.g. allocation of registers. It also simplifies composition and staging. 

The assembler never sees this monad. It must already be handled in evaluation of 'result'. But most assembly code may be written effectfully, simply assuming a suitable handler.

Some features the assembly monad and the threaded context could tackle:

- allocation of static memory (bss, data, rodata, text)
- content-addressing of read-only memory (rodata, text)
- abstract interpretations of machine 'state', e.g. types, registers in use
- allocation of registers
- logically tracking data stack frames, offsets, avoiding unnecessary updates
- heap or arena allocations, tracking logical heap and allocation 'effects'
- OS integration, e.g. tracking signaling 'effects'

This monad is user-defined, thus we have freedom to extend it and explore alternatives. However, it isn't necessarily easy to adapt existing assembly libraries to leverage these extensions. Thus, it's best to achieve some de facto standardization early. 

## Syntax

This section proposes a syntax for ".g" files, the initial syntax for glam systems. My intention is that the syntax should be pleasant to work with (at least for me), while still supporting the 'feel' of assembly code, and being readable. Pattern matching and conditional behavior need careful attention because I've been unsatisfied with how they're handled in most languages.

Some desiderata:

- Haskell-style lambdas, but pattern matching is separated from lambdas and definitions. 
- Syntactic support for controlling laziness: sequences, sparks. 
- Files limited to printable ASCII. We might expand this later, but keeping it simple for now.
- Lines may terminate with CR, LF, or CRLF. We'll implicitly translate line terminals to LF.
- Line comments only, starting with '#' Python style.
- We'll broadly organize code into logical lines or sections. 

I anticipate that most code will be developed and maintained in this syntax, with user-defined syntax focused on areas where it offers significant benefits (DSLs, graphical programming, etc..).

### Names and Namespace

Basic names may use conventional, C-style standard alphanumeric encodings, i.e. `[a-zA-Z0-9_]*` with exceptions for numbers, keywords, etc.. Names starting with two underscores are also reserved by the compiler. Namespaces are modeled as dictionaries. Names shall be abstracted as atoms for purpose of indexing the namespace. A subset of names are reserved as keywords, depending on a language version declaration.

### 

### Pattern Matching

I want to desugar all pattern matching to monadic expressions, and I also want to support transactional backtracking conditionals by default. Support for 'what-if' pattern matching is simply very convenient.


Although we could support Haskell-style `match Expr with (Pattern -> Outcome)+` syntax, providing the pure handler, it's a little awkward to extend this syntax for effectful patterns, and it may be better to integrate the 'Expr' into the Pattern, allowing for more than one (e.g. as guards). I'm contemplating alternative syntax, e.g. based on unification or `Pattern = Expr` structures. We could feasibly integrate pattern matching into monads in general.


### Keywords

import, include, module, prior, overrides, scope,

The language may include some keywords, e.g. 'import' and 'include', that are not defined by the user. Language extensibility is a concern: there may be a conflict with old code when we introduce new keywords. This is mitigated by user-defined syntax, i.e. the client of a module may manage syntax used to interpret a module. It may be further mitigated by 'pragma language' declarations.

We may provide access to keywords like 'scope' for a dictionary representing names in scope, or 'prior' for a dictionary representing the base module namespace and perhaps 'module' referencing the module-level 'self'. The 'scope' may include keyword definitions like 'module' and 'prior'.

### Comments

We'll use `#` for conventional line comments. Everything from `#` to end of line (LF, CR, CRLF) is treated as part of the comment. There is no separate syntax for multi-line comments, so bring an editor that supports commenting multiple lines at once.

### Language Version 

        ('lang'|'language') ('g0'|Alt) ('with' FeatureFlagsAndExtensions)?

The first non-comment line in the ".g" file should be a language version declaration. This provides an opportunity to develop the syntax, e.g. introducing new keywords, while sharing the ".g" file extension and front-end compiler and ensuring stable outcomes. A front-end compiler does not need to recognize all language versions, but it should raise an error rather than attempt to parse a version it does not recognize.


### Lambdas

Haskell-style `\ x y z -> Expr` is adequate, though not pretty. We could treat definitions as a special syntactic case, e.g. `name x y z = ...` rewrites to `name = \ x y z -> ...`. 

We can support Haskell-style `let x = Expr1 and y = Expr2 in Expr` or `Expr where x = Expr1 and y = Expr2` syntactic forms that desugar to applied lambdas. Monadic desugaring may also need some attention.

### Definitions

I'd prefer to avoid bulky prefixes for introducing names, such as 'define name = '. Just directly support 'name =' or 'name x y z =' for an implicit lambda. We may have special forms for specifications and other structures, e.g. `class foo(bar, baz):`.

Overrides must be declared, e.g. `overrides foo, bar, baz` as a declaration.

No true 'private' definitions at module scope, but we can use '`_name`' as a simple convention, Python style. Defining 'api.\*' is better for distinguishing a library's public API, intended for the shared environment.

### (Tentative) Built-in Definitions

We'll need a few functions to work conveniently with lists, dictionaries, etc.. Most of these might use keywords or operators. But, if necessary, we may support compiler built-in definitions. Viable approaches: 

- reserve `__name` for compiler-provided definitions
- import of compiler built-in 'modules'

I think it's probably best to support both opportunities, which merely requires reserving names that start with `__`. But I'd prefer to focus on the operators and keywords route.

### Operators

To keep it simple, operators are all defined by the front-end compiler. That is, there is no operator override except via user-defined syntax. In context of bootstrapping, it's important to ensure any user-defined operators aren't in the bootstrap path.

### Symbol Generation



### Object Specifications

We could use a 'class' or 'spec' keyword. I do favor 'spec' for better connotations, but 'class' would be more familiar. 

The names 'self' and 'base' can be implicit parameters to the class or specification, such that 'self.foo' refers to the final definition of 'foo'. Names not accessed through 'self' or 'base' refer to the module layer. We can also provide access to 'class' or 'spec' as an interface.

We may need special syntax to override specification definitions, unless I can still use '=' vs. ':=' in this role. 

### Embedded Texts

Proposed syntax:

        "inline text"

        """
        " multi-line texts may include "quotes"
        " start of each line is "SP
          " vertical alignment recommended
        " each line terminated by LF
        "   even if host file uses CR or CRLF
        """

There are no escape characters and there is no built-in formatting. Instead, users must explicitly postprocess texts, perhaps passing 'scope' to access names. This keeps it simple and flexible at the cost of being slightly more verbose.

### Namespace Capture

For metaprogramming-like tasks, such as formatting strings, it's convenient to capture the current namespace as a first-class value. I propose 'scope' as a simple keyword for this role, returning a dictionary. In context of shadowing, this dictionary would contain only the final, shadowed form of a name.

A relevant design challenge is how 'scope' should interact with syntactic sugar for monadic fixpoint. Efficient fixpoint requires minimizing scope of fixpoint. Perhaps we mitigate this by requiring explicit forward declarations for monadic fixpoint (e.g. 'future names' or 'declare names') instead of implicit fixpoints.

### Embedded Numbers

All numbers are modeled as exact rationals, no hidden size or precision limits.

        0
        1
        -42
        1/7
        1.234
        1.23e-7

It is also convenient to support binary (0b) or hexadecimal (0x) natural numbers. 

        0x1234fedc
        0b10010001101001111111011011100

Divide by zero diverges lazily, reporting an error when forced or observed. Modeling complex numbers, vectors, matrices, etc. is left to developers. Integers and base

*Note:* Exact rational numbers are not suitable for high-performance number crunching. This may be mitigated by optimizing for e.g. rationals of form `M * 2^K` (for integers M and K), using a representation analogous to floating point under the hood. But if we need good performance, we'll need acceleration to leverage SIMD or GPGPU and fixed-width numeric encodings.


### Macros

Macros can be defined normally, or may even be a computed expression, so long as it can be computed at compile-time. It's mostly invocations that require special attention, distinguishing macros from normal function calls. I propose to borrow Rust's macro invocation syntax, e.g. `name!(Args)`, also permitting `name![...]` and `name!{...}`. This is more shouty than I'd prefer, but it's acceptable if we don't need macros frequently. 

Macro definitions are expressed effectfully. We'll initially support only rewriting macro text, but we can expand from there, e.g. to access the compiler's tokenizer or AST, emit warnings or errors, update the namespace, etc.. There won't (initially) be any built-in syntax for concisely defining macros.

### User-Defined Types

I would like to support lightweight declarations of type constructors and matching patterns.








### Numbers and Arithmetic

We'll support conventional numeric representations, including scientific notations, hexadecimal, and binary. We'll also support exact rational numbers via '/'.

        1.234
        1.234e-6
        2/3
        0b10111
        0xFEDCBA9876543210

We may accelerate conversions to and from binaries, and we'll support basic arithmetic (e.g. +-*/). Division by 0 is a lazy error, halting the assembler when observed.

*Note:* There is no notion of word size or endianness for assembler-level arithmetic.

### Pattern Matching

### Rejecting Operator Overloading






### Embedded Texts and Binaries

I don't like escape characters in programs. Instead, we can embed some texts then explicitly postprocess them



### Pointers (Tentative)

We could support 'pointers' via desugaring 

### Hierarchical Names

We can support 'deep' edits to names in hierarchical dictionaries.

        foo.bar.baz = BodyExpr

It might be convenient to support something like namespaces.

        @foo.bar
        baz = BodyExpr

        [foo.bar]
        baz = BodyExpr

This does require some way to reference 'root' names. I'll also want to reference names from within objects, which should be consistent. This suggests that the rules against shadowing would apply hierarchically.

### Numbers and Arithmetic

### Introduce vs. Override



### Design Constraints and Considerations

- No shadowing. Names used within a file and scope have only one meaning.
  - Keywords may be used as names, but cannot also be used as keywords in the same file.
  - Names via 'include' are initially invisible. As are methods inherited by an object.
    - We could explicitly declare such names in scope before use to make them 'visible'.
    - Visibility may be via 'using'.
    - May support aliasing in front-end compiler, e.g. 'using x as y', to avoid conflicts.
  - Users must distinguish introduction vs. override of words. 
    - no more than one override per file or object, to preserve consistent meaning.
- No user-defined operators, modulo updating user-defined syntax.
  - In part because user-defined operators don't align nicely with imports.
  - In part because operator overloading is easily confusing to users.
- Ideally, pattern matching is extensible and composable.
  - Consider model matching monadically? I.e. match then 'Return' a result or another monadic operation. 
  - 
- No escape characters. I really hate how those explode. Use explicit postprocessing instead.
- Tests, types, visualizations, etc. are bound to names.



### Data Embeddings

Some design constraints and desiderata:

- names have one meaning in visible scope
  - i.e. no shadowing of visible toplevel names
  - may shadow an unused toplevel name or keyword
  - rule allows for extensible set of keywords
  - may need to explicitly bring included names into scope
- distinguish intro vs. override 
  - modules, objects, standard effects
  - perhaps also dictionary updates
- operators have one meaning, globally
  - no user-defined operators modulo user-defined front-end compilers
  - ad hoc polymorphism across types only if meaning is consistent
  - dotted paths need some attention here, objects vs. dicts? all dicts?
- operators for flexible function compositions
  - pipes in either direction `|>` or `<|`
  - monad composition operator `>>=`
- no escape characters, e.g. no '\22' or '\"' characters in strings
  - well, '\22' could be used, but is just 3 chars until processed
  - user-defined postprocessing of texts instead, convenient syntax
- convenient multi-line and programmed texts
  - perhaps via stream writer monad, or writing a stateful buffer
  - target buffer could be indirect, abstracted via environment 
  - should be easy to compose writers procedurally, hierarchically
- clear 'sections' for error isolation
  - can separate sections without parsing content
  - e.g. based on indentation
- can capture module namespace (self or base) as a dictionary
- few basic arithmetic operators.
- limited dependency on precedence for operators.

- optimize syntax for naming things instead of arithmetic
- effective dotted path and indexed update notations
- dictionary composition (`d1 with d2`?)
- monadic syntactic sugar, explicit 'do'
  - RecursiveDo by default
  - distinguish = and <-
  - 
- specialized monad for writing lists, multi-line texts? Tentative. 

- vertical structure, avoids 'deep' indentation
- clear sections, i.e. for error isolation or REPL-like output
- no visible shadowing, names have clear meaning across scopes
  - may shadow names that aren't visible/mentioned in outer scope
  - may require explicit 'expect/extern' to bring names into scope
- clear distinction for introduce vs. override of names
  - may enforce this in the underlying syntax 
- machine-code mnemonic sequences *looks and feels* like assembly
- lightweight, composable syntax for multi-line and computed text
  - possibly a monadic syntactic sugar? or extension thereof?
- user-defined types and object interfaces
  - possible type-indexed behavior bound to named types?
- objects may use explicit 'self' and 'base'?





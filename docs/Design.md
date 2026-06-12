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

Linearization requires an identifier to distinguish whether two specifications are the same. This should be paired with an assertion that specifications with same identifier are equivalent, such that the assumption can be verified based on structural or referential equality. In context of singleton instantiations, all the necessary information could be held under the 'spec' interface within a dictionary.

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

*Note:* A transformer variation of 'run' is feasible. We'd likely want to lift Alt, Cut, Fail, and Fix, while rewriting the higher-order effects. But *Extensible Effects* offers a better direction.

### Extensible Effects

Instead of pattern-matching request constructors like 'Get' and 'Set', we could express requests as `\api -> api.get` and `\api -> api.set s`. This efficiently supports a large number of effects and abstracts which effects are 'primitive' in terms of yielding. We can further integrate `Return` and `>>=` through this API, perhaps using `api.seq` for `>>=`. Users may test which effects are available.

I propose tag `eff:(\api -> ...)` to clearly distinguish effects from functions and serve as a calling convention of sorts.

        op >>= k = eff:(\api -> api.seq op k)
        return r = eff:(\api -> api.ret r)
        a <+> b = eff:(\api -> api.alt a b)
        using ext op = eff:(\api -> api.using ext op)

Although this is flexible enough to support stateful APIs, I believe best practice should be subprogram-scoped extensions. Above, I propose a generic `using` method that applies extension `ext` in scope of `op`. To maximize extensibility, I propose to model `api` as an object and `ext` as a mixin. Multiple inheritance can automatically deduplicate and merge compatible features. We can detect structurally-incompatible extensions based on explicit overrides and linearization conflicts.

Applying `ext` directly to `api` would gives the client final say. In practice, the host will want to control which fields or effects the client can directly override. This can be supported by instead applying `ext` to a precursor for `api`, i.e. host provides initial Base, apply stack of client `ext`, host applies final mixin. Names 'introduced' by a final mixin would conflict with user definitions.

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
- *interactive development*: `conf.ide : Eff [Refl, TTY, Net, File, GUI] ()` - see *Interaction* 
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

The assembler shall recognize 'refl.\*' tasks defined within the namespace. These tasks should be expressed effectfully, i.e. `eff:(\api -> ...)` where the 'api'  

These are run lazily, contingent on at least one definition within the names

to perform upon loading. The assembler runs these tasks concurrently upon loading a module, providing a reflection API. Use cases include testing, typechecking, visualization, and cache management. Reflection tasks do not directly influence assembly output, but they may raise warnings or errors.

To keep implementation small and simple, the provided reflection API is version-specific, specialized to an executable's representations and capabilities. The bloat of portability and policy is pushed to user-defined adapters. Users can build transactions, queues, CRDTs, etc. upon shared state and a few atomic ops.

### Interaction

Instead of repeatedly asking an assembler to evaluate a result, we can ask an assembler to repeatedly evaluate a result, i.e. external versus internal loops. Although this simplifies in-memory caching, the primary benefit is that the process remains available for interrogation. This provides a foundation for interactive debugging and development.

The assembler executable shall support interactive mode via simple command-line switch:

- `--batch` (default) - evaluate and extract result then return
- `(-i|--interactive)` - configurable user interface, maintains result
  - `--discard` by default, but compatible with `-o`

To avoid bloat, the assembler pushes interaction logic to the user configuration, running 'conf.ide' with a limited effects API. This API includes reflection and a few methods for user interaction, e.g. listening for TCP connections (or unix domain sockets) to support HTTP and other protocols. Also: access TTY, a few methods to edit sources. I hesitate to support native GUI, but I may try if the API is extremely lightweight (perhaps Dear ImGUI).

Although reflection tasks do not directly interact with the user, they may interact with the IDE. Thus, we might use reflection tasks to notify or even extend the IDE.

### Integration

The assembler does not directly support execution of assembled code, but we should at least support atomic updates to results in the filesystem. I.e. use Linux rename or Windows ReplaceFile and FILE_SHARE_DELETE. Staging might be controlled via environment variable.

### File Metadata

Aside from primary outputs, we might want the assembler to automatically set permissions on files in case of `-o` destinations. It isn't be difficult to support `asm.file_meta` to declare permissions for associated files.

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

* *Visualization*: We can support developers in viewing and understanding the assembly process, auxilliary processes such as typechecking, testing, or theorem proving, and the outcomes of these various processes. We can draw user attention where it's needed. Ideally, we can support interactive visualization, with progressive disclosure and support the user in understanding problems. Bonus points if interactive visualization integrates smoothly with editable projection.

* *Tracing*: An assembler can maintain some metadata to trace outputs back to sources. However, there's a rather severe tradeoff between precision and performance. This can be mitigated by replay with greater precision or with an intersection of different mappings. Ideally, developers may also guide tracing via annotations.

* *Profiling*: Feedback about where an assembler is spending its time is useful for identifying infinite loops and improving performance of the assembly process. It won't help much with output modulo profiling of emulations.

* *Abstract Interpretation*: We can interpret machine code against an abstract representation of machine state. With reflection, we can do similarly for lambdas. We can include assumptions and test for contradiction or consistency. It is feasible to integrate a constraint model to perform the actual checks.

### Reflection

The logic required for reasoning is non-trivial, and I'd prefer to keep it separate from the assembly executable. The proposed alternative is that the assembler provides a low-level reflection API and runs user-defined reflection tasks. Front-end compilers may further contribute, e.g. by exporting intermediate representations.

### Constraint Solver

Many forms of reasoning benefit from a high-performance constraint solver. I'd prefer to keep this separate from the assembler executable, but we could feasibly configure access to a constraint or SMT solver, whether remote or via dynamic linking. Access to this solver can be provided through the reflection API.

### Logging and Visualization

Reflection tasks won't directly interact with the user. Indirectly, they may interact with 'conf.ide', which interacts with the user. See *Interaction*. Instead of logging strings, I suggest logging 'objects' that support multiple views, including progressive disclosure and such. Of course, a 'text' view may be a common one. 

It is feasible to bridge the gap between logging, visualization, and projectional editing insofar as the reflection API can trace values to relevant sources.

## Monadic Assembly

It is very convenient to express assembly effectfully. Each mnemonic becomes an operation that writes machine code to implicit state. Assembly macros become simple procedures. Aside from machine code, we may also declare 'bss' or 'data' sections. We could also track which registers are in use to resist against accidental smashing, or even track abstract types for stack and heap memory.

With good syntactic sugar, monadic assembly can also be very lightweight. E.g. phrases such as `.movl 'eax ['ebx, 4]` might desugar as `eff:(\__api -> __api.movl 'eax ['ebx, 4])`, and do notations can support beautiful vertical columns of mnemonics. This mitigates boiler-plate pollution of the toplevel namespace for defining mnemonics, though we must be careful to avoid conflicts between mnemonics and other effects. It also cleanly separates the concerns of how we write machine code from where we express program logic.

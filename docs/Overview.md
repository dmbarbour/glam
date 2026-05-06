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

- looks and feels like well-documented assembly code
- automated verification of assumptions and reasoning
- easily visualize assembly, interactive debug views
- easy live coding, continuous feedback during change 
- flexible metaprogramming with macros and DSLs

These present significant design challenges. I'll generally prioritize system-level features over experiential properties, but some difficult design decisions are required.

## Why Another Language?

Some existing languages align with some of my desiderata. For example, F\* or Vale support reasoning, and Unison supports reproducible modularity. But none attend the whole range, much less offer the programmer experience I want. Historically, assembly languages haven't received a lot of love from programming language designers.

## Semantics

I propose to build upon a pure, untyped lambda calculus with lazy evaluation, extended with annotations and module-level gensym. I do not believe untyped lambda calculus or lazy evaluation require introduction. We model data, effects, and OO-style inheritance upon this foundation:

- Data is *logically* Church or Scott encoded. Underlying representations are accelerated.
- Monadic effects, via [Free-er Monads, More Extensible Effects](https://okmij.org/ftp/Haskell/extensible/more.pdf).
- Inheritance, adapting [Prototypes: Object-Orientation, Functionally](http://fare.tunes.org/files/cs/poof.pdf).

Users never touch the raw lambdas. Instead, we Scott encode a tagged union to distinguish numbers, lists, symbols, dictionaries, and functions. User-defined types are generally modeled as singleton dictionaries with symbolic keys. This supports ad hoc polymorphism similar to dynamic types.

The extensions are more structural than semantic:

Annotations are structured comments to support tooling. Use cases include logging, profiling, visualization, type checking, testing, tracing, acceleration, and memoization. For extensibility reasons, annotations are aligned with namespaces instead of terms, i.e. we annotate names within modules or objects.

Symbols are abstract data with equality checks. Guaranteed-unique symbols are useful for data abstraction, access control, and conflict avoidance. However, pure functions cannot locally construct globally-unique values. To mitigate this, we borrow uniqueness from the module system: modules may declare unique symbols aligned with transitive dependency structure.

## Performance

Performance of lambda calculus is mediocre by default, and bignum arithmetic certainly won't help. But performance can be significantly enhanced with guidance from annotations. This becomes relevant in context of intensive testing or metaprogramming. The most relevant patterns:

* *Acceleration*: Annotate a user-defined function to be substituted by a built-in. We can accelerate matrix arithmetic or interpretation of memory-safe DSLs for programming a CPU or GPGPU.

* *Parallelism*: Annotate that a lazy thunk will be needed later, but evaluate in a background worker thread. Depending on configuration, this may extend to remote workers. Called 'sparks' in Haskell.
  - We can also obtain parallelism from acceleration, e.g. SIMD.

* *Caching*: Annotate that a function is memoized to avoid rework. We can feasibly support both ephemeral and persistent memoization. Depending on configuration, we can feasibly share persistent memoization with a community.

Aside from these patterns, it is feasible to support just-in-time compilation of functions, and annotation-guided optimization thereof. But I won't pursue that until post-bootstrap.

## Data

The basic data types are numbers, lists, symbols, dicts, and functions. Symbols and functions are opaque, i.e. the underlying representation cannot be directly observed. Data is immutable, e.g. to 'update' a dictionary returns a new dictionary with the update applied. This can be efficient due to structure sharing and clever encodings under the hood.

- Numbers include bignum integers and rationals, without implicit overflows or loss of precision. Exact arithmetic becomes intractable within a loop, and users may need to round numbers. (For high-performance number crunching, we'll rely on *acceleration*.)
- Lists are used for all sequential structures. Large lists are represented by finger-tree ropes under the hood to efficiently support most operations. Binaries are lists of small integers (0..255) and optimized very heavily.
- Symbols are abstract data that support only equality comparisons. Symbols can be constructed in two ways: Modules may define unique, unforgeable symbols statically upon import. Any data supporting equality can be converted to a symbol.
  - A useful pattern is to declare shared symbols from URLs, e.g. `true = symbol("glam-lang.org/2026/boolean/true")`.
  - Underlying representation for a symbol may be 'interned' for efficient equality comparison.
- Dictionaries are finite key-value lookups where the keys are transitively composed of basic data. Tagged unions are modeled as singleton dictionaries. Dictionaries do not support iteration over keys containing symbols, a simple basis for data abstraction.
- Functions are, essentially, dynamically computed key-value lookups. Some computations may diverge. We cannot observe whether two functions are equal, but we can assert they are equal (via annotation).

User-defined data types will mostly be modeled as tagged unions with declared unique symbols. By hiding the symbol, this effectively serves an abstract data type, enabling the module to control construction and observation.

*Note:* Tentatively, we might support abstract *Objects* as a distinct abstraction from functions, especially with multiple inheritance. 

## Objects

Pure functions can model stateless objects in terms of open recursion via latent fixpoint. A basic object model with mixin composition is `Dict -> Dict -> Dict` in roles `Base -> Self -> Instance`. Here, 'Base' is a parent class, initially empty, and 'Self' is a future fixpoint.

        mix child parent = λbase. λself.
           let base' = parent base self in
           child base' self
        fix f = -- lazy fixpoint
            let x = f x in x        
        new obj = fix (obj Dict.empty) 

Most observations on Base or Self prior to instantiation either diverge on fixpoint or compromise extensibility. Although fixpoint divergence is easy to detect and debug, an opportunity cost to extensibility is invisible and awkward to explain. So, it's best to design a syntax for constructing objects that avoids the pitfalls.

A use case for objects is extensibility in context of mutually recursive definitions. For example, a stateless object may model a grammar. Methods, as dict values, may represent parser combinators for different cases (parsing integers, for example). The ability to update (extend, disable, etc.) a parse rule without rebuilding the entire grammar is convenient when developing variations on a language.

### Singleton Instantiation

For stateless objects, we don't need more than one object instance. Instead of presenting a `Dict -> Dict -> Dict` function, we can directly instantiate the dictionary while preserving the mixin under a special interface. 

### Multiple Inheritance

We can feasibly model multiple inheritance, where an object specification inherits from several others that may share ancestors. We can apply a linearization algorithm, ensuring each shared ancestor is mixed in only once and in a consistent order. Linearization requires an identifier to distinguish whether two specifications are the same, and a representation of the inheritance list. 

We can maintain the identifier and inheritance list alongside the mixin within the instance specification interface. Ideally, the specification interface is concrete, such that we can 'dynamically' define new objects.

### Interfaces and Access Control

We can easily support hierarchical namespaces within both modules and objects. Further, we can support indexing by an expression. Viable syntax:

        [IndexExpr] = base.[IndexExpr] with { foo = ... }
        [IndexExpr].foo = 
        [IndexExpr]:
          foo = 

I propose to model interfaces is indexing by unique symbols, i.e. such that we use a symbol expression (evaluated at module scope, no access to 'self'). We can support 'private' or 'protected' interfaces by simply controlling access to the symbol. We can also extract the interface if we want a specific 'view' of an object.

### Explicit Override

To resist accidents, it's very useful to syntactically distinguish between introducing and overriding a name. We could aim for a lightweight syntax like '=' vs. ':=', or something more visible and obvious like '(override) name = ...', or just 'override' or '@override'. 

### Effectful Specification

Mixins can easily model state-like updates, treating Base as a prior State.

        λbase. λself. Base with { 
            v = base.next, 
            next = 1 + base.next 
        }

This pattern is useful for generating symbols, building lists, tables, or other intermediate structures as part of specifying an object. We can feasibly generalize to monadic construction of objects (see *Effects*), even program search (via backtracking Choice). 

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

Effectively, '>>=' captures the continuation into 'Yield'. Unfortunately, the Kleisli composition `>=>` is left-associative, i.e. `((((k1 >=> k2) >=> k3) >=> k4) >=> k5)`. Right-associative `(k1 >=> (k2 >=> (k3 >=> (k4 >=> k5))))` performance is vastly superior. Ideally, the assembler optimizes this, e.g. by acceleration of `>=>`, similar to tail-call optimization.

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

It is feasible to compose effects via stack of 'monad transformers'. We can simply pass unrecognized effects up the stack, perhaps add some scoping rules.

        -- environment transformer with implicit Lift
        runEnvT e (Yield Env k) = runEnvT e (k e)
        runEnvT e (Yield rq k) = Yield rq (runEnvT e . k)
        runEnvT _ r@(Return _) = r

        -- state transformer with explicit Lift
        runStateT s (Yield Get k) = runStateT s (k s)
        runStateT _ (Yield (Put s) k) = runStateT s (k ())
        runStateT s (Yield rq k) = Yield rq (runStateT s . k)
        runStateT s (Return r) = Return (r,s)

        -- scope effects at boundary
        runScopeT (Yield (Outer rq) k) = Yield rq (runScopeT . k)
        runScopeT (Yield _ k) = error "unrecognized effect in scope"
        runScopeT r@(Return _) = r

Monad transformers can often be composed with a trivial runPure.

        runPure (Return r) = r
        runPure (Yield rq k) = error "unhandled effect in runPure"

        runEnv e = runPure . runEnvT e
        runState s = runPure . runStateT s

Unfortunately, monad transformers don't play nicely with higher-order effects. For example, a `runContT . runStateT s` would not include state in the higher-order `(Shift fn)` or `(Reset op)`. In practice, the best solution is to define a general-purpose *Monolithic Effects* handler, then support extension and composition of effects aligned with dimensions other than the 'call stack'.

*Note:* It is not difficult to model an IO monad with access to filesystem, network, etc.. We will support limited IO when scripting an IDE for interactive development. However, the assembler is not intended to be a general-purpose runtime, and I hope to minimize 'runtime' concerns for both safety and performance.

### Monadic Fixpoint

Haskell has *MonadFix* and a *RecursiveDo* syntactic sugar, enabling a result to be used before it is defined. In context of assembly, this would be convenient because it enables users to reference forward to labels for branches or jumps. We might encode this as `(Yield (Fix f) k)` where `f : a -> Eff rq a`. 

To evaluate a Fix request requires lazy handling of a future Return value, passing the main result back into 'f' and handling state correctly. Ultimately, 'Fix' must be passed up the handler stack and correctly handled at every step until closed by a 'runPure' or equivalent.

        runStateT s (Yield (Fix f) k) = Yield (Fix f') k' where
            f' = runStateT s . f . fst
            k' (r, s') = runStateT s' (k r)

        runPure (Yield (Fix f) k) = runPure (k (fix (runPure . f)))

Fixpoint is not compatible with all effects. But it may be feasible to restrict troublesome effects within scope of 'Fix'.

### Monolithic Effects

Instead of custom handlers per task, I propose to develop one general-purpose handler then rely on indexed state and continuations to build a library of extensible effects. We can also support 'fixpoint' for lazy futures, and non-deterministic choice for search. 

A viable general-purpose handler:

        run s = runChoice . runContStateT [] s
        
        runContStateT cc s (Yield rq k) = match rq with
            Get -> runContStateT cc s (k s)
            (Set s') -> runContStateT cc s' (k ())
            (Shift ix fn) -> match cc with
                (ix',k'):cc' ->
                    -- pop the associated Reset frame; 'fn' may restore it
                    if (ix == ix') then runContStateT cc' s ((fn k) >>= k') else
                    -- otherwise, preserve Reset frame within continuation
                    runContStateT cc' s (Yield rq (\r -> Yield (Reset ix' (k r)) k')) 
                [] -> error "unhandled shift in scope"
            (Reset ix op) -> runContStateT ((ix,k):cc) s op
            (Fix f) -> Yield (Fix f') k' where
                f' = runContStateT [] s . f . fst
                k' (r,s') = runContStateT cc s' (k r)
            _ -> Yield rq (runContStateT cc s . k)
        runContStateT ((_,k):cc) s (Return r) = runContStateT cc s (k r)
        runContStateT [] s (Return r) = Return (r,s)

        runChoice (Return r) = List.singleton r
        runChoice (Yield rq k) = List.flatMap (runChoice . k) <| match rq with
            (Choice xs) -> xs
            (Fix f) -> fixChoice (runChoice . f)

        fixChoice f = match (fix (f . head)) with
            [] -> []
            (x:_) -> x : fixChoice (tail . f)

Unfortunately, fixpoint and continuations are not fully compatible. We mitigate this by restricting scope of Shift under Fix. Performance will also suffer if we have too many Choices under Fix.

*Note:* We can easily model heap-like, dynamically allocated arenas within state.

### Transactions

The 'Choice' effect externalizes decisions. Although expressive, this makes it difficult to reason locally about performance. An alternative is to implement transactions as sequential operations by explicitly capturing and restoring state. This could be expressed as `try op onPass onFail`, equivalent to `op >>= onPass` if 'op' does not fail, `onFail` otherwise. It is feasible to implement 'try' within our monolithic handler. For example:

        symbol FailScope
        try op onPass onFail = 
            eff Get >>= \ s ->
            eff (Reset FailScope (op >>= Return . List.singleton)) >>= \ result ->
            match result with
              [r] -> onPass r
              [] -> eff (Set s) >>= \ () -> onFail
        fail = eff (Shift FailScope (const []))
        eff rq = Yield rq Return
        const c _ = c

This assumes there are no outputs other than return value and final state. Otherwise, we'd need additional state to manage an 'undo' stack. But this condition does hold for the monolithic handler.

### Threads

Threads are useful for decomposing large tasks into interactive subtasks. I don't imagine they'll see much use in assembly programming, but it is possible to model cooperative threads or coroutines via continuations and state, with locks or semaphores for coordination. It is possible to switch threads upon every Yield, but to simplify local reasoning it's best to limit context switching to specific effects like 'pause' or awaiting a semaphore. We can also model a very simple and predictable scheduler, e.g. round robin.

Continuations cover the per-thread stack, but we might also want to model thread-local storage that is swapped whenever we context switch between threads. For example, thread-local storage could maintain a notion of 'frames' for RAII-style patterns. Further, there may be a use case for explicit 'atomic' sections, where a thread cannot yield to external threads, but may yield to other threads spawned within the atomic section.

TODO: model cooperative threads, mutexes, semaphores, thread-local storage, frames, atomic sections.

### Commutative Effects

Monads easily overspecify order. In context of parallel evaluation (via sparks or acceleration), users may benefit from buffering requests or heuristic scheduling based on analyzing pending requests in cooperative threads. However, unless we eventually pursue distributed evaluation of assemblies, we probably don't need to dive into modeling queues and CRDTs.

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
- *Command-line macros*: define a rewrite for command-line arguments. Applied if (and only if) the first command-line argument does not start with '-'. Supports extensible user experience. 
- *Development environment*: Define 'ide' loop body for '-i' interactive mode. Define 'refl' to intercept reflection tasks defined in the configuration or assembly. Handle log messages in batch mode. These influence the user experience. 
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

It is not difficult to model 'private' module-local interfaces and export control via gensym. However, for extensibility reasons, we should avoid annotating private definitions (see *Reasoning (Integration)*). For consistency of annotations, we may prefer to elide 'private' definitions entirely.

An alternative approach to access control involves managing definitions accessible through 'env.\*'. As a simple convention, a module representing a shared library might define a public 'api.\*', which we then integrate into the environment:

        import "foo.g" as libfoo
        env.foo = libfoo.api
        libfoo.settings.x = 42

Providing shared libraries through the environment is analogous to 'installing' shared libraries into a filesystem. The host controls the version of libfoo, the environment visible to libfoo, and the opportunity to override configuration options such as 'libfoo.settings.\*' that aren't part of the public API. Most clients use 'env.foo.\*' as the public API.

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

When loading a module, a front-end compiler is selected from the provided environment based on file extension, i.e. `Base.env.lang.[FileExt]` should evaluate to an object or dict that defines 'compile'. This object may also define auxilliary methods for tooling or the interaction loop, e.g. syntax highlighting or a specialized language-server protocol. To normalize FileExt, we lower-case 'A-Z' and strip initial '.'. In case of ".g" files, the assembler provides a built-in as a fallback, but we favor a user-defined compiler.

The 'compile' method is expressed effectfully, as a parser combinator. The effects are carefully designed to simplify tracing of errors back to sources, isolate parse errors, support edit suggestions.

- Parser combinators are a great starting point. Parser combinators can implicitly track parse locations and describe what they 'expect' to see at any given step, providing effective feedback in case of syntax errors and metadata for tracing.
- To simplify tracing, blame, error isolation, etc. the compiler must avoid directly observing parse results. Even parse errors must be abstracted. To enforce this, we return abstract data from parse operations by default, perhaps expressed via applicative functor. We must provide built-in combinators for optionals and loops.
- In context of error isolation, we cannot model monadic 'state' at compile time in general, but we could support a few special cases where state is easily 'forked' for parsing subcomponents, especially gensym: abstract, unique, unforgeable symbol generation.
- We can feasibly support something like a 'writer' monad for tests, illustrations, editable projections, indexes, etc.. However, if we hope to preserve lazy loading, we must be careful about automatic composition of indices. Use of annotations may mitigate this.
- To support syntax-driven effects without observing the syntax, we can introduce an eval effect that lifts a user expression into the front-end compiler. The result of eval is then abstracted. This effectively supports some forms of macros.
- It is useful to support multi-pass parses. For example, a first pass might delimit the 'region' for another pass. Even if we drop the first pass result, this can be useful for error isolation.
- Compiler keeps module dictionaries (Base, Self) abstract, user only accesses them indirectly. This simplifies monadic tracking of dependencies.

Expressing a compiler without being able to 'see' the binary seems possible in theory, but I'm not entirely convinced that we won't reintroduce the tracing problem via Eval. To gain confidence, I must try it first with the standard syntax. After all, we should be able to override and extend the standard syntax, too. Worst case, I'm back to the conventional approach.

### Syntactic Bootstraps

If the final `Self.env.lang.[FileExt].compile` method is different from the Base version, the assembler attempts bootstrap. This involves recompiling with the Self version, repeating until fixpoint is reached.

        bootstrap(ext, base, binary, compiler) =
            let m = compile(compiler, base, binary) 
            let compiler' = 
                m.env.lang.[ext].compile if defined
                or builtin if ext is "g"
            if(compiler == compiler') then m else
            bootstrap(ext, args, compiler')

A built-in compiler is simply treated as one more compiler in the bootstrap cycle, equivalent only to itself. 

### Editable Projections

Some user-defined syntax may be graphical. And even for purely textual syntax, we'll often want to integrate some visualizations or provide edit widgets like color pickers. Miscellaneous observations:

- We can only edit terms annotated with source locations, parser context, and an encoder that converts the parse result back into source (aka lenses or prisms). We can verify any edits for round-trip between parse and encode.
- Some content is naturally read-only, e.g. content-addressed remote files, read-only local files. In these cases, we can still support navigation, views, etc.. and partially-editable views are feasible.
- Support for navigation, progressive disclosure, etc. benefits from user-local or session state. 
- It is convenient to treat a file that does not exist as equivalent to an empty file for purpose of imports and projections.
- Ideally, we can obtain immediate feedback on edits by visualization of tests. This may require some integration of projections and tests.

TBD: This is non-trivial, and I don't have a solid handle on exactly how to approach it.

### Language Versioning

We may update front-end compilers with new features. When these updates are not backwards compatible, we may need to switch to an older version of the compiler for processing specific files. But this is awkward for the root ".g" language. For that case, we may benefit from an explicit language version selector near the top of each file, perhaps expressed as a pragma.

In practice, we can always override the front-end compiler for remote files, and we can always edit local files. So, we probably don't need the pragma. But it seems like a convention that contributes to more-robust systems development.

## Reasoning

What can we feasibly implement to support developers in reasoning about the assembly process and product?

* *Types*: We can annotate assumptions about programs and data in a composable, machine-checkable way. It is feasible to reason about an executable binary's runtime behavior insofar as it is encoded into types. We may leverage 'type interpreter' functions that compute type annotations from data or AST embeddings, similar to GADTs. In context of gradual or partial type annotations, we must assume not every term is typed, and the assembler makes a best effort to prove or disprove types.

* *Tests*: We can sample subprogram behavior under various conditions. With acceleration, it is feasible to emulate execution of machine code. Tests should support heuristic, non-deterministic choice to include fuzzing and property checking and race conditions, enabling the assembler to heuristically choose tests from a vast space for maximal branch coverage. Tests should be a convenient foundation for visualization of program behavior.

* *Contracts*: Contracts might describe a monadic subprogram's preconditions, postconditions, and invariant assumptions in a locally testable way, perhaps via assertions. A relevant challenge is how to effectively use contracts in context of staged metaprogramming of an executable binary.

* *Theorems*: The main difference between a theorem and a test is a 'forall' or 'there exists', and the need for abstract interpretation to support the proof. In practice, we may need explicit proofs or proof-hints to efficiently verify theorems. 

* *Visualization*: Start with logging and profiling, extend to graphical views of tests, progressive disclosure or search, etc.. I propose to model log messages as dicts or objects implementing multiple views. Plain text is one view, but we can feasibly render icons, interactive widgets, etc..

* *Tracing*: Maintain metadata to trace outcomes (errors, data, etc.) back to contributing sources. There's a tradeoff between precision and performance. We can feasibly leverage reproducibility by replaying a computation under a few different tracer setups to obtain more precision.

* *Abstract Interpretation*: Given a representation of a program (e.g. machine code) we can implement an 'interpreter' using variables instead of data. Users add their own assumptions about this interpretation, then we check for conflicts. Essentially, we can mechanically implement a type system scoped to our target. Can feasibly integrate types via 'type interpreter' functions, and tests via constraint systems.

Ideally, we can support these mechanisms without overly complicating the assembler executable. This suggests a common mechanism that provides users the tools to implement everything else. For this role, I propose reflection.

### Reflection

In the general case, reflection is troublesome for local reasoning and reproducibility. Reflection can easily break abstractions and access control, observe optimizations such as caching and partial evaluation, analyze resource consumption. It is also difficult to develop a stable reflection API up front. It is much more convenient to implement whatever's easy and useful, develop adapters for portability, allow inertia and de facto standardization to carry the system.

We mitigate these concerns by restricting scope of reflection to reasoning annotations, the '-i' interaction-mode loop, and other configuration options. Then, although reflection may change or even break, assembly output remains unaffected. 

Use cases:

- Lazy testing or typechecking may await observation of thunks.
- Visualizations and type checks may tap actual arguments and contexts.
- Visualize locations, where things are defined and used.
- Compare functions for referential or structural equivalence.
- Support ad hoc abstract interpretation, static analysis, proofs.
- Compile a monadic interactive mode GUI to JavaScript or WASM.
- Visualize and manage cache, acceleration, sparks, distribution.

It is convenient to express reflection effectfully. This enables a reflection API to maintain local contexts, abstract over graph structures, write logs, manage checkpoints, explore non-deterministic choices, and generally align with the assembly executable.

If implemented well, reflection provides a singular mechanism and foundation upon which all other reasoning tools may be built. This also reduces the amount of logic that must be embedded within the assembler executable. Of course, it is okay for an assembler version to provide a built-in typechecker or whatever via extensible reflection API.

### Integration

In context of potential non-monotonic or 'breaking' changes, reasoning annotations should be represented in the namespace, providing an opportunity to override and repair broken types, tests, visualizations, etc.. That is, we reject 'anonymous' annotations. A minimum viable foundation: the monad for user-defined syntax shall provide an abstract mechanism to activate (and deactivate) definitions by name as reflection tasks. We may extend this to hierarchical activations and deactivations for modules and objects.

In context of lazy loading, it is convenient to support lazy testing. To support this, I propose a reflection API that asks the runtime to await a list of thunks, proceeding only when at least one thunk in that list is forced externally.

Aside from conventional reflection operations, the assembler executable should support monadic fixpoint and non-deterministic choice. Non-deterministic choice provides a simple mechanism to explore a space, whether for testing or to independently handle reflection subtasks. Ideally, we support both continuous and discrete choice, i.e. rational and integral.

For extensibility and portability, the user configuration has an opportunity to intervene by defining its own 'refl' handler for reflection tasks. This may represent adapter, filter, or other features.

*Note:* A front-end compiler may introduce specialized names like 'foo/type' under-the-hood, so long as the annotation is associated with a name. These names are not interpreted by the assembler, however.

### Constraints

Constraint systems are a convenient mechanism for abstract interpretation. We can feasibly accelerate constraint systems with cvc5, Z3, or other solvers. In the general case, the accelerator cannot observe only the sat/unsat judgement, the solution being non-deterministic. Fortunately, we can also provide access to the constraint solver as part of the reflection API. In this context, observing a solution is permitted, which is convenient for visualization.

Ideally, the DSL for constraints is adapted almost directly from SMTLIB2, albeit structured. For variables, we might use `(Var x)`, where 'x' is any valid dictionary key. Users provide their own naming schemes and allocators.

I envision building a constraint system statefully as part of building an assembled program. We could check constraints before reaching the program endpoint. In other contexts, we might check constraints as part of modeling a dependent type system.

### Visualization

In context of interactive mode, every 'update' to the program will reset relevant reflection tasks. In context of rich structures, we'll inevitably want some 'view' state for progressive disclosure. It's important to maintain a relatively stable visualization and view state across updates, hence view state must be separate from reflection tasks, yet indexed in a stable manner.

In interactive mode, the configured interaction loop controls which elements are rendered. It may receive some means to query and filter views, to duplicate them, to manage view states, etc.. In batch mode, the configuration may provide a simplified mechanism to filter and render visualizations for standard error. Ideally, logging is still relatively stable to simplify comparisons of logs.

If a reflection API emits multiple visualizations, it may be convenient to support integration between them, i.e. adjusting a slider impacts multiple views in scope instead of just one view. This suggests something like a global 'view' state, albeit optionally allocating a local view identifier. Ideally, the user may view state, bookmark it, reset it, etc..

Visualization is ultimately limited to what we can access through reflection. It is feasible to support reflection within evaluation of a view, enabling a more continuous view of reflection state alongside view state. If we ultimately support projectional editing through reflection APIs, visualization could directly bridge projectional editing.

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

## Standard Syntax

This section proposes the initial syntax for ".g" files. We'll be limited to minor changes, so I'll take some effort to make it good.

### Names and Namespaces

Will use the conventional alphanumeric encodings, i.e. `[a-zA-Z0-9_]*` with a few exceptions. Dotted paths support 'deep' access into hierarchical dictionaries. Convenient access to methods in object specifications also needs some attention. We can include dictionary indices as names or within names, e.g. `[IndexExpr] = ...` or `foo.[IndexExpr]`. 

The language may include some keywords, e.g. 'import' and 'include', that are not defined by the user. Language extensibility is a concern: there may be a conflict with old code when we introduce new keywords. This is mitigated by user-defined syntax, i.e. the client of a module may manage syntax used to interpret a module. It may be further mitigated by 'pragma language' declarations.

We may provide access to keywords like 'scope' for a dictionary representing names in scope, or 'prior' for a dictionary representing the base module namespace and perhaps 'module' referencing the module-level 'self'. The 'scope' may include keyword definitions like 'module' and 'prior'.

### Lambdas

Haskell-style `\ x y z -> Expr` is adequate, though not pretty. We could treat definitions as a special syntactic case, e.g. `name x y z = ...` rewrites to `name = \ x y z -> ...`. 

We can support Haskell-style `let x = Expr1 and y = Expr2 in Expr` or `Expr where x = Expr1 and y = Expr2` syntactic forms that desugar to applied lambdas. Monadic desugaring may also need some attention.

### Definitions

I'd prefer to avoid bulky prefixes for introducing names, such as 'define name = '. Just directly support 'name =' or 'name x y z =' for an implicit lambda. We may have special forms for specifications and other structures, e.g. `class foo(bar, baz):`, or `symbols x, y, z`, avoiding '=' in these cases.

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

We could use a 'class' or 'spec' keyword. I do favor 'spec' for better connotations, but 'class' would be more familiar. We can also define 'interfaces' that objects implement. It might be useful to align syntax, e.g. `[interface]`, for symbolic indices.  

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

### Pattern Matching

I'd like to support flexible pattern matching, including user-defined constructors and other patterns.



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
- symbol generation is always via static toplevel declared names
  - not even appearance of generating symbols within a function
  - first-class "objects" defined in functions have explicit tags 
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





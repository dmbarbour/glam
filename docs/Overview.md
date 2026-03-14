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

- Data is logically [Church or Scott encoded](https://en.wikipedia.org/wiki/Church_encoding). 
- Monadic effects, via [Free-er Monads, More Extensible Effects](https://okmij.org/ftp/Haskell/extensible/more.pdf).
- Inheritance, adapting [Prototypes: Object-Orientation, Functionally](http://fare.tunes.org/files/cs/poof.pdf).

Users never touch the raw lambdas. Instead, we Scott encode a tagged union to distinguish numbers, lists, symbols, dictionaries, functions, objects, etc.. This supports ad hoc polymorphism similar to dynamic types.

The extensions are more structural than semantic:

Annotations are essentially structured comments to support tooling. Use cases include logging, profiling, visualization, type checking, testing, tracing, acceleration, and memoization. But annotations must not be observable *within* the assembly.

Symbols are abstract data with equality checks. Unique symbols are useful for data abstraction, access control, and conflict avoidance. However, pure functions cannot locally construct globally-unique values. To mitigate this, the module system serves as a source of uniqueness: during import, modules may generate unique symbols.

## Performance

Performance of lambda calculus is mediocre by default, and bignum arithmetic certainly won't help. But performance can be significantly enhanced with guidance from annotations. This becomes relevant in context of intensive testing or metaprogramming. The most relevant patterns:

* *Acceleration*: An annotation requests that a user-defined function is substituted by a specified built-in. The assembler performs ad hoc verification then replaces the function or emits a warning. Although we can accelerate individual functions such as matrix multiplication, the more general solution is to develop memory-safe DSLs that easily compile to CPU or GPGPU, then accelerate interpreters for those DSLs.

* *Parallelism*: An annotation indicates that a lazy thunk will be needed later. The assembler adds the thunk to a queue to be processed by a background worker thread. Depending on configuration, this can be extended to remote workers. (This pattern is called 'sparks' in Haskell.)

* *Caching*: An annotation suggests specific functions should be memoized to avoid rework. The annotation may provide further guidance on mechanism: persistent or ephemeral, cache size and replacement policy, coarse-grained lookup table versus fine-grained decision-tree traces. Depending on configuration, and leveraging PKI signatures and certificates, it is feasible to share remote cache with a trusted community.

Aside from these patterns, it is feasible to support annotation-guided just-in-time compilation of functions. Although, assembler functions are only used at assembly-time, this could offer significant performance benefits for testing. But probably not worth pursuing before bootstrapping the assembler.

## Data

The basic data types are numbers, lists, symbols, and dicts. Data is immutable, i.e. to 'update' a dictionary returns a new dictionary with the update applied. This can be efficient due to structure sharing and clever encodings under the hood.

- Numbers include bignum integers and rationals, without implicit overflows or loss of precision. Exact arithmetic becomes intractable within a loop, and users may need to round numbers. (For high-performance number crunching, we'll rely on *acceleration*.)
- Lists are used for all sequential structures. Large lists are represented by finger-tree ropes under the hood to efficiently support most operations. Binaries are lists of small integers (0..255) and optimized very heavily.
- Symbols are abstract data that support only equality comparisons. Symbols can be constructed in two ways: modules may declare guaranteed-unique symbols when imported, and any composition of basic data may be abstracted as a symbol.
- Dictionaries are finite key-value lookups where the keys are transitively composed of basic data. Tagged unions are modeled as singleton dictionaries. Dictionaries do not support iteration over keys containing symbols, a simple basis for data abstraction.

User-defined data types will mostly be modeled as tagged unions with declared unique symbols. By hiding the symbol, this effectively serves an abstract data type, enabling the module to control construction and observation.

## Objects

Pure functions can model stateless objects in terms of open recursion via latent fixpoint. A basic object model with mixin composition is `Dict -> Dict -> Dict` in roles `Base -> Self -> Instance`. Here, 'Base' represents the mixin target or host environment, and 'Self' the future fixpoint.

        mix child parent = λbase. λself.
            (child (parent base self) self)
        new spec env = fix (spec env)
        # fix is lazy fixpoint

Most observations on Base or Self prior to instantiation will either diverge on fixpoint or compromise extensibility. Although fixpoint divergence is easy to detect and debug, an opportunity cost to extensibility is invisible and awkward to explain. It's best to provide a syntax that avoids potential pitfalls.

Inheritance and override is a useful mechanism for extensibility. For example, we can model a grammar as an object where the methods represent parser combinators, then extend the integer parser. In context of binary specification, available overrides will likely be less structured, more organic, but still useful.

Note that implementation inheritance is the focus, not subtyping or substitutability. Extensibility is producing useful variations of a system without invasive edits. But useful variation doesn't imply monotonic updates. For example, it might be useful to disable a parse rule to restrict a language.

*Aside:* I'll call 'specification' or 'spec' what most OO languages call 'class'. I feel specification has cleaner connotations, notably avoiding connotations of subtyping.

### Explicit Override

To resist accidents, it's useful to syntactically distinguish between introducing a name and overriding a name. Doesn't need to be much, e.g. `=` vs. `:=` is probably sufficient. Will figure this out when I start detailing syntax.

### Multiple Inheritance

Multiple inheritance is convenient when composing systems that build upon common foundations or frameworks. Given `C:A,B` and `A:F`, `B:F`, where F is the shared framework and C is the composition, we want a final mixin order `C,A,B,F`. Relevantly, F is not duplicated and appears *after* both A and B, ensuring consistent order, though A's view of F is now influenced by B. 

Multiple inheritance is implemented by reifying dependencies then applying a linearization algorithm such as C3. Of course, lambdas are incomparable. And we probably shouldn't compare objects based on shared behavior regardless - it's 'purpose' we want to avoid duplicating. To support linearization, we'll pair functions with tags as proxy to purpose. 

By default, we'll implicitly declare a globally unique symbol to tag every syntactically-defined object. This probably covers 99.9% of use cases. We can let users explicitly provide a symbolic tag to cover the exceptions.

Tags don't need to be globally unique but must not be reused in scope of linearization. To express and verify this local-uniqueness assumptions, we can introduce annotations for asserting incomparable values are equivalent. Although the assembler cannot prove equivalence of functions in general, it at least can warn when not obviously equivalent (i.e. referentially or structurally), or raise an error if obviously not equivalent. 

### Symbolic Method Names

Using short strings as method names, especially generic and contextual names like "map" or "draw", easily leads to ambiguities and collisions, especially in context of dynamic mixins and multiple inheritance. There are other weaknesses: no feedback upon deprecating a name, no clear mechanism for private names.

A robust alternative is symbolic names. 

Modules may generate unforgeable symbols upon import, then export them. By using symbols as method names, we eliminate ambiguity and we gain object-capability security for individual methods, a secure yet flexible basis for privacy.

At external tooling boundaries, e.g. logging or IDE, it is awkward to reference names through the module system. Instead, we'll construct global symbols such as `symbol("glam-lang.org/2026/log/text")`. We can at least ensure these symbols are unambiguous based on naming convention.

At the module layer, we use short strings: they're necessary for concise syntax! Fortunately, modules are constrained in ways that mitigate concerns of collision: no multiple inheritance, static mixins only (via include), and *Explicit Override* still applies.

## Effects

Even without a runtime, effects are convenient for implicit dataflow, backtracking and error handling, flexible composition, extensible behavior, etc.. I'll use Haskell(-ish) syntax to describe these, but it should be easy to translate.

        type Eff rq a =
            | Yield (rq x) (x -> Eff rq a)
            | Return a

We can model a free-er effects monads as either *yielding* a `(request, continuation)` pair or *returning* a final answer. In case of yield, the continuation expects the response type from the request. Of course, in context of untyped lambda calculus and gradual type annotations, enforcement may be a bit shoddy.

We can easily introduce some syntactic sugar:

        (sugar)         (monadic ops)
        a <- op1        op1 >>= \ a ->
        op2             op2 >>
        op3 a           op3 a

Haskell also has a *RecursiveDo* sugar, enabling a result to be used before it is defined. I'm less familiar with this desugaring, but we'll probably want RecursiveDo by default. It seems convenient for branching to forward labels, for example.

We can specialize the monadic operators for our only monad. Our untyped lambda calculus doesn't offer a direct solution to type-indexed behavior, such as typeclasses, so this is convenient:

        (Yield rq k1) >>= k2 = (Yield rq (k1 >>> k2))
        (Return a) >>= k = k a
        k1 >>> k2 = (>>= k2) . k1

Effectively, '>>=' captures the continuation into 'Yield'. Unfortunately, it grows left-associative, i.e. `((((k1 >>> k2) >>> k3) >>> k4) >>> k5)`. Right-associative `(k1 >>> (k2 >>> (k3 >>> (k4 >>> k5))))` performance is vastly superior. To resolve this, I propose to *accelerate* function composition (the '.' op) to dynamically rewrite to right-associative. 

Behavior is embodied in the runners aka handlers. It is convenient to express 'stacks' of partial handlers for local subtasks that forward unrecognized requests. Basically, any effect can be encoded except race conditions (outcome is deterministic). I wrote a few examples to help myself grasp this: 

        -- generalize Reader to pure queries; forwards unhandled queries
        runEnvT env (Yield (Env q) k) | Just r <- env q = runEnvT env (k r)
        runEnvT env (Yield rq k) = Yield rq (runEnvT . k)
        runEnvT _ r@(Return _) = r

        -- generalize State to indexed, hierarchical Memory
        -- use guaranteed-unique symbols to avoid conflicts
        runMemT m (Yield (Mem idx op) k) = match idx with
            Outer idx' -> Yield (Mem idx' op) (runMemT m . k)
            _ -> match op with
                Get -> runMemT m (k (m.[idx])) 
                Put v -> runMemT (m with { [idx]: v }) (k ())
                Del -> runMemT (m without idx) (k ())
        runMemT m (Return r) = (Return (r, m))

        -- delimited continuations (hierarchical)
        runContT (Yield (Cont (Reset op)) k) = runContT op >>= runContT . k
        runContT (Yield (Cont (Shift fn)) k) = runContT (fn k)
        runContT (Yield (Cont (Outer rq)) k) = Yield (Cont rq) (runContT . k) 
        runContT (Yield rq k) = Yield rq (runContT . k)
        runContT r@(Return _) = r

        -- cooperative threads (round robin, non-preemptive, hierarchical)
        --  with per-thread continuations (to model mutexes, semaphores)
        runThreadT (Yield (Thread op) k):ts = match op with
            (Spawn t) -> runThreadT (k ()):t:ts
            Pause -> runThreadT (ts ++ [k()])
            (CallCC fn) -> runThreadT (fn k):ts
            (Outer rq) -> Yield (Thread rq) (runThreadT . (:ts) . k)
        runThreadT (Yield rq k):ts = Yield rq (runThreadT . (:ts) . k)
        runThreadT (Return ()):ts = runThreadT ts
        runThreadT [] = Return ()

We can also model runners that scope effects:

        -- forbid effects from escaping
        runPure (Return r) = r
        runPure (Yield _ _) = error "unhandled effect in runPure"
 
        runEnv env = runPure . runEnvT env
        runMem m = runPure . runMemT m
        runCont = runPure . runContT
        -- pure 'runThread' is useless

        -- pure choice can be heavily optimized with laziness
        runChoice (Yield (Choice xs) k) = List.flatMap (runChoice . k) xs
        runChoice (Yield _ _) = error "unhandled request in runChoice"
        runChoice (Return result) = List.singleton result

We'll need a library of useful, reusable handlers. However, I hope we deliberately design handlers with flexibility and extensibility in mind! Regular users should rarely feel the need to write custom handlers. Any 'good' monadic API is essentially architecting a framework.

*Note:* It would not be difficult to leverage monadic effects for general-purpose programming, like Haskell. However, I fear dilution of design.

### Commutative Effects

Monads easily overspecify order. Many effects can be at partially reordered without influencing outcome, yet with a significant impact on performance. To mitigate this, it can be useful to model asynchronous and threaded effects.

Asynchronous effects enable the runner to aggregate several requests from a single thread, enabling a reordering before we compute observations. Threaded effects enable the runner to examine pending requests from multiple threads before making any decisions.

        [(Yield BranchingQuery k1), (Yield TightConstraint k2), ...]
        -- constraint-choice runner: let's apply TightConstraint next!

These techniques work very well together, e.g. we can model asynchronous interactions between threads. They also work nicely with spark-based parallelism: a runner heuristically sparks evaluation for a past request when the data becomes available, or a runner can batch-process several threaded effects then parallelize evaluation of each thread's next step.

## Modularity

A module is represented by a file. Modules may reference other files within the same folder or subfolders, or a content-addressed remote. We forbid Parent-relative ("../") and absolute filepaths. These constraints ensure folders are location-independent and temporally stable, yet editable by conventional means.

Modules are modeled as basic objects with limited effects during construction, conceptually `Dict -> Dict -> {import, gensym} Dict` (in roles `Base -> Self -> Instance`), but this is abstracted by an effects API for *User-Defined Syntax*. There are two mechanisms to integrate more modules:

* *include* - bind included module's Base to host's 'current' Base; share Self. Effectively applies a module as a mixin, also treating prior definitions as mixins. Eager evaluation.
* *import* - binds imported module's Base to a host-provided environment (e.g. `{ "env": Self.env }` by default). Defines local name to instance dictionary. Lazy loading. To 'override' an import, simply replace the definition.

Dependencies between files must form a directed acyclic graph. However, each import or include is independent for gensym and Base, and it's awkward to maintain scattered references to content-addressed remotes. In most use cases, we'll share definitions through 'env.\*' instead of loading a module twice.

To ensure extensibility, there are no 'private' definitions or export controls at the module layer except via naming convention, such as Python's `"_name"`. Access to private definitions is mitigated by content-addressing of remote dependencies, i.e. a breaking change won't propagate implicitly.

### Configuration

The assembler implicitly loads a configuration module based on the `GLAM_CONF` environment variable or an OS-specific default, i.e. `"~/.config/glam/conf.g"` in Linux or `"%AppData%\glam\conf.g"` in Windows. A small, local user configuration typically extends a large, remote community or company configuration.

The configuration serves several roles:

- *Assembly environment*: define 'env'. This is passed to the assembly as if importing the assembly into the configuration, e.g. `{ "env": Config.env }`. This environment is analogous to system includes and shared libraries, supporting adaptive assembly. 
- *Command-line macros*: if the first command-line argument to the assembler does not start with '-', we apply 'cli', which should be a function of type `List of String -> List of String` that returns valid arguments or empty list.
- *Development environment*: Define an *Integrated Development Environment* for '-i' interactive mode. Filter outputs to standard error. Decide which logs are saved to disk, and at what level of detail.
- *Resource management*: ad hoc, e.g. specify GPGPUs available for acceleration, cache locations and replacement heuristcs, history management, shared proxy compilation and cache, search locations for content-addressed remotes, tune assembler JIT or GC heuristics, control expensive tests and checks (e.g. assertions, fuzzing).

Configurations never directly control assembler output: An assembly may ignore your configured environment and substitute its own. Command-line macros may always be written out long form. Resources influence performance and error detection but not a valid binary result.

To support project-specific overrides or sharing of a system configuration, `GLAM_CONF` is not limited to one file. Users may list multiple files (same OS-specific separator as the `PATH` variable). We apply these as mixins, each file overriding those listed later.

### Assembly

The assembler receives command-line arguments that express an assembly module as a list of mixins. Though, in practice, it's usually just one file or script. Relevant arguments:

- `(-f|--file) FileName` - list a file to include; first file is included last, overriding those listed later. Depending on the configured environment, assembly files aren't limited to ".g" (see *User-Defined Syntax*).
- `(-s|--script).FileExt Text` - behaves as a remote file with the given file extension and text. Scripts cannot import local files.
- `-- List Of Args` - assembler defines 'args' before including files or scripts. Default is empty list, but caller may override with elements following the '--' separator.

Aside from 'args', the assembly module is implicitly parameterized by the configured 'env'. The assembly module shall define 'result', representing the assembled product, i.e. a binary or folder.

### Remotes

The assembler will support content-addressed remote folders or files. The source is uniquely identified and authenticated by secure hash of content or DVCS revision. However, remotes are not *located* by secure hash. The developer may supply a list of locations to search. The user configuration may rewrite this list to suggest alternatives, adjust priorities, etc..

For DVCS, we also name a tag or branch. Not to replace the hash, but to reduce network traffic, e.g. `"git clone -b Branch --single-branch URL"`. If the branch has updated, we can may force to an earlier revision but also warn that the remote has been updated so users may update.

I intend to start with support for 'git'. Perhaps add mercurial and darcs later. It is feasible to hash individual files and access them via HTTP (like the dhall configuration language), but my intuition is that DVCS offers the superior maintenance experience.

## Assembler

Primary behavior and inputs are detailed in *Modularity*. Roughly:

- load a configuration (`GLAM_CONF`) 
- construct an assembly (`-f -s --`)
- evaluate then extract the 'result'

By default, we expect a binary result and extract to standard output. However, the assembler supports a few other filesystem-aligned options for extraction:

- *Expectation:* data type of result
  - `--binary` (default) - result is binary 
    - simply a list of integers in 0..255
  - `--folder` - result models folder as dict 
    - dict keys are file and folder names
    - dict values are binaries or folders
- *Extraction:* where to put result
  - `--stdout` (default) - write binary to standard output
    - incompatible with folders and interactive development
  - `--discard` - extract for testing; drop the data
  - `(-o|--out) Destination` - output to named file or folder

Although we could model folders via tarball or zipfile, making folders explicit is more convenient in context of interactive development.

Machine-code mnemonics are entirely left to libraries and syntactic sugars. Assuming suitable accelerators and user-defined syntax, it should be easy to adapt the assembler to many targets: configuration files, ray tracing, typesetting, constructing websites, simulations, etc..

The above covers basic non-interactive extraction of a completed assembly product. But there is a lot more to say about development and debugging!

### Development

Instead of repeatedly asking an assembler to evaluate the result, we can ask an assembler to repeatedly evaluate the result, i.e. external versus internal loops. An internal loop introduces intriguing opportunities:

- efficient, in-memory incremental computing
- on-demand output (user request or libfuse)
- interaction via HTTP, native GUI, or TUI

Of these, interaction has the greatest impact on the developer experience. The assembler can implement an integrated development environment, editable projections, language server protocol, graphical and interactive visualizations with filters, sorts, progressive disclosure, etc..

- `--batch` (default) - evaluate and extract result once
- `(-i|--interactive)` - interactive mode with continuous assembly
  - `--fuse` (tentative) - 'mount' folder to '-o' destination

Instead of cluttering the command line with dozens of interaction options, the assembler asks the configuration for a *Integrated Development Environment* (IDE), modeled as an object. This IDE receives various capabilities from the assembler, including access to environment variables and the assembly. Thus, with a few conventions, there is an opportunity for task-specific and assembly-specific tweaks without updating the configuration.

The IDE does not receive general access to effects, merely enough to perform its role.  

### Debugging

Developers will support debugging via annotations, e.g. for logs and profiles, types and tests, tracing and blame. Logging extends to graphical visualizations. Testing includes non-deterministic choice to support heuristic fuzzing and property checks. 

Debugging machine code needs some attention. With accelerated interpretation of abstract machines, it should be feasible to emulate machine code for testing purposes. Performance and scale suffer, but we can improve our confidence before trying things for real. With an accelerated SMT solver (via cvc5 or Z3) we can also 'test' sat/unsat for constraint models. This could support abstract interpretation of machine code to check assumptions.

Debugging is best performed in context of interactive development. Tracing errors or outcomes back to sources is less difficult with replay. Interactive views of graphs are more convenient than guessing what a user will want to see later. In context of non-deterministic testing, users might focus on specific branches that catch their attention. 

In non-interactive mode, we can print some text messages to standard error and save some logs to disk. The configuration may influence this, but we'll at least report number of skipped messages and severity. Fortunately, for most tests, it is sufficient to record enough information to replay the failed test, e.g. test name and a sequence of indices representing non-deterministic choices. Users can fire up interactive mode to view failed tests. 

For very long-running tests, saving and trawling logs might be more efficient than replay. But this can be mitigated by a checkpoint system for tests, continuing from an easily serializable state. We can heuristically choose a checkpoint leading to failure based on replay cost.

*Note:* In interactive mode, the IDE may disable standard error to support TUI. But the content remains available for perusal, up to some configurable quota. We'll manage persistence and such independent of interactive mode.

### Live Programming



### Interaction


The above covers most non-interactive use of the assembler, but we'll also support an interactive mode. In interactive mode, users may edit files and we'll continuously update the result, debug outputs, etc.. We also don't close the assembler until requested, allowing for interactive debugging. (Details TBD.)




Such messages benefit significantly from lazy evaluation. But they also assume the assembler doesn't immediately exit when the user is finished. We may need a command-line parameter for debug or IDE mode, where the assembler opens a GUI or local HTTP port.


### History






, though it's still evaluated 



In some cases, we may be more interested in the secondary output, e.g. we could 

And there are some questions on what to do with 'result', e.g. write to standard out? to file? continuously maintain as live code? unzip into a folder?


By default, the assembler returns the binary by writing it to standard output. It also writes errors, warnings, status, etc. to standard error. Standard input might be monitored for queries in long-running computations, e.g. push 'p' to get detailed profiling info or 'g' for garbage collector metadata. But 




We also write errors, warnings, and status to standard error. 

 to standard error, and perhaps monitor standard input for 

 close standard input. However

But there are other options: we 

However, writing to a stream limits our options. For example, we cannot model *Live Coding* with standard output. So, we'll also support arguments like `"-o Outfile"` to write the content to a file directly. Then, our assembler could usefully  up a 


 but streaming output limits our options. Users may instead
, but users may redirect to a file or folder via `"-o Outfile"`

useful status information. 

 However, this may be influenced by other command-line arguments. For example, `"-o Outfile"` could write to a file. And if we're writing to a file, we have additiona

This binary should be fully deterministic, independent of executable version.

Aside from this primary behavior



load a configuration, interpret an assembly based on a few command-line arguments, 

. The generated binary should be deterministic, independent of executable version.




*Note:* Efficient integration of the binary requires additional attention in contexts such as *Live Coding*.


In practice, we'll frequently want to *accelerate* expression of the binary result. For example, instead of directly representing the binary list, we could express a binary as an effectful operation that yields `(Write Binary)` any number of times before returning.  

By default, that binary is written to standard output, but the assembler provides additional command-line arguments to manage output; see *Assembler Output*.

guide or integrate output, e.g. `"-o Outfile"`.


output may be guided by other command-line arguments, e.g. `"-o Outfile"`, and *Live Coding* is feasible.





Although 'resul



By default, this binary is written to standard out, but arguments such as `"-o Outfile"` may further guide *Assembly Output*.






However, for performance reasons, we may define 'result' to 




, or a binary generator - an effectful expression that yields `(Write Binary)` a number of times before returning an exit code.




By default, we'll write the binary to standard output. However, there will be other arguments to guide output, e.g. `-o Outfile`. Although the result is non-interactive, the assembly system may be: depending on arguments, the assembler may even present a projectional editor and render interactive debug views while continuously updating Outfile.

We can reasonably extend assemblies to define *folders* of outputs. Although we can indirectly model this via tarball or zipfile, efficient live coding would be complicated by the intermediate binary representation. To avoid complicating sem




Even if we want more imports, it might be more convenient to model them via macros within a file or script that act based on Args.  

Command-line macros may favor a short script (referencing the configured environment or a specialized file extension) that performs imports or includes based on arguments. So, 

while sophisticated use is left to command-line macros (see *Configuration*). Though, command-line macros may favor expressing behavior in the arguments, returning a script that integrates modules based on arguments.

The goal of the assembly module is (almost always) to specify a binary.

I propose to express this as defining 'result' to either a binary value or an effectful expression that yields `(Write Binary)` a number of times before returning an exit code. For streaming binary output, the latter should be easier to manage. (Laziness can be finicky.)

 The user may redirect it. However, there will be other command-line arguments to adjust this behavior, e.g. `"-o OutFile"`. The assembler can feasibly support projectional editing, debugging, and live coding, e.g. updating OutFile when there are no obvious errors.


*Note:* Essentially, an assembly combines properties of functions, objects, and modules: functions that receive 'env' and 'args' and return 'result'; objects subject to override and extension; modules supporting integration of multiple sources. Relevantly, we can import based on 'args'


### History

Pure lambdas, location-independent folders, and content-addressed remotes each simplify reproduction. However, flexible composition of multiple files is a confounding factor, as is support for arguments. Actual reproducibility requires reconstructing initial conditions. Doing so is difficult when those initial conditions are long forgotten or scattered across someone else's filesystem.

Actual reproducibility requires maintaining a history by default. This has a cost, but we can do a lot to mitigate it.



We can support users in maintaining the history, but 


Further, this history would involve recording the necessary elements such as files and folders, and the redirects. 

. We can ignore content-addressed remotes and just focus on the local files. When we first encounter a file, we might need to copy it, but subsequent operations may patch the file. 


content-addressed remotes

 Moreover, a history that is easy to query, extract from, and share.


The assembler could record its operations, building a 'history'. 



The assembler can support reproducibility by recording and compressing the necessary elements, on the premise that most assembly operations are repeats with slightly different arguments or files. We could copy files and folders


, and also compression things. We don't necessarily need to 

 However, in the general case, such records would be very expensive - copying files and folders, revisions and patches. It's very convenient if local files are also under DVCS *and* are recently committed. Then we need only need to record the current revision and the backup URLs.

It should be possible to maintain a history if we insist that the patches and copies are small enough. We don't need to copy files and folders repeatedly, just on the first use. 

e.g. one folder per entry, doing the necessary copies and patches and such. But we'd need to configure 

A viable approach is for the assembler to maintain some configurable amount of history, with a configurable size-per-entry. We can warn users if a history entry would be too large.



 However, reproducibility still relies on ability to reconstruct initial conditions. The ability for `GLAM_CONF` and the assembler CLI to reference multiple files scattered across a filesystem is a confounding factor.


## User-Defined Syntax

User-defined syntax is a convenient approach to external DSLs and metaprogramming, and naturally extends to graphical programming. 

When loading a module, a front-end compiler is selected from the provided environment based on file extension, i.e. `Base.env.lang.[FileExt]` should evaluate to a language object that defines 'compile'. This is also the case for "g" files, but in that special case the assembler provides a built-in as a fallback. The only file that must be "g" is the user configuration.

A significant design challenge for user-defined syntax is integration with a development environment: tracing bugs, isolating syntax errors, visualization and editable projections, index and search, autocomplete, autoformat, etc.. The conventional solution is to develop a suite of external tools, IDE plugins, etc. for each syntax. The language object serves is intended to serve this role: aside from 'compile' it may define methods for tooling, e.g. a language-server protocol.

But the conventional solution is not very well integrated. It involves a lot of rework, and is fragile to changes in syntax. I hope we can do better by integrating features into the compiler.

Some design thoughts:

- Parser combinators are a great starting point. Parser combinators can implicitly track parse locations and describe what they 'expect' to see at any given step, providing effective feedback in case of syntax errors.
- To simplify tracing, blame, error isolation, etc. the compiler must avoid directly observing parse results. Even parse errors must be abstracted To enforce this, we can return abstract data from parse operations by default. This might be expressed as an applicative functor. We can also provide built-in combinators for common loop structures.
- To support syntax-driven effects without observing the syntax, we can introduce an eval effect that lifts a user expression into the front-end compiler. The result of eval is then abstracted. This effectively supports some forms of macros.
- It is useful to support multi-pass parses. For example, a first pass might delimit the 'region' for another pass. Even if we drop the first pass result, this can be very useful for error isolation. 
- Aside from source locations, the front-end compiler should be able to inject additional annotations on abstract nodes to support validation, visualization, editable projections, indexing, etc.. 
- Using gensym, we can generate a unique abstract data type for the applicative per file. This is useful to control staging, i.e. it is useless to hold onto the abstract data.

Expressing a compiler without being able to 'see' the binary seems possible in theory, but I'm not entirely convinced that we won't reintroduce the tracing problem via Eval. To gain confidence, I must try it first with the standard syntax. After all, we should be able to override and extend the standard syntax, too. Worst case, I'm back to the conventional approach.

*Note:* We'll convert FileExt ASCII characters to lower-case and strip an initial '.', but otherwise it's just taken as a string. 

### Syntactic Bootstraps

If the final `Self.env.lang.[FileExt].compile` method is different from the Base version, the assembler attempts bootstrap. This involves recompiling with the Self version, repeating until fixpoint is reached.

        bootstrap(ext, base, binary, compiler) =
            let m = compile(compiler, base, binary) 
            let compiler' = 
                m.env.lang.[ext].compile if defined
                or builtin if ext is "g"
            if(compiler == compiler') then m else
            bootstrap(ext, args, compiler')

The built-in compiler is simply treated as one more compiler in the bootstrap cycle, equivalent to itself. A module may simply delete a provided "g" implementation to avoid user-defined feature extensions. In practice, syntax bootstrapping should be relatively rare: it's expensive and easily goes wrong. 

But it has some use cases. Mostly, adding features to the primary ".g" language and shifting dependencies from assembler version to module system.

### Editable Projections

Some user-defined syntax may be graphical. And even for purely textual syntax, we'll often want to integrate some visualizations or provide edit widgets like color pickers. Miscellaneous observations:

- We can only edit terms annotated with source locations, parser context, and an encoder that converts the parse result back into source (aka lenses or prisms). 
- Some content is naturally read-only, e.g. content-addressed remote files, read-only local files. In these cases, we can still support navigation, views, etc.. and partially-editable views are feasible.
- Editable views benefit from some reflection, e.g. support indices and aggregated views across multiple files.
- It is convenient to treat a file that does not exist as equivalent to an empty file for purpose of imports. This way, an import reference obtains a hyperlink and projectional editor.
- I'll want to flexibly mix editable views with rendering test results, providing a round-trip between updates and outcomes. Interactions for filtering or progressive disclosure of tests should be possible.

TBD: This is non-trivial, and I don't have a solid handle on exactly how to approach it.

## Integrated Development Environment



## Reasoning

What can we feasibly implement to support developers in reasoning about the assembly process and product?

* *Testing*: Sample system behavior under various conditions, and make pass/fail judgements. Should be able to visualize test results in tables and graphs. Should support fuzzing, heuristic exploration of condtions, paralleism, and incremental computing. Ideally supports blame, too.

* *Visualization*: Start with logging and profiling, but extend to graphical views, interactions for progressive disclosure or search, etc.. I propose to model log messages as mixins implementing multiple views. Plain text is one view, but we can feasibly render icons, interactive widgets, etc..

* *Tracing*: Maintain metadata to trace outcomes (errors, data, etc.) back to contributing sources. There's a tradeoff between precision and performance. We can feasibly leverage reproducibility by replaying a computation under a few different tracer setups to obtain more precision.

* *Types*: We can annotate our assumptions about data and programs in a machine-checkable way. The assembler makes a best effort to prove or disprove types, possibly using dynamic checks. Unfortunately, gradual typing won't be very effective for sophisticated uses (phantom types, substructural types, etc.).

* *Abstract Interpretation*: Given a representation of a program (e.g. machine code) we can implement an 'interpreter' using variables instead of data. Users add their own assumptions about this interpretation, then we check for conflicts. Essentially, we can mechanically implement a type system scoped to our target.

I envision use of type annotations to catch obvious errors in the assembly process, abstract interpretation as our primary means to reason about the product, and testing in a more ad hoc role. Visualization should build on tests and integrate nicely with a projectional editor.

### Integration

An relevant concern is how automated reasoning interacts with extension, especially breaking changes. Ideally, extensions can suppress expected errors and express updated assumptions. This suggests integration with the namespace, such that we can override the troublesome elements.

We can model module-level type declarations and tests as naming conventions, perhaps `"type:foo"` and `"test:xyzzy"`. The assembler may recognize these conventions then automate testing and type checking when the module is loaded. 

For anonymous assertions, logging, embedded type annotations, etc. we might reference an abstract, static 'channel' declared at the module level. Users may override the channel to disable or reconfigure. It should be feasible to route channels from the user configuration, enabling effective user control over assertions and such.

### Test Monad

Tests can be expressed monadically with simple, useful effects:

- Choice. Non-deterministic choice will support selecting test parameters, simulating race conditions, fuzz testing, etc.. We can choose from a list (discrete), or choose a rational number within given bounds (continuous). Choosing from an empty list will cancel a test.
- Status. A description of test parameters, intermediate states, and outcomes. Test status should contain data that we might be interested in displaying as points within a graph. We can potentially animate tests by rendering evolving status.
- Log. A record of remarkable events during a test. Remarkable in the sense that logging is literally remarking on them.

Tests may return pass, fail, indeterminate, or checkpoint. On checkpoint, the test is restarted with its current status. Status must be serializable (no functions, objects, or symbols). This is intended for long-running tests, enabling a failed test to be recorded for replay nearer to the failure. 

A test runner is relatively easy to implement. But good heuristics for non-deterministic choice, focusing on branch coverage and edge conditions, are difficult. The assembler may use some simplistic heuristics to start, forcing developers be more cautious about presenting efficient choices.

### Constraints

Constraint systems are a convenient mechanism for abstract interpretation. We can feasibly accelerate constraint systems with cvc5, Z3, or other solvers. Unfortunately, the accelerator cannot observe the *solution* because it's effectively non-deterministic. But we can use the sat/unsat judgement.

The DSL for constraints can be directly adapted from SMTLIB2. For variables, we can easily use `(Var symbol)` or similar, using a distinct symbol per variable. We easily can control naming conflicts between globally-unique symbols and constructed symbols with local naming strategies.

It is relatively convenient to build a constraint system statefully, within a monad, and perhaps occasionally 'Check' for sat/unsat. This is the approach I intend to pursue for assembly code.

### Messages

I propose to model 'messages' as mixins. 

In the simplest case, a message may define 'text' with a string. But we can extend the message with methods for multiple views (e.g. svg, http, [typst](https://typst.app/)) and metadata for search and filtration (priority, domain, etc.). We can express templated methods, and easily extend the templates. The system can provide some metadata through Base, e.g. about source locations or continuations, that would violate abstractions normally.

Although messages are stateless, it may be useful to model stateful sessions or interactions with them to support dynamic views, queries, filters, progressive disclosure, etc.. This can be supported monadically, e.g. one memory monad per 'session', allowing for branching sessions and trivial undo.

I'm not certain we need all the flexibility mixins provide, but I'm confident it won't hurt. But we'll be depending heavily on lazy evaluation for performance.

## Assembly-Level Programming

TBD: So, we have tools to output a binary and reason about it. But how should we express machine code, concretely?


## Standard Syntax

An important design constraint (in my heart) is that assembly code must look and feel like assembly. I cannot afford much clutter, e.g. for preconditions and postconditions and other annotations. Any metaprogramming must fit nicely into assembly descriptions.

Fortunately, I think Haskell's do/mdo notation is a decent fit for assembly. Labels need some attention, though.


# OLD CONTENT

## Syntax Ideas 

### User Data

I hope to develop a lightweight syntax for users to define data constructors, leveraging unique symbols for pattern matching and sealing. We can leverage smart constructors, active patterns, and type annotations.

## Assembly Ideas

### Content-Addressed Memory

Instead of manually managing layout, it is feasible to 'jump' to code or 'load' data and have these converted later to memory addresses based on content and metadata. For mutable targets ('.bss' or '.data') we can include a symbol in metadata to distinguish multiple instances.

This design significantly simplifies metaprogramming: we can allocate resources and computed subroutines as needed without predicting them in advance. To restore control over memory layout, an assembly may define interfaces to guide layout based on provided metadata.

If writing out assembly mnemonics is modeled effectfully, we can feasibly capture the data and integrate the final location after we've collected all the data.

### Local Labels

Content-addressed memory doesn't work for local labels used in branches and jumps. But we still cannot assume the label target is defined before it is used, i.e. we can jump to something later within the assembly program.

Labels may need specialized syntax within an assembly definition.

### Structured Stacks

Although we can directly manipulate the stack pointer, it would be convenient if we can automatically allocate stack locations based on usage, similar to how we allocate '.bss' sections. This seems feasible if we automatically accumulate sizes for referenced stack data elements.

*TBD:* Generalize so we can leverage yet again for associative heap allocations.


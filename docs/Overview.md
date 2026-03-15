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

- Data is *logically* Church or Scott encoded. Actual encoding is accelerated.
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
        fix f = let x = f x in x     -- lazy fixpoint
        new spec env = fix (spec env)

Most observations on Base or Self prior to instantiation will either diverge on fixpoint or compromise extensibility. Although fixpoint divergence is easy to detect and debug, the opportunity cost to extensibility is invisible and awkward to explain. It's best to ensure syntax avoids potential pitfalls.

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

Using short strings as method names, especially generic and context-dependent names like "map" or "draw", easily leads to ambiguities and collisions, especially in context of multiple inheritance or dynamic mixins. There are other weaknesses: no clear mechanism for private names, no feedback on deprecating names.

A robust alternative is symbolic names. 

Modules may generate unforgeable symbols upon import, then export them. By using symbols as method names, we eliminate ambiguity and we gain object-capability security for individual methods, a secure yet flexible basis for privacy.

At external tooling boundaries, e.g. logging or IDE, it is awkward to reference names through the module system. Instead, we'll construct global symbols such as `symbol("glam-lang.org/2026/log/text")`. We can at least ensure these symbols are unambiguous based on naming convention.

At the module layer, we're effectively stuck with short strings: they're necessary for concise syntax! But modules are constrained in other ways that mitigate concerns of collision: no multiple inheritance, static mixins only (via include). Also, *Explicit Override* still applies, and the module dictionary is abstracted.

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

*Note:* Haskell also has a *RecursiveDo* sugar, enabling a result to be used before it is defined. I'm less familiar with this desugaring, but I'll surely want RecursiveDo by default. It seems convenient for branching to forward labels, for example. This has consequences for syntax: no shadowing names, because it would be unclear whether you're referring forwards or backwards.

We can specialize the monadic operators for our only monad. Our untyped lambda calculus doesn't offer a direct solution to type-indexed behavior, such as typeclasses, so this is convenient:

        (Yield rq k1) >>= k2 = (Yield rq (k1 >>> k2))
        (Return a) >>= k = k a
        k1 >>> k2 = (>>= k2) . k1

Effectively, '>>=' captures the continuation into 'Yield'. Unfortunately, the Kleisli composition is left-associative, i.e. `((((k1 >>> k2) >>> k3) >>> k4) >>> k5)`. Right-associative `(k1 >>> (k2 >>> (k3 >>> (k4 >>> k5))))` performance is vastly superior. To resolve this, we could lift into semantics (change Yield continuation to a queue), or insist on a built-in optimization (similar reasoning as tail-call optimization). I favor the latter.

Behavior is embodied in the runners aka handlers. It is convenient to express 'stacks' of partial handlers for local subtasks that forward unrecognized requests. Basically, any effect can be encoded except race conditions (outcome is deterministic). I wrote a few examples to help myself grasp this: 

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
        --  with per-thread continuations (needed for mutexes, semaphores)
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

Monads frequently overspecify order. Many effects can be at partially reordered without influencing outcome, yet with a significant impact on performance. To mitigate this, it is useful to model asynchronous and threaded effects.

Asynchronous effects simply buffer requests to perform later. They may return an abstract future in some cases. Regardless, the handler has an opportunity to reorder requests prior to implementing them.

Threaded effects involve running multiple monadic threads. Like runThreadT but without Pause. Instead, the handler actually examines available requests and makes heuristic decisions about which to handle next, or perhaps handling them all. 

        [(Yield BranchingQuery k1), (Yield TightConstraint k2), ...]
        -- constraint-choice runner: let's apply TightConstraint next!

These techniques work well together, e.g. we can buffer asynchronous requests locally per thread, then yield heuristically (to flush the buffer) or when an external response is needed. The main benefit of buffering would be doing more work per step, which is mostly useful in context of spark-based parallelism.

## Modularity

A module is represented by a file. Modules may reference other files within the same folder or subfolders, or a content-addressed remote. We forbid Parent-relative ("../") and absolute filepaths. These constraints ensure folders are location-independent and temporally stable, yet editable by conventional means.

Modules are modeled as mixins objects with limited effects during construction, conceptually `Dict -> Dict -> Eff {load | gensym} Dict` (in roles `Base -> Self -> Instance`). The module type is fully abstracted (see *User-Defined Syntax*). But we'll discuss it in these terms here.

There are a few mechanisms to integrate loaded modules:

* *include* - bind included module's Base to host's current Base, treating prior definitions as mixins. Share Self. Effectively applies a module as a mixin. Adds arbitrary names to namespace.
  * *include at* - apply to a dictionary defined within Base instead of directly to Base. Useful for lazy loading.
* *import as* - equivalent to introducing a dictionary with `{ "env": Self.env }` then translating an include to that dictionary. 

This design ensures there is only one Self for an entire configuration or assembly. We can override names defined deeply within other modules. Although definitions aren't mutable, they'll feel mutable in context of the toplevel. Meanwhile, we can lazily load scoped includes.

Dependencies between files must form a directed acyclic graph. However, each import or include is independent for gensym and Base, and it's awkward to maintain scattered references to content-addressed remotes. In most use cases, we'll share definitions through 'env.\*' instead of loading a module twice.

Although it would not be difficult to support 'private' definitions per module (via gensym), doing so would harm extensibility and complicate the intuition of 'include'. We'll just use a convention like Python's `"_name"` at the module level. 

### Configuration

The assembler implicitly loads a configuration module based on the `GLAM_CONF` environment variable or an OS-specific default, i.e. `"~/.config/glam/conf.g"` in Linux or `"%AppData%\glam\conf.g"` in Windows. A small, local user configuration typically extends a large, remote community or company configuration.

The configuration serves several roles:

- *Assembly environment*: define 'env'. This is passed to the assembly as if importing the assembly into the configuration, e.g. `{ "env": Config.env }`. This environment is analogous to system includes and shared libraries, supporting adaptive assembly. 
- *Command-line macros*: if the first command-line argument to the assembler does not start with '-', we apply 'cli', which should be a function of type `List of String -> List of String` that returns valid arguments or empty list.
- *Development environment*: Define an *Integrated Development Environment* for '-i' interactive mode. Filter outputs to standard error.
- *Resource management*: ad hoc, e.g. specify GPGPUs available for acceleration, cache locations and replacement heuristcs, history management, shared proxy compilation and cache, search locations for content-addressed remotes, tune assembler JIT or GC heuristics, control expensive tests and checks (e.g. assertions, fuzzing).

Configurations never directly control assembler output: An assembly may ignore your configured environment and substitute its own. Command-line macros may always be written out long form. Resources influence performance and error detection but not a valid binary result.

To support project-specific overrides or sharing of a system configuration, `GLAM_CONF` is not limited to one file. Users may list multiple files (same OS-specific separator as the `PATH` variable). We apply these as mixins, each file overriding those listed later.

### Assembly

The assembler receives command-line arguments that express an assembly module as a list of mixins. Though, in practice, it's usually just one file or script. Relevant arguments:

- `(-f|--file) FileName` - list a file to include; first file is included last, overriding those listed later. Depending on the configured environment, assembly files aren't limited to ".g" (see *User-Defined Syntax*).
- `(-s|--script).FileExt Text` - behaves as a remote file with the given file extension and text. Scripts cannot import local files.
- `-- List Of Args` - assembler defines 'args' before including files or scripts. Default is empty list, but caller may override with elements following the '--' separator.

Aside from 'args', the assembly module is implicitly parameterized by the configured 'env'. The assembly module shall define 'result', representing the assembled product, i.e. a binary or folder.

### Scripts

In some cases, it is convenient to just compute some text then interpret it as an include or import with a specified file extension. We already do this when specifying an *Assembly*. We can easily support it as a special case for import and include within a module, too.

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
    - still accessible in interactive mode, e.g. HTTP request
  - `(-o|--out) Destination` - output to named file or folder

Although we could model folders via tarball or zipfile, making folders explicit is more convenient in context of interactive development.

Machine-code mnemonics are left to libraries and syntactic sugars. Assuming suitable accelerators and user-defined syntax, it should be easy to adapt the assembler to many targets: configuration files, ray tracing, typesetting, constructing websites, simulations, etc..

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

Developers support debugging via annotations, e.g. for types and tests, logs and profiles, tracing and blame. Logging extends to graphical visualizations. The assembler has built-in support for fuzz testing and property checking via non-deterministic choice in named tests.

Effective debugging of assembly will inevitably depend on acceleration. With acceleration of an abstract machine, it is feasible to test machine code by emulation. With acceleration of an SMT solver, it is feasible to test machine code via abstract interpretation.

Although we do not require use of type annotations, we'll not discourage them. Enforcement is best effort, warning if an error is neither proven nor disproven. I hope to support a debuggable visualization of the typechecking process, and to support constraint-based heuristic analysis to isolate errors similar to [Cornell's SHErrLoc project](https://www.cs.cornell.edu/projects/SHErrLoc/).

Debugging is best performed in context of interactive development. Access to replay greatly simplifies some tracing and analysis methods. The GUI view simplifies attention, visualization, and comprehension. And because everything *except* non-deterministic choice in named tests is highly reproducible (unlike runtime errors) it is no problem to fire up the IDE for interactive debugging. 

In non-interactive mode, we'll print some messages to standard error in a relatively conventional manner. The configuration may provide some filters. Then we instead report a number of skipped messages by domain and severity. We can heuristically cache failed tests (name, checkpoint, sequence of choices) for replay in interactive mode.

*Note:* In interactive mode, the IDE may disable standard error to support TUI, and the IDE may do whatever it wants with messages.

### Live Programming

Another process may 'run' interactive assembly output, watching for changes and integrating them. What can we easily do to improve this use case?

- Windows:
  - Readers open files with FILE_SHARE_DELETE. (They continue reading 'old' data.)
  - Writer edits temporary file then calls ReplaceFile 
    - or MoveFileEx with `MOVEFILE_REPLACE_EXISTING` 
- Linux:
  - Readers open file
  - Writer edits temporary file then calls rename

This allows the writer to proceed without waiting on readers. We can feasibly do better with shared memory and explicit double buffering, but it will require another extraction mode. (We could use a RAM disk instead.)

Aside from efficient and atomic updates, it is convenient if the runner can report discovered issues, e.g. that there's a problem at byte 30467, so we can trace sources and draw the user's attention. This could be supported by an HTTP POST to the IDE running in interactive mode. 

### History

Deterministic functions, location-independent folders, and content-addressed remote modules all contribute to reproducibility. But we still cannot reproduce the output if we cannot reproduce the initial conditions. To improve practical reproducibility, the assembler should automatically maintain a sufficient history to reproduce prior assemblies, ideally with structure sharing and effective pruning.

In practice, this may require copying local files and maintaining a local DVCS repo to represent the history. Though, we'll also need a little attention on merging history for concurrent assembler processes. (I wonder if darcs would be a good fit here.)

## User-Defined Syntax

User-defined syntax is a convenient approach to external DSLs and metaprogramming, and provides an opportunity for graphical programming.

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
- Users should never directly see module Base or Self, and we should restrict module-level names to strings.
  - Might also restrict object-level names to symbols.

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
- Status. A serializable description of test parameters, summary of intermediate states, and outcomes. Test status should contain data that we might be interested in displaying within a graph. 
  - It should be feasible to animate tests by rendering evolving status, forking on choice. Like starting with a single car on a racetrack, cloning on each decision, then rendering many crashes.
  - The IDE should support user interaction to trim some branches, expand on others, to aid comprehension.
- Log. A record of remarkable events during a test. Remarkable in the sense that logging is literally remarking on them.

Tests may return pass, fail, indeterminate, or checkpoint. On checkpoint, the test is directly restarted with its current status, thus externalizing a test loop. This enables us to record a long-running failed test nearer to the point of failure.

A test runner is relatively easy to implement. But good heuristics for non-deterministic choice, focusing on branch coverage and edge conditions, are difficult. The assembler may use some simplistic heuristics to start, forcing developers be more cautious about presenting efficient choices.

*Note:* We cannot afford non-deterministic choice for anonymous assertions because there is no effective way to replay them. Assertions will be simple, deterministic.

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


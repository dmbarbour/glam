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
- Dictionaries are finite key-value lookups where the keys are transitively composed of basic data. Singleton dictionaries are useful for modeling tagged unions. Dictionaries do not support iteration over symbolic keys, providing a simple basis for data abstraction.

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

Multiple inheritance is implemented by reifying dependencies then applying a linearization algorithm such as C3. Of course, `Dict -> Dict -> Dict` functions are incomparable. To support linearization, we pair these functions with tags as proxy identifiers. 

By default, we'll implicitly declare a globally unique symbol to tag every syntactic object. This probably covers 99.9% of use cases. We can let users manually provide the tag to cover rare exceptions such as metaprogramming of objects.

Tags don't need to be globally unique but must not be reused in scope of linearization. To express and verify this local-uniqueness assumptions, we can introduce annotations for asserting incomparable values are equivalent. Although the assembler cannot prove equivalence of functions in general, it can easily warn when not obviously equivalent (i.e. referentially or structurally), or raise an error if obviously not equivalent.

### Symbolic Names

In context of mixins and multiple inheritance, using strings as names, especially generic and contextual names like "map" or "draw", easily leads to ambiguities and collisions. There are also other weaknesses: no feedback on deprecating a name, no clean approach to private names.

A more robust solution is to declare unique symbols for distinct meanings. Access to the symbol is routed through the module system, subject to qualification or aliasing. If we delete or deprecate the symbol, we can easily chase errors. Private names are easily modeled by controlling exports.

OTOH, a relevant concern with symbolic names is risk of namespace clutter. Also, we cannot use unique symbols for names at the module scope. Making symbolic names pleasant to use will inevitably require careful attention to syntax.

### Testing

We might express assumptions about objects as a collection of named assertions. These assertions may be disabled or overridden. We could automatically assert the tests when instantiating the object.

## Effects

Even without a runtime, effects are convenient for implicit dataflow, backtracking and error handling, flexible composition, extensible behavior, etc.. I'll use Haskell(-ish) syntax to describe these, but it should be easy to translate.

        type Eff rq a =
            | Yield (rq x) (x -> Eff rq a)
            | Return a

We can model a free-er effects monads as either *yielding* a `(request, continuation)` pair or *returning* a final answer. In case of yield, the continuation expects the response type from the request. But in context of untyped lambda calculus, we won't be rigorously enforcing these type annotations, just borrowing the structure. 

We can easily introduce some syntactic sugar:

        (sugar)         (monadic ops)
        a <- op1        op1 >>= \ a ->
        op2             op2 >>
        op3 a           op3 a

Haskell has a *RecursiveDo* extension, enabling the user to capture outputs from a monad's future as inputs to prior operations. In context of assembly, we'll likely want this feature by default. For example, with assembly it's convenient if we can reference forward labels that haven't been defined yet. To keep it simple, we'll generally forbid name shadowing.

We can specialize the monadic operators for our only monad. Our untyped lambda calculus doesn't offer a direct solution to type-indexed behavior, such as typeclasses, so this is convenient:

        (Yield rq k1) >>= k2 = (Yield rq (k1 >>> k2))
        (Return a) >>= k = k a
        k1 >>> k2 = (>>= k2) . k1

Effectively, '>>=' captures the continuation into 'Yield'. Unfortunately, it's left-associative, i.e. `((((k1 >>> k2) >>> k3) >>> k4) >>> k5)`. If implemented directly, this will repeatedly rebuild four compositions every time 'k1' yields. Right-associative `(k1 >>> (k2 >>> (k3 >>> (k4 >>> k5))))` performance is vastly superior. To resolve this, it is feasible to explicitly model a queue of continuations, or to *accelerate* '.' (function composition). I favor acceleration. 

Behavior is embodied in the runners (aka handlers). It is often convenient to express 'stacks' of partial handlers for local subtasks that forward unrecognized requests. Basically, any effect can be encoded except race conditions (outcome is deterministic). Examples:

        -- generalize Reader to pure queries; forwards unhandled queries
        runEnvT env (Yield (Env q) k) | Just r <- env q = runEnvT env (k r)
        runEnvT env (Yield rq k) = Yield rq (runEnvT . k)
        runEnvT _ r@(Return _) = r

        -- generalize State to indexed, scoped Memory
        --   memory is extensible via unique symbols
        runMemT m (Yield (Mem idx op) k) =
            match idx with
              Outer idx' -> 
                Yield (Mem idx' op) (runMemT m . k)
              _ ->
                match op with
                  Get -> runMemT m (k (m.[idx])) 
                  Put v -> runMemT (m with { [idx]: v }) (k ())
                  Del -> runMemT (m without idx) (k ())
        runMemT m (Return r) = (Return (r, m))

        -- cooperative threads (round robin, no preemption, no sync)
        runThreadT (Yield (Spawn t) k):ts = runThreadT (k ()):t:ts
        runThreadT (Yield Pause k):ts = runThreadT (ts ++ [k ()])
        runThreadT (Yield rq k):ts = Yield rq (runThreadT . (:ts) . k)
        runThreadT (Return ()):ts = runThreadT ts
        runThreadT [] = Return ()

        -- delimited continuations 
        runContT (Yield (Reset op) k) = runContT op >>= runContT . k
        runContT (Yield (Shift fn) k) = runContT (fn k)
        runContT (Yield rq k) = Yield rq (runContT . k)
        runContT r@(Return _) = r

        -- effectful choice, commit on Return but captures search state
        --   (initial bt: Return None)
        runChoiceT bt (Yield (Choose x:xs) k) =
            let bt' = runChoiceT bt (Yield (Choose xs) k)
            runChoiceT bt' (k x)
        runChoiceT bt (Yield (Choose []) _) = bt
        runChoiceT bt (Yield rq k) = Yield rq (runChoiceT bt . k)
        runChoiceT bt (Return r) = Return (Just (r, bt))

We can also model runners that scope effects:

        -- forbid effects from escaping
        runPure (Return r) = r
        runPure (Yield _ _) = error "unhandled effect in runPure"
 
        runEnv env = runPure . runEnvT env
        runMem m = runPure . runMemT m
        runCont = runPure . runContT
        -- pure 'runThread' is useless

        -- pure choice can be heavily optimized with laziness
        runChoice (Yield (Choose xs) k) = List.flatMap (runChoice . k) xs
        runChoice (Yield _ _) = error "unhandled request in runChoice"
        runChoice (Return result) = List.singleton result

We'll want a library of useful, reusable handlers. However, I hope we deliberately design handlers with flexibility and extensibility in mind. Regular users should rarely feel the need to write custom handlers.

*Note:* Although we can leverage effects for general-purpose programming, doing so would dilute design. Eschewing the runtime eliminates or ameliorates many complicating factors for reasoning and performance. So, we'll focus exclusively on binary assembly.

### Commutative Effects

Monads easily overspecify order. Many effects can be at partially reordered without influencing outcome, yet with a significant impact on performance. To mitigate this, it can be useful to model asynchronous and threaded effects.

Asynchronous effects enable the runner to aggregate several requests from a single thread, enabling a reordering before we compute observations. Threaded effects enable the runner to examine pending requests from multiple threads before making any decisions.

        [(Yield BranchingQuery k1), (Yield TightConstraint k2), ...]
        -- constraint runner: let's apply TightConstraint next!

These techniques work very well together, e.g. we can model asynchronous interactions between threads. They also work nicely with spark-based parallelism: a runner heuristically sparks evaluation for a past request when the data becomes available, or a runner can batch-process several threaded effects then parallelize evaluation of each thread's next step.
 
## Modularity

A module is represented by a file. Modules may reference other files within the same folder or subfolders, or content-addressed remote folders. We forbid Parent-relative ("../") and absolute filepaths. These constraints ensure folders are location-independent and temporally stable, yet editable by conventional means.

A content-addressed remote folder is uniquely identified and authenticated by secure hash of content or DVCS revision history. However, they are not *located* by secure hash. Instead, a remote import may provide a list of locations (URLs), and configurations may suggest a few more. It is useful to include a DVCS tag or branch name for shallow cloning and to notify users of updates, but it cannot replace the secure hash.

Modules are modeled as basic objects with limited effects during construction, roughly `Dict -> Dict -> {load, gensym} Dict` (in roles `Base -> Self -> Instance`). We aren't bothering with multiple inheritance. The Base argument receives a host environment (dict 'env') for adaptability, while Self supports overrides for extensibility. Here 'load' brings another module into scope, and 'gensym' is our source of unique identifiers. We can enforce filepath constraints on load, and also raise an error for dependency cycles.

Aside from adaptability and extensibility, a motivating use case for parametric modules is to isolate remote references into very a few files, simplifying maintenance. Aside from one imports file per project, desired definitions should be available through 'env.\*'. We assume lazy loading and caching to support very large environments.

We provide two mechanisms to integrate modules:

* *include* - bind included module's Base to host's current Base and share Self. Essentially, included module applies as mixin. 
* *import* - bind imported module's Base to `{ "env": env }` in host, instantiate, then assign local name to returned dict.

These cover two distinct use cases: include for extension, import for lazy loading. Instead of extending an import, we override the dictionary, perhaps even replace it by another import. Due to laziness, prior definitions are never loaded.

Regarding 'private' definitions, they aren't difficult to model via gensym, but they hurt extensibility at this layer. It seems wiser to use plain-text names and simple naming convention like Python's `_name` at the module layer. We'll still want explicit override to resist accidents, and we'll heavily use symbolic names within object specifications.

*Note:* Aliasing definitions from an imported dictionary is a separate declaration. This separation is slightly inconvenient, but the intention is clearer that we're binding to the dictionary (which is subject to override), and not to the import. 

### Configuration

The assembler implicitly loads a module based on the `GLAM_CONF` environment variable or an OS-specific default, i.e. `"~/.config/glam/conf.g"` in Linux or `"%AppData%\glam\conf.g"` in Windows. This module specifies a configuration. A small, local user configuration typically extends a large community or company configuration, imported from DVCS.

A configuration specifies an initial environment for the assembly module. We'll simply forward whatever the configuration defines as 'env', logically importing the assembly into the configuration. The assembly is free to ignore the user environment and substitute its own, but this provides an opportunity for adaptation or even script-like assemblies.

Excepting 'env', configuration shall not influence an assembly's binary output. It may influence logging, testing, caching, GPGPU resources for acceleration, distributed computing and work sharing, etc..

### Assembly

Any module that specifies a binary, or comes close enough for the command line to cover the gap, effectively serves as an assembly. The assembler CLI will support parameterizing an assembly-defined function with few command-line arguments, and scripting (i.e. of a mixin for an assembly).

The selected module is parameterized by the configured environment. Some assemblies may be script-like, mostly composing resources defined in this environment. Users may also extract binaries from the configured environment without reference to a separate assembly module. 

Unlike configuration files, assembly modules don't need the ".g" extension. The configured enviroment may define front-end compilers for other file extensions. See *User-Defined Syntax*.

*Note:* The assembler has no built-in knowledge of CPU architectures or assembly mnemonics. It is feasible to 'assemble' documents, ray-traced images, JSON configuration files, websites, etc.. Multi-file assemblies can be expressed via tarball or zipfile or similar.

## User-Defined Syntax

User-defined syntax is a convenient approach to external DSLs and metaprogramming. When loading a module, a front-end compiler is selected from the provided environment based on file extension, i.e. `Base.env.lang.[FileExt]` evaluate to a language object that defines 'compile'. This is also the case for the standard "g" files, but the assembler provides a built-in fallback.

A significant design challenge for user-defined syntax is integration with a development environment: tracing bugs, isolating syntax errors, visualization and editable projections, index and search, autocomplete, autoformat, etc.. The conventional solution is to develop a suite of external tools, IDE plugins, etc. for each syntax. The language object serves is intended to serve this role: aside from 'compile' it may define methods for tooling, e.g. a language-server protocol.

But the conventional solution is not very well integrated. It involves a lot of rework, and is fragile to changes in syntax. I hope we can do better by integrating features into the compiler.

Some design thoughts:

- Parser combinators are a good starting point. Parser combinators can easily describe what they 'expect' to see at any given step, and they can track source location, providing effective feedback in case of syntax errors. We can extend the monad with more effects.
- However, to simplify tracing, blame, error isolation, etc. the compiler cannot directly observe parse results. We could feasibly return some abstract data from each parse operation, and manipulate this data indirectly similar to applicative functors.
- Observing a failed parse is equivalent to observing the binary, e.g. because we can test for 0, fail, try 1, etc.. Thus, we'll capture failure within the abstract data, too. This could be supported via something like a Maybe value from all parses. *Note:* We cannot *directly* use failure to model parse loops. But the parser effects API may provide loops as a built-in.
- To support syntax-driven effects without observing the syntax, we may need to introduce an eval effect that lifts a user expression into the front-end compiler, e.g. `(Eval expr)`. The result of Eval is then abstracted. This effectively also supports forms of macros.
- It is useful to support multi-pass parses, e.g. where one parse delimits the 'region' for another. This is very useful for error isolation.
- Aside from source locations, the front-end compiler should be able to inject additional annotations on abstract nodes to support validation, visualization, editable projections, indexing, etc.. 
- Using gensym, we can generate a unique abstract data type for the applicative per file. This is useful to control staging, i.e. it is useless to hold onto the abstract data.

Expressing a compiler without being able to 'see' the binary seems possible in theory, but I'm not entirely convinced that we won't reintroduce the tracing problem via Eval. To gain confidence, I must try it first with the standard syntax. After all, we should be able to override and extend the standard syntax, too. Worst case, I'm back to the conventional approach.

*Note:* We'll convert FileExt ASCII characters to lower-case and strip an initial '.', but otherwise it's just taken as a string. 

### Syntax Bootstrap

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

- At the lowest level, our editable projections will benefit from compiler integration, e.g. with lenses for editing the relevant file sources, maintaining metadata about source locations. 
- The 'edit' half of a projectional editor is essentially limited to actual files in a local project folder. But read-only views are still useful for remote sources, read-only files in the local folder, and namespace macros.
- We can treat a file that does not exist as equivalent to an empty file. This way, simply referring to the file is sufficient to obtain the hyperlink and a projectional editor (on the empty file).
- It may be useful to support indexing of modules and build projectional editors from indices.
- We'll also want to visualize outcomes, i.e. based on the final definitions with overloads. These might not be editable, but we'll want the view of outcomes to be at least spatially adjacent to the editor.

## Reasoning

What can we feasibly implement to support developers in reasoning about the assembly process and product?

* *Testing*: Sample system behavior under various conditions, and make pass/fail judgements. Should be able to visualize test results in tables and graphs. Should support fuzzing, heuristic exploration of condtions, paralleism, and incremental computing. Ideally supports blame, too.

* *Visualization*: Start with logging and profiling, but extend to graphical views, interactions for progressive disclosure or search, etc.. Perhaps we can model log messages as 'objects' implementing many views, e.g. both plain text and GUI widgets.

* *Tracing*: Maintain metadata to trace outcomes (errors, data, etc.) back to contributing sources. There's a tradeoff between precision and performance. We can feasibly leverage reproducibility by replaying a computation under a few different tracer setups to obtain more precision.

* *Types*: We can annotate our assumptions about data and programs in a machine-checkable way. The assembler makes a best effort to prove or disprove types, possibly using dynamic checks. Unfortunately, gradual typing won't be very effective for sophisticated uses (phantom types, substructural types, etc.).

* *Abstract Interpretation*: Given a representation of a program (e.g. machine code) we can implement an 'interpreter' using variables instead of data. Users add their own assumptions about this interpretation, then we check for conflicts. Essentially, we can mechanically implement a type system scoped to our target.

I envision use of type annotations to catch obvious errors in the assembly process, abstract interpretation as our primary means to reason about the product, and testing in a more ad hoc role. Visualization should build on tests and integrate nicely with a projectional editor.

### Integration

An relevant concern is how automated reasoning interacts with extension, especially breaking changes. Ideally, extensions can suppress expected errors and express updated assumptions. This suggests integration with the namespace, such that we can override the troublesome elements.

We can express module-level type declarations and tests via simple naming convention, supported by front-end compilers and recognized by the assembler, perhaps `"type:foo"` and `"test:xyzzy"`. The assembler can automate testing and type checking when the module is loaded. 

For anonymous assertions, logging, embedded type annotations, etc. we might reference an abstract, static 'channel' declared at the module level. Users may override the channel to disable or reconfigure. It should be feasible to route channels from the user configuration, enabling effective user control over assertions and such.

### Test Monad

Tests can be expressed monadically with simple, useful effects:

- Choice. Non-deterministic choice will support selecting test parameters, simulating race conditions, fuzz testing, etc.. It's also convenient for sharing test setup and parallel evaluation. Choosing from the empty list will cancel a test, neither pass nor fail; useful if our test conditions are out of bounds.
- Status. Essentially a test's 'public' state, expressed as a dictionary. At the end of a test, this should contain test parameters, relevant outcomes, perhaps a log message, i.e. anything we might want to cache or visualize. We can feasibly *animate* the evolution of status as a test runs (perhaps keep a 'frame' parameter in Status for this purpose). We can also use Status for Choice heuristics.

The pass/fail/etc. result is indicated by a final return value.

A test runner is relatively easy to implement, e.g. it's basically just a fusion of runChoice and runMemT to ensure we can view status before tests complete. But heuristics for effective fuzzing are not so easy to implement. And ideally, tests should also support incremental computing.

### Constraints

Constraint systems are a convenient mechanism for abstract interpretation. We can feasibly accelerate constraint systems with Z3 or other solvers. Unfortunately, the accelerator cannot observe the *solution* because it's effectively non-deterministic. But we can use the sat/unsat judgement.

The DSL for constraints can be directly adapted from SMTLIB2. For variables, we can easily use `(Var symbol)` or similar, using a distinct symbol per variable. We easily can control naming conflicts between globally-unique symbols and constructed symbols with local naming strategies.

In context of writing code monadically, it is convenient to build a constraint system statefully, within a monad. We can do so for assembly code, and it may also be useful to do so via the front-end compiler. Perhaps we can directly model our type systems.

### Log Messages




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


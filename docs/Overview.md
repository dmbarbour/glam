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

* *Parallelism*: An annotation indicates that a lazy thunk will be needed later. The assembler adds the thunk to a queue to be processed by a background worker thread. Depending on configuration, this can be extended to remote workers.

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

Haskell has a *RecursiveDo* extension, enabling the user to capture outputs from a monad's future as inputs to prior operations. In context of assembly, we'll probably want this feature by default to support jumping or branching to forward labels. I don't anticipate any trouble encoding this, but actually using it requires caution to avoid divergence.

We can specialize the monadic operators for our only monad. Our untyped lambda calculus doesn't offer a direct solution to type-indexed behavior, such as typeclasses, so this is convenient:

        (Yield rq k1) >>= k2 = (Yield rq (k1 >>> k2))
        (Return a) >>= k = k a
        k1 >>> k2 = (>>= k2) . k1

Effectively, '>>=' captures the continuation into 'Yield'. Unfortunately, it's left-associative, i.e. `((((k1 >>> k2) >>> k3) >>> k4) >>> k5)`. If implemented directly, this will repeatedly rebuild four compositions every time 'k1' yields. Right-associative `(k1 >>> (k2 >>> (k3 >>> (k4 >>> k5))))` performance is vastly superior. To resolve this, it is feasible to explicitly model a queue of continuations, or to *accelerate* '>>>' (or '.' in general). In context, I favor acceleration. 

Behavior is embodied in the runners (aka handlers). It is often convenient to express 'stacks' of partial handlers for local subtasks that forward unrecognized requests.

        -- generalize Reader to pure queries; forwards unhandled queries
        runEnvT env (Yield (Env q) k) | Just r <- env q = runEnvT env (k r)
        runEnvT env (Yield rq k) = Yield rq (runEnvT . k)
        runEnvT _ r@(Return _) = r

        -- generalize Env to effectful requests, a command shell
        runCmdT sh (Yield (Cmd cmd) k) = sh cmd >>= runCmdT sh . k
        runCmdT sh (Yield rq k) = Yield rq (runCmdT sh . k)
        runCmdT _ r@(Return _) = r

        -- generalize State to indexed, scoped Memory
        --   memory is extensible via unique symbols
        runMemT m (Yield (Mem idx op) k) =
            match idx with
              Outer idx' -> 
                Yield (Mem idx' op) (runMemT m . k)
              _ ->
                match op with
                  Get -> runMemT m (k (m[idx])) 
                  Put v -> runMemT (m with { [idx] = v }) (k ())
                  Del -> runMemT (m without idx) (k ())
        runMemT m (Return r) = (Return (r, m))

        -- cooperative threads: round-robin, no preemption
        runThreadT (Yield rq k):ts = 
            match rq with
              Spawn t -> runThreadT (k ()):t:ts
              Pause -> runThreadT (ts ++ [k ()])
              _ -> Yield rq (runThreadT . (:ts) . k)
        runThreadT (Return _):ts = runThreadT ts
        runThreadT [] = return ()

        -- effectful choice, commit on Return but captures search state
        runChoiceT bt (Yield rq k) =
            match rq with
              Choose (x:xs) ->
                let bt' = runSearchT bt (Yield (Choose xs) k)
                runSearchT bt' (k x) 
             Choose [] -> bt
             _ -> Yield rq (runSearchT bt . k)
        runChoiceT bt (Return r) = Return (r, bt)

        -- sample auxilliary functions for runChoiceT
        eff rq = Yield rq Return
        fork xs = eff (Choose xs)
        fail = fork [] 
        return = Return
        onAbort op = 
            fork [false, true] >>= \ aborted ->
            if aborted then (op >> fail) else return ()

We can also model runners that scope effects:

        -- fobid effects from escaping
        runPure (Return result) = result
        runPure (Yield _ _) = error "unhandled request in runPure"
 
        runEnv env = runPure . runEnvT env
        runMem m = runPure . runMemT m
        -- pure 'runThread' is useless

        -- choice can be heavily optimized with laziness
        runChoice (Yield (Choose xs) k) = List.flatMap (runChoice . k) xs
        runChoice (Yield _ _) = error "unhandled request in runChoice"
        runChoice (Return result) = List.singleton result

        -- secure effects with bearer token
        runSecureT auth (Yield rq k) =
            match rq with
            | (Auth tok rq) | (auth tok rq) -> 
                Yield rq (runSecureT auth . k)
            | _ -> error "unauthorized request"
        runSecureT _ r@(Return _) = r

We'll need a library of useful, reusable handlers. However, we don't need a lot of handlers, just several deliberately designed handlers that flexibly cover most use cases. For example, I introduce MemT as a more-extensible State monad, and EnvT as a more-extensible Reader, because I frequently found the Haskell originals awkward to work with.

*Note:* Although we can leverage effects for general-purpose programming, doing so dilutes design. Many of my design decisions about reasoning and performance are dubious in context of runtime effects. This project shall focus exclusively on assembly-time programming.

### Commutative Effects

Monads sequence effects. Unfortunately, monads *overspecify* order in many cases. Many effects can be partially reordered without influencing outcome, yet with a significant impact on performance. Unfortunately, with free-er monads and separate runners, it's infeasible to optimize implicitly. What we can do is explicitly 'fork' subtasks then heuristically merge effects based on pending requests.

        [(Yield BranchingQuery k1), (Yield TightConstraint k2), ...]
        -- constraint runner: let's add TightConstraint before branching!

Essentially, we can context switch heuristically based on pending requests. We don't need fully commutative effects: a partial order is sufficient. In the stack transform context, to preserve structure, we might distinguish a 'main' thread that is allowed to pass requests up the stack.

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

When loading a module, a front-end compiler is selected from the provided environment based on file extension, i.e. `Base.env.lang.[FileExt]` should define a language object that defines a 'compile' method. Users may indicate interpretation as an alternative file extension, overriding the actual file extension.

The 'compile' method is a monadic expression with access to several effects:

- parser combinator over file binary
  - integrates metadata about what we 'expect' to parse for debugging
  - maintains source location for source-mapping annotations
  - split binary into sections that are parsed separately to isolate errors
  - multi-pass, e.g. to extract names or general structure in separate rounds
- gensym for unique symbols
- import and include operations
- write definitions instead of returning them
  - ensures we never define things twice
  - can skip writes from broken sections
  - captures parse locations and dependencies

An important design constraint is that the front-end compiler never directly observe Base or Self arguments to a module. This significantly mitigates risk of divergence on fixpoint or compromising extensibility, at least at the module layer. It also never directly observes filepaths, mitigating risk of location dependence. Integration with debugging and isolation of errors are also valuable.

### Syntax Bootstrap

If the final `Self.env.lang.[FileExt].compile` method is different from the Base version, the assembler attempts bootstrap. This involves recompiling with the Self version, repeating until fixpoint is reached.

        # pseudocode
        #   args include Base, file binary
        bootstrap(ext, args, compiler) =
            let m = compile(compiler, args) 
            let compiler' = 
                m.env.lang.[ext].compile if defined
                or builtin if ext is "g"
                or error "no compiler found"
            if(compiler == compiler') then m else
            bootstrap(ext, args, compiler')

The built-in compiler is simply treated as one more compiler in the bootstrap cycle, equivalent to itself. A module may simply delete a provided "g" implementation to avoid user-defined feature extensions. In practice, syntax bootstrapping should be relatively rare: it's expensive and easily goes wrong. 

But it has some use cases. Mostly, adding features to the primary ".g" language and shifting dependencies from assembler version to module system.

## Editable Projection

I have the idea and intention that, alongside definitions - perhaps as a form of annotation - we could 'write' some objects representing views and editable projections of code. Details TBD. Miscellaneous observations:

- The 'edit' half of a projectional editor is essentially limited to a local project folder. Users that share a project through DVCS would still edit locally. But a projectional editor should be able to navigate remote DVCS resources, support visualization, and support users in updating revision hashes.
- Editable views must be bound to source elements at meaningful boundaries, i.e. essentially to parser combinators that also describe what is 'expected'. We can feasibly inject some metadata alongside the indicator that we're parsing an 'integer'. For example, we could provide a function that converts an integer into valid source code at that location. This must be captured for projectional editing. 
- Ideally, editable views may gather related elements from multiple locations within a file, and even across multiple files, to support collective views and shared editing. As a simple example, we might want to lookup and rename all references to a module definition. This suggests 'writing' some content-dependent indices alongside writing definitions. 
- To support projectional editing, we cannot just view sources. We also need a clear view of outcomes, what happens when you twiddle this or that bit. A viable solution is to visualize testing alongside projections, treating tests as viewports into outcomes.

## Reasoning

What can we feasibly implement to support developers in reasoning about the assembly process and product?

* *Testing*: Testing involves sampling behavior and judging it. I propose to model tests monadically with non-deterministic choice. Choice simplifies work sharing, parallelization, fuzzing, simulation of race conditions, etc.. Ideally, tests also support effective visualization, sorting, and graphing. This suggests reporting test conditions and outcomes as a structure that is fairly uniform between tests from the same origin. Instead of reporting only the final record at the end, it would be useful to report partial outputs. Each test may also generate a 'log' to flag events and states for future review.

* *Visualization*: We can easily introduce annotations for logging and profiling to obtain feedback. Plain text is very limiting. The assembler can provide a local web server or projectional editor GUI, and the 'log messages' might be expressed as renderable objects. Although immutable, interaction with visualizations is still useful for progressive disclosure, filtered views, rotating graphs, queries, etc.. (Of course, messages should *also* support a plain text rendering.)

* *Reflection*: A peek under the hood might let us render computations in small steps to understand a problem. Access to the continuation might support testing and visualization with 'what if' scenarios. I hesitate to introduce reflection in general because it compromises type and abstraction-based reasoning. But we can safely provide a reflection API in context of assertion or logging annotations.

* *Tracing*: The assembler can maintain metadata to trace outcomes (errors, data, etc.) back to relevant sources - a reverse lookup index on the assembly process. Tracing will be heuristic and lossy for performance reasons, but we can use annotations to guide heuristics and potentially recompute for precision.

* *Types*: Developers may annotate terms or definitions with partial types descriptors (i.e. optional, gradual typing). The assembler makes a best effort to prove or disprove types, i.e. error if disproven, warning if neither proven nor disproven. In some cases, this best-effort may be 'dynamic', examining actual arguments and return values during assembly. Aside from annotations, we can model abstract, nominative data types by controlling export of unique symbols.

* *Abstract Interpretation*: Leveraging monadic effects, a library of assembly mnemonics could simultaneously 'write' machine code, maintain abstract machine states with Hoare logic, and propagate state changes through a constraint system. Users then express assumptions as additional constraints. Effectively, users are free to reinvent type systems within a limited scope.

Note how tests and types are bound to definitions. It would not be difficult to annotate types and tests deep within functions. However, doing so hurts extensibility because we cannot update deeply embedded annotations in the same way we update definitions. In practice, even types on individual definitions may prove troublesome.

I hope to leverage abstract interpretation as the main tool for sophisticated reasoning about assembly output. I find it easier to trust code that I control directly (as opposed to an assembler's "best effort" at types), and there is more opportunity extend or tune the reasoning. A constraint system can be very expressive, effective for phantom types, substructural types, dependent types. We can feasibly leverage explicit reasoning outside of annotations for program search.

### Constraint Systems

To support abstract interpretation, we can introduce a constraint monad. Essentially, this is a more sophisticated choice monad that remembers prior choices, binding them to variables. Relevant operations:

- Query properties on variables, e.g. `(x < 5)` or `(x > y)`. This returns a true/false branching *choice*. But if we already know something about the variable, e.g. that `x = 3`, then we implicitly fail the conflict branch.
- Insist on properties, i.e. to push constraints. This is equivalent to query followed by failing the false branch. The only reason for insist is performance. (I'd use "assert", but that has other connotations in programming.)
- Check, i.e. trigger an sat/unsat check for the current branch. Useful for performance or for a 'runConstraintT' where we want to record some metadata that we won't backtrck.
- Choose from a list, same as runChoice. Failure is expressed as choosing from an empty list. Technically, this can be expressed with Query and a throw-away variable, so introducing Choose is for performance. 

Ignoring the several performance options, this essentially reduces to Query. We need a DSL for the query, but fortunately that design is already covered: directly adapt SMTLIB2 to support acceleration! Variables shall be globally named, e.g. as `(Var "x")` in the DSL, where `"x"` may be any valid dictionary key. We'll rely on unique symbols and user-defined allocation and naming strategies to control conflicts.

We cannot ask for the value of a variable. Potential values for 'x' knowing `(x < 5)` include 4.9999999, 4.9999998, 3.1415926, -12, etc. so asking would murder performance. Even with acceleration, we *cannot view* solutions discovered by an accelerator such as Z3 because the solution Z3 discovers certainly isn't the solution the accelerated function would discover. Only sat/unsat is safely observable. But we can extract the full constraint system and unevaluated branches upon return.

Aside from analyzing for sat/unsat, it may prove useful to analyze 'blame', e.g. based on [Cornell's SHErrLoc project](https://www.cs.cornell.edu/projects/SHErrLoc/). After we isolate blame to some fragment of the output, we can trace it further to assembly sources.

*Aside:* A constraint monad is an obvious candidate for *Commutative Effects* described earlier. The order in which constraints are pushed, branches are pruned, does not influence outcome, but can hugely impact performance. May need to add an effect for this.


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


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

Regarding the extensions:

Annotations are structured comments for tooling. Annotations may influence instrumentation, verification, or performance. Use cases include logging, profiling, debug views, type checking, assertions, acceleration, and memoization. However, as comments, annotations are not observable within the program.

Symbols are abstract data with equality checks. Unique symbols are useful for data abstraction, access control, and conflict avoidance. Unfortunately, pure functions cannot locally construct globally-unique values. To resolve this, we treat the module system as a source of uniqueness; front-end compilers may allocate unique symbols as needed, but only a static number per import.

## Performance

Performance of lambda calculus is mediocre by default, and bignum arithmetic certainly won't help. But performance can be significantly enhanced with guidance from annotations. This becomes relevant in context of intensive testing or metaprogramming. The most relevant patterns:

*Acceleration*: An annotation requests that a user-defined function is substituted by a specified built-in. The assembler performs ad hoc verification then replaces the function or emits a warning. Although we can accelerate individual functions such as matrix multiplication, the more general solution is to develop memory-safe DSLs that easily compile to CPU or GPGPU, then accelerate interpreters for those DSLs.

*Parallelism*: An annotation indicates that a lazy thunk will be needed later. The assembler adds the thunk to a queue to be processed by a background worker thread. The assembler provides a number of worker threads similar to the number of cores. Depending on configuration, this can feasibly be extended to remote workers.

*Caching*: An annotation suggests specific functions should be memoized to avoid rework. The annotation may provide further guidance on mechanism: persistent or ephemeral, cache size and replacement policy, coarse-grained lookup table versus fine-grained decision-tree traces. Depending on configuration, and leveraging PKI signatures and certificates, it is feasible to share remote cache with a trusted community.

Aside from these patterns, it is feasible to support annotation-guided just-in-time compilation of functions. But I wouldn't pursue JIT before bootstrapping the assembler.

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

Most observations on Base or Self either diverge on fixpoint or compromise extension. A useful exception is 'ifdef' on Base: mixins can express simple conditional definitions based on whether a prior definition exists.

Inheritance and override is a useful mechanism for extensibility. For example, we can model a grammar as an object where the methods represent parser combinators, then extend the integer parser. In context of binary specification, available overrides will likely be less structured, more organic, but still useful.

Note that implementation inheritance is the focus, not subtyping or substitutability. Extensibility is producing useful variations of a system without invasive edits. But useful variation doesn't imply monotonic updates. For example, it might be useful to disable a parse rule to restrict a language.

*Aside:* I'll call 'specification' or 'spec' what most OO languages call 'class'. I feel specification has cleaner connotations, notably avoiding connotations of subtyping.

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

Even without a runtime, effects are very convenient for tacit dataflow and program decomposition. We'll use effects for writing outputs, maintaining context, parsing inputs and backtracking, non-deterministic choice for testing, etc.. Perhaps we'll even model concurrent coroutines for task-based decomposition.

Oleg's paper on free-er monads (linked earlier) is developed in context of Haskell. It dedicates a lot of structure to work within Haskell's type system. In context of the untyped lambda calculus, we can ignore most of that, reducing to:

- monadic expressions return final result or yield `(request, continuation)` pairs
- continuations as queues to lazily flatten left-associative monadic compositions

The latter is a performance concern that is better resolved by the assembler, i.e. provide standard optimizations for both left-associative function composition and tail calls.

In context of tagged data and uniform syntactic sugar for effects, we might introduce a standard tag for effects, e.g. `(Eff rq k)`. Then, any other tag is a final result. Handlers are simply functions that process effects. Typically, they are recursive until we return the final result from the continuation.

        runReader env (Eff "get" k) = runReader env (k env)
        runReader _ result = result

        runState st (Eff "get" k) = runState st (k st)
        runState _ (Eff {"put":st'} k) = runState st' (k ())
        runState st result = (result, st)

In practice, we'll usually want partial handlers that handle only a subset of effects, passing others onwards. This requires some additional composition:

        runReaderT env (Eff "ask" k) = runReaderT env (k env)
        runReaderT env (Eff op _) = (Eff op (runReaderT env . k))
        runReaderT _ result = result

Ideally, we'll have some good syntactic sugar for monads and partial handlers, covering all the use cases. Better, by only providing access use through the sugar, we can forbid `Eff` from ever appearing as data. The user pattern-matches requests, and recursion state may be reified as data in most cases.

*Note:* It is not difficult to lift monadic effects into general-purpose programming. But doing so dilutes language design, e.g. to account for runtime reasoning and performance. We'll optimize this language for assembly-time metaprogramming.

## Modularity

A module is represented by a file. Modules may reference other files within the same folder or subfolders, or content-addressed remote folders. We forbid Parent-relative ("../") and absolute filepaths. These constraints ensure folders are location-independent and temporally stable, yet editable by conventional means.

A content-addressed remote folder is uniquely identified and authenticated by secure hash of content or DVCS revision history. However, they are not *located* by secure hash. Instead, a remote import may provide a list of locations (URLs), and configurations may suggest a few more. It is useful to include a DVCS tag or branch name for shallow cloning and to notify users of updates, but it cannot replace the secure hash.

To ensure extensibility, I propose to model modules as objects with a few effects prior to fixpoint, roughly `Dict -> {import, gensym} Dict -> Dict`. A consequence of this design is that importing a file multiple times will result in distinct symbols and definitions (depending on the provided 'Base' environment). This is mitigated by sharing through 'Self.env.\*' instead.

Modules are lazily loaded, i.e. import is deferred until a definition is observed. It is expected that users develop very large namespaces then use only a few definitions for any given assembly.

### Configuration

The assembler implicitly loads a module based on the `GLAM_CONF` environment variable or an OS-specific default, i.e. `"~/.config/glam/conf.g"` in Linux or `"%AppData%\glam\conf.g"` in Windows. This module specifies a configuration. A small, local user configuration typically extends a large community or company configuration, imported from DVCS.

A configuration primarily specifies an initial environment for an assembly module, provided as the Base dict. This shall be the only configuration element that potentially affects assembly binary output. Assemblies may ignore or hide this environment.

Configurations more freely influence tuning and resources for logging, testing, caching, GPGPU acceleration, distributed computing, etc.. Anything that doesn't affect assembly output.

### Assembly

Any module that specifies a binary, or comes close enough that a binary can be specified on the command line, effectively serves as an assembly. 

The selected module is parameterized by the configured environment. Some assemblies may be script-like, mostly composing resources defined in this environment. Users may also extract binaries from the configured environment without reference to a separate assembly module. 

The assembler has no built-in knowledge of CPU architectures or assembly mnemonics. It is feasible to 'assemble' documents, ray-traced images, or JSON configuration files. The language isn't optimized for those use cases but external DSLs are feasible.

## Reasoning

The untyped lambda calculus does not require type annotations. However, type annotations are useful to express and verify assumptions or isolate blame. I propose to support gradual typing with an ad hoc mix of static and dynamic checks. However, it is awkward to encode sophisticated properties into a type system.

Annotations can express assertions. Because there are no time-sensitive runtime effects, we can evaluate assertions in parallel with or after generating the binary. Instead of pure boolean expressions, I propose to model assertions as effectful computations with *non-deterministic choice*. This extends assertions to fuzz testing, property testing, constraint satisfaction. We may introduce additional effects providing heuristic guidance for search.

Instead of relying on fine-grained type annotations, I imagine assembly programs will simultaneously write out a binary (or staged precursor) and a representation of abstract meaning, a constraint system. Ideally, borrowing heuristic blame annotations from [Cornell SHErrLoc project](https://research.cs.cornell.edu/SHErrLoc/). We then 'assert' constraints (ideally with a Z3 or SMTLIB2 accelerator) at suitable boundaries.

Ideally, we verify all functions terminate based on static analysis of the assembly. Annotations may provide hints to guide analysis. However, can be difficult to prove and doesn't imply *timely* termination even when proven. Ultimately, we'll likely rely on configurable quotas to robustly reason about termination.

## Debug Views









## User-Defined Syntax

Users may specify a compiler function on import, or one will be selected for them based on file extension and argument to the module.

Users should be able to redefine the built-in syntax, so we'll start by discussing how users integrate.

(TBD)

Users may explicitly specify a front-end compiler when importing a file. In the trivial case, this could directly return a file binary. 

A front-end compiler can be expressed as a parser combinator: a monadic function with limited effects for backtracking, declaring symbols, defining specifications, and so on. Ideally, we carefully design this monad to uniformly support syntax highlighting, error isolation and reporting, edit suggestions, editable projections, source mapping for blame or debug views, etc.. This is a significant design challenge that I'll expand on later.

If the user does not specify a front-end compiler, the assembler will default to a built-in for ".g" files or query the import argument for other file extensions. (This might be expressed as a general 'compile' function that takes a cleaned up file extension as an extra argument. Details TBD.)

Usefully, we'll apply the same rule at the assembler CLI, allowing assemblies to be specified in any front-end syntax supported by the configured environment.

*Note:* Namespace macros shall essentially be front-end compilers *hold the file*, i.e. importing with just the front-end compiler expression instead of the file context or input, preserving relative paths to the current file for further imports.


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

## Verification

Verification is driven by annotations. Though, it may also be relaxed or disabled by configuration.

There are two problems here: verifying the assembly process, and verifying the generated binary. In general, it's a lot easier to verify the process, so it's convenient to align verification of the generated binary with verification of the process.

But it is also feasible to support something like Hoare logic at the level of registers, and build up precise descriptions of the behavior for every CPU instruction. We might need to pass resulting constraint systems to an SMT solver, perhaps as an annotation, a specialized assertion of sorts.

TBD: review SMT solvers, or perhaps [SHErrLoc's](https://www.cs.cornell.edu/projects/SHErrLoc/) intermediate constraint language.

### Assertions

We can easily have assertions at many layers: modules, specs or objects (as a whole), and functions (local per call). Assertions modeled as annotations cannot influence output, but we coould feasibly support effectful assertions that raise an error. Probably want to distinguish the two.

### Types

Pervasive use of tagged data supports flexible pattern matching and ad hoc polymorphism. Effectively, we have a dynamically typed language. Though, in context of compile-time metaprogramming, this 'dynamic' refers to a late stage after modules are loaded.

However, it is feasible to perform static analysis to detect some errors earlier. We can introduce type annotations to guide this analysis.

Usefully, type annotations can potentially express 

Most types cannot say much about the assembly code, only about the metaprogramming framework. 

only about the framewor

Ultimately, we'll pro also want the opportunity to analyze assembly

These 'types' apply to the compile-time metaprogram. It may be feasible to support some assertions that are contingent on assembly, but it certainly is not trivial.

## Tests

To detect errors earlier, we can support tests per assembly module. 

A simple way to express tests is a simple `Integer -> Boolean` function.  

To mitigate, we can express some tests per module, e.g. as assertions. 

Of course, we can expres tests for modules, perhaps even per specification.  tests for modules



Ideally, we can detect many errors earlier, before modules are shared. 

We can perform some type inference and check for consistency issues. But 


Although it is better to detect errors later than never, it is best to detect errors early. One relevant boundary is to detect errors within a module before it is shared and used in an assembly.

We can develop static analysis leveraging dataflow, abstract interpretation, etc. to detect some errors ahead of time. But, in context of higher-order metaprogramming, inference is limited. To support analysis, we can introduce annotations 


for assembly modules that search for problems. We can introduce type annotations to precisely describe programmer assumptions.






given assembly languages have no runtime, and all this computation is

this 'dynamic' must be understood in the context of assembly-time stages. 

The 

 to dynamically typed languages. 

though 'dynamic' in this context refers to the 'evaluation' stage of assembly, which is still 

We may benefit from analysis to  prior to evaluati

Although all computation is static, we can still distinguish stages such as parse and evaluation.

 Although f




We build upon the untyped lambda calculus. However, t



Static dataflow analysis can potentially detect some errors before evaluation.







 may detect obvious errors, e.g. providing a number where a function is expected.

The 


I'm envisioning gradual types, partial types, using types to help detect or isolate errors. In some contexts, failure to prove a type might be a warning instead of an error, and only an active disproof is an error. 

Non-trivial type checking may be separated from the assembler, run as an independent operation with its own output and debugger.

## Instrumentation

logging, Profiling, 

## Assembler CLI

This project should develop an initial assembler executable

 is parameterized by an assembly file and specification name.

## Syntax Ideas 

### User Data

I hope to develop a lightweight syntax for users to define data constructors, leveraging unique symbols for pattern matching and sealing. We can leverage smart constructors, active patterns, and type annotations.

### Texts

The syntax will support '"'-quoted strings of printable ASCII (0x20-0x7E minus 0x22) as a limited embedding for binary data. There are no escape characters, but users may apply a functions to process the string, e.g. to support tabs and newlines, hexadecimal, or base64.

For large texts (and binaries), users may simply load a file from the module system as a binary. This may eventually support DSLs or higher-level general purpose languages.

*TBD:* I'll want a lightweight syntax for building lists. A syntactic sugar for a writer monad (or free monad used as writer) seems viable.

## Annotations

Annotations are structured comments that support instrumentation, verification, performance, and other tools. Annotations must have no impact on primary outputs except to halt compilation with errors. But they may influence *auxilliary* outputs, such as:

- message logs: debug trace, warning
- compile-time performance, profiling
- caching for incremental compilation
- source maps for runtime debugging

To resist silent degradation, we'll report warnings for unrecognized or unsupported annotations.

## Acceleration

With guidance from annotations, compilers may substitute user-defined functions with built-ins. This pattern is called *acceleration*. The aforementioned *Data Extensions* are essentially standard accelerators.

An especially useful pattern is to accelerate an *interpreter* for a virtual CPU or GPGPU, such that we can leverage low-level hardware for number crunching at compile-time. 

## 


###

## Modeling Objects

## Content-Addressed Memory


## Sketch of Syntax

TBD.

I'd like to avoid pervasive use of indentation. But we have specs, definitions within specs, etc. Perhaps we can simply use a visually distinctive section separators.

A literate programming style might also be nice.

## Design Concerns

### Reproducibility

If there is no error, primary assembler output is a deterministic binary on stdout. This binary must depend only on locally reproducible inputs: e.g. source code and a clear subset of command-line arguments (e.g. after '--'). *Note:* Auxilliary outputs (logs, warnings, cache, heat generated, etc.) are not restricted.

### Update Path

We can extend assemblers with new features, but we must respect reproducibility. Thus, attempting to use the features with the prior version of the assembler must generate an error. Conversely, we must also be able to deprecate old features, so long as we raise an error when an old assembly depends on the deprecated feature.

A viable approach is to forbid reflection (ifdef or iteration) on some dictionaries. This allows us to provide assembler features without letting binary output observably depend on assembler version.


...



If there is no error, primary assembler output shall be a deterministic binary, depending only on source code, command-line arguments, and optionally a binary input. If there is an error, a partial binary may be produced before halting with error.

Auxilliary outputs (logs, cache, heat generated, etc.) are less restricted and may depend on assembler version. Assemblers may introduce new 'features' but only if it results in errors.

independent of assembler version. Of course, it does depend on source updates and command-line parameters and source updates, of course.

This usually represents an executable binary or library, but nothing prevents leveraging assembly language as a simple calculator. 


That is, there are no implicit optimizers.

Though, it may depend on command-line parameters passed to the assembly. 

 modulo updates to source code and a few command-line arguments passed to the assembly. There is no hidden optimization step. 

The 'compiler' in this case is responsible for loading modules and lazily evaluating pure functions, ultimately extracting a deterministic binary from the module system.









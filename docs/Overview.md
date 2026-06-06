# Project Overview

Tentatively called 'glam' - general language assembly.

This is not a conventional programming language. Its foundation is a binary-assembly programming language that uses a pure lambda calculus and a few design patterns - notably, free monads and open-recursive objects - to support an extraordinarily expressive, extensible metaprogramming system. 

## Design Goals

Relevant design goals:

- Assembly programming look and feel, where appropriate. 
  - The look aspect involves vertical columns of concise mnemonics without line noise, ad hoc '.bss' and '.data' section declarations, jumping to labels, etc.. We'll support this with careful design and a spoonful of syntactic sugar.
  - The feel aspect is all about personal control, that the only design decisions users must compromise with are their own. This requires careful attention to module system design, with focus on stability, extensibility, and local reasoning.
    - Stability means dependencies aren't changing when I'm not looking. Manual updates only.
    - Extensibility means I can modify a module non-invasively, from within my assembly. 
    - Local reasoning means I can reason about distrusted code without reading it.
- Flexibility. Assemble x86, ARM, WASM, etc.. Also assemble PDFs, PCM, etc.. Multi-file assembly.
- Reproducibility. Given the same sources, produce the same binary. Every time. Anywhere.
- Adaptability. Divide sources flexibly between assembly and system configuration. Support multi-target assembly. 
- Verifiability. Flexible analysis, testing, visualization, and interactive debugging of assembly process and product.  
- Scalability. Effective support for enormous, expensive assemblies. Or at least a clear design path. 
- Tiny. The assembler executable should be small and simple. Push logic into sources where feasible. Eventual bootstrap.

I'm not aware of any existing language that fits more than a couple of these design goals at once.

## Design Overview

- Foundation is pure, untyped, lazy lambda calculus with built-in numbers, lists, dicts. 
  - Plus a few annotations for performance or debugging, e.g. to control laziness.
- Objects are modeled as dictionary mixins with open recursion via fixpoint.
  - Supports inheritance, override; multiple inheritance is feasible.
- Module system namespace is modeled as one very large object. 
  - We 'include' modules for inheritance, acts as anonymous mixin.
  - Hierarchical modules for lazy loading and namespace control.
  - Can override deep definitions, e.g. `foo.bar.baz := .prior + 1`.
  - Thread 'env' to hierarchical modules by default for adaptability.
- Two root modules: assembly (via command line) and configuration (via environment var).
  - Configuration primarily defines 'conf.env' as initial 'env' for assembly.
  - Assembly may ignore the configured environment and substitute its own.
  - Assembly defines 'asm.result', usually a binary, as final assembled outcome.
  - Configuration also guides performance tuning, debugging, resource management, etc..
- Stable, reproducible modules via local isolation and content-addressed remotes. 
  - Remotes are primarily identified by DVCS revision hashes. Transitively immutable.
  - Folders as isolated packages: import forbids `"../"` and absolute filepaths.
- User-defined syntax aligned with file extensions, e.g. via `env.lang.[FileExt].compile`
  - Front-end compiler for ".g" syntax is built into the executable.
  - Expressed effectfully, API supports incremental compilation, debugging.  
  - Implicit bootstrap by overriding the initial front-end compiler.
  - Serves as a last resort for absolute control over module system.
- Reflection tasks for reasoning and visualization.
  - Expressed effectfully. Assembler provides an abstract reflection API.
  - Can peek into functions, support ad hoc analysis, e.g. typechecking.
  - Can subscribe to functions, e.g. to observe and render arguments. 
- Assembly mnemonics are expressed effectfully, as a sequence of writes ops.
  - Can also model '.bss' section declarations as state updates.
  - Assembly applies a pure 'runner' to compute the binary result.
  - Concision via desugaring `%movl` to invoke an abstract effects API.
- Path for scalability and performance via sparks, accelerators, caching.
  - Design GPGPU-friendly DSL, 'accelerate' via compilation to actual GPGPU.
  - Sparks trigger lazy thunks to eval in background. Configure CPUs, cloud.
  - Cache for incremental computation, avoid rework. Proxy cache to share. 

It isn't a small design. Even this overview is a wall of text. But we can isolate it down to just the difficult logic the assembler is responsible for:

- a few monadic effects APIs: front-end compilers, reflection tasks
- initial front-end compiler for ".g" files. Bootstrap loop.
- remote DVCS access (git, hg, darcs). Maybe start with just git.
- performance features: accelerators, sparks, garbage collection, JIT

It seems feasible to keep this logic relatively small, so long as we aren't eager to add accelerators. Best to design just a few accelerators that cover many use cases.

## Detailed Documentation

These documents cover a great deal of detail. They're also very large, at the moment. I'd like to break them down further so agents can read just the parts they need.

- [Syntax.md](./Syntax.md) - initial ".g" syntax
- [Design.md](./Overview.md) - underlying semantics 


# Project Overview

This project is tentatively called 'glam' - general language assembly.

## Design Docs

- [Design Principles](./DesignPrinciples.md) - Guiding principles for the design. Summary:
  - absolute control - the guiding star for all design decisions is localizing control
  - extensibility, stability, inverted access control - control in context of modularity
  - reproducibility, verifiability, scalability, comprehensibility - contribute to control
  - flexibility, adaptivity, interactivity - further enhance the programming experience
- [Design](./Design.md) - Summary:
  - pure, untyped lambda calculus for metaprogramming of assembly
    - built-in numbers, lists, and dicts for convenience
    - model objects via open fixpoint dictionaries
    - model effects via free monads, object as API
  - no app runtime; extract pure binary or dict folder
    - but we do run reflection tasks and optional IDE
    - focus of reflection or IDE is entirely inwards
  - content-addressed, location-independent modules
  - namespace as one big object; modules as mixins
  - sources divided between assembly and configuration
    - config provides 'env' to assembly for adaptability
    - config also defines resources and the IDE process
  - express assembly effectfully
    - users 'write' mnemonics via abstract effects API
    - can also 'write' section declarations (.bss, .data)
    - procedures effectively serve as assembly macros
    - highly extensible state and control flow
- [Syntax Design](./Syntax.md) - design of initial syntax
  - borrows a lot from Haskell, but untyped by default
  - innovations on pattern matching, event continuations

## Assembler Executable

The design document details the executable. Relevant logic:

- free monadic effects APIs: front-end compilers, reflection tasks, user interaction
- initial front-end compiler for ".g" files; a bootstrap loop for compiler overrides
- remote DVCS access (git, hg, darcs) and local caching. Likely start with just git.
- performance features: accelerators, sparks, incremental computing, JIT, cloud, etc.

A minimum viable API for user interaction is very small, e.g. listen for TCP connections to service HTTP requests. Interaction logic can be moved into the user configuration. 

Performance features are the main area where the assembler executable may potentially grow without bounds. We can always introduce another accelerator, improve caching, further optimize JIT. Of course, assembly-time performance does not affect assembly results, only how swiftly we obtain them. We can pick some low-hanging fruit and leave the rest for later.


# Project Overview

This project is tentatively called 'glam' - general language assembly.

## Design Docs

- [Design Principles](./DesignPrinciples.md) - Guiding principles for the design. Summary:
  - absolute control - the guiding star for all design decisions is localizing control
  - reproducibility, verifiability, scalability, comprehensibility - contribute to control
  - flexibility, adaptivity, interactivity - enhance the programming experience
- [Design](./Design.md) - underlying semantics. Summary:
  - pure, untyped lambda calculus for metaprogramming of assembly
    - built-in numbers, lists, and dicts for convenience
  - model objects via open fixpoint, module namespace as objects 
  - model effects via free monads, effects APIs as abstract objects
  - assembly mnemonics expressed effectfully, 'writing' a binary
  - assembly 'result' is pure binary, interprets expressed effect
  - folders as packages, content-addressed remotes (DVCS rev)
  - concurrent reflection tasks for verifiability and interactivity
    - more ad hoc, relaxes reproducibility, cannot affect result
  - sources divided between assembly and configuration modules
    - configuration influences result via 'env' arg for adaptability
    - configuration also supports scalability and interactivity
- [Syntax Design](./Syntax.md) - design of initial syntax for ".g" files.

## Assembler Executable

The design document details the executable. Relevant logic:

- free monadic effects APIs: front-end compilers, reflection tasks, user interaction
- initial front-end compiler for ".g" files; a bootstrap loop for compiler overrides
- remote DVCS access (git, hg, darcs) and local caching. Likely start with just git.
- performance features: accelerators, sparks, incremental computing, JIT, cloud, etc.

A minimum viable API for user interaction is very small, e.g. listen for TCP connections to service HTTP requests. Interaction logic can be moved into the user configuration. 

Performance features are the main area where the assembler executable may potentially grow without bounds. We can always introduce another accelerator, improve caching, further optimize JIT. Of course, assembly-time performance does not affect assembly results, only how swiftly we obtain them. We can pick some low-hanging fruit and leave the rest for later.


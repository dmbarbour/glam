# Design Principles

## Absolute Control

The core principle or vibe of an assembly language is absolute control. That feeling of "I am the orchestrator of my own reality.". 

In part, control is achieved by low-level machine-code mnemonics and ability to manage layout. In context of a filesystem, the output of an assembly is a file binary, or perhaps a folder of binaries. And users should control every bit.

Control doesn't mean programmers are forced to painstakingly write low-level code. Ideally, programmers should control the level of abstraction at which they express themselves, and the interpretation of that expression. This can be supported via metaprogramming, internal and external DSLs, a tower of languages. 

Of course, in context of modularity, we'll inevitably borrow languages and interpreters developed by others. The module system requires careful attention: Stability, so dependencies never shift underfoot outside one's control. Extensibility, so users aren't stuck with another's decisions on pain of a rewrite. Conventional access control should be inverted: the client can fully access a module (no export control), yet robustly control what the module observes and how outputs are handled.

Control means the levers exist and are readily accessible "if I only wanted to".

In conventional languages, users abandon control for convenience. But it isn't the case that convenience requires giving up control. The only thing that fundamentally requires giving up control is a dependency on external systems. With careful design, we can control compilers and modules. It is feasible to extend this further: assemble unikernels (VM or bare metal) instead of executables beholden to an operating system; assemble network overlays or kubernetes systems to extend control beyond one device.

## Control Adjacent

- Reproducibility: Given the same sources, produce the same binaries. Every time. Anywhere. Without reproducibility, control is severely compromised. Reproducibility benefits from careful attention to versioning of language built into an executable, precise control of contributing sources, content-addressed versioning of remote modules, and a deterministic model of computation.

- Verifiability: Analysis, testing, and visualization of assembly process and product. Without verification, control is fragile, a false confidence easily eaten by a few bugs. Verifiability enables users to recover and maintain their confidence. However, in context of modularity and a community of programmers, we must ensure that the means of verification do not compromise control by clients of a module, e.g. ability to disable a rule that a client chooses to break.

- Scalability: From "hello world" to world domination. Although I exaggerate, there shouldn't be an upper bound on what can be reasonably assembled: executables, unikernels, kubernetes systems. Nor any artificial limits on assembly process: ray tracing, physics simulation, train an AI as part of generating assembly. We don't need to support massive systems immediately, but there should at least be a clear path forward.

- Comprehensibility: Users should be able to fully comprehend the assembler executable. They should easily be able to manually bootstrap the executable in a reasonable time frame. Though, achieving full-featured scalability, JIT of assembly-time metaprogramming, etc. may be difficult. In any case, they control this, too.

## Secondary Goals

- Flexibility: This is a 'file' assembler, i.e. it assembles binaries, files, or folders aligned in context of a filesystem. The assembly language should support x86, ARM, WASM, etc. via libraries. With suitable DSLs, we could use the assembler to output typeset PDFs, PCM music, etc..

- Adaptability: Assemblies and modules may adapt to context, e.g. for portability or to integrate resources. We'll approach this in terms of dividing sources between assembly and configuration. The configuration provides an 'env' argument to the assembly, and by default we'll thread 'env' into hierarchical module imports. We should support the continuum from script-like assemblies (compose logic from 'env') to monolithic assemblies (ignore configured 'env' and substitute your own).

- Interactivity: Visualization is a form of verification - an informal 'looks right' form. Without interaction, visualization is output-only, e.g. writing a log and perhaps a few associated files (e.g. for tables or graphs). Interaction enables progressive disclosure. Further, we inevitably edit code based on feedback. It is feasible to integrate visualization and editing, supporting an integrated development environment or projectional editing. We can push interaction logic to the same configuration needed for adaptability.




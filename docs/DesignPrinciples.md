# Design Principles

## Absolute Control

The core principle or vibe of an assembly language is absolute control. That feeling of "I am the orchestrator of my own reality.". 

In part, control is achieved by low-level machine-code mnemonics and ability to manage layout. In context of a filesystem, the output of an assembly is a file binary or folder thereof, users controlling every bit. But control should not mean programmers are forced to painstakingly write low-level code. Ideally, programmers should also control the level of abstraction at which they express intended behavior and interpretation of that expression. This can be supported via metaprogramming, internal and external DSLs, a tower of languages.

At larger scales, in context of modularity, users inevitably build upon systems developed by others. This easily conflicts with vibe of local user control: it is annoying to be constrained by the designs and decisions of other users. To mitigate this, we'll use an extensible namespace so users can override any definition, content-addressed remote modules so users can precisely control dependencies. Users may also override front-end compilers to control what a module exposes. Conventional access control is inverted: a module developer cannot hide anything from the client, but clients control a module's observations.

I imagine users will rarely exercise those opportunities. The control vibe mostly requires that the necessary levers exist and are readily accessible "if I only wanted to".

The only feature that fundamentally requires reducing control is dependency on external systems or services. There are essential system boundaries for anything we define. To extend control further requires expanding the scope and scale of assembly definition beyond OS executables. For example, by assembling 'unikernels', we can gain control of physical devices. By assembling 'network overlays' or kubernetes systems, we can extend control to a network. We can feasibly assemble hardware descriptions.

## Control Adjacent

- Reproducibility: Given the same sources, produce the same binaries. Every time. Anywhere. Without reproducibility, control is severely compromised. Reproducibility benefits from careful attention to versioning of language built into an executable, precise control of contributing sources, content-addressed versioning of remote modules, and a deterministic model of computation.

- Verifiability: Analysis, testing, and visualization of assembly process and product. Without verification, control is fragile, a false confidence easily eaten by a few bugs. Verifiability enables users to recover and maintain their confidence. However, in context of modularity and a community of programmers, we must ensure that the means of verification do not compromise control by clients of a module, e.g. ability to disable a rule that a client chooses to break.

- Scalability: From "hello world" to world domination. Although I exaggerate, there shouldn't be an upper bound on what can be reasonably assembled: executables, unikernels, kubernetes systems. Nor any artificial limits on assembly process: ray tracing, physics simulation, train an AI as part of generating assembly. We don't need to support massive systems immediately, but there should at least be a clear path forward.

- Comprehensibility: Users should be able to fully comprehend the assembler executable. They should easily be able to manually bootstrap the executable in a reasonable time frame. Though, achieving full-featured scalability, JIT of assembly-time metaprogramming, etc. may be difficult. In any case, they control this, too.

## Secondary Goals

- Flexibility: This is a 'file' assembler, i.e. it assembles binaries, files, or folders aligned in context of a filesystem. The assembly language should support x86, ARM, WASM, etc. via libraries. With suitable DSLs, we could use the assembler to output typeset PDFs, PCM music, etc..

- Adaptability: Assemblies and modules may adapt to context, e.g. for portability or to integrate resources. We'll approach this in terms of dividing sources between assembly and configuration. The configuration provides an 'env' argument to the assembly, and by default we'll thread 'env' into hierarchical module imports. We should support the continuum from script-like assemblies (compose logic from 'env') to monolithic assemblies (ignore configured 'env' and substitute your own).

- Interactivity: Visualization is a form of verification - an informal 'looks right' form. Without interaction, visualization is output-only, e.g. writing a log and perhaps a few associated files (e.g. for tables or graphs). Interaction enables progressive disclosure. Further, we inevitably edit code based on feedback. It is feasible to integrate visualization and editing, supporting an integrated development environment or projectional editing. We can push interaction logic to the same configuration needed for adaptability.




# glam — Distilled Design Reference

**glam** ("general language assembly") is a file assembler: sources in ".g" files metaprogram the production of binaries or filesystem folders. There is no application runtime — the output is pure data (binary or dict-as-folder). Machine-code mnemonics, executable formats, and target architectures (x86, ARM, WASM, PDFs, audio, etc.) are all library concerns, not built-ins.

## Guiding Principles

**Absolute control** is the guiding star: users control every output bit, the level of abstraction, and the interpretation of their expression. Supporting principles: **reproducibility** (same sources → same binaries, always), **verifiability** (testing, analysis, visualization — but clients can disable a module's rules), **scalability** (no upper bound on assembly ambition), **comprehensibility** (users can bootstrap the assembler). Access control is *inverted*: no export control — clients see and can override everything in a module, but robustly control what the module observes (via `env`).

## Semantics

- **Pure, lazy, untyped lambda calculus** at toplevel; **interaction nets** (graph-structured, symmetric, one exposed port) available for performance-critical dataflow via `interaction_net` and an effects API (`.bind`, `.copy N`, `.data`, `.wire`).
- **Data types** (all immutable, implicitly type-tagged): numbers (exact rationals, unbounded — precision loss is always explicit), lists (finger-tree ropes; append `++` at either end, log-time split), dicts (finite key-value; `{}` is both empty dict and the 'undefined' value; `{foo:{}}` ≡ `{}`; no key iteration), functions.
- **Tagged data** = singleton dict: `tag:Data` ≡ `{tag:Data}`; `:tag` is the constructor function. **Atoms**: `'name` ≡ `["name"]:()`; `()` is the built-in unit atom. `anno 'scope_unique` uniquely marks atoms (comparing same atom with different marks diverges); enables ephemeron/weakref dict keys.
- **Objects** = dicts containing `spec:{name, defs, deps}`. `defs` is a mixin `\_self self -> _self with ...`; `deps` lists parent specs; multiple inheritance via linearization (C3), deduplicating by `spec.name` (globally unique via `abstract_global_path` for toplevel declarations). Anonymous objects (`object _`) skip deduplication, apply before named parents. Dicts are treated as anonymous objects for inheritance.
- **Explicit override**: `=` introduces (error if already defined), `:=` overrides (error if undefined), `::=` is a non-observing update taking the prior value (`name ::= \prior -> ...`). `_name` references the prior definition.
- **Effects**: freer monad; requests as `eff:(\api -> api.op)`, sugar `.op`; application `(eff:f) x = eff:(\api -> f api x)`. **Standard effects**: `.r` (return), `.seq` (`>>=`), `.alt`/`.fail`/`.cut` (backtracking choice), `.fix` (monadic fixpoint, used for recursive-do and forward label references), `.get Path`/`.set Path Val` (hierarchical state), `.reset Key`/`.shift Key` (delimited continuations). Handlers recognize `{eff:_, _}`; method objects `{apply:f,_} x = f x` extend applicability.
- **Conditionals are effects**: booleans desugar to pass/fail effects (`.r ()` / `.fail`), `or`→`.alt`, `and`→`.seq`. Pure `if`/`match` run under a compiler-provided stateless handler; `try`/`try_match` run in the host environment (backtracking with state access).
- **Annotations**: `anno Annotation Term` — never observable in evaluation (though may block it: divergence is unobservable). Uses: caching, sparks/parallelism, acceleration (substitute reference function with built-in, e.g. hardware emulators), `'error`/`'TBD`/`'deprecated`, scope-unique atoms. Assembler warns on unrecognized annotations.

## Modules

- Namespace = one big object; modules = mixins. Files reference only local relative paths (no `../`, no absolute, no dot-files) or content-addressed remotes (DVCS protocol + revision hash + filename + URL search list). Every folder is a standalone package.
- Import forms: `import "F.g"` (mix into current namespace), `as m` (introduce), `at m` (extend existing), `binary as b` (raw bytes, no compile), `import 'prelude` (built-in modules named by atoms). Remote: `import as q from { ref:…, rev:…, search:[…] }`.
- **Configuration** (`GLAM_CONF`, mixin list): defines `conf.env` (the `env` object passed to assembly — the sole adaptability channel), `conf.cli`, `conf.log`, `conf.ide`, resource tuning.
- **User-defined syntax**: `env.lang.[FileExt].compile` (effectful: parser combinators, import integration, namespace access). Assembler bootstraps: recompiles with the resulting compiler until it stabilizes. Compilers can't see filenames/locations (reproducibility); that metadata is reflection-only.

## Syntax (".g" files) — key points

- First declaration: `language g0 with utf8` (fail-fast versioning). ASCII default; `#` line comments only; declarations start at column 0, continuing lines indented.
- Names: `[a-zA-Z][a-zA-Z0-9]*` parts joined by `_`; paths dotted; expression-indexed paths `.[Expr]` / `.(ListExpr)`. 
- Keywords: `import as module abstract using unique with without do let in where object extend self if then else match try when and or` etc. 
- `abstract Name, ...` declares externally-provided names (also enables recursive-do forward references). `unique Foo, Bar` declares scope-unique atoms from namespace paths.
- **Object scoping**: inside objects, names bind to `self` by default; `^name` escapes one lexical level (composes: `^^name`); `as Name` gives the object a local alias instead. Shadowing warns by default. `using Dict in Expr` treats any dict as a temporary object scope. Prior definitions via `_name` (or `_self.name`).
- **Do notation**: supports both `Pattern <- op` and `op -> Pattern` (the latter suits vertical assembly columns); `Pattern = Expr` without `let`; applicatives `!>` / `<!`; pure pipes `|>` `<|`, function composition `>>` `<<`; no mixing opposing directions without parens.
- **Numbers**: `_42` is negative (prefix underscore is part of the literal); `1_000_000`, `1.23e_7`, `0xc0de`, `0b1010`.
- **Texts**: raw, no escape characters; multi-line via `"""` blocks with `"`-prefixed lines, LF-separated, no trailing LF; postprocess explicitly (e.g. `|> hex2bin`). Prefer `import "file" binary as x` for large data.
- **Patterns**: dicts `{x:P, rem}`, `{:x,:y}` sugar; lists with one variable segment (`[x]++xs`, `xs++[x]`); view patterns `(View -> P)` (must parenthesize; run before match); predicate patterns `(Pred P)` (pass/fail, captures original input); `as`, local `when` guards. Guard clauses compose with `and`; may be effects, pattern-binds, or booleans. 
- **Tentative choice** `then?`/`-?>`: branch body explicitly returns `.r Result` or `.fail`, enabling refactoring of conditional structure.
- **Macros**: `@name` / `@(Expr)`; effectful, read/write source at text or AST level; cannot escape their bracket/indent scope, can't see comments, whitespace, or other macro invocations; may write lazy thunks as embedded-data AST nodes (compile-time → assembly-time transfer).
- `with` updates on dicts/objects use the same `=`/`:=`/`::=` discipline; `Dict as d with ...` captures; on objects, `with` bodies are anonymous mixins and `extend Name with ...` updates the spec and re-instantiates.

## Assembler Executable

CLI: `-f file` / `-s.ext script` / `-- args` build an anonymous assembly module (earlier files override later). Output: `asm.result` as `--binary` (default, to `--stdout`) or `--folder` (`-o dest`); `asm.file_meta` for permissions; atomic file replacement. Modes: `--batch` (default) or `-i` (runs `conf.ide`: TTY, TCP/socket, light GUI, limited file editing).

**Reflection**: version-specific effects API; runs `refl.*` definitions, `conf.log`/`conf.ide`, and effectful annotations as concurrent tasks with STM-shared state. Reflection can inspect everything (bypasses abstraction), suggest edits, drive SMT solvers (Z3/cvc5), but *cannot* observably influence pure computation — `asm.result` stays reproducible. Reflection itself is not reproducible.

**Performance**: laziness (default), persistent caching (incremental assembly, shareable via proxy), sparks/inet parallelism, acceleration, ephemerons. Assembly-time performance never affects results, only speed.

## Programming Styles

- **Direct-style assembly**: mnemonics as write-only effects (`.movl 'rax 60`); procedures are assembly macros. Extensions: singletons (write-once by name/content), write cursors (multiple linked sections: bss, rodata, stack frames), abstract interpretation of machine state carried on labels, program search with backtracking, obligations, constraint models between stages.
- **Structured assembly** (aspirational): coroutines, Kahn networks, state charts, transaction loops (atomic isolated transactions, replaceable at runtime — basis for live systems). Concurrency modeled deterministically via shift-reset heap transfer; design for confluence (CALM, CRDTs, promises).
- **Multi-level DSLs**: model each language as an assembly target; domain knowledge as objects with DSL definitions (extensible, substrate-independent); loops as plain recursive functions or "loop objects" wired via continuation overrides; open continuations via extension of abstract method objects.
- **Proof-carrying code**: possible but unproven at scale (cf. VALE); assisted proving via reflection + SMT + IDE edit suggestions.


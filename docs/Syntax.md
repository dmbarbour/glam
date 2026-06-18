# Initial Syntax

This document describes an initial syntax for ".g" files, and motives for it. Design goals include:

- a syntax that I find pleasant to work with
- supports an assembly programming look and feel
- concise, vertical columns of assembly mnemonics
- generalist, not specialized for targets or domains
- an extraordinarily high abstraction ceiling

## Language Version Declaration

Reproducibility requires that the same sources produce the same outcome, but there is an implicit condition: an outcome is produced.

A language version declaration makes versioning of the language more flexible and robust in context of reproducibility. We can fail fast at the declaration if we don't support the requested version. We can adjust 'meaning' of a keyword between versions. It's also a clear opportunity to declare compiler-recognized language extensions - fine-grained compared to full versioning, but more verbose.

Proposed syntax:

        (lang|language) (BaseVer) (with Extensions)?
        lang g0 with utf8

The version declaration should be the first toplevel declaration in a ".g" file.

## Character Set

We'll start with printable ASCII and whitespace (0x21-0x7E, SP, CR, LF). It is not difficult to extend to UTF-8, though I'm concerned about legibility. We'll recognize CR, LF, and CRLF as line endings. The compiler shall emit a warning if the file uses inconsistent line endings.

## Comments

We'll support Python-style line comments, i.e. `#...` to end of line. Comments are treated as whitespace by the compiler and macros, but may be structured in context of external tooling (literate programming, projectional editing, extracting API docs, etc.). There is no syntax-layer support for multi-line comments, so just use a decent editor with vertical cursor selections.

## Toplevel Structure

The module toplevel consists of a sequence of 'declarations'. Each declaration starts a new line. If a declaration requires more than one line, any continuing lines (excepting blanks) must be indented by at least one space. This structure simplifies error isolation, local reasoning, and parallel processing.

Each declaration starts with either a keyword (such as `import`, `spec`, or `unique`) or is a basic definition of form `name = Expr` or one of its variants (args in lhs, `:=`, `::=`, etc.). We'll favor basic definitions where feasible, thus keywords are mostly for special forms.

In context of errors, the errors can be reported but we can also make a best effort to proceed with errors. This might depend on configuration options or command-line arguments.

## Keywords

Keywords are names reserved by the compiler. Users may not define or shadow keywords, nor directly use them in dotted paths or tags. Users may freely construct atoms from keywords, e.g. `'if` and `'else`. 

Recognized keywords may vary with language version declaration. By reserving candidate keywords we can avoid breaking code. I propose to reserve all keywords I introduce in this document, all English conjunctions and prepositions, and a handful of additional names. Will update this section later.

## Names

We accept a subset of C names, mostly restricting use of underscores. A viable regex:

        Part = [a-zA-Z][a-zA-Z0-9]*
        Name = Part('_'Part)*
        Path = Name('.'Name)*

Namespaces are modeled as hierarchical dictionaries, accessed via dotted path, e.g. `foo.bar.baz`. To index the dictionary, we translate names into atoms, and paths as lists thereof. For convenience, users may quote names, e.g. `'foo` becomes constructed atom `["foo"]:()` (see *Atoms*), and quote dotted paths, `'.foo.bar.baz` becomes `['foo, 'bar, 'baz]`. By default, the atoms map to dict keys, but see *Aliasing*.

In the general case, we also support expression-indexed paths using `.(ListExpr)` or `.[...]` for a literal list. These indices are interpreted such that `.([1, 'two] ++ [3])` is equivalent to `.[1].two.[3]`. The empty list is permitted, e.g. `foo.[]` is equivalent to `foo`, and `foo.[ ].bar` permits spaces, newlines, and comments mid-path.

As a special form, `.Path` expressions bind to an abstract effects API, e.g. `.foo.bar` to `eff:(\api -> api.foo.bar)`. This provides concise, convenient access to extensible effects (see *Effects*). 

Best practice is to avoid expression-indexed paths at the toplevel namespace, but it's available as an escape hatch. To resist confusion with effects, `.(...)` and `.[...]` are forbidden as initial path components at the toplevel namespace. Instead, users write `module.[Idx] = ...` in LHS or evaluate `module.[Idx]` as an expression, where `module` is a keyword aliasing the toplevel namespace. 

## Operators

Operators are essentially infix functions. We'll support Haskell-style operator sections, such that `((>>= k) op)` is equivalent to `(op >>= k)`. To avoid unnecessary parentheses, we'll support precedence between most operators. To mitigate confusion, not every pair of operators will have valid precedence, e.g. cannot mix both `>>` and `<<` without parentheses.

We may support a few special non-binary forms, e.g. `(x < y =< z)` as shorthand for `((x < y) and (y =< z))`. We'd also support `(< =<)` operator sections. Risk of confusion is mitigated because we cannot compare booleans for less-than or greater-than.

Operators may support limited ad-hoc polymorphism. For example, `>` could compare two numbers, two lists, two tuples. For lists and tuples, we use a lexicographic comparison of elements. Comparing a number to a list, or a list to a tuple, would simply diverge with an error. As a rule, ad-hoc polymorphism must preserve laws or intuitions, e.g. don't use `+` to append lists because it does not preserve commutativity of `+` on numbers.

### Application

Application is essentially expressed as a whitespace 'operator', i.e. `f x` applies `f` to `x`. Usually, we apply pure functions. But in context of ad hoc polymorphism, we might extend application to effects and objects:

- Application of effects, `eff:(f) x = eff:(\api -> f api x)`, greatly simplifies lightweight effects APIs, i.e. because the compiler doesn't need to know how many arguments an effect accepts, just how many are given. 
- Application of objects, perhaps recognized by 'spec' and 'args', may apply a mixin extending a list of arguments, i.e. `args ::= \ prior -> prior ++ [new_arg]`. This provides a basis for var-args.

Functions, effects, and objects are likely all we need, but it is feasible to extend application further.

## Introductions and Overrides

We'll syntactically distinguish introductions vs. overrides. It's an error to introduce a name that already has a definition, or to override a name that does not have a prior definition. We use `name = Expr` for introductions, `name := Expr` for overrides, and `name ::= \ prior -> Expr` for overrides with convenient access to the previous definition.

In most cases, module-scope names evaluate to final definitions. To support access to previous definitions (outside of `::=`) I propose keywords `ns_prior Expr` and `ns_final Expr`. These evaluate names in `Expr` in the specified scope. The `ns_prior` scope is definitions up through the toplevel declaration. 

As a special case, `ns_prior` is implicit in a few contexts where evaluation must occur at compile-time:

- macro invocations: `@(Expr)` is always equivalent to `@(ns_prior Expr)`
  - macros may logically 'read' more expressions under `ns_prior` scope
- toplevel expression-indexed defs: `foo.[Bar] := ...` uses `ns_prior Bar`

Use of `ns_prior module` will capture the full prior namespace. It is feasible to rewrite entire namespaces (without macros!) via `module ::= \ prior -> ...`, though it isn't really encouraged because it hinders tracing. 

## Forbid Name Shadowing

Name shadowing occurs when a name masks access to another name in scope. This usually happens with generic local names like `\map -> ...` where 'map' may mean many different things (e.g. to apply a function over a list, an associative data structure, a game world map). Unfortunately, this easily results in bugs that humans easily miss when reading code: contextual usage is obvious to humans, but the compiler's interpretation of context is not.

In context of open recursion for inheritance and override, name shadowing would be even more problematic. Masking names hinders extension, and it becomes confusing what the final definition for any given use of a name refers to. 

Thus, as a rule, we'll forbid name shadowing. But only for static contexts: we forbid shadowing of names from an included module, but not for shadowing of names introduced by *including* a module. This may involve threading metadata through includes via the namespace. It isn't difficult to avoid name conflicts.

*Aside:* Macros should receive compiler support to generate local vars without risk of shadowing.

## Abstract Definitions

In context of modules as mixins, we may assume some names are introduced by the module's client. However, it is convenient to report 'undefined' errors closer to the code that leaves names undefined. To support these cases, I propose a toplevel declaration:

        abstract Name(, Name)*

This declaration is not required for names brought into scope (or already declared abstract) via 'include'. The intuition we want for include is that integrates all toplevel declarations (modulo language version declaration). Also, hierarchical names are captured implicitly, i.e. `abstract foo` implies `abstract foo.bar`. As the standard case, `abstract env` is implicit.

The tracking of abstract definitions across is essentially just maintaining an extra set of prefixes for which we'll suppress 'undefined' errors. Should be easy to implement with just a little metadata hidden in the namespace.

## Aliasing (Tentative, Leaning No)

Support for aliasing can potentially improve the user experience for working with hierarchical namespaces, cherry-picking the elements needed. A viable approach:

        # a compact special form
        alias foo: bar.baz, qux as q,

        # desugars to
        baz = foo.bar.baz
        q = foo.qux
        module.[alias:'baz] = '.foo.bar.baz
        module.[alias:'q] = '.foo.qux

        # later, based on alias
        baz := ...
        # rewrites to
        foo.bar.baz := ...

The objection to aliasing is that it complicates implementation and comprehension, and requires its own escape hatches. For consistency, we must extend aliasing to pattern matching and object specs. I'm not convinced these tradeoffs are worthy or a good fit for the assembly vibe. Let's first see how far we can effectively flatten the namespace between `.name` effects and toplevel includes.

### Final Definitions 

In a few rare cases, it is useful to guard against accidental updates to definitions. For example, when specifying an object, it's usually an error to update the object instance instead of the object specification. Thus, we might mark the instance as final. 

A relevant question is how to express this constraint without hurting extensibility. Clients must be able to force an update regardless. A proposed solution is that to mark `foo` as final, define `module.[final:'foo] = ns_prior foo`. This convention may be recognized by reflection task, which compares each final `foo` with `.[final:'foo]` then reports an appropriate warning or error.

## Effects

We'll almost directly adopt Haskell's do notation. 

I propose a variation for RecursiveDo: forward declaration of fixpoint names. In part for visibility and clarity, in part because fixpoint scopes shift. To express forward declarations, we could use a keyword such as `expect x, y, z`.

To support concise effects without polluting the toplevel namespace, I propose to evaluate `.name` to `eff:(\api -> api.name)` and implicitly support application of effects: `eff:(f) x = eff:(\api -> f api x)`. The application rule supports `.name x y <| z` without the compiler knowing arity in advance.

Haskell has applicative style via `<*>` and `<$>` that is convenient for some use cases. We could provide similar operators, though I hope to be more concise. Will probably defer these for now.

*Aside:* We can feasibly model 'stack frames' upon a foundation of shift-reset (for early return) and state (for local vars, deferred ops). Explicit use of stack frames would enable more-conventional procedural programming. But it isn't clear whether this would benefit from dedicated keywords. Perhaps `.frame/.defer/.fexit` is adequate. Maybe add a frame 'tail-call' variant.

## Macros

In context of lazy loading, macro invocations must be distinct from normal evaluation. Proposed syntactic forms:

        @(Expr)             
        @macro_name         short for @(macro_name)

The compiler lazily evaluates `(ns_prior Expr)` at compile-time. This must return a recognized effect, i.e. `eff:(\api -> ...)`. The compiler runs this effect, providing an API to read and write code at flexible levels of abstraction. Macros can be parameterized using `@(foo arg1 arg2)` or by writing `@foo arg1 arg2` where `foo` effectfully reads its arguments. 

To simplify local reasoning and isolate errors, the macro effects API shall protect scope, i.e. balance of brackets, braces, parentheses, etc.. A toplevel macro only reads one toplevel declaration (based on indentation) but may write many. Within scope, macros may read and write expressions with flexible levels of abstraction, e.g. 'write' abstract lazy data without serializing it. Macro DSLs are feasible, with localized keywords, special forms, and operators.

To simplify interaction between macros, the API shall prevent macros from observing other macro invocations. E.g. in `(@foo @bar ...)` we'll block evaluation of `@foo` until `@bar` completes if the former attempts to read anything potentially touched by `@bar`. Macros also cannot read comments or count whitespace.

## Tagged Data

Tagged data is convenient for modeling extensible variants and resisting assembly-time type errors.

        tag:Data
        [TagExpr]:Data

Tagged data is modeled as a singleton dictionary. That is, `tag:Data` is equivalent to `{ tag:Data }` in most contexts. Constructed tags are expressed using `[Expr]:Data`. For consistency, this syntax limits users to a singleton tags, i.e. no dotted paths, exactly one element in tag constructor `[Expr]` form. Users may access tagged data via pattern matching via dotted path, e.g. `(tag:Data).tag` evaluates to `Data`.

## Atoms

Atoms are data where the only useful observation is equality.

The unit value is a built-in atom, expressed and matched as `()`. Tagged unit data, i.e. `tag:()` or `[TagExpr]:()`, serves as a symbolic atom. As shorthand, `'name` rewrites to `["name"]:()`. Note that `'tag` and `tag:()` are different values: `["tag"]:()` vs. `['tag]:()`. We'll use symbolic atoms for booleans, `'t` and `'f`. They're also convenient for naming registers such as `'eax` without defining things.

For access control and conflict avoidance, we can leverage the global namespace as a stable source of unique atoms. A viable approach is `Foo = abstract_global_path Foo`. Here `abstract_global_path` returns an atom based on the final location of `Foo` in the module system, and defining `Foo` resists accidental reuse (i.e. 'allocating' the name). I propose a toplevel declaration `unique Foo, Bar, Baz` for bulk definitions of this form. *Aside:* `abstract_global_path` is intentionally verbose to encourage the `unique` form.

Scope-unique atoms are useful for the ephemeron performance pattern. To support this pattern, we can introduce a term annotation, e.g. `(anno 'scope_unique) : Atom -> Atom` returns the same atom except annotated with unique metadata. If ever we compare the same atom with different metadata, we diverge instead, thus we never observe the violation of scope uniqueness. When used as dict keys, we can associate the data with a weakref of the metadata.

## Dicts

In expression context, `{}` constructs the empty dictionary, and `{ name1:Expr1, name2:Expr2, ...}` expresses a literal dictionary. We'll translate the literal form to a sequence of definitional steps, preserving order e.g. `{} with { name1 = Expr1; name2 = Expr2 }`. Order becomes relevant when we generalize names to dotted paths due to default introductions, e.g. `{ foo:{}, foo.baz:1 }` is okay, but the reverse order would be an error because `foo.baz` implicitly introduces `foo` and override would lose information.

Multi-line literal dictionaries accept a leading comma for convenient line-editing, consistent with lists:

        {
        , name1:Expr1
        , name2:Expr2
        ...
        }

But users will likely prefer multi-line 'with' forms:

        {} with
            name1 = Expr2
            name2 = Expr2

Expression-indexed names need some special attention. Users are free to write `{ [0]:"Hello", [1]:"World" }`. In the definitional form, this becomes `{} with { .[0] = "Hello"; .[1] = "World" }` usually multi-line.  

Dictionary updates are generally expressed using `with` and `without` special forms. These are applied much like infix operators, but the RHS of `with` uses the similar syntax as the module namespace. One difference is we can use expression-indexed paths directly. 

        Dict with 
            x ::= \ old_x -> old_x + 42
            y := 10
            .[1] = "this is new"

        Dict without x, y, z

The `with` syntax still distinguishes introductions and overrides. The `without` form removes listed names if they exist, but it is not an error to remove an undefined name. In case of removing dotted paths, it implicitly removes empty hierarchical dictionaries. For example, `{foo:{bar:42}} without foo.bar` evaluates to `{}` instead of `{foo:{}}`.

Pattern matching on dictionaries uses the literal form with an optional remaining pattern, e.g. `{ (ListExpr):(a,b,c), x:42, Pattern }`. We *evaluate* key expressions, match the referenced items, remove the matched keys, then match the remaining pattern. The default remaining pattern is `{}`, i.e. requiring a complete match.

## Embedded Texts

Proposed syntax:

        "inline text"

        """
        " multi-line texts may include "quotes"
        " each line starts with " followed by SP
        " lines are separated by LF (no final LF)
        "   (even when host file uses CR or CRLF)
        """

There are no escape characters. Texts are always 'raw', and postprocessing is left to the user. If users want to embed a binary, that might be expressed as something like:

        """
        " 74686572 65206973 206E6F20 68696464 
        " 656E206D 65737361 67652C20 6A757374
        " 20612073 696C6C79 20657861 6D706C65
        """ |> hex2bin

That said, it is awkward to maintain embedded binaries or large texts within ".g" source files. Users are encouraged to move large texts or binaries into separate files, maintain them with dedicated tools, then load them through the module system.

## Numbers

Number literals are using the same characters as names, albeit in such a way that they don't overlap names. 

        0
        1
        _42
        1.234
        1.23e_7

        1e6
        1000000
        1_000_000

We use a prefix underscore to indicate negative numbers. This is part of the number literal, not a separate unary negation operator. Internal underscores (between digits) are ignored by the parser, existing only to enhance legibility for humans. Decimal floating point or scientific notation can be encoded directly using an 'e' separator for the exponent.

        0xc0de
        0b10010_00110100_11111110_11011100

We'll support hexadecimal (0x) and binary (0b) number literals, too. We can feasibly provide some 'bitwise' operators or accelerated functions on natural numbers. Although numbers don't have a built-in notion of word size or encoding, it isn't difficult to impose one.

The compiler will provide a few useful operators - `+ * / -`. Common functions, such as rounding numbers, should be accelerated.

Numbers are modeled as exact rationals with no bound on size or precision. Thus, any loss of precision is under user control. This has severe performance implications. If users ever need high-performance assembly-time number crunching, they'll be relying on accelerated evaluation of CPU or GPGPU DSLs instead of built-in arithmetic.

*Note:* As a rule, use of characters in numbers is not case sensitive, e.g. we could write `0XC0DE` or `1.2E3`.

## Lists

I propose to use square brackets and commas for inline lists.

        []
        [1]
        [1,2,3]

Multi-line lists admit a leading comma for consistent line editing.

        [
        , 1
        , 2
        , 3
        ]

We'll use `++` to compose lists by appending them. In contrast to Haskell's `x:xs`, there is no dedicated 'cons' operator, though we can define `cons x xs = [x]++xs`. One motive for this is symmetry: lists are typically implemented as finger-tree ropes, so we can work efficiently at either end (and split or append in log-time). We may generally use `++` in pattern matching, limited to one variable-length list, e.g. `[x]++xs` or `xs++[x]` or `[x0,x1]++xs++[xn]`. 

We can introduce a few term annotations to manage representations, e.g. flattening a list into an array. We'll rely on accelerated functions on lists, too.

*Aside:* For very large lists, literals are not the best expression. They are awkward to abstract, refactor, extend, or compose. Instead, consider a writer or alternative/choice effect to generate the list. Even better, use an object spec to express component elements. 

## Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially a list with different connotations - fixed size, non-homogeneous - and distinct pattern matching. In practice, we'll usually access tuples via pattern matching, e.g. `(P1, P2, P3)` for a triple. *Note:* the tuple syntax does not support leading commas or tuples smaller than a pair. For any such cases, favor `tuple:ListExpr` instead.

Tuples are sometimes more convenient than small dictionaries, mostly due to concision. Unfortunately, compared to dictionaries, tuples are much less extensible. Consequently, tuples should be used only where they represent stable types or local intermediate representations. This is mitigated by ad hoc polymorphism: it is feasible to continue supporting `(X,Y,Z)` during a transition to `{x:X, y:Y, z:Z, ...}`.

## Relational Style? Use Macros.

Dictionaries are not tables, i.e. because users cannot iterate them. But it may prove convenient to support tables and relations explicitly. A table could be expressed as a list of tuples or dicts with invariant fields. With extensibility and stateful specification patterns, we can easily build tables as we express code, e.g. `tbl_foo.data ::= \p -> p++[...]`. 

At this time, there is no dedicated syntax for building tables. We should explore this space with macros then decide whether macros are adequate. But I suspect good syntax for tables will be convenient integrating definitions from multiple sources, e.g. a web service from individual pages.

## Functions

I propose to adopt Haskell's use of `\` for lambdas.

        \ x y z -> Expr
        \ x -> \ y -> \ z -> Expr

We'll also support Haskell-style `name args = ...` as a syntactic sugar. 

        name = \ x y z -> Expr
        name x y z = Expr
        name x y z := Expr
        name x y z ::= \ prior -> Expr

In the `::=` case, we'll insist `\ prior -> ...` remains the first argument on the right-hand side. 

Unlike Haskell, there is no support for pattern matching in definitions or lambdas. 

## Partial Functions

        error      recognized errors
        TBD        incomplete definitions

These expressions always diverge when observed, but for different reasons - error or an incomplete definition. The reflection API may observe these errors together with full context (continuation, call stack, etc.). In practice, we'll often write `(error Expr)` (or `(TBD Expr)`) to provide extra context, treating `error` as a missing function. But the argument isn't required.

## Pipes

Borrowing F#'s syntax here:

        f <| arg = f arg
        arg |> f = f arg

I propose to also support directional function composition:

        f >> g = \ h -> g (f h)     
        g << f = f >> g

## Booleans

I propose to encode Boolean values, true and false, as the concise atoms, `'t` and `'f` respectively. There are no implicit conversions, no 'truthy' values. 

I propose to use keywords `and, or, not` for boolean composition. (IMO, `&&` and `||` begin to feel like line noise, and `!` should not be wasted on flipping a bit.) We can treat `and` and `or` as infix operators.

Although I have a vision for pattern matching, we'll support the conventional and familiar `if Cond then A else B` expressions, and also the `A if Cond else B` variant, which is more convenient in cases where we want to emphasize the operation over the condition. These are limited to pure boolean expressions. As a special case, in context of `do` notation, users may write `if Cond then A` or `A if Cond` without an `else` condition (defaults to return unit). 

## Type Annotations (Defer)

For now, we won't have any built-in syntax for types, just leave this to macros and reflection. But the general idea is to use `module.[type:'foo]` to define the type of `foo`. Type systems and type checkers will ultimately be user-defined reflection tasks.

## Modules

Need a syntax for `import`, `include`, `load`

### Access Control

There is no notion of export control. Such a concept contradicts extensibility goals and conflicts with modules-as-mixins. However, we can easily invert this to distinguish public interfaces. We already use `conf.*` for the configuration, `asm.*` for the assembly, `env.*` for a threaded environment. We can similarly use `api.*` for a library's public interface. If this were an application language, perhaps `app.*` for application methods. 

        import "MyFooLib.g" as libfoo
        env.foo = libfoo.api
        alias libfoo.api: foo, bar, baz as qux
        foo ::= \ old_foo -> ...
        ...

There are forms of access control between subprograms. This is supported via unique atoms.

## Pattern Matching

## Object Specs

We'll provide some convenient syntax for specifying objects. Unfortunately, it's a little awkward to override the specification as a whole. I propose instead that the syntax for specifications implicitly introduces `module.[spec:'foo]` and `foo` as the instance, implicitly introducing `foo.spec = module.[spec:'foo]` as a final mixin. We'll implicitly finalize `foo` to encourage updating the spec instead. 

It's convenient to model this as a multi-level syntactic sugar: spec expands to spec definitions (mixin, inheritance list, etc.) and instantiation, instantation expands to linearization and finalization, etc..


To support 'override' of specifications, it might be useful to break them into several toplevel definitions. E.g. a unique declaration for the identity, a mixin definition, and the instance definition. Each with a distinct suffix. We could also bind a refl task to each instance, lifting the internal refl task. Users then mostly interact with the instance, but override the mixin. 

We can model naked mixins as `\base self -> base .with ...`. But for multiple inheritance and singleton instantiation, we'll need implicit unique identifiers from the module toplevel. Ideally, we can also eliminate the aforementioned mixin boilerplate, at least by default. Or perhaps we can separate the specification and instantiation from the mixin.


Some challenges:
- unique identifiers for specs for multiple inheritance
- introducing vs. overriding specs
- access to base and self, default names? explicit parameters?
- overriding methods
- semantics: is Spec interface introduced as final mixin or via Base? Leaning towards implicit final mixin before instantiation.

## Annotations

We can broadly have two kinds of annotations: 

- Namespace annotations by associative naming conventions such as `.[type:'foo]` or `.[final:'foo]` are open to extension, but limited to annotation of names. These would mostly be implemented via reflection tasks. The front-end compiler could install an associated reflection task for each feature.
- Term annotations by keyword, e.g. `anno AnnoExpr TermExpr`, where `AnnoExpr` is recognized by the assembler. Unrecognized or unsupported annotations shall result in warnings. Term annotations shall logically return the same term, but may modify representations for performance or debugging. Extension is troublesome, but users can mitigate.

I hesitate to support term annotations at all, due to the extensibility concern. But it's more convenient for many use cases, and the extensibility issue isn't too bad if we carefully control the scope of term annotations. Examples of term annotations:

- accelerated functions, JIT hints, cache hints
- laziness control (e.g. Haskell-style seq and par)
- scope_unique for ephemeral atoms
- logging and tracing for visualization
- profiling? unsure, perhaps better as a reflection task
- assertions, keep conditional checks optional

It seems convenient to support `anno refl:Effect` to run anonymous reflection tasks during another computation. The reflection effect could receive access to the continuation. This would enable user-defined visualizations and breakpoints.

*Note:* I originally was planning keywords for most annotations, but on review I believe just one keyword is better for clarity and extensibility. Users are encouraged to define wrapper functions for annotations.


## Pattern Matching

View patterns permit more than one match, however.

I want to desugar all pattern matching to monadic expressions, and I also want to support transactional backtracking conditionals by default. Support for 'what-if' pattern matching is simply very convenient.


Although we could support Haskell-style `match Expr with (Pattern -> Outcome)+` syntax, providing the pure handler, it's a little awkward to extend this syntax for effectful patterns, and it may be better to integrate the 'Expr' into the Pattern, allowing for more than one (e.g. as guards). I'm contemplating alternative syntax, e.g. based on unification or `Pattern = Expr` structures. We could feasibly integrate pattern matching into monads in general.

## User-Defined Types?

I would like to support lightweight declarations of type constructors and matching patterns.

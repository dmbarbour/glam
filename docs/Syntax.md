# Base Syntax

This document describes an initial syntax for ".g" files, and motives for it. Design goals include:

- a syntax that I find pleasant to work with
- supports an assembly programming look and feel
- concise, vertical columns of assembly mnemonics
- generalist, not specialized for targets or domains
- an extraordinarily high abstraction ceiling

## Language Version Declaration

Reproducibility requires that the same sources produce the same outcome, but there is an implicit condition: an outcome is produced. This condition provides opportunity to update the language: we can introduce new operators without affecting existing code, or introduce new keywords accepting that we break existing code. User-defined syntax mitigates broken-code concerns: we can write a wrapper that injects an older version of `env.lang.["g"].compile`.

A language version declaration makes versioning of the language more flexible and robust. We can fail fast at the declaration if we don't support the requested version. We can adjust 'meaning' of a keyword between versions. It's also a clear opportunity to declare compiler-recognized language extensions - fine-grained compared to full versioning, but also more verbose.

Proposed syntax:

        (lang|language) (BaseVer) (with Extensions)?
        lang g0 with utf8

The version declaration should be the first toplevel declaration in a ".g" file.

## Character Set

We'll start with printable ASCII and whitespace (0x21-0x7E, SP, CR, LF). It is not difficult to extend to UTF-8, though I'm concerned about legibility. We'll recognize CR, LF, and CRLF as line endings. The compiler shall emit a warning if the file uses inconsistent line endings.

## Comments

We'll support Python-style line comments, i.e. `#...` to end of line. Comments are treated as whitespace by the compiler and macros, but may be structured in context of external tooling (literate programming, projectional editing, extracting API docs, etc.). There is no syntax-layer support for multi-line comments, so just use a decent editor.

## Code Structure

The module toplevel consists of a sequence of 'declarations'. Each declaration starts a new line. If a declaration requires more than one line, any continuing lines (excepting blanks) must be indented by at least one space. This structure simplifies error isolation, local reasoning, and parallel processing.

Each declaration starts with either a keyword (such as `import`, `spec`, or `unique`) or is a basic definition of form `name = Expr` or one of its variants (args in lhs, `:=`, `::=`, etc.). We'll favor basic definitions where feasible, thus keywords are mostly for special forms.

Toplevel macro invocations, `@macro_name ...`, are scoped as declarations. The macro can *and must* read to the start of the next declaration (modulo whitespace). The toplevel macro may emit many toplevel declarations. Effectively, toplevel macros serve as user-defined keyword forms. 

Aside from toplevel declarations, several special forms for `Expr` are also sensitive to whitespace and indentation by default. But, in these cases, users always have the option to use braces and semicolons instead of indentation and newlines.

## Keywords

Keywords are names reserved by the compiler. Users may not define or shadow keywords, nor directly use them in dotted paths or tags. Users may construct atoms from keywords, e.g. `'if` and `'else`. 

Recognized keywords may vary with language version declaration. By reserving candidate keywords we can avoid breaking code. I propose to reserve all keywords I introduce in this document, all English conjunctions and prepositions, and a handful of additional names. Will update this section later.

## Names

We accept a subset of C names, mostly restricting use of underscores. A viable regex:

        NameFrag = [a-zA-Z][a-zA-Z0-9]*
        Name = NameFrag('_'NameFrag)*

Namespaces are hierarchical, modeled as dictionaries that may contain dictionaries. Hierarchical access or update is typically expressed as a dotted path, e.g. `foo.bar.baz`. When used as an index into a namespace, we translate each name to the corresponding atom `.['foo, 'bar, 'baz]`.

In the general case, we support expression-indexed dotted-path names. This is expressed as `.(ListExpr)` or `.[...]` for a literal list. These indices are interpreted such that `.([1, 'two] ++ [3])` is equivalent to `.[1].two.[3]`. The empty path is permitted, e.g. `foo.[]` is equivalent to `foo`. 

I propose `module` as a keyword referring to the current module's namespace. Thus, users can write `module.[42]` to access non-conventional names. (*Note:* The `.[42] = ...` form is accepted on the lhs, but use `module.[42]` for evaluation.)

## Operators

Operators are compiler-defined binary infix functions. We'll support Haskell-style operator sections, such that `((>>= k) op)` is equivalent to `(op >>= k)`. 

To avoid unnecessary parentheses, we'll support precedence between most operators. To mitigate confusion, not every pair of operators will have valid precedence, e.g. cannot mix both `>>` and `<<` without parentheses. 

There are no implicit conversions, e.g. in case of `2 + "3"` the result is an assembly-time type error (instead of `5` or `"23"`). But some ad hoc polymorphism may apply, e.g. `==` works for all dict key types, and `>` might support comparing numbers or comparing lists of comparables, but it's a type error to compare lists to numbers.

## Introductions and Overrides

We'll syntactically distinguish introductions vs. overrides. It's an error to introduce a name that already has a definition, or to override a name that does not have a prior definition. We use `name = Expr` for introductions, `name := Expr` for overrides, and `name ::= \ prior -> Expr` for overrides with convenient access to the previous definition.

By default, module-scope names evaluate to final definitions, subject to overrides. To support access to previous definitions (outside scope of `::=`) I propose keywords `module_prior Expr` and `module_final Expr`. These evaluate names in `Expr` in the specified scope. The `module_prior` scope is definitions up to the current toplevel declaration. Use `module_prior module` to capture the entire previous namespace.

### Forbid Name Shadowing

Name shadowing occurs when a name masks access to another name in scope. This usually happens with generic local names like `\map -> ...` where 'map' may mean many different things (e.g. to apply a function over a list, an associative data structure, a game world map). Unfortunately, this easily results in bugs that humans easily miss when reading code: contextual usage is obvious to humans, but the compiler's interpretation of context is not.

In context of open recursion for inheritance and override, name shadowing would be even more problematic. Masking names hinders extension, and it becomes confusing what the final definition for any given use of a name refers to. 

Thus, as a rule, we'll forbid name shadowing. But only for static contexts: we forbid shadowing of names from an included module, but not for shadowing of names introduced by *including* a module. This may involve threading metadata through includes via the namespace. It isn't difficult to avoid name conflicts.

*Aside:* Macros should receive compiler support to generate local vars without risk of shadowing.

### Abstract Definitions

In context of modules as mixins, we may assume a name is introduced without defining it locally. However, to resist errors, it is convenient to report 'undefined' errors closer to the code that leaves names undefined. To support these cases, I propose a toplevel declaration:

        abstract Name(, Name)*

This declaration is not required for names brought into scope (or declared abstract) via 'include'. The intuition we want for include is that we're including all relevant definitions and declarations, including `abstract`. Hierarchical names are captured implicitly, i.e. if we `abstract foo` we don't need `abstract foo.bar`. For convenience, `abstract env` is implicit.

The tracking of abstract definitions across includes is essentially just maintaining a set of dotted-path prefixes that we *don't* report 'undefined' errors about. 

### Aliasing (Tentative, Low Priority)

To work more conveniently with hierarchical definitions in context of inheritance and overrides, we can support logical aliasing by rewriting names before they're used. We can also define aliases to resist shadowing and ambiguity.

        # form similar to
        alias foo: bar.baz, qux as q

        # implicitly defines
        baz = foo.bar.baz
        q = foo.qux
        .[AliasRules] ::= \ prior -> 
            ... rewrite q to foo.qux, rewrite baz to foo.bar.baz

        # later, based on AliasRules
        baz := ...
        # rewrites to
        foo.bar.baz := ...

A concise syntax for bulk aliasing from hierarchical namespaces is very convenient for 'cherry picking'. We'll preserve alias rules across includes. We'll need careful attention to how alias rules compose hierarchically, but it should be feasible.

## Effects

We'll almost directly adopt Haskell's do notation, `do ...`. 

Instead of desugaring to monadic operators, we'll desugar to a tagged function of form `eff:(\__api -> Body)`, where `__api` is guaranteed to not shadow anything. The `eff` tag helps distinguish effects from pure functions and doubles as a calling convention. Within the 'do' context, we'll desugar `%name` to `__api.name`. This generalizes, e.g. `%[Expr].op` to `__api.[Expr].op`, or `%[]` to capture the effects API. The compiler will essentially use `%Seq` to compose within Body.

*Note:* Alternatively, `.name` to `__api.name`. May need to experiment.

We'll define several operators in these terms, e.g. `op >>= k = eff:(\api -> api.Seq op k)` and `>>>` if op returns unit. For convenience, I propose `.ret` as a built-in for `\x -> eff:(\api -> api.Return x)`. Other operators and keywords may support applicative styles, e.g. `<*>` and `<|>`, `.fail` and `.cut`, etc.. 

I intend to diverge from Haskell regarding RecursiveDo, requiring explicit forward declaration of locals whose values are determined later. This improves visibility and mitigates issues like the conflict between fixpoint and continuations.

## Macros

In context of lazy loading, macro invocations must be distinct from normal evaluation. Proposed syntactic forms:

        @(Expr)
        @macro_name         short for @(.module_prior macro_name)

The compiler lazily evaluates and interprets `Expr` at compile-time. If this evaluates to a function, the compiler parses an argument `Expr`, applies the function, then repeats. Thus, macros may be parameterized as normal functions of any arity. After all arguments are read, the macro should evaluate to a `eff:(\api -> Body)` effect. The compiler provides a localized effects API then runs the effect. 

The effects API provides parser combinators to read code, supporting macro DSLs, and emitters to write code. Reads and writes both have flexible levels of abstraction, e.g. we can work with raw text, ASTs, abstract expressions, etc.. To isolate errors and simplify local reasoning, macros cannot escape their scope, and balance of brackets, braces, parentheses, etc. are preserved by both parsers and emitters. Without looking at its definition, we know `(@foo ...)` will read and write within those parentheses. Also, macros may also only read from their right-hand side.

A relevant concern is how macros interact, e.g. in context of `(@foo @bar ...)`. To keep it simple, I propose transactional semantics with a predictable schedule: each macro evaluates to completion in one step, and we always favor the earliest (i.e. leftmost, topmost) macro. This design still admits sophisticated interactions insofar as macros emit more macro invocations, but it ensures syntactic locality of such interactions.

Aside from reading and writing code, macros may provide access to other compiler-provided effects, e.g. access to built-in functions or writing messages to a log.

## Tagged Data

        tag:Data

Tagged data is modeled as a singleton dictionary. But the compiler implicitly annotates tagged data raise an error upon update (via `.with`). Thus, `{ tag:Data }` is distinct from `tag:Data` regarding opportunity for future updates. The tag generalizes to dotted-path names. The primary use case is `.[TagExpr]:Data` for a computed tag with a single-level index.

Pattern matching, in the general case of `.(TagList):Pattern`, would evaluate `TagList`, extract the indexed element while verifying that a singleton dictionary at each level, then match the given pattern. 

## Atoms

Atoms are data where the only useful observation is equality.

Constructed atoms are useful for structured data. The unit value is a built-in atom, expressed and matched as `()`. Tagged unit data, i.e. `tag:()` or `.[TagExpr]:()`, serves as a constructed atom. Names are indexed as constructed atoms of form `.["name"]:()`. We provide syntax `'name` for capturing name indices: `tag:()` is equivalent to `.['tag]:()`. We use this for booleans (`'t` and `'f`), and they're convenient for naming registers and such (`'eax`).

Aside from unit, the assembler provides to the front-end compiler an atom abstracting location in the hierarchical module namespace. This is useful as a seed to derive 'unique' atoms. I propose `.unique Foo, Bar, Baz` declarations to introduce unique atoms at the module toplevel. Unique atoms are useful for access control and conflict avoidance, but they must be used via indexed names, e.g. `.[Foo]:(...)`.

Scope-unique atoms are useful for the ephemeron performance pattern. To support this, `.scope_unique : Atom -> Atom` returns the same atom annotated with unique metadata. When matching or comparing atoms with different metadata, we diverge with error. Thus, we never observe scope-uniqueness to be violated. When used as dict keys, we can collect associated data when metadata becomes unreachable.

*Note:* It is feasible to introduce unique atoms (aligned with global namespace paths) without declaring them as names, but declaring them as names

## Dicts

For simple, literal dictionaries, I propose syntactic form `{ name1:Expr1, name2:Expr2, ... }`. This desugars to `{} .with { name1 = Expr1, name2 = Expr2, ... }`, where `{}` is the empty dictionary and `=` represents namespace introduction. It also generalizes to dotted-path and expression-indexed names, e.g. `{ .[1]:"hello", foo.[2]:"world" }`.

Dictionary updates are generally expressed using `.with` and `.without` special forms. These are applied much like infix operators, but the RHS isn't an expression:

        Dict .with 
            x ::= \ old_x -> old_x + 42
            y := 10
            .[1] = "this is new"

        Dict .without x, y, z

The `.with` syntax enforces explicit overrides, i.e. it's an error to introduce a name that already exists or override a name that does not exist. The `.without` form removes listed names if they exist, but is not an error if the name does not exist. In case of dotted-path names, it also removes empty hierarchical dictionaries in the removed path prefix. Thus, in case of `{foo:{bar:42}} .without foo.bar` the result is `{}` instead of empty directory `{foo:{}}`. 

Pattern matching on dictionaries uses the literal form with an optional remaining pattern, e.g. `{ .(Expr):(a,b,c), x:42, Pattern }`. We *evaluate* key expressions within the pattern, and we remove matched keys (via `.without`) before matching on the remaining pattern. The default remaining pattern is `{}`, requiring a complete match.

In the general case, users may want conditional behavior based on whether a dictionary contains a given field. This can be expressed in terms of pattern matching.

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

That said, it is awkward to maintain embedded binaries or large texts. Users are encouraged to move large texts or binaries into separate files then load them through the module system.

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

We use a prefix underscore to indicate negative numbers. This is part of the number literal, not a separate unary negation operator. Internal underscores (i.e. between digits) are ignored by the parser but may enhance legibility for humans. Decimal floating point or scientific notation can be encoded directly using an 'e' separator (or 'E', not case sensitive) for the exponent.

        0xc0de
        0b10010_00110100_11111110_11011100

We'll support hexadecimal (0x) and binary (0b) number literals, too (not case sensitive). These may be negative (e.g. `_0xff` is `_255`) though conventionally we'd only use this for natural numbers. 

We'll provide some arithmetic operators for numbers, e.g. `+ * / -`. Divide by zero will diverge lazily. We'll also support some comparisons, e.g. `> >= == =< <`. We might provide a few built-ins or accelerators for other common use cases.

Numbers are modeled as exact rationals with no bound on size or precision. Any loss of precision is under user control. This has severe performance implications, but they won't impact most assembly use cases. Where assembly-time number crunching performance is an issue, we'll develop accelerators. 

## Lists

I propose to use square brackets and commas for literal lists.

        []
        [1]
        [1,2,3]

We'll use `++` to compose lists by appending them. There is no dedicated 'cons' operator in syntax, but we can express `cons x xs = [x]++xs`. We may generally use `++` in pattern matching, limited to one variable-length list, e.g. `[x]+xs` or `xs+[x]` or `[x0,x1]+xs+[xn]`.

Currently, there is no syntax for list length, slicing lists, etc.. We'll need accelerated functions in those roles.

## Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially a list with different connotations - fixed size, non-homogeneous - and distinct pattern matching. In practice, we'll usually access tuples via pattern matching, e.g. `(P1, P2, P3)` for a triple. We can feasibly accelerate short tuples to reduce the number of allocations. There is no dedicated syntax for tuples smaller than a pair, though users are free to manually write `tuple:[a]`. 

Tuples are sometimes more convenient than small dictionaries. A relevant cost is extensibility. Tuples should mostly be used for either very stable structures or local intermediate representations.

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

        .error      recognized errors
        .tbd        incomplete definitions

These expressions always diverge when observed, but for different reasons - error or an incomplete definition. The reflection API may observe these errors together with full context (continuation, call stack, etc.). In practice, we'll often write `(.error Expr)` (or `(.tbd Expr)`) to provide extra context for visualization, treating `.error` as a function. 

## Pipes

Borrowing F#'s syntax here:

        f <| arg = f arg
        arg |> f = f arg

I propose to also support directional function composition:

        f >> g = \ h -> g (f h)     
        g << f = f >> g

## Booleans

I propose to encode boolean values true and false as concise atoms, `'t` and `'f` respectively. There are no implicit conversions, no 'truthy' values. We can support the conventional if/then/else (and elif) form, and perhaps an `Expr .when Cond .else Expr2` form. In context of pattern matching, we'll use boolean expressions for pattern guards.

## Modules

Need a syntax for `.import`, `.include` 

### Access Control

There is no notion of module-private names or export control. Such a concept contradicts extensibility goals and conflicts with modules-as-mixins. However, we can easily invert this to distinguish public interfaces. We already use `conf.*` for the configuration, `asm.*` for the assembly, `env.*` for a threaded environment. We can similarly use `api.*` for a library's public interface. 

        import "MyFooLib.g" as libfoo
        env.foo = libfoo.api
        alias libfoo.api: foo, bar, baz as qux
        foo ::= \ old_foo -> ...
        ...

There are forms of access control between subprograms. This is supported via unique atoms.

## Conditional Code (Rejected)

I've contemplated support for toplevel `.ifdef` and such, but I feel that conditional definitions complicate reasoning about the namespace even before I contemplate whether we're referring to past or future definitions. Best to stick with aggregate definitions via `name ::= \ p -> p ++ [...]` or similar.

## Object Specs

We can model naked mixins as `\base self -> base .with ...`. But for multiple inheritance and singleton instantiation, we'll need implicit unique identifiers from the module toplevel. Ideally, we can also eliminate the aforementioned mixin boilerplate, at least by default. Or perhaps we can separate the specification and instantiation from the mixin.


Some challenges:
- unique identifiers for specs for multiple inheritance
- introducing vs. overriding specs
- access to base and self, default names? explicit parameters?
- overriding methods
- semantics: is Spec interface introduced as final mixin or via Base? Leaning towards implicit final mixin before instantiation.

## Annotations

acceleration, sparks, sequencing, robust tail-call optimizations, logging, assertions, tracing

A potential issue with logging and assertions is that we cannot 'repair' an anonymous assertion or log message. Perhaps we should 

 to work with anonymous locations. But this can be mitigated by implicit tracing, since we can only observe these 






## Comments

- potential `.nb Expr` ? This would hinder override for 'repair' though. Perhaps better keep active comments to defined things.
- line comments (//, #, -- ?)
- disabling sections of code (`.DISABLE_START` and `.DISABLE_END` perhaps? or just leave to IDE).
- potential logging

## Laziness and Sparks

## Accelerators

## Language Version Declaration (Tentative)

A language version declaration enables a compiler to adapt to programs written in older versions of a language, or to detect early whether a program uses a more advanced version of the language than the compiler recognizes. But it seems much less necessary with keywords separated from user definitions.

## Pattern Matching

View patterns permit more than one match, however.

I want to desugar all pattern matching to monadic expressions, and I also want to support transactional backtracking conditionals by default. Support for 'what-if' pattern matching is simply very convenient.


Although we could support Haskell-style `match Expr with (Pattern -> Outcome)+` syntax, providing the pure handler, it's a little awkward to extend this syntax for effectful patterns, and it may be better to integrate the 'Expr' into the Pattern, allowing for more than one (e.g. as guards). I'm contemplating alternative syntax, e.g. based on unification or `Pattern = Expr` structures. We could feasibly integrate pattern matching into monads in general.

## User-Defined Types?

I would like to support lightweight declarations of type constructors and matching patterns.

## Data Embeddings

Some design constraints and desiderata:

- specialized monad for writing lists, multi-line texts? Tentative. 
- vertical structure, avoids 'deep' indentation
- user-defined types and object interfaces
  - possible type-indexed behavior bound to named types?


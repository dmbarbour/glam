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

## Toplevel Structure

The module toplevel consists of a sequence of 'declarations'. Each declaration starts a new line. If a declaration requires more than one line, any continuing lines (excepting blanks) must be indented by at least one space. This structure simplifies error isolation, local reasoning, and parallel processing.

Each declaration starts with either a keyword (such as `import`, `spec`, or `unique`) or is a basic definition of form `name = Expr` or one of its variants (args in lhs, `:=`, `::=`, etc.). We'll favor basic definitions where feasible, thus keywords are mostly for special forms.

In context of errors, the errors can be reported but we can also make a best effort to proceed with errors. This might depend on configuration options or command-line arguments.

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

Operators are essentially infix functions. We'll support Haskell-style operator sections, such that `((>>= k) op)` is equivalent to `(op >>= k)`. To avoid unnecessary parentheses, we'll support precedence between most operators. To mitigate confusion, not every pair of operators will have valid precedence, e.g. cannot mix both `>>` and `<<` without parentheses.

We may support a few special non-binary forms, e.g. `(x < y =< z)` as shorthand for `((x < y) and (y =< z))`. We'd also support `(< =<)` operator sections. Risk of confusion is mitigated because we cannot compare booleans for less-than or greater-than.

Operators may support limited ad-hoc polymorphism. For example, `>` could compare two numbers, two lists, two tuples. For lists and tuples, we use a lexicographic comparison of elements. Comparing a number to a list, or a list to a tuple, would simply diverge with an error. As a rule, ad-hoc polymorphism must preserve laws or intuitions, e.g. don't use `+` to append lists because it does not preserve commutativity of `+` on numbers.

## Introductions and Overrides

We'll syntactically distinguish introductions vs. overrides. It's an error to introduce a name that already has a definition, or to override a name that does not have a prior definition. We use `name = Expr` for introductions, `name := Expr` for overrides, and `name ::= \ prior -> Expr` for overrides with convenient access to the previous definition.

By default, module-scope names evaluate to final definitions, subject to overrides. To support access to previous definitions (outside scope of `::=`) I propose keywords `ns_prior Expr` and `ns_final Expr`. These evaluate names in `Expr` in the specified scope. The `ns_prior` scope is definitions up through the toplevel declaration. Use `ns_prior module` to capture the entire previous namespace.

### Forbid Name Shadowing

Name shadowing occurs when a name masks access to another name in scope. This usually happens with generic local names like `\map -> ...` where 'map' may mean many different things (e.g. to apply a function over a list, an associative data structure, a game world map). Unfortunately, this easily results in bugs that humans easily miss when reading code: contextual usage is obvious to humans, but the compiler's interpretation of context is not.

In context of open recursion for inheritance and override, name shadowing would be even more problematic. Masking names hinders extension, and it becomes confusing what the final definition for any given use of a name refers to. 

Thus, as a rule, we'll forbid name shadowing. But only for static contexts: we forbid shadowing of names from an included module, but not for shadowing of names introduced by *including* a module. This may involve threading metadata through includes via the namespace. It isn't difficult to avoid name conflicts.

*Aside:* Macros should receive compiler support to generate local vars without risk of shadowing.

### Abstract Definitions

In context of modules as mixins, we may assume some names are introduced by the module's client. However, it is convenient to report 'undefined' errors closer to the code that leaves names undefined. To support these cases, I propose a toplevel declaration:

        abstract Name(, Name)*

This declaration is not required for names brought into scope (or already declared abstract) via 'include'. The intuition we want for include is that integrates all toplevel declarations (modulo language version declaration). Also, hierarchical names are captured implicitly, i.e. `abstract foo` implies `abstract foo.bar`. As the standard case, `abstract env` is implicit.

The tracking of abstract definitions across is essentially just maintaining an extra set of prefixes for which we'll suppress 'undefined' errors. Should be easy to implement with just a little metadata hidden in the namespace.

### Aliasing (Tentative, Low Priority)

To work more conveniently with hierarchical definitions in context of inheritance and overrides, we can support logical aliasing by rewriting names before they're used. We can also define aliases to resist shadowing and ambiguity.

        # form similar to
        alias foo: bar.baz, qux as q,

        # implicitly defines
        baz = foo.bar.baz
        q = foo.qux
        CompilerInternals.AliasRules ::= \ prior -> 
            ... rewrite q to foo.qux, baz to foo.bar.baz

        # later, based on AliasRules
        baz := ...
        # rewrites to
        foo.bar.baz := ...

A concise syntax for bulk aliasing from hierarchical namespaces is very convenient for 'cherry picking'. We'll preserve alias rules across includes. We'll need careful attention to how alias rules compose hierarchically, but it should be feasible.

## Effects

We'll almost directly adopt Haskell's do notation. 

I propose a variation for RecursiveDo: forward declaration of fixpoint names. In part for visibility and clarity, in part because fixpoint scopes shift. To express forward declarations, we could use a keyword such as `expect x, y, z`.

To support concise effects without polluting the namespace, I also propose to evaluate `.name Args` to `eff:(\__api -> __api.name Args)`. To make this work nicely with curried arguments, we can extend the implicit whitespace 'apply' to also support effects. This isn't difficult, just needs a little ad-hoc polymorphism: `eff:(f) x = eff:(\api -> f api x)`.

Haskell has applicative style via `<*>` and `<$>` that is convenient for some use cases. I'm contemplating similar operators, but it isn't clear to me whether I can make them more concise. Will probably defer these for now, though.

*Aside:* I've been contemplating concise access to indexed state. Use of `.get ['foo]` and `.set ['foo] 42` is probably adequate. 

*Tentative:* I like the idea of explicitly modeling conventional "stack frames" with early returns (via shift-reset), frame-local variables (via state), deferred operations (invoked in reverse order on frame exit). We could feasibly provide some syntax around this. We'd also need explicit tailcalls that pop the frame for recursion. Perhaps explore what can be done here then develop some syntax extensions. Maybe a `proc` keyword?

## Macros

In context of lazy loading, macro invocations must be distinct from normal evaluation. Proposed syntactic forms:

        @(Expr)
        @macro_name         short for @(ns_prior macro_name)

The compiler lazily evaluates `Expr` at compile-time to an effect or function. A function is interpreted as an effect that reads one expression then applies it as an argument. An effect is more flexible, capable of reading and emitting code through a compiler-provided API. Reads and writes shall support flexible levels of abstraction, e.g. source text, ASTs, abstract data embeddings, etc..

To isolate errors and simplify local reasoning, reads and emits will preserve balance of brackets, braces, parentheses, indentation, etc.. Without even looking at its definition, we know `(@foo ...)` cannot read or write outside those parentheses. A toplevel macro is scoped to one declaration (based on indentation), but may emit many declarations. 

A relevant concern is how macros interact, e.g. in context of `(@foo @bar ...)`. For user comprehension, the optimal answer is that macros don't interact. Thus, each macro can be considered in isolation, and macros never observe another macro invocation. This can be implemented via restriction on the reader API. Similar restrictions exist for comments and imbalanced parentheses.

Macros may also receive compiler-provided APIs to report errors and warnings. 

## Tagged Data

        tag:Data

Tagged data is modeled as a singleton dictionary. But the compiler implicitly annotates tagged data raise an error upon update (via `.with`). Thus, `{ tag:Data }` is distinct from `tag:Data` regarding opportunity for future updates. The tag generalizes to dotted-path names. The primary use case is `.[TagExpr]:Data` for a computed tag with a single-level index.

Pattern matching, in the general case of `.(TagList):Pattern`, would evaluate `TagList`, extract the indexed element while verifying that a singleton dictionary at each level, then match the given pattern. 

## Atoms

Atoms are data where the only useful observation is equality.

Symbolic atoms are useful for structured data. The unit value is a built-in symbolic atom, expressed and matched as `()`. Tagged unit data, i.e. `tag:()` or `.[TagExpr]:()`, serves as a symbolic atom. As shorthand, `'name` rewrites to `.["name"]:()`. Note that `'tag` and `tag:()` are not equivalent: the latter is equivalent to `.['tag]:()`. We'll use symbolic atoms for booleans (`'t` and `'f`), and they're also convenient for naming registers (`'eax`).

Unique atoms are useful for access control and conflict avoidance. A viable approach is `Foo = (abstract_global_path Foo)`. This leverages the global namespace as a stable source of identity (that does not compromise extensibility or reproducibility), and also defines `Foo` to resist accidental reuse. I propose a toplevel declaration `unique Foo, Bar, Baz` for bulk definitions of this form. *Aside:* `abstract_global_path` is verbose to discourage direct use.

Scope-unique atoms are useful for the ephemeron performance pattern. To support this pattern, `scope_unique : Atom -> Atom` returns the same atom annotated with unique metadata. If ever we compare the same atom with different metadata, we diverge instead, thus we never observe the violation of scope uniqueness. When used as dict keys, we can associate the data with a weakref of the metadata.

## Dicts

For simple, literal dictionaries, I propose syntactic form `{ name1:Expr1, name2:Expr2, ... }`. This desugars to `{} with { name1 = Expr1; name2 = Expr2; ... }`, where `{}` is the empty dictionary and `=` represents namespace introduction. It also generalizes to dotted-path and expression-indexed names, e.g. `{ .[1]:"hello", foo.[2]:"world" }`. For multi-line dicts, a leading comma is permitted (like lists) but we'd usually favor the `{} with ...` form.

Dictionary updates are generally expressed using `with` and `without` special forms. These are applied much like infix operators, but the RHS isn't an expression:

        Dict with 
            x ::= \ old_x -> old_x + 42
            y := 10
            .[1] = "this is new"

        Dict without x, y, z

The `with` syntax still distinguishes introductions and overrides. The `without` form removes listed names if they exist, but it is not an error to remove the name if it does not exist. In case of dotted-path names, it also removes empty hierarchical dictionaries in the removed path prefix. Thus, in case of `{foo:{bar:42}} without foo.bar` the result is `{}` instead of empty directory `{foo:{}}`. 

Pattern matching on dictionaries uses the literal form with an optional remaining pattern, e.g. `{ .(Expr):(a,b,c), x:42, Pattern }`. We *evaluate* key expressions within the pattern, and we remove matched keys (via `without`) before matching on the remaining pattern. The default remaining pattern is `{}`, requiring a complete match.

In a few rare cases, notably tagged data and object instances, I want to prevent direct updates to a dictionary. For tagged data, users should simply construct a new value. For objects, users should update the specification (pre-fixpoint) instead of the instance, otherwise updates won't propagate properly. Mostly, this is about protecting user intuitions against surprises or accidents. I propose a `(dict_freeze DictExpr)` annotation that returns a 'frozen' dictionary, prohibiting updates via `with`, `without`. We can also introduce `(dict_thaw FrozenDictExpr)` to relax this constraint. 

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

Multi-line lists admit a leading comma, i.e. `[ LF, ...]` for more consistent line editing.

        [
        , 1
        , 2
        , 3
        ]

Though, do consider a writer effect to build large lists instead of a literal in these cases.

We'll use `++` to compose lists by appending them. In contrast to Haskell's `x:xs`, there is no dedicated 'cons' operator, though we can define `cons x xs = [x]++xs`. One reason for this is symmetry: 

We may generally use `++` in pattern matching, limited to one variable-length list, e.g. `[x]++xs` or `xs++[x]` or `[x0,x1]++xs++[xn]`. 

There are no list comprehensions. We might change that with macros. Use of Alt+Fail effects to build lists is just as expressive, but a tad more verbose.

Aside from append and pattern matching, there are no primitives on lists. I propose to build everything else via user definitions and acceleration.

## Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially a list with different connotations - fixed size, non-homogeneous - and distinct pattern matching. In practice, we'll usually access tuples via pattern matching, e.g. `(P1, P2, P3)` for a triple. We can feasibly accelerate short tuples to reduce the number of allocations. There is no dedicated syntax for tuples smaller than a pair, though users are free to manually write `tuple:[a]`. 

Tuples are sometimes more convenient than small dictionaries. But compared to dictionaries, tuples are much less extensible. Tuples should mostly be used for either very stable structures or local intermediate representations.

## Relational Style? Use Macros.

It is feasible to use extensions to build tables, e.g. `tbl_foo ::= \p -> p++[...]`. I'd like to support relational style, but I'm not convinced it's worth a dedicated syntax at this time. Let's try out some macro DSLs in this role.

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

## Modules

Need syntax for `import`, `include` 

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

To support 'override' of specifications, it might be useful to break them into several toplevel definitions. E.g. a unique declaration for the identity, a mixin definition, and the instance definition. Each with a distinct suffix. We could also bind a refl task to each instance, lifting the internal refl task. Users then mostly interact with the instance, but override the mixin. 

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

## Logging (Tentative)

I'm not convinced we should support logging directly from functions. The main issue is that we lose a lot of ability to override logging behavior. This could be mitigated by some standard per-module logging overrides, e.g. a filter-map for log messages based on call stacks. 

But this gets pretty messy. Perhaps leave support for logging to macros or explicit reflection tasks for now. We can pick a syntax if this stabilizes and we feel we can do better than macros.

## Assertions (Tentative)

Anonymous assertions risk breaking code in ways that are difficult to repair through extensions. OTOH, they're convenient. I propose to approach assertions much like logging, e.g. disable per module or per function.

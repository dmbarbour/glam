# Glam Syntax

This document describes an initial syntax for ".g" files, and the motives for it. Primary design goals include a syntax that I find pleasant to work with, and supporting the assembly programming vibe. 

As a design principle, this isn't a minimalist syntax, but it is generalist. I'll introduce operators and keywords where it offers significant benefits (improved clarity, reduced boilerplate, etc.) without becoming overly specialized to a problem domain. Macro DSLs and user-defined syntax should cover the rest. 

### Character Set

We'll limit to printable ASCII and whitespace (0x21-0x7E, SP, CR, LF) for assembler-provided front-end compilers. We could easily extend to UTF-8 post-bootstrap.

### Keywords and Operators

Keywords and operators are compiler-defined. Their meaning should be stable for entire subcommunities. User-defined syntax, where the user provides or overrides the front-end compiler, offers a mechanism to extend keywords and operators. But, I imagine most user projects will favor macro DSLs when they need a few local syntactic tweak.

Keywords are distinguished from user definitions by prefix '.', e.g. `.keyword`. This fits the assembly aesthetic, simplifies backwards compatible language extension, and is clear to readers without syntax highlighting. It has grown on me since I started using it. Keywords are handled much the same as operators.

To avoid unnecessary parentheses, the compiler shall have a sophisticated model of precedence and associativity for operators. This is most useful for associative constructs like `f >> g >> h` or `x + y + z`. Not every pair of operators will have a precedence, however. For example, if we use both '>>' and '<<', parentheses are required. 

We'll support Haskell-style closure of binary operators, e.g. such that `((>>= k) op)` and `(op >>= k)` are equivalent. This does not extend to keywords. In general, for operators both arguments are expressions. Keywords are a mixed bag, but frequently involve special syntactic forms.

*Note:* There are no implicit conversions. Users are free to leverage ad hoc polymorphism, but the built-in keywords and operators will expect one type and report errors.

### Names and Namespaces

Names use a fairly conventional alphanumeric encoding, e.g. regex `'_'?[a-zA-Z][a-zA-Z0-9_]*`. 

Namespaces are concretely represented as dictionaries. Names desugar into atoms for purpose of indexing dictionaries, e.g. `name` becomes `.["name"]:()`. In context of hierarchical namespaces, we use dotted paths, e.g. `foo.bar.baz`.

In the general case, we support expression-indexed names. This is expressed as `.(ListExpr)` or `.[...]` for a literal list. These names are interpreted such that `.([1, .["two"]:()] ++ [3])` is equivalent to `.[1].two.[3]`. The empty path is permitted, e.g. `foo.[]` is equivalent to `foo`. Users are not encouraged to use expression-indexed names in the module toplevel.

#### Introductions and Overrides

We'll syntactically distinguish introductions vs. overrides. It's an error to introduce a name that already has a definition, or to override a name that does not have a prior definition. We use `name = Expr` for introductions and `name := Expr` for overrides. In the latter case, `.prior` implicitly refers to the previous definition. 

By default, module-scope names evaluate to final definitions, subject to overrides. But there are a few cases where we'll want to reference prior definitions. I propose `(.module_prior Expr)` and `(.module_final Expr)` to control bindings in scope. We essentially default to the `.module_final` scope.

#### Forbid Name Shadowing

Name shadowing occurs when a name masks access to another name in scope. This usually happens with generic local names like `\map -> ...` where 'map' may mean many different things (e.g. to apply a function over a list, an associative data structure, a game world map). Unfortunately, this easily results in bugs that humans easily miss when reading code: contextual usage is obvious to humans, but the compiler's interpretation of context is not.

In context of open recursion for inheritance and override, name shadowing would be even more problematic. Masking names hinders extension, and it becomes confusing what the final definition for any given use of a name refers to. 

Thus, as a rule, we'll forbid name shadowing. But only for static contexts: we forbid shadowing of names from an included module, but not for shadowing of names introduced by *including* a module. This may involve threading metadata through includes via the namespace. It isn't difficult to avoid name conflicts.

#### Abstract Definitions

In context of modules as mixins, we may assume a name is introduced without defining it locally. However, to resist errors, it is convenient to report 'undefined' errors closer to the code that leaves names undefined. To support these cases, I propose a toplevel declaration:

        .abstract Name(, Name)*

This declaration is not required for names brought into scope (or declared abstract) via 'include'. The intuition we want for include is that we're including all relevant definitions and declarations, including `.abstract`. Hierarchical names are captured implicitly, i.e. if we `.abstract foo` we don't need `.abstract foo.bar`. For convenience, `.abstract env` is implicit.

The tracking of abstract definitions across includes is essentially just maintaining a set of dotted-path prefixes that we *don't* report 'undefined' errors about. 

#### Aliasing (Tentative, Low Priority)

We can write `baz = foo.bar.baz` as a definition. However, a definition isn't an alias. In context of macros, we'd need to write `baz = .module_prior foo.bar.baz` instead. We must be aware of the distinction between overriding `baz` and `foo.bar.baz`. 

We could support aliasing in terms of rewriting names. But we should be very careful about name shadowing and stability of names. As a minimum viable aliasing model, we could support declarations of form:

        .alias baz = foo.bar.baz

This introduces a rewrite rule such that future references to `baz` expand to `foo.bar.baz`. To resist ambiguity errors, it's useful to also introduce a definition for `baz`. This could be a placeholder like `(.error "aliased")` or `foo.bar.baz` as a backup.

When basic aliasing is working properly, we could feasibly introduce a syntax for bulk aliasing. Something like:

        .using foo: bar.baz, q = qux
            # logically expands to
        .alias baz = foo.bar.baz
        .alias q = foo.qux

There are remaining design challenges, such as how aliasing should interact with includes and imports. 

### Effects

We'll almost directly adopt Haskell's do notation, `.do ...`. 

Instead of desugaring to monadic operators, we'll desugar to a tagged function of form `eff:(\__api -> Body)`, where `__api` is guaranteed to not shadow anything. The `eff` tag helps distinguish effects from pure functions and doubles as a calling convention. Within the '.do' context, we'll desugar `%name` to `__api.name`. This generalizes, e.g. `%[Expr].op` to `__api.[Expr].op`, or `%[]` to capture the effects API. The compiler will essentially use `%Seq` to compose within Body.

We'll define several operators in these terms, e.g. `op >>= k = eff:(\api -> api.Seq op k)` and `>>>` if op returns unit. For convenience, I propose `.ret` as a built-in for `\x -> eff:(\api -> api.Return x)`. Other operators and keywords may support applicative styles, e.g. `<*>` and `<|>`, `.fail` and `.cut`, etc.. 

I intend to diverge from Haskell regarding RecursiveDo, requiring explicit forward declaration of locals whose values are determined later. This improves visibility and mitigates issues like the conflict between fixpoint and continuations.

### Macros

In context of lazy loading, macro invocations must be distinct from normal evaluation. Proposed syntactic forms:

        @(Expr)
        @macro_name         short for @(.module_prior macro_name)

The compiler lazily evaluates and interprets `Expr` at compile-time. If this evaluates to a function, the compiler parses an argument `Expr`, applies the function, then repeats. Thus, macros may be parameterized as normal functions of any arity. After all arguments are read, the macro should evaluate to a `eff:(\api -> Body)` effect. The compiler provides a localized effects API then runs the effect. 

The effects API provides parser combinators to read code, supporting macro DSLs, and emitters to write code. Reads and writes both have flexible levels of abstraction, e.g. we can work with raw text, ASTs, abstract expressions, etc.. To isolate errors and simplify local reasoning, macros cannot escape their scope, and balance of brackets, braces, parentheses, etc. are preserved by both parsers and emitters. Without looking at its definition, we know `(@foo ...)` will read and write within those parentheses. Also, macros may also only read from their right-hand side.

A relevant concern is how macros interact, e.g. in context of `(@foo @bar ...)`. To keep it simple, I propose transactional semantics with a predictable schedule: each macro evaluates to completion in one step, and we always favor the earliest (i.e. leftmost, topmost) macro. This design still admits sophisticated interactions insofar as macros emit more macro invocations, but it ensures syntactic locality of such interactions.

Aside from reading and writing code, macros may provide access to other compiler-provided effects, e.g. access to built-in functions or writing messages to a log.

### Tagged Data

        tag:Data

Tagged data is modeled as a singleton dictionary. But the compiler implicitly annotates tagged data raise an error upon update (via `.with`). Thus, `{ tag:Data }` is distinct from `tag:Data` regarding opportunity for future updates. The tag generalizes to dotted-path names. The primary use case is `.[TagExpr]:Data` for a computed tag with a single-level index.

Pattern matching, in the general case of `.(TagList):Pattern`, would evaluate `TagList`, extract the indexed element while verifying that a singleton dictionary at each level, then match the given pattern. 

### Atoms

Atoms are data where the only useful observation is equality.

Constructed atoms are useful for structured data, flags, and names. The unit value is a built-in atom, expressed and matched as `()`. Tagged unit data, i.e. `tag:()` or `.[TagExpr]:()`, serves as a constructed atom. Names are indexed as constructed atoms, e.g. `.["name"]:()`, thus `tag:()` is technically shorthand for `.[.["tag"]:()]:()`. The assembler should recognize and optimize these atoms. 

Unique atoms are useful for access control and conflict avoidance. I propose a `.unique Foo, Bar, Baz` declaration at the module toplevel. These atoms derive uniqueness from the implicit path through a hierarchical module namespace, with just a little support from the assembler. Use cases include access control and conflict avoidance. 

Scope-unique atoms are useful for the ephemeron performance pattern. To support this, `.scope_unique : Atom -> Atom` returns the same atom annotated with unique metadata. When matching or comparing atoms with different metadata, we diverge with error. Thus, we never observe scope-uniqueness to be violated. When used as dict keys, we can collect associated data when metadata becomes unreachable.

### Dicts

For simple, literal dictionaries, I propose syntactic form `{ name1:Expr1, name2:Expr2, ... }`. This desugars to `{} .with { name1 = Expr1, name2 = Expr2, ... }`, where `{}` is the empty dictionary and `=` represents namespace introduction. It also generalizes to dotted-path and expression-indexed names, e.g. `{ .[1]:"hello", foo.[2]:"world" }`.

Dictionary updates are generally expressed using `.with` and `.without` special forms. These are applied much like infix operators, but the RHS isn't an expression:

        Dict .with 
            x := prior + 42
            y := 10
            .[1] = "this is new"

        Dict .without x, y, z

The `.with` syntax enforces explicit overrides, i.e. it's an error to introduce a name that already exists or override a name that does not exist. The `.without` form removes listed names if they exist, but is not an error if the name does not exist. In case of dotted-path names, it also removes empty hierarchical dictionaries in the removed path prefix. Thus, in case of `{foo:{bar:42}} .without foo.bar` the result is `{}` instead of empty directory `{foo:{}}`. 

Pattern matching on dictionaries uses the literal form with an optional remaining pattern, e.g. `{ .(Expr):(a,b,c), x:42, Pattern }`. We *evaluate* key expressions within the pattern, and we remove matched keys (via `.without`) before matching on the remaining pattern. The default remaining pattern is `{}`, requiring a complete match.

In the general case, users may want conditional behavior based on whether a dictionary contains a given field. This can be expressed in terms of pattern matching.

### Embedded Texts

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

### Numbers

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

### Lists

I propose to use square brackets and commas for literal lists.

        []
        [1]
        [1,2,3]

We'll use `++` to compose lists by appending them. There is no dedicated 'cons' operator in syntax, but we can express `cons x xs = [x]++xs`. We may generally use `++` in pattern matching, limited to one variable-length list, e.g. `[x]+xs` or `xs+[x]` or `[x0,x1]+xs+[xn]`. 

Currently, there is no syntax for list length, slicing lists, etc.. We'll need accelerated functions in those roles.

### Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially a list with different connotations - fixed size, non-homogeneous - and distinct pattern matching. In practice, we'll almost always access tuples via pattern matching, e.g. `(Pattern, Pattern, Pattern)` for a triple. We can feasibly accelerate short tuples to reduce the number of allocations. There is no dedicated syntax for tuples smaller than a pair, though users are free to manually write `tuple:[a]`. 

Tuples are sometimes more convenient than small dictionaries. A relevant cost is extensibility. Tuples should mostly be used for either very stable structures or local intermediate representations.

### Functions

I propose to adopt Haskell's use of `\` for lambdas.

        \ x y z -> Expr
        \ x -> \ y -> \ z -> Expr

We'll also support Haskell-style `name args = ...` as a syntactic sugar. This extends to `:=`.

        name = \ x y z -> Expr
        name x y z = Expr
        name x y z := Expr

Pattern matching is entirely separated from argument bindings. It gets very awkward in context of overrides.

### Partial Functions

Functions may diverge in general, e.g. entering an infinite loop or dividing by zero. In practice, users will also experiment with incomplete implementations. We'll provide a little syntax for the cases where a user 

        .error Expr         recognized errors
        .tbd Expr           incomplete definitions

The expressions are visible to the reflection API, along with context. The should be something an IDE or logger can render. A string is adequate, but I suggest a dictionary or object that provides multiple interfaces for flexible filtering and views. Of course, one of those interfaces could be 'text'. 

### Pipes

Borrowing F#'s syntax here:

        f <| arg = f arg
        arg |> f = f arg

I propose to also support directional function composition:

        f >> g = \ h -> g (f h)     
        g << f = f >> g

### Booleans

Booleans need special attention in context of effectful conditional expressions.

A first take is to model booleans as atoms, i.e. `true:() | false:()`. Although this works, it does not make it convenient to mix effectful operations into conditional expressions. We can feasibly extend to `true:() | false:() | eff:(...)` where the effect returns a boolean, but I'd prefer to not mix layers like this. It may prove more convenient to directly use effectful `.ret ()` and `.fail` as booleans.

In any case, I propose `.t` and `.f` to abstract booleans and support pattern matching on booleans. 

### Modules

Need a syntax for `.import`, `.include` 

### Rejected Conditional Code

I've contemplated support for toplevel `.ifdef` and such, but I feel that conditional definitions overly complicate the namespace even before I contemplate how it should interact with overrides. Better to stick with aggregate definitions via `name := .prior ++ [...]` or similar.

### Object Specs

### Annotations



### Comments

- potential `.nb Expr`
- line comments
- disabling sections of code (`.DISABLE_START` and `.DISABLE_END` perhaps? or just leave to IDE).

### Laziness and Sparks

### Accelerators

### Language Version Declaration (Tentative)

A language version declaration enables a compiler to adapt to programs written in older versions of a language, or to detect early whether a program uses a more advanced version of the language than the compiler recognizes. But it seems much less necessary with keywords separated from user definitions.

### Pattern Matching

View patterns permit more than one match, however.

I want to desugar all pattern matching to monadic expressions, and I also want to support transactional backtracking conditionals by default. Support for 'what-if' pattern matching is simply very convenient.


Although we could support Haskell-style `match Expr with (Pattern -> Outcome)+` syntax, providing the pure handler, it's a little awkward to extend this syntax for effectful patterns, and it may be better to integrate the 'Expr' into the Pattern, allowing for more than one (e.g. as guards). I'm contemplating alternative syntax, e.g. based on unification or `Pattern = Expr` structures. We could feasibly integrate pattern matching into monads in general.

### User-Defined Types?

I would like to support lightweight declarations of type constructors and matching patterns.

### Data Embeddings

Some design constraints and desiderata:

- specialized monad for writing lists, multi-line texts? Tentative. 
- vertical structure, avoids 'deep' indentation
- user-defined types and object interfaces
  - possible type-indexed behavior bound to named types?


# Initial Syntax

This document describes an initial syntax for ".g" files, and motives for it. Design goals include:

- a syntax that I find pleasant to work with
- supports an assembly programming look and feel
- concise, vertical columns of assembly mnemonics
- generalist, not specialized for targets or domains
- an extraordinarily high abstraction ceiling

## Language Version Declaration

Reproducibility requires that the same sources produce the same outcome, but there is an implicit condition: an outcome is produced. A language version declaration simplifies reproducibility because the compiler can fail fast, refusing to produce any outcome rather than a result that drifts as the compiler is updated.

Proposed syntax:

        (lang|language) (BaseVer) (with Extensions)?
        language g0 with utf8

The version declaration should be the first toplevel declaration in a ".g" file. The BaseVer is a recognized string for a package of features, while extensions modify that package.

In practice, if a compiler halts on language version, we must be using different executable or configuration (if configuration defines `conf.env.lang.["g"].compile`) from development conditions. Users can resolve this by reproducing the executable (e.g. via nix) or by defining compatible compilers in the module system.

## Character Set

We'll start with printable ASCII and whitespace (0x21-0x7E, SP, CR, LF). It is not difficult to extend to UTF-8, though I'm concerned about legibility. We'll recognize CR, LF, and CRLF as line endings. The compiler shall emit a warning if the file uses inconsistent line endings.

## Comments

We'll support Python-style line comments, i.e. `#...` to end of line. There are no multi-line comments. An editor with vertical selection is recommended if users intend to comment out large sections of code. Comments are treated as whitespace by compiler and macros, but may be structured for purpose of external tooling (literate programming, projectional editing, extracting API docs, etc.).

## Toplevel Structure

The module toplevel consists of a sequence of 'declarations'. Each declaration starts a new line. If a declaration requires more than one line, any continuing lines (excepting blanks) must be indented by at least one space. This structure simplifies error isolation, local reasoning, and parallel processing.

Each declaration starts with either a keyword (such as `import`, `spec`, or `unique`) or is a basic definition of form `name = Expr` or one of its variants (args in lhs, `:=`, `::=`, etc.). We'll favor basic definitions where feasible, thus keywords are mostly for special forms.

In context of errors, the errors can be reported but we can also make a best effort to proceed with errors. This might depend on configuration options or command-line arguments.

## Keywords

Keywords are names reserved by the compiler. Users are not permitted to define or use keywords directly as names. An exception is made for atoms. For example, given keyword `import`, users may define `.['import] = ...` or reference `module.['import]. The set of keywords may vary with the language version declaration.

Proposed keywords:

        import                                      modules
        module, abstract, shadow                    namespace
        anno                                        annotations
        unique, abstract_global_path                special atoms
        with, without                               dict update
        do                                          effects
        object, extend, extends                     objects

        TBD:
        orchestration, pattern matching, conditionals

I'm still considering whether to support booleans at all. But potential keywords here:

        if, then, else, elif                        basic conditionals
        and, or, not, is                            comparisons

## Names and Paths

We accept a subset of C names, mostly restricting use of underscores. A viable regex:

        Part = [a-zA-Z][a-zA-Z0-9]*
        Name = Part('_'Part)*
        Path = Name('.'Name)*

Namespaces are modeled as hierarchical dictionaries, accessed via dotted path, e.g. `foo.bar.baz`. To index the dictionary, we translate each names into an atom, i.e. name `foo` translates to atom `'foo` (see *Atoms*), which is used as a key for a dictionary. Users may similarly quote path suffixes into lists, e.g. `'.foo.bar.baz` evaluates as `['foo, 'bar, 'baz]`. 

In the general case, we also support expression-indexed paths using `.(ListExpr)` or `.[...]` for a literal list. These indices are interpreted such that `.([1, 'two] ++ [3])` is equivalent to `.[1].two.[3]`. The empty list is permitted, e.g. `foo.[]` is equivalent to `foo`, and `foo.[ ].bar` admits spaces, newlines, and comments in names if needed.

Best practice is to avoid expression-indexed paths in module or object namespaces, but it's available as an escape hatch for integration. Users may define `.[Idx] = Def` at the module toplevel. Later access to this name requires `module.[Idx]`. Users may understand `module` as a keyword that aliases the module toplevel namespace.

Special cases: 
- `.path` desugars to `eff:(\api -> api.path)` to support lightweight effects. See *Effects.*
- `^path` refers to host namespace in context of hierarchical specifications. See *Objects.*

### Introductions and Overrides

When defining names, we'll distinguish introductions versus overrides. An introduction `name = Expr`. An override uses `name := Expr` or `name ::= \ prior -> Expr`. It is an error to introduce a name that is already defined, or to override a name that isn't already defined. This resists ambiguity issues, i.e. a name is introduced with some intention or purpose, and overrides should preserve purpose.

In context of overrides, it is often necessary to reference the prior definition. The `::=` form supports access to prior definitions without repeating names. But for the more general case, I propose a `_` prefix on names, i.e. `_foo` refers to the prior definition of `foo`. Similarly, use of `_module` refers to prior module namespace.

*Note:* Deleting definitions isn't recommended or syntactically supported. Better to allocate a fresh, hierarchical module or object namespace then build monotonically. But if users insist `.[] ::= \ prior -> prior without foo` would do the trick. 

### Abstract Definitions

To localize errors, and to simplify analysis of name shadowing, names in use shall be defined or declared. I propose a lightweight declaration for names that we assume to be provided externally:

        abstract Name(, Name)*

Essentially, these declarations build a list of toplevel names that that compilers won't complain about being undefined locally. We don't bother with granularity below the toplevel name.

To share abstract declarations across includes, we'll represent them in our namespace. This might simply be defined in `meta.abstract_names` or similar. The compiler may introduce `abstract env` implicitly.

### Final Definitions 

In some cases, it is useful to guard against accidental updates to definitions. The most obvious example is to block accidental updates to an object instance because users should be updating the specification instead. But it's important to preserve the ability to update definitions regardless. 

To this end, we might use a pattern such as defining `final_of.foo = _foo` then assigning a reflection task to scan definitions and perform comparisons. We could introduce a syntactic form for it, but it might not be worthwhile.

### Forbidden Shadows

Name shadowing, where a function argument or local variable accidentally masks another name defined or declared in lexical scope, is a common source of subtle bugs. Humans are a lot more flexible about referential context than a lambda calculus, thus easily overlook the error when reading code. To resist this bug, we'll warn on name shadowing by default.

It is feasible to introduce a special form to admit shadowing, such as `shadow Name(, Name)* in Expr`. But I'm not convinced this is a good idea. We can explore shadowing further via language extensions.

### Unused Names

As a general rule, the compiler will warn for unused locals. Use of `_` in place of a variable name indicates data is explicitly dropped. It is tempting to support `_name` for naming local variables that may be accessed (as `name`) or dropped. OTOH, a simple `skip (foo,bar) baz` where `skip _ x = x` is adequate.

### Associated Names

In many cases, we'll want to associate one name with another. The proposed convention is a dict named with an `_of` suffix. For example, given a name `foo`, we can also reference `type_of.foo` or `spec_of.foo`. The assembler ignores associated names, and I anticipate users mostly work with such names indirectly.

### Module Metadata

The front-end compiler might generally maintain metadata in `meta.*`. This would include features such as a list or index of introduced names, a set of abstract names, etc.. Macros may also contribute. This is for module-level intrinsic properties that are essentially 'owned' by the compiler or extension-like macros.

### Reflection Tasks

The compiler will arrange to automatically run `refl.*` definitions as reflection tasks. The assembler doesn't interpret `refl.*` implicitly, so this arrangement must be expressed as a compile-time effect or (very awkwardly) a term annotation. 

## Operators

Operators are essentially infix functions. We'll support Haskell-style operator sections, such that `((>>= k) op)` is equivalent to `(op >>= k)`. To avoid unnecessary parentheses, we'll support precedence between most operators. To mitigate confusion, not every pair of operators will have valid precedence, e.g. cannot mix both `>>` and `<<` without parentheses.

We may support a few special non-binary forms, e.g. `(x < y =< z)` as shorthand for `((x < y) and (y =< z))`. We'd also support `(< =<)` operator sections. Risk of confusion is mitigated because we cannot compare booleans for less-than or greater-than.

Operators may support limited ad-hoc polymorphism. For example, `>` could compare two numbers, two lists, two tuples. For lists and tuples, we use a lexicographic comparison of elements. Comparing a number to a list, or a list to a tuple, would simply diverge with an error. As a rule, ad-hoc polymorphism must preserve laws or intuitions, e.g. don't use `+` to append lists because it does not preserve commutativity of `+` on numbers.

*Tentative:* Minimal operator precedence, mostly for associative structure. Require parentheses everywhere else.

## Application

Application is essentially expressed as a whitespace 'operator', i.e. `f x` applies `f` to `x`. Usually, we apply pure functions. But in context of ad hoc polymorphism, we might extend application to effects and objects:

- Application of effects, `(eff:f) x = eff:(\api -> f api x)`, simplifies integration of lightweight effects APIs, enabling curried arguments and reducing local knowledge of arity.
- Application of objects, recognized by 'spec' and 'args', might apply a mixin to extend a list, i.e. `args ::= \ prior -> prior ++ [new_arg]`. Supports var-args.

Functions, effects, and objects are likely all we need, but it is feasible to extend application further.

## Effects

We'll almost directly adopt Haskell's do notation. For aesthetic reasons, we'll support both `var <- op` and `op -> var`. Semicolons can serve as virtual line separators as needed. To support concise, convenient effects without polluting the toplevel namespace, we'll desugar `.name` to `eff:(\api -> api.name)` and define application to work with effects: `(eff:f) x = eff:(\api -> f api x)`. RecursiveDo is implicit and benefits from forbidden shadowing.

Aesthetically, this should support a direct assembly programming style where we have a column of operations on the left and the occasional label tabbed out to the right. 

        my_loop = do
            .label                      -> loop_start
            .movl 'eax ['ebx, 4]
            ...

Haskell has applicative style via `<*>` and `<$>` that is convenient for some use cases. We could provide similar operators, though I hope to be more concise. Will probably defer these for now.

## Macros

In context of lazy loading, macro invocations must be distinct from normal evaluation. Proposed syntactic forms:

        @(Expr)                 general form
        @macro_name             short for @(_module.macro_name)

The preferred form is `@macro_name`, but the `@(Expr)` form is more general. The compiler lazily evaluates `Expr` at compile-time. This should return an effect of form `eff:(\api -> ...)`. The compiler provides a monadic API to read and write code at flexible levels of abstraction. Modules are typically parameterized in terms of the macro effectfully reading its body, e.g. `@foo arg1 arg2`. This supports macro DSLs in the general case.

To simplify local reasoning and isolate errors, the macro effects API shall protect scope, i.e. balance of brackets, braces, parentheses, etc.. Macros logically read from their right and write to their left, never reading their own writes. Macros cannot read other macro invocations, e.g. in `(@foo @bar ...)`, `@foo` may block on a reader until `@bar` is processed. A toplevel macro is scoped to reading one toplevel declaration (based on indentation), but may write many declarations.

## Annotations

        anno : Annotation -> Term -> Term

To express term annotations, I propose keyword `anno`, referring to a built-in function that applies an annotation in context of a term, then returns the term. Annotations are not observable modulo reflection, but may guide performance, debugging, and other use cases. 

The assembler recognizes effectful annotations, of form `eff:(\api -> ...)`. The assembler provides the reflection API, runs the operation to completion, then returns the given term. Depending on the API, the effect may have limited access to term and continuation, e.g. to surgically apply more annotations. If the effect diverges, so does the `anno` expression.

The assembler (or compiler) may recognize other ad hoc annotations such as `accel:'.list.split`. To avoid silent degradation of performance or reasoning, the assembler shall warn about unrecognized annotations. For convenient composition, there shall be an effect to apply more annotations to the term.

## Tagged Data

Tagged data is convenient for modeling extensible variants and resisting assembly-time type errors.

        tag:Data
        [TagExpr]:Data

Tagged data is modeled as a singleton dictionary. That is, `tag:Data` is equivalent to `{ tag:Data }` in most contexts. Constructed tags are expressed using `[Expr]:Data`. For consistency, this syntax limits users to a singleton tags, i.e. no dotted paths, exactly one element in tag constructor `[Expr]` form. Users may access tagged data via pattern matching via dotted path, e.g. `(tag:Data).tag` evaluates to `Data`.

*Note:* Syntactically, tagged data will bind tighter than application, e.g. `fn foo:bar baz` would pass `fn` two arguments, `(foo:bar)` and `baz`, as opposed to `foo:(bar baz)`. Otherwise we're stuck 

## Atoms

Atoms are data where the only useful observation is equality.

The unit value is a built-in atom, expressed and matched as `()`. Tagged unit data, i.e. `tag:()` or `[TagExpr]:()`, serves as a symbolic atom. As shorthand, `'name` rewrites to `["name"]:()`. Note that `'tag` and `tag:()` are distinct atoms: `["tag"]:()` vs. `['tag]:()`. We'll use symbolic atoms for booleans, `'t` and `'f`. They're also convenient for naming registers such as `'eax` without defining things.

For access control and conflict avoidance, we can (with a little support from the assembler) leverage the global namespace as a stable source of unique atoms. A viable approach is `Foo = abstract_global_path Foo`. Here `abstract_global_path` returns an atom based on the final location of `Foo` in the module system, and defining `Foo` resists accidental reuse (i.e. 'allocating' the name). I propose a toplevel declaration `unique Foo, Bar, Baz` for bulk definitions of this form. *Aside:* `abstract_global_path` is intentionally verbose to encourage the `unique` form.

Scope-unique atoms are useful for the ephemeron performance pattern. To support this pattern, we can introduce a term annotation, `'scope_unique`, that wraps a given atom with unique metadata. If ever we compare the same atom with different metadata, we diverge instead, thus never observing the violation of scope uniqueness. When used as dict keys, we associate data to a weakref of that metadata.

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

We use a prefix underscore to indicate negative numbers. This is part of the number literal, not a separate operator. Internal underscores between digits (i.e. digit on both sides) are ignored by the parser, existing only to enhance legibility for humans. Decimal floating point or scientific notation can be encoded directly using an 'e' separator for the exponent.

        0xc0de
        0b10010_00110100_11111110_11011100

We'll support hexadecimal (0x) and binary (0b) number literals, too. We can feasibly provide some 'bitwise' operators or accelerated functions on natural numbers. Although numbers don't have a built-in notion of word size or encoding, it isn't difficult to impose one.

The compiler will provide a few useful operators - `+ * / -`. Common functions, such as rounding numbers, should be accelerated.

Numbers are modeled as exact rationals with no bound on size or precision. Thus, any loss of precision is under user control. This has severe performance implications. If users ever need high-performance assembly-time number crunching, they'll be relying on accelerated evaluation of CPU or GPGPU DSLs instead of built-in arithmetic.

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

*Note:* It is possible to use quoted paths to build lists. As an expression, `'.[x0,x1].(xs).[xn]` evaluates similarly to `[x0,x1] ++ xs ++ [xn]`. But the compiler may raise an error if a quoted path doesn't support `==`, and in context of pattern matching we implicitly evaluate the quoted path then compare via `==`.

## Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially list with different connotations. Lists tend to be variable-size but homogeneous. Tuples tend to be fixed-size but non-homogeneous. We append lists, but we tend to simply construct or match on tuples inline. There is no dedicated syntax for empty or singleton tuples, but users are always free to use the `tuple:[a]` syntax.

Tuples are concise, but they negatively impact extensibility and scalability. This is mitigated by ad hoc polymorphism, e.g. we can easily match both `(X,Y,Z)` and `{x:X, y:Y, z:Z}` within a context. But, in practice, it's best to use tuples only for private intermediate representations or stable public interfaces.

## Tables and Databases? Defer.

I'm interested in supporting relational systems. A table might be expressed as a list of tuples paired with a header. A database is essentially a collection of tables and computed views. We could model a base database as a dict of tables, and the computed views as a mixin.

I don't intend to commit to a syntax anytime soon, but we can explore options with macros or declared language extensions. 

        @table.create Cities
            name: text, primary_key
            lat: number, range(-90, 90)
            lon: number, range(-180,180)
        @table.insert Cities
            "San Francisco" 37.7 -122.4
            ...

## Functions

I propose to adopt Haskell's use of `\` for lambdas.

        \ x y z -> Expr
        \ x -> \ y -> \ z -> Expr

We'll also support Haskell-style `name args = ...` as a syntactic sugar. 

        name = \ x y z -> Expr
        name x y z = Expr
        name x y z := Expr
        name x y z ::= \ prior -> Expr

The `::=` case is a bit awkward semantically, but we'll insist `\ prior -> ...` remains an argument on the right-hand side. We'll immediately bind prior to `_name`. 

Unlike Haskell, there is no support for pattern matching on lambda or definition arguments. 

## Partial Functions

We can simply use some term annotations for partial functions.

        anno 'error         Expr        recognized errors
        anno 'tbd           Expr        incomplete definitions
        anno 'deprecated    Expr        transitional code

In these cases, `Expr` may indicate the nature of the error or future intentions for a TBD. 

## Pipes

Borrowing F#'s syntax here:

        f <| arg = f arg
        arg |> f = f arg

I propose to also support directional function composition:

        f >> g = \ h -> g (f h)     
        g << f = f >> g

Ideally, we arrange precedences such that we can write stuff like:

        .op1 >>= f >> g >> .op2 >>= h >> .r
        .op1 >>= (f >> g >> .op2) >>= (h >> .r)

## Booleans

I propose to encode Boolean values, true and false, as the concise atoms, `'t` and `'f` respectively. There are no implicit conversions, no 'truthy' values. 

I propose to use keywords `and, or, not` for boolean composition. (IMO, `&&` and `||` begin to feel like line noise, and `!` should not be wasted on flipping a bit.) We can treat `and` and `or` as infix operators.

Although I have a vision for pattern matching, we'll support the conventional and familiar `if Cond then A else B` expressions, and also the `A if Cond else B` variant, which is more convenient in cases where we want to emphasize the operation over the condition. These are limited to pure boolean expressions. As a special case, in context of `do` notation, users may write `if Cond then A` or `A if Cond` without an `else` condition (defaults to return unit). 

## Modules

I've decided to consolidate module access into 'import', with the various cases indicated by a few extra words.

        import "Foo.g"                      # without 'as' or 'at' is mixin on toplevel namespace
        import "Bar.g" as b                 # 'as' for default introduction or 'at' for a mixin
        import "BigText.txt" binary as t    # introduces t as a binary

        import "Qux.g" as q from {
            , rev:Text                      # content hash of containing folder
            , search:[
                , tag:Text                  # tag or branch name for precise cloning
                , url:Text                  # a remote lookup, uses most recent tag
                , url:Text
                ]
            }

        import 'default from {
            , default:"src/main.g"
            , rev:Text
            , ...
            }

Remote modules are indicated by a `from` expression, typically a dict literal. The `rev` field is required, ensuring we uniquely identify a remote file and its transitive dependencies. Search hints are optional but recommended. We may use an atom instead of a string if the `from` location defines it. It is possible to build an index of modules within the module system. 

### Access Control

There is no notion of export control. Such a concept contradicts extensibility goals and conflicts with modules-as-mixins. However, we can easily invert this to distinguish public interfaces. We already use `conf.*` for the configuration, `asm.*` for the assembly, `env.*` for a threaded environment. We can similarly use `api.*` for a library's public interface. If this were an application language, perhaps `app.*` for application methods. 

        import "MyFooLib.g" as libfoo
        env.foo = libfoo.api
        alias libfoo.api: foo, bar, baz as qux
        foo ::= \ old_foo -> ...
        ...

There are forms of access control between subprograms. This is supported via unique atoms.

## Objects

For concision, names within object methods are localized by default, i.e. `foo` instead of `self.foo`, and `_foo` instead of `base.foo`. Instead, we pay extra to refer to the lexical host via `module.name` or `^name`. The latter extends to hierarchical objects, e.g. `^^^method` refers three levels up. Analogous to keyword `module`, `object` (or `_object`) as an expression refers to the object namespace as a whole, i.e. so we can reference `object.[42]`.

A minimal object syntax can be quite compact. I propose:

        object foo extends bar, baz with
            def1 = ...
            def2 := ...

The compiler will introduce `spec_of.foo` then define `foo` in terms of instantiating the spec, implicitly introducing `foo.meta.spec = spec_of.foo` as a final override. The `extends` section is optional, and enables multiple inheritance. Minimum viable object spec is just three elements:

        defs        mixin, i.e. \base self -> ...
        deps        list of object specifications
        uid         unique atom for linearization

We'll use the common C3 algorithm for multiple inheritance (MI), using `uid` to distinguish specifications. The linearization algorithm shall verify via reflection that `uid` is associated uniquely with one specification in scope.

To modify an object's definitions, I introduce a related declaration:

        extend object foo with
            ...

Note that this doesn't touch dependencies. Modifying object dependencies should by very rare in practice, but users may override  `spec_of.foo.deps := ...` or the full specification.

Object namespaces admit many toplevel declarations. Imports are an exception, but features that desugar to definitions are accepted, e.g. `unique`, `abstract`, and `object`. The compiler shall support `refl.*` tasks per object, ideally arranging them to run lazily when the object is used. In context of reflection, objects may define a `type_of.def1` and other associated definitions.

## Orchestration

### Brainstorming

I propose to model orchestration components as objects. The user applies a bundle of continuations and effects parameters via overrides. It may be useful to include defaults in some cases, e.g. define some continuations or effects in terms of others, and declare a minimal specifications of overrides (similar to Haskell typeclasses). The component is also situated, sequentially, within some context. Multiple results - emitters - are defined contingent on overrides and env.

The static context is determined at assembly-time. It describes features such as runtime locations where a subprogram looks for inputs.  Usefully, it may include first-class effects, making some effects available to continuations. We could use such effects to express open loops without explicit recursion. 

Instead of just a handful of values, it might prove convenient to model static context as something closer to tacit programming environment in its own right.

It is easiest to model information as flowing *forward*, aligned with sequential composition. But it is very useful to model some information as flowing in other directions. A minimum viable foundation is single-assignment variables, with assignment tracked as a linear obligation. We could introduce a notion of 'tentative' assignments for auto-commit when dropped. We can assign vars with a 'future' effect that may read and wait on other vars, extending to futures and promises.

We should generally be able to 'splat' a dict of effects or continuations as parameters when invoking things.

We might wrap these objects, e.g. with `proc:Object`, to resist accidents. I'm currently leaning towards use of `proc` as the primary naming convention for orchestration of runtime procedures and processes.

### Syntax 

We could feasibly borrow from Koru here, e.g. using `~` everywhere and `!` or `|` for bindings. 

- declare a proc
- define basic emitters
  - access to env, eff, cont
- define flow emitters
- invoke the proc
  - splat a subset of args

I currently use `|>` for functions, but we could readily overload it for `proc` objects. 




## Pattern Matching

View patterns permit more than one match, however.

I want to desugar all pattern matching to monadic expressions, and I also want to support transactional backtracking conditionals by default. Support for 'what-if' pattern matching is simply very convenient.


Although we could support Haskell-style `match Expr with (Pattern -> Outcome)+` syntax, providing the pure handler, it's a little awkward to extend this syntax for effectful patterns, and it may be better to integrate the 'Expr' into the Pattern, allowing for more than one (e.g. as guards). I'm contemplating alternative syntax, e.g. based on unification or `Pattern = Expr` structures. We could feasibly integrate pattern matching into monads in general.


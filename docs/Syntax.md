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

We'll support Python-style line comments, i.e. `#...` to end of line. There are no multi-line comments. An editor with vertical selection is recommended if users intend to comment out large sections of code. Comments are treated as whitespace by compiler and macros, but may be structured for purpose of external tooling (literate programming, projectional editing, extracting API docs, etc.).

## Toplevel Structure

The module toplevel consists of a sequence of 'declarations'. Each declaration starts a new line. If a declaration requires more than one line, any continuing lines (excepting blanks) must be indented by at least one space. This structure simplifies error isolation, local reasoning, and parallel processing.

Each declaration starts with either a keyword (such as `import`, `spec`, or `unique`) or is a basic definition of form `name = Expr` or one of its variants (args in lhs, `:=`, `::=`, etc.). We'll favor basic definitions where feasible, thus keywords are mostly for special forms.

In context of errors, the errors can be reported but we can also make a best effort to proceed with errors. This might depend on configuration options or command-line arguments.

## Keywords

Keywords are names reserved by the compiler. Users may not define or shadow keywords, nor directly use them in dotted paths or tags. Users may freely construct atoms from keywords, e.g. `'if` and `'else`, but most other uses are restricted.

Recognized keywords may vary with language version declaration. By reserving candidate keywords we can avoid breaking code. I propose to reserve all keywords I introduce in this document, all English conjunctions and prepositions, and a handful of additional names. Will update this section later.

## Names and Paths

We accept a subset of C names, mostly restricting use of underscores. A viable regex:

        Part = [a-zA-Z][a-zA-Z0-9]*
        Name = Part('_'Part)*
        Path = Name('.'Name)*

Namespaces are modeled as hierarchical dictionaries, accessed via dotted path, e.g. `foo.bar.baz`. To index the dictionary, we translate each names into an atom, i.e. name `foo` translates to atom `'foo` (see *Atoms*), which is used as a key for a dictionary. Users may similarly quote path suffixes into lists, e.g. `'.foo.bar.baz` evaluates as `['foo, 'bar, 'baz]`. 

In the general case, we also support expression-indexed paths using `.(ListExpr)` or `.[...]` for a literal list. These indices are interpreted such that `.([1, 'two] ++ [3])` is equivalent to `.[1].two.[3]`. The empty list is permitted, e.g. `foo.[]` is equivalent to `foo`, and `foo.[ ].bar` admits spaces, newlines, and comments in names if needed.

Best practice is to avoid expression-indexed paths in module or object namespaces, but it's available as an escape hatch for integration. Users may define `.[Idx] = Def` at the module toplevel. Later access to this name requires `module.[Idx]`. Users may understand `module` as a keyword that aliases the module toplevel namespace.

Special cases: 
- `.path` evaluates to `eff:(\api -> api.path)` to support lightweight effects. See *Effects.*
- `^path` refers to the host namespace in context of hierarchical specifications. See *Objects.*

### Introductions and Overrides

When defining names, we'll distinguish introductions versus overrides. An introduction `name = Expr`. An override uses `name := Expr` or `name ::= \ prior -> Expr`. It is an error to introduce a name that is already defined, or to override a name that isn't already defined. This resists ambiguity issues, i.e. a name is introduced with some intention or purpose, and overrides should preserve purpose.

In context of overrides, it is often necessary to reference the prior definition. The `::=` form supports access to prior definitions without repeating names. But for the more general case, I propose a `_` prefix on names, i.e. `_foo` refers to the prior definition of `foo`. Similarly, use of `_module` refers to prior module namespace.

*Note:* Deleting definitions isn't recommended or syntactically supported. Better to allocate a fresh, hierarchical module or object namespace then build monotonically. But if users insist `.[] ::= \ prior -> prior without foo` would do the trick. 

### Abstract Definitions

To localize errors, and to simplify analysis of name shadowing, names in use shall be defined or declared. I propose a lightweight declaration for names that we assume to be provided externally:

        abstract Name(, Name)*

Essentially, these declarations build a list of toplevel names that that compilers won't complain about being undefined locally. We don't bother with granularity below the toplevel name. 

*Note:* As the standard case for external definitions, `abstract env` is implicit.

### Final Definitions 

In some cases, it is useful to guard against accidental updates to definitions. The most obvious example is to block accidental updates to an object instance because users should be updating the specification instead. We could express this by defining `final_foo = _foo` at some point, then later a reflection task verifies `foo` and `final_foo` are the same.

### Forbidden Shadows

Name shadowing, where a function argument or local variable accidentally masks another name defined or declared in lexical scope, is a common source of subtle bugs. Humans are a lot more flexible about referential context than a lambda calculus, thus easily overlook the error when reading code. To resist this bug, we'll warn on name shadowing by default.

It is feasible to introduce a special form to admit shadowing, such as `shadow name(, name)* in Expr`. But I'm not convinced this is a good idea. We can explore shadowing further via language extensions.

### Unused Names

As a general rule, the compiler will warn for unused locals. Use of `_` in place of a variable name indicates data is explicitly dropped. It is tempting to support `_name` for naming local variables that may be accessed (as `name`) or dropped. OTOH, a simple `skip (foo,bar) baz` (where `skip _ x = x`) seems adequate. I'll defer support for `_name` in this role because I feel it may be a bit confusing.

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

I favor the above form. We could put labels on the left, but we'd need semicolons to break up indentation, for example:

        my_loop = do
         ;  loop_start <- .label
         ;                .movl 'eax ['ebx, 4]
         ;                ...


Haskell has applicative style via `<*>` and `<$>` that is convenient for some use cases. We could provide similar operators, though I hope to be more concise. Will probably defer these for now.

*Aside:* We can feasibly model 'stack frames' upon a foundation of shift-reset (for early return) and state (for local vars, deferred ops). Explicit use of stack frames would enable more-conventional procedural programming. But it isn't clear whether this would benefit from dedicated keywords. Perhaps `.frame/.defer/.fexit` is adequate. Maybe add a frame 'tail-call' variant.

## Macros

In context of lazy loading, macro invocations must be distinct from normal evaluation. Proposed syntactic forms:

        @(Expr)
        @macro_name         short for @(_module.macro_name)

The preferred form is `@macro_name`, which assumes macros are defined only in the toplevel namespace. But the `@(Expr)` form is more general. The compiler lazily evaluates `Expr` at compile-time. This shall return an effect of form `eff:(\api -> ...)`. The compiler provides a monadic API to read and write code at flexible levels of abstraction. Modules are typically parameterized in terms of the macro effectfully reading its body, e.g. `@foo arg1 arg2`. This supports macro DSLs in the general case.

To simplify local reasoning and isolate errors, the macro effects API shall protect scope, i.e. balance of brackets, braces, parentheses, etc.. Macros logically read from their right and write to their left, never reading their own writes. Macros cannot read other macro invocations, e.g. in `(@foo @bar ...)`, `@foo` may block on a reader until `@bar` is processed. A toplevel macro is scoped to reading one toplevel declaration (based on indentation), but may write many declarations.

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

*Note:* It is possible to use quoted paths to build lists. As an expression, `'.[x0,x1].(xs).[xn]` is equivalent to `[x0,x1] ++ xs ++ [xn]`. There is a distinction in context of pattern matching: a quoted path is evaluated then matched exactly.

## Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially a list with different connotations - fixed size, non-homogeneous - and distinct pattern matching. In practice, we'll usually access tuples via pattern matching, e.g. `(P1, P2, P3)` for a triple. *Note:* the tuple syntax does not support leading commas or tuples smaller than a pair. For any such cases, favor `tuple:ListExpr` instead.

Tuples are sometimes more convenient than small dictionaries, mostly due to concision. Unfortunately, compared to dictionaries, tuples are much less extensible. Consequently, tuples should be used only where they represent stable types or local intermediate representations. This is mitigated by ad hoc polymorphism: it is feasible to continue supporting `(X,Y,Z)` during a transition to `{x:X, y:Y, z:Z, ...}`.

## Tables and Databases? Defer.

I'm interested in supporting relational systems. A table might be expressed as a list of tuples paired with a header. A database is essentially a collection of tables and computed views. We could model a base database as a dict of tables, and the computed views as a mixin.

I'm uncertain what syntax to support here. But it's something we can explore easily with macros. 

        @table.create Cities
            name: text, primary_key
            lat: number, range(-90, 90)
            lon: number, range(-180,180)
        @table.insert Cities
            "San Francisco" 37.7 -122.4
            ...

Anyhow, I'll put this off for now. Perhaps macros will prove sufficient in practice.

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

        anno 'error Expr        recognized errors
        anno 'tbd Expr          incomplete definitions

In these cases, `Expr` may indicate the nature of the error or future intentions for a TBD. 

## Pipes

Borrowing F#'s syntax here:

        f <| arg = f arg
        arg |> f = f arg

I propose to also support directional function composition:

        f >> g = \ h -> g (f h)     
        g << f = f >> g

Ideally, we arrange precedences such that we can write stuff like:

        .op1 >>= f >> g >> .op2 >>= h >> .pure
        .op1 >>= (f >> g >> .op2) >>= (h >> .pure)

## Koru-Style Events

The Koru language is entirely designed around metaprogramming, and I believe its best ideas can be adopted here.

## Booleans

I propose to encode Boolean values, true and false, as the concise atoms, `'t` and `'f` respectively. There are no implicit conversions, no 'truthy' values. 

I propose to use keywords `and, or, not` for boolean composition. (IMO, `&&` and `||` begin to feel like line noise, and `!` should not be wasted on flipping a bit.) We can treat `and` and `or` as infix operators.

Although I have a vision for pattern matching, we'll support the conventional and familiar `if Cond then A else B` expressions, and also the `A if Cond else B` variant, which is more convenient in cases where we want to emphasize the operation over the condition. These are limited to pure boolean expressions. As a special case, in context of `do` notation, users may write `if Cond then A` or `A if Cond` without an `else` condition (defaults to return unit). 

## Type Annotations (Defer)

For now, we won't have any built-in syntax for types, just leave this to macros and reflection. But the general idea is to use `foo_type` as the type of `foo`, then separately use a reflection task to verify consistency. Type systems and type checkers are ultimately user-defined reflection tasks.

## Modules

Need a syntax for `import`, `include`, `source`

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

For concision, names within an object are implicitly local, i.e. `foo` refers to object `self.foo` and `_foo` to object `base.foo`. Instead, users pay extra to reference names in the module or host scopes, e.g. `module.name` or `^name`. Note that `^name` is required even when the object does not shadow `name`. 

*Aside:* Users may compose `^^^_name` to refer to the host's host's host's base `name`, but it's recommended to stick with at most one `^name` and avoid composing with `_name`. 

This design encourages a more 'complete' object namespaces, integrating content from module scope. 


I'd like to avoid verbose names within specs, including `self.*`. Instead, we might invert names, make access to the module scope more expensive, e.g. requiring `module.name` or perhaps a generic `^name` or `~name` to access the host in a hierarchical specifications. The internal name then does not need a prefix, and we can use `foo` or `_foo` within a spec to concisely refer to `self.foo` and `base.foo` respectively.

We'll provide some convenient syntax for specifying objects. Unfortunately, it's a little awkward to override the specification as a whole. I propose instead that the syntax for specifications implicitly introduces `foo_spec` and `foo`. We implicitly introduce `foo.spec = foo_spec` as an implicit final mixin. We can finalize the instance to guard against accidental updates.

The specification will need only a few elements, names tbd:

- mixin with defs
- inheritance list
- unique identifier

We can introduce some syntax for efficiently extending (overriding) a prior specification, but it's awkward to extend this to the inheritance graph. Users can override spec components, including inheritance lists, via `foo_spec.* := ...`.  

Some challenges:
- unique identifiers for specs for multiple inheritance
- access to base and self, default names? explicit parameters?
- overriding methods
- semantics: is Spec interface introduced as final mixin or via Base? Leaning towards implicit final mixin before instantiation.

## Annotations

We broadly have two kinds of annotations: 

- Namespace annotations by associative naming conventions such as `foo_type` or `final_foo` are open to extension, but limited to annotation of names. These are generally implemented via reflection tasks. The front-end compiler could install an associated reflection task for each feature.
- Term annotations as a 'flavored' identity function, e.g. `anno Anno Term`. The assembler evaluates and recognizes `Anno`, does something (perhaps with `Term`), then returns `Term`. Observable behavior of `Term` must be invariant, but annotations may influence representations (e.g. list to array, accelerate function) and forbid some observations.

Users may define reflection tasks in `refl.*` within a namespace or object instance. Named reflection tasks are convenient because we can easily debug or disable by name, which aligns with extensibility goals. The compiler shall arrange for these tasks to run at an appropriate time, i.e. after overrides, but before definitions are accessed. This may involve assembler support via term annotations.

Anonymous reflection tasks are expressed as term annotations. Relevantly, the assembler shall recognize `eff:(\api -> ...)` as an effectful annotation, and the provided API shall support both reflection and a few annotation-specific features such as loading term, continuation, call-stack, or applying more annotations to `Term`.

Effectful annotations are convenient because we can write `anno (.log Message) Term` and have it be meaningful. It's very extensible even without tags other than `eff`. But we might support other tags where convenient, e.g. for acceleration or error values.

## Pattern Matching

View patterns permit more than one match, however.

I want to desugar all pattern matching to monadic expressions, and I also want to support transactional backtracking conditionals by default. Support for 'what-if' pattern matching is simply very convenient.


Although we could support Haskell-style `match Expr with (Pattern -> Outcome)+` syntax, providing the pure handler, it's a little awkward to extend this syntax for effectful patterns, and it may be better to integrate the 'Expr' into the Pattern, allowing for more than one (e.g. as guards). I'm contemplating alternative syntax, e.g. based on unification or `Pattern = Expr` structures. We could feasibly integrate pattern matching into monads in general.

## User-Defined Types?

I would like to support lightweight declarations of type constructors and matching patterns.

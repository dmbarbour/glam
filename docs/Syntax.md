# Initial Syntax

This document describes an initial syntax for ".g" files, and design motives for it. Design goals include:

- a syntax that I find pleasant to work with
- supports an assembly programming look and feel
- concise, vertical columns of assembly mnemonics
- generalist, not specialized for targets or domains
- extraordinarily high abstraction ceiling

## Language Version Declaration

Reproducibility requires that the same sources produce the same outcome, but there is an implicit condition: an outcome is produced. A language version declaration simplifies reproducibility because the compiler can fail fast, refusing to produce any outcome rather than a result that drifts as the compiler is updated.

Proposed syntax:

        (lang|language) (BaseVer) (with Extensions)?
        language g0 with utf8

The version declaration should be the first toplevel declaration in a ".g" file. The BaseVer is a recognized string for a package of features. Extensions may modify that package, but the parser for extensions may also depend on BaseVer.

In practice, if a compiler halts on language version, we must be using different executable or configuration (if configuration defines `conf.env.lang.["g"].compile`) from development conditions. Users can resolve this by reproducing the executable (e.g. via nix) or by defining compatible compilers in the module system.

## Character Set

We'll start with printable ASCII and whitespace (0x21-0x7E, SP, CR, LF). It is not difficult to extend to UTF-8, though I'm concerned about legibility. We'll recognize CR, LF, and CRLF as line endings. The compiler shall emit a warning if the file uses inconsistent line endings.

## Comments

We'll support Python-style line comments, i.e. `#...` to end of line. There are no multi-line comments. An editor with vertical selection is recommended if users intend to comment out large sections of code. Comments are treated as whitespace by compiler and macros, but may be structured for purpose of external tooling (literate programming, projectional editing, extracting API docs, etc.).

## Toplevel Structure

The module toplevel consists of a sequence of 'declarations'. Each declaration starts a new line. If a declaration requires more than one line, any continuing lines (excepting blanks) must be indented by at least one space. Special exception: a final line consisting entirely of `}])` characters and whitespace does not need to be indented. The goal is to simplify error isolation, local reasoning, and parallel processing of declarations.

Each declaration starts with either a keyword (such as `import`, `spec`, or `unique`) or is a basic definition of form `name = Expr` or one of its variants (args in lhs, `:=`, `::=`, etc.). We'll favor basic definitions where feasible, thus keywords are mostly for special forms.

In context of errors, the errors can be reported but we can also make a best effort to proceed with errors. This might depend on configuration options or command-line arguments.

## Keywords

Keywords are names reserved by the compiler to support special forms or aesthetics. Users are not permitted to define or use keywords directly as names. An exception is made for atoms. For example, although `import` is a keyword, users may define `.['import] = ...` or reference `module.['import]`.

Proposed keywords:

        import, as                                  modules
        module, abstract, using                     namespace
        unique, abstract_path                       access
        with, without                               dict 
        do                                          effects
        let, in, where                              locals
        object, extend(s), self                     objects

        if, then, else                              basic conditionals
        match, try, when, try_match                 advanced conditionals
        and, or

The set of recognized keywords may vary per module based on language version declaration.

## Names and Paths

We accept a subset of C names, mostly restricting use of underscores. A viable regex:

        Part = [a-zA-Z][a-zA-Z0-9]*
        Name = Part('_'Part)*
        Path = Name('.'Name)*

Namespaces are modeled as hierarchical dictionaries, accessed via dotted path, e.g. `foo.bar.baz`. To index the dictionary, we translate each names into an atom, i.e. name `foo` translates to atom `'foo` (see *Atoms*), which is used as a key for a dictionary. Users may similarly quote path suffixes into lists, e.g. `'.foo.bar.baz` evaluates as `['foo, 'bar, 'baz]`. 

In the general case, we also support expression-indexed paths using `.(ListExpr)` or `.[...]` for a literal list. These indices are interpreted such that `.([1, 'two] ++ [3])` is equivalent to `.[1].two.[3]`. The empty list is permitted, e.g. `foo.[]` is equivalent to `foo`, and `foo.[ ].bar` admits spaces, newlines, and comments in names if needed.

Best practice is to avoid expression-indexed paths in module or object namespaces, but it's available as an escape hatch for integration. Users may define `.[Idx] = Def` at the module toplevel. Later access to this name requires `module.[Idx]`. Users may understand `module` as a keyword that aliases the module toplevel namespace, and `self` as the current object namespace (`self` aliases `module` at toplevel to simplify macros).

### Introductions and Overrides

When defining names, we'll distinguish introductions versus overrides. An introduction `name = Expr`. An override uses `name := Expr` or `name ::= Update` (rewrites to `name := (Update) _name`). It is an error to introduce a name that is already defined, or to override a name that isn't already defined. This resists ambiguity issues, i.e. a name is introduced with some intention or purpose, and overrides should preserve purpose.

In context of overrides, it is often necessary to reference the prior definition. The `::=` form supports access to prior definitions without repeating names. But for the more general case, I propose a `_` prefix on names, i.e. `_foo` refers to the prior definition of `foo`. Similarly, use of `_module` refers to prior module namespace, or `_self` to the prior object namespace.

*Note:* Deleting definitions isn't recommended or syntactically supported. Better to allocate a fresh, hierarchical module or object namespace then build monotonically. But if users insist `.[] ::= \ prior -> prior without foo` would do the trick. 

### Abstract Definitions

To localize errors, and to simplify analysis of name shadowing, names in use shall be defined or declared. I propose a lightweight declaration for names that we assume to be provided externally:

        abstract Name(, Name)*

Essentially, these declarations build a list of toplevel names that that compilers won't complain about being undefined locally. We don't bother with granularity below the toplevel name.

To share abstract declarations across includes, we'll represent them in our namespace. This might simply be defined in `meta.abstract_names` or similar. The compiler may introduce `abstract env` implicitly.

### Associated Names (Convention)

In many cases, we'll want to associate one name with another. The proposed convention is a dict named with an `_of` suffix. For example, given a name `foo`, we can also reference `type_of.foo`. The assembler ignores associated names, and I anticipate users mostly work with such names indirectly.

### Final Definitions (Convention)

In some cases, it is useful to guard against accidental updates to definitions. The most obvious example is to block accidental updates to an object instance because users should be updating the specification instead. But it's important to preserve the ability to update definitions regardless. 

To this end, we might use a pattern such as defining `final_of.foo = _foo`. We can assign a reflection task to verify.

### Forbidden Shadows

Name shadowing, where a function argument or local variable accidentally masks another name defined or declared in lexical scope, is a common source of subtle bugs. Humans are a lot more flexible about referential context than our compiler, thus easily overlook the error when reading code. To resist this bug, we'll warn on name shadowing by default.

There are a few special scopes such as `object` or `using` where we shadow *all* the names. In these cases, users must write `^name` (or `^(Expr)`) to escape the scope. But there is no fine-grained shadowing.

### Unused Locals

An unused local, e.g. a lambda or let var, will report a warning, but this may be suppressed by use of `_name` when introducing the local.

        # assume foo undefined, bar defined
        let  foo = 42 in foo    # ok (basic use case) 
        let  foo = 42 in bar    # warns (unused foo!)
        let _foo = 42 in foo    # ok (no '_' in rhs!)
        let _foo = 42 in bar    # ok (error suppressed)

Motives for the `_foo` form include TBD code or if it's unclear whether macros will use names. 

Users may also write just `_` if they know a value won't be used, e.g. `skip _ y = y`. This is much less useful in context of `let _ = 42 in bar`, but still valid.

### Module Metadata

In some cases, compilers need to thread some state through toplevel imports, e.g. to track names declared `abstract`. To support this, the compiler will store such metadata under `meta.*` within the module. This is also visible to users, e.g. macros can support similar features or as an interaction surface with the compiler. Alternatively, we could use a name that is difficult to write. But it seems better to just be open about it.

### Reflection Tasks

The compiler will arrange to automatically run `refl.*` definitions as reflection tasks. The assembler doesn't interpret `refl.*` implicitly, so this arrangement must be expressed as a compile-time effect or (very awkwardly) a term annotation. 

### Using Scopes

        using Dict in Expr
        using Dict do Body      # short for `using Expr in do Body`

Evaluates `Expr` in context of a temporary object. Within that scope, `self` is equivalent to `Dict`, `_self` is equivalent to `{}`. Users escape the scope just as they do for objects, via `^name` (or `^(Expr)`). *Note:* `Dict` doesn't need to be a valid object. 

The main use case for `using` is to manage namespaces without polluting them. Not suitable for subexpressions that require many escapes.

## Operators

Operators are essentially infix functions. We'll support Haskell-style operator sections, such that `((>>= k) op)` is equivalent to `(op >>= k)`. To avoid unnecessary parentheses, we'll support precedence between most operators. To mitigate confusion, not every pair of operators will have valid precedence, e.g. cannot mix both `>>` and `<<` without parentheses.

We may support a few special non-binary forms, e.g. `(x < y =< z)` as shorthand for `((x < y) and (y =< z))`. We'd also support `(< =<)` operator sections. Risk of confusion is mitigated because we cannot compare booleans for less-than or greater-than.

Operators may support limited ad-hoc polymorphism. For example, `>` will only compare two numbers, two lists, or two tuples. For lists and tuples, we use a lexicographic comparison of elements. Comparing a number to a list, or even a list to a tuple, would simply diverge with an error. As a rule, ad-hoc polymorphism must preserve laws or intuitions, e.g. don't use `+` to append lists because it does not preserve commutativity of `+` on numbers.

*Tentative:* Minimal operator precedence, mostly for associative structure. Require parentheses everywhere else.

### Application

Application is essentially expressed as a special whitespace 'operator', i.e. `f x` applies `f` to `x`. The compiler supports some ad hoc polymorphism for application:

- functions via lambda or interaction net
- method objects, `{apply:f,_} x = f x`
- lightweight effects, `(eff:f) x = eff:(\api -> f api x)`

This is a compiler feature: it does not implicitly extend to other languages or definition of interaction nets. We'll generally model advanced features (multimethods, keyword args, hooks for observability, etc.) via method objects. 

## Effects

We'll adopt Haskell's do notation. For aesthetic reasons, we'll support both `Pattern <- op` and `op -> Pattern`. The latter is a lot more convenient for vertical columns of assembly mnemonics. We support `Pattern = Expr` (no `let`). Pattern matching either captures locals or evaluates to `.fail`. 

Lightweight effects are supported: we desugar `.name` to `eff:(\api -> api.name)`, and we support application `(eff:f) x = eff:(\api -> f api x)`. This enables us to work with APIs concisely without redefining things:

        my_loop = do
            .movl 'eax ['ebx, 4]
            ...

Aside from do notation, we'll support the `>>=` composition and `>=>` Kleisli composition, and `=>>` for dropping a unit result.

### Recursive Do

Use of fixpoint within do notation is not implicit. It's problematic for it to be implicit because it easily conflicts with shift-reset and features that build upon it. Instead, we'll forward-declare the names we need via `abstract`.

        do
            abstract foo
            ... wire foo up, but don't observe foo ...
            op -> foo 
            ... at this point foo is no longer abstract ...

The compiler will leverage `.fix` to capture the name. Haskell already does this with `mdo`, so we can reference that implementation.

### Applicatives

I propose `!>` and `<!` to support applicative style programming. These correspond to Haskell's `<**>` and `<*>` respectively. I despise Haskell's choice of syntax here. Note that `!>` and `<!` correspond to `|>` and `<|` for pure functions. 

        (!>) : Eff a -> Eff (a -> b) -> Eff b   # right associative
        (<!) : Eff (a -> b) -> Eff a -> Eff b   # left associative

We always 'run' these effects from left to right, preserving order. 

Because `.r` is concise, users can directly write `.r f <! op1 <! op2`. No need for a `<$>` equivalent.

## Macros

In context of lazy loading, the compiler must *know* when a macro is being called. Two approaches: declare macros, e.g. a set of macros in `meta.*`, or a distinct invocation syntax. I favor the latter because it also lets readers locally recognize special forms. Proposed syntactic forms:

        @(Expr)                 general form
        @macro_name             short for @(_module.macro_name)

The `@macro_name` form is preferred for user-defined macros, but `@(Expr)` form is more general. `Expr` must evaluate as an effect. The compiler provides an effects API to read and write source at flexible levels of abstraction (text or AST). Macros are parameterized in terms of effectfully reading their parameters, e.g. in `@foo arg1 arg2` we expect `@foo` to read its arguments.

There are several structural constraints enforced by the API: 

- macros cannot escape their scope (brackets, braces, parentheses, indentation)
- macros cannot partially read or write an embedded text, whole chunks only
- macros cannot read other macro invocations (instead, awaits macro output)
- macros cannot read comments or count whitespace (whitespace is stretchy)

These are enforced by restrictions on readers and writers, e.g. all reads or writes of parentheses are balanced pairs, and the `#` and `@` characters are processed before macros ever see them. Regarding flexible abstraction, macros may *write* a lazy thunk as an abstract embedded data AST node, which provides a simple means to move data from compile-time to assembly-time.

There is no dedicated syntax for defining macros. It is convenient to define macros within objects: we need `_names` for inheritance and extraction, but we can use normal `name` internally to the object. With careful naming, we can also support an acceptable aesthetics without extraction, e.g. `@table.create`. Eventually, `@macro.rules` might help users define macros directly.

*Aside:* Most conventional use cases for macros evaporate between lazy evaluation and first-class effects, but we still benefit from embedded DSLs or abstracting namespace boilerplate.

## Annotations

We'll express annotations as a builtin function.

        import 'anno
        anno : Annotation -> Term -> Term

Annotations are not observable within the computation, but may guide performance, debugging, and other use cases. To avoid silent degradation of performance or reasoning, the assembler shall warn about unrecognized annotations. 

Effectful annotations of form `eff:(\api -> ...)` should receive access to the reflection API. The assembler will run it before returning the associated term. The reflection API should receive relevant context, e.g. associated term and continuation.

## Local Definitions

We'll support Haskell-style locals. 

        # basic let-in form for one-liner
        let Name = Def in Body

        # the Body itself may continue a one-liner 
        # but Name and Def must fit inline
        let Name = Def in this is a large Body expr
          and requires multiple lines

        # common multi-line form does not use 'in'
        # Body indentation must align with 'let'
        let Name = Def
        Body

        # continue large Def by indentation past Name
        let Name = This is a very long definition and it
              continues on the next line past the name
        Body

        # semicolon separator for multiple names inline
        # each group of names is mutually recursive
        let Name1 = Def1; Name2 = Def2 in Body

        # line separator for multiple lines of definitions
        let Name1 = Def1
            Name2 = Def2
        Body

        # the 'where' form is essentially a post-hoc 'let'
        Body where Name1 = Def1; Name2 = Def2

        # multi-line version
        Body where 
          Name1 = Def1
          Name2 = Def2
          Name3 = This is a very long definition and it
            continues on the next line past the name

Aside from `let` and `where`, locals can be introduced by pattern matching. See *Conditionals*.

## Tagged Data

Tagged data is modeled as singleton dictionaries. In these cases, braces may be omitted.

        tag:Data            # same as { tag:Data }
        :tag                # same as (\ Data -> tag:Data)

This extends to computing singleton paths, further eliding the '.' prefix.

        [TagExpr]:Data      # same as { .[TagExpr]:Data }
        :[TagExpr]          # same as (\ Data -> [TagExpr]:Data )

Tagged data is essentially just a syntactic convenience. 

## Atoms

Atoms are data where the only useful observation is equality.

The unit value `()` is a built-in atom. `'name` is sugar for a tagged unit value, `["name"]:()`. Tagged unit data effectively serves as an atom because we cannot observe `"name"`, we can only test whether it is present. Note that `'tag` and `tag:()` are distinct: the latter is equivalent to `['tag]:()`. Atoms of the `'eax` form are convenient for expressing small enums.

Scope-unique atoms are useful for the ephemeron performance pattern. To support this pattern, we can introduce a term annotation, `anno 'scope_unique`, that wraps a given atom with unique metadata. If ever we compare the same atom with different metadata, we diverge instead, thus never observing the violation of scope uniqueness. When used as dict keys, we associate data to a weakref of that metadata.

For access control and conflict avoidance, we can leverage the namespace as a stable source of unique atoms. A viable approach is `Foo = anno 'scope_unique (abstract_path Foo)`, borrowing `spec.name` as a seed for uniqueness. To resist accidental reuse, I propose a toplevel declaration `unique Foo, Bar, Baz` to 'introduce' such definitions, i.e. we see an error when the name is reused. In case `spec.name` is not globally unique, we also annotate scope-uniqueness, so we see an error upon observing conflict.

## Dicts

In expression contexts, `{}` is the empty dictionary, and `{ Path1:Expr1, Path2:Expr2, ...}` expresses a literal dictionary. Note that computed paths require the `.` prefix, e.g. `{ .[0]:A, .[1]:B }`.

Within a dictionary, `{}` serves as the 'undefined' value. For example, `{foo:{}}` is equivalent to `{}`. Only a finite subset of dictionary elements may be defined. In general, we can compose dictionaries: `{ D1, D2, D3 }` is a hierarchical union of three dictionaries. For example: `{{foo:{bar:0}}, {foo:{baz:1}}}` evaluates as `{foo:{bar:0, baz:1}}`. However, it is an error the dictionaries share any defined elements.

Multi-line literal dictionaries accept a leading comma for convenient line-editing, consistent with lists:

        {
        , name1:Expr1
        , name2:Expr2
        ...
        }

Dictionaries have access to a `with` syntax for definition-style updates. This supports explicit overrides. The `as with` variant also supports mutually recursive definitions.

        {} with
            name1 = Expr2
            name2 = Expr2

        Dict as d with  
            x := _d.x + 1   # prior d.x
            y =  d.x + a    # final d.x

We can also use `Dict as self with Body` for the full 

        Dict as self with
            x := _x + 1
            y = x + ^a

The compiler reserves key `spec` to distinguish dictionaries and objects. It's an error for dictionary literals or `with` syntax to define `spec`. Objects expose final `o.spec` but never prior `_o.spec`. The `with` syntax handles objects differently, updating spec then rebuilding lazily.

We also have a special form to remove elements:

        Dict without x, y, z
        Object without spec, x, y, z

We can also remove elements via update to `{}`, but the `without` form has two advantages: we don't need to consider whether `x` is already defined (explicit overrides), and `without` is the ony way to remove `spec`, thereby converting object to dictionary.

Pattern matching on dictionaries generally have the form `{Path1:Pattern1, Path2:Pattern2, RemainingPattern }`. There is at most one remaining pattern, default `{}` thus requiring a full match. Users may write `{:x,:y,:z}` as shorthand for `{x:x, y:y, z:z}`. 

*Aside:* Users can bypass syntactic guardrails via prelude of dict methods.

## Embedded Texts

Proposed syntax:

        "inline text"

        """
        " multi-line texts have the form:
        "
        "  """  <- three quotes followed by newline
        "  " <- one SP before text; optional for empty lines
        "
        "  # comment and blank lines are permitted and erased
        "  " lines are *separated* by LF, i.e.
        "  "   - no implicit final LF
        "  "   - LF even if source uses CR or CRLF
        "  """ |> postprocessing here is convenient
        " 
        # consistent indentation of '"' is NOT optional
          # but that does not extend to comment lines
        """

Texts concretely translate to binaries, using ASCII encoding (or utf8 under some extensions). There are no escape characters, i.e. texts are raw and postprocessing is explicit. If users want to embed a binary, that might be expressed as something like:

        """
        " 74686572 65206973 206E6F20 68696464 
        " 656E206D 65737361 67652C20 6A757374
        " 20612073 696C6C79 20657861 6D706C65
        """ |> hex2bin

In practice, it is terribly inconvenient to maintain large embedded texts, much less embedded binaries. Instead, leverage the module system to import file binaries:

        import "MyFile.md" binary as my_file

This enables users to use conventional tools to edit and maintain the text.

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

We use a prefix underscore to indicate negative numbers. This is part of the number literal, not a separate operator. Internal underscores between digits (i.e. digit on both sides) existing only to enhance legibility for humans. Decimal floating point or scientific notation can be encoded directly using an 'e' separator for the exponent.

        0xc0de
        0b10010_00110100_11111110_11011100

We'll support hexadecimal (0x) and binary (0b) number literals, too. We can feasibly provide some 'bitwise' operators or accelerated functions on natural numbers. Although numbers don't have a built-in notion of word size or encoding, it isn't difficult to impose one.

The compiler will provide a few useful operators - `+ * / -` and a prelude to work with numbers.

Numbers are modeled as exact rationals with no bound on size or precision. Thus, any loss of precision is under user control. This has severe performance implications. If users ever need high-performance assembly-time number crunching, they'll be relying on accelerated evaluation of CPU or GPGPU DSLs instead of built-in arithmetic.

*Aside:* We should model any non-trivial math libraries via embedded DSLs. This enables us to evaluate at assembly-time, interpret abstractly, or generate machine code.

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

*Notes:* 
- optional values are represented as `[A]` vs. `[]`
- favor effects to construct big lists, never literals

## Tuples

        (a,b)       tuple:[a,b]
        (x,y,z)     tuple:[a,b,c]

A tuple is essentially list with different connotations. Lists tend to be variable-size but homogeneous. Tuples tend to be fixed-size but non-homogeneous. We append lists, but we tend to simply construct or match on tuples inline. There is no dedicated syntax for empty or singleton tuples, but users are always free to use the `tuple:[a]` syntax.

Tuples are concise, but they negatively impact extensibility and scalability. This is mitigated by ad hoc polymorphism, e.g. we can easily match both `(X,Y,Z)` and `{x:X, y:Y, z:Z}` within a context. But, in practice, it's best to use tuples only for private intermediate representations or stable public interfaces.

## Tables and Databases

One way to maintain tables is to simply import from a database in a file:

        # assuming env.lang.["db"].compile
        import "MyData.db" as my_db

        # alternatively, postprocess
        import "MyData.sqlite" binary as my_sql
        my_sql ::= lazy_sqlite  # in place rewrite

Consequently, we don't need embedded tables for embedded data. 

Embedded tables are still necessary when elements include functions or objects, e.g. a dispatch table for multimethods. In general, I highly recommend modeling table objects within database objects. Each table object may have its own metadata, indexed views, reflection tasks to check 'foreign keys' in other tables. The database object ensures we can update (extend) or fork (inherit) whole databases cohesively. We can also model some tables as computed views within the database object.

We can update such tables manually.

        extend my_db with
          extend my_table with
            data ::= \ prior -> prior ++ using ^^self in
              [ # (col1, col2, col3)
              ,   (  42,   53,   f1)
              ,   (  54,   72,   f2)
              ]

The above form is awkward, tedious, and error prone. Tables would benefit from dedicated syntax, but I'm unwilling to commit to any at this time. Instead, I recommend developing embedded DSLs. Perhaps something closer to:

        @table.insert my_db/my_table do
          .h 'col1 'col2 'col3
          .r    42    53    f1
          .r    54    72    f2

## Functions

There are two ways to express functions: lambdas and interaction nets.

### Lambdas

We'll adopt Haskell's use of `\` for lambdas.

        \ x y z -> Expr
        \ x -> \ y -> \ z -> Expr

We'll also support Haskell-style `name args = ...` as a syntactic sugar. 

        name = \ x y z -> Expr
        name x y z = Expr
        name x y z := Expr
        name x y z ::= Update       # name := \ x y z -> (Update) _name

Unlike Haskell, there is no support for pattern matching on lambda or definition arguments. 

### Interaction Nets

Interaction nets are expressed effectfully and constructed via builtin `interaction_net`. Effects API is detailed in the *Design* doc. Eventually, we may want a macro DSL or user-defined syntax to support direct expression of inets. 

## Errors

We can use annotations to indicate known errors or issues.

        anno 'error         recognized errors
        anno 'TBD           incomplete definitions
        anno 'deprecated    transitional code

In these cases, `Expr` may indicate the nature of the error or future intentions for a TBD. For deprecated code, it should be valid, but we'll report a warning.

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

We'll generally forbid mixing right-pipes and left-pipes without explicit parentheses.

## Modules

Modules are primarily accessed by `import` declarations.

        import ModuleRef (binary)? ((as|at) Name)? (from RemoteRef)?

### Local Modules

We optimize for loading local files:

        import "Foo.g"          # integrate with current namespace 
        import "Bar.g" as b     # 'as' for default introduction 
        import "Baz.g" at b     # 'at' to extend the existing 'b'
        import "A/B/C.g"        # access to subfolders

### Remote Modules

Remote modules are indicated by a 'from' field with a revision hash.

        import "Qux.g" as q from {
            , rev:Text          # hash of folder content or revision history
            , search:[
                , tag:Text      # to help filter downloads
                , url:Text      # main search
                , url:Text      # backups
                ] 
            }

### Binary Resources

Sometimes we just want the raw data. 

        import ModuleRef binary as Name (from RemoteRef)?
        import "MyData.csv" binary as csv

### Builtins

The compiler provides built-in definitions via built-in modules. These are essentially local modules, but the naming convention uses atoms instead of filepaths.

        import 'prelude as p
        import 'trig as t

For reproducibility, built-in definitions must be stable. But we can introduce new builtins a lot more easily than keywords. Builtins should be favored over keywords for anything that can be expressed as a normal function or value. 

### Access Control

There is no notion of export control. That concept conflicts with my extensibility goals and with modules-as-mixins. However, we can easily invert this to explicitly distinguish public interfaces. For example, libraries may define a public `api.*` intended for integration into `env.*`. 

        import "MyFooLib.g" as libfoo
        env.foo = libfoo.api
        libfoo.internal_method := ...

Controlling what subprograms observe is useful for local reasoning. It starts with hiding a few definitions, but 

## Objects

Object syntax can and should be compact by default. I propose:

        # declaration
        object foo extends bar, baz with
            def1 = ...
            def2 := ...

        # desugars as expression
        foo = object (abstract_path foo) extends [bar, baz] with 
            def1 = ...
            def2 := ...
        
        # roughly evaluates as
        foo = __instance {
            , name:(abstract_path foo)
            , deps:[bar.spec, baz.spec]
            , defs:\_self self -> _self with  
                def1 = ...
                def2 := ...
            }

        # general declaration format
        object Name (as Name)? (extends Name(, Name)*)? (with Body)?

        # general expression format
        object (NameExpr|_) (as Name)? (extends ObjectList)? (with Body)? 

To improve concision, expressions within objects are localized. That is, we bind `foo` as `self.foo` and `_foo` as `_self.foo`, where `self` is a keyword referencing the local object namespace, analogous to `module`. Users instead pay a small syntactic tax to access the host scope via `^name`, `^(Expr)`, or use of `module`. Use of `^` composes, e.g. `^^^method` escapes three lexical levels. But it's best to keep syntax shallow.

The `extends` and `with` sections are optional, with `spec.deps` and `spec.defs` respectively defaulting to the empty list and const function (`\x _ -> x`). If provided, they cannot be empty. In general, `spec.name` may be any value with equality, e.g. `"foo"`. Toplevel object declarations use `abstract_path` to ensure globally unique names, but it's sufficient that we don't reuse a name for two different specs across transitive deps.

To instantiate the object, the compiler applies a linearization algorithm (C3?) to deduplicate and merge components. The compiler uses `spec.name` to distinguish specifications, and asserts (via reflective term annotation) that `spec.name` is not used for two different specs in linearization scope. After specifications are ordered, we apply `spec.defs` to an empty base `{}` then finally introduce `spec` as an implicit final mixin. 

For consistency and convenience, the compiler exposes an instantiation function via built-in `__instance`, which attempts to build an object from any specification.

        extend foo with
            def1 := ...

        extend Name (as Name)? with Body        # declaration

We also have syntax `extend Object with ...`, which updates the specification then re-instantiates the object, preserving name and deps. This is declaration-only because it's usually a bad idea to preserve name while forking identity. Users can always bypass restrictions via `__instance`. But see *Anonymous Extension* below.

*Note:* For objects mostly used in prior form, inherit in prior form. For example, if using `_foo.op`, define `object foo extends _bar, _baz`. Otherwise, `_foo.op` generally refers to *future* versions of `bar` or `baz`. This is relevant for macros and a few other cases.

### Anonymous Objects

An anonymous object has no name, i.e. `spec.name` is intentionally left undefined. Users may express anonymous objects via `_` in name position, e.g. `object _ extends foo, bar with ...` (or minimally, just `object _`). Anonymous objects must not appear within `spec.deps`, otherwise `__instance` will report an error. Hence, anonymous objects do not participate in multiple inheritance. They may participate in mixin composition.

*Note:* In `object (NameExpr|_) ...`, it's an error if `NameExpr` evaluates to `{}`. We'll insist anonymous objects are explicitly indicated by `_`. 

### Abstract Objects

An abstract object has a full specification but the rest of the body missing, i.e. a singleton `spec:{name, defs, deps}`. This is be expressed via `abstract object ...`, as declaration or expression.

### Mixin Composition

Introducing operator `&>` (and its mirror `<&`). These operators compose two objects and return an anonymous object. We generally view this as `Obj &> Mixin` such that we 'apply' the mixin to the 'object' (consistent with `|>` and `!>`).

        toAnon o = 
            if (o.spec has name) then 
                abstract object _ extends o 
            else o

        O &> M = __instance mixed_spec where
            AO = (toAnon O).spec
            AM = (toAnon M).spec
            mixed_spec = { deps: mixed_deps, defs:mixed_defs }
            mixed_deps = AM.deps ++ AO.deps
            mixed_defs = mix AM.defs AO.defs
            mix c p = \ b s -> c (p b s) s

        M <& O = O &> M

A known weakness is that all definition updates from anonymous objects apply *after* all definition updates from named objects. This easily results in non-intuitive behavior regarding order of overrides. To mitigate, we'll warn if users apply named mixins to  anonymous objects. This ensures 'anonymous' is always at the end of the chain. 

### Explicit Scope

We also support an `as Name` modifier for object declarations and expressions. The default for `object` is `as self`, which is why we have the default local names and `^` escapes. In some contexts, it is more convenient to use a local name so we don't need escapes.

        object Name (as Name)? (extends ObjList)? (with Body)?              # object declaration
        object (NameExpr|_) (as Name)? (extends ListExpr)? (with Body)?     # object expression
        extend Name (as Name)? with Body                                    # extend declaration

For example:

        a = 1
        b = 2 

        object bar with 
            C = 3

        object foo as f extends bar with
            A = f.B + a
            B = f.C + b

In this context, `foo.A == 6`. Note that we do not need `^a` to reference the global `a`, but now we use `f.B` to reference the local `B`. To reference prior definitions, we'd use `_f.B` instead. 

### Anonymous Extension

The `with` and `as with` syntax for dict updates also works for objects, and is essentially equivalent to an anonymous mixin:

        Object with Body
        Object &> abstract object _ as _ with Body

        Object as Name with Body
        Object &> abstract object _ as Name with Body

This supports lightweight extensions

        foo = op1 >>= op2 >>= op3 with 
            A := 42
            B := op4 >>= op5 >>= op6 as o with
                C c = op7 ...
          where
            op1 = ...

        foo = op1 >>= op2 >>= op3a where
            op3a = op3 with
                ...

## Conditionals

I propose to model conditional behavior as effectful and backtracking, i.e. in terms of `.alt/.fail/.cut`. 

Boolean expressions become pass/fail effects, i.e. `.r ()` and `.fail`. This impacts all boolean operators, e.g. `(3 > 4)` evaluates as `.fail`, `or` is modeled via `.alt`, `and` via `.seq`. Negation can be expressed via staged effect:

        not C = .alt (C =>> .r .fail) (.r (.r ())) >>= \ op -> op
        could C = not (not C)

In this case, `could` will run `C` to prove it works, backtrack, then continue running. With just `.alt/.cut/.fail` there is no way to exfiltrate details about the success, other than the observation that it would have passed. 

### If Then Else

I support `if/then/else` for reasons of familiarity and convenience. We'll desugar as a match form.

        # basic forms
        if C then A else B
        A if C else B

        # in general, desugars as
        match when
            C -> A
            _ -> B

Note that conditions are not expressions. Instead, they're guard clauses, i.e. a sequence of `Guard (and Guard)*`. Relevantly, this admits pattern guards, which are often convenient, and effects guards, which can express branching conditions.

        if Pattern = Expr and a > b then A else B

Users are encouraged to switch to the `match` form rather than chaining `else if` many times.

### Try Variants

I propose to support a 'pure' `if/then/else` or `match` syntax in terms of the compiler providing a local effects handler implementing the stateless subset of standard effects (`.alt/.fail/.cut/.r/.seq/.fix`). Effectful variants `try/then/else` and `try_match` instead run in the host environment. They're invalid outside of an effectful context, but more flexible within it.

### Tentative Choice

Instead of confidently returning a result, we can extend the conditional into the result via `then?` (or `-?>` for `match`). 

        if C then? .r A else B          # same as if C then A else B
        if C then? .fail else B         # always returns B

The motive for this is to support refactoring of conditional *structures*. Not just patterns, but actually factoring chunks from the middle of a conditional pattern.

        # snip chunk from middle
        if C1 then E1
        else if C2 then E2
        else if C3 then E3
        else if C4 then E4
        else E5

        if C1 then E1
        else if _ then?
            # can now move this
            if C2 then .r E2
            else if C3 then .r E3
            else .fail
        else if C4 then E4
        else E5

The `then?` branch has access to the same effects as guard conditions, and must explicitly return the branch result or fail. This isn't convenient: we manually wrap `E2` and `E3` (with `.r`) and add `.fail` on the `else` branch. But it is possible, and it generalizes. 

### Match

I borrow a lot of inspiration from Haskell's syntax for `match`. Common use cases are basically the same.

        match Expr with
            Pattern1 -> Result1
            Pattern2 -> Result2 
            _ -> Result3

We also support branching guard clauses. We use `when` to separate the pattern from the a branching clause. base case, we have only one branch. But we'll need another `when` for hierarchical branching.

        match Expr with
            P1 when C1 -> R1        # basic
            P2 when                 # multiline
                C2_a -> R2_a
                C2_b -> R2_b
            P3 when
                C3_a when           # multi-level
                    C3_a_a -> R3_a_a
                    C3_a_b -> R3_a_b
                C3_b -> R3_b

If users don't need the pattern, they may just write `match when` instead. 

        match when
            C1 -> R1
            C2 when
                C2a -> R2a
                C2b -> R2b

Tentative choice is expressed using `-?>`.

### Guard Clauses

Several forms of guard clauses:

- `Pattern = Expr` - bind Pattern or reject match
- `Effect` - evaluates to `{eff:(_),_}`, executes
  - reject on `.fail`
  - accept on `.r ()` 
  - error on `.r Other` (implicit data loss)
- `Effect -> Pattern` or `Pattern <- Effect` 
  - reject on `.fail`
  - accept `.r Result when Pattern = Result`
- `_` - no-op pass, eqv. to `.r ()`

Guard clauses compose sequentially via 'and': `Guard (and Guard)*`. 

This supports both booleans and effects via ad hoc polymorphism, but note that we do not *mix* effects into a boolean expression. 

### Pattern Matching

Patterns offer a concise way of extracting data from similar structure.

        Name                        # bind as local name (permits '_' and '_Name' too)
        ()                          # unit
        (Pattern)                   # scoped pattern
        Pattern as Name             # capture pattern target as a local

        {}                          # empty dict
        {d}                         # any dict
        {:x,:y,:z}                  # same as {x:x, y:y, z:z}
        {foo.bar.baz:Pattern, _}    # deep refs
        {.(Expr):Pattern, _}        # eval list-path expr, extract, match Pattern

        tag:Pattern                 # same as {tag:Pattern}
        :tag                        # same as tag:tag
        [TagExpr]:Pattern           # same as {.[TagExpr]:Pattern}
        'name                       # a constant, same as ["name"]:()

        []                          # empty list
        [a,b,c]                     # list of three items
        [x]++xs                     # we can use append notation in patterns
        xs++[x]                      
        [x0]++xs++[xN]
        # lhs++rhs                  # ILLEGAL - limit one variable sublist

        "foo"                       # match text
        "foo"++xs                   # texts are just lists

        (P1,P2,...,PN)              # same as tuple:[P1,P2,...,PN]

        42                          # match exact number
        _1.23
        1/6                         # exact rationals supported

        (View -> Pattern)           # view patterns *must* be parenthesized
        (Pattern <- View)           
        (Predicate Pattern)         # predicate patterns (special view)
        (Pattern when Guard)        # local guards (to control order)

### View Patterns

View patterns enable us to spill patterns into the rest of the namespace. For example, if we want to insist a number is a nat, we could write `(Nat n < 100)` instead of `(0 =< n < 100) when (n == __floor n)` or similar. View patterns are expressed within a pattern context as:

        (View -> Pattern)     # or equivalently
        (Pattern <- View)

The primary difference between a view pattern and effectful guard clause is that, in the pattern context, we have an input other than the effectful environment. The viewer has access to the *same* effects as the guard clauses and tentative choice.

As a rule, view patterns apply before the pattern is matched. If users need a different order, use a guard.
*Note:* View patterns are an approach to refactoring *patterns*. In contrast, tentative choice is supports refactoring *conditional structures*. In practice, it's usually more convenient to refactor patterns. 

### Predicate Pattern

Predicate patterns are a specialized case of view patterns. The predicate is pass/fail. The value captured is not a computed view, but the original input. In most cases, the pattern is a name.

        (Pred Pattern)          # recognized by whitespace as op

        # examples
        (Nat n)                 # check if Nat, capture n
        (Prime n)
        (UTF8 text)
        (Prefix "foo-" text)    # only last arg is pattern

        # as a view pattern
        (p2v Pred -> Pattern)
            where p2v p x = do { p x; .r x }

Consistent with view patterns, we forward the argument to the inner pattern only on pass, i.e. the predicate runs first. If users want to run the predicate after the match, use `(Pattern as tmp when Pred tmp)` instead. 

## Loops

As with Haskell, we don't need keywords to support loops. Using *objects*, we can do even better, states in the loop object as method objects that we 'wire' together by overriding 'continuations'. But simple loops should be normal functions. Examples:

        # loop until step failure, backtrack final step
        untilFail Action s0 = .cut (.alt RunLoop EndLoop) >>= \ op -> op where
            RunLoop = Action s0 >>= \ s1 -> .r (untilFail Action s1)
            EndLoop = .r (.r s0)

        # foreach [1,2,3] \item-> do Body
        foreach L Action = match L with
            [x]++xs -> (Action x) >>= foreach xs Action
            [] -> .r ()

        untilDone s0 Action = match s0 with
            done:R -> .r R
            _ -> Action s0 >>= \ s1 -> untilDone s1 Action 

At least for now, I'll defer keywords for loops. But there are at least a few motives for syntax-supported loops: user familiarity, and tighter integration with pattern matching. Perhaps more if we could make loop objects syntactically convenient to work with. Will review later.

## Open Continuations

Assuming the *Anonymous Extensions* syntax for objects, we can support continuation-passing style via extension of abstract method objects. This is another way of passing parameters, more extensible and flexible than function arguments. Moreover, it shifts some parameters from horizontal to vertical layout, and avoids some redundancy of reference. The resulting syntax might look a bit like this:

        foo x y = op1 x >>= op2 y >>= op3 with
            A a = op4 x >>=\_-> op5 a >>= op6 as op with
                B := ... op.F ... 
                C c = ...
            D ::= \ prior -> prior + 42

I'm uncertain how useful this 'style' will be, but Koru language is essentially built around a restricted subset of this form of composition.


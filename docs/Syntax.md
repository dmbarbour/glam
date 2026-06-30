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

The version declaration should be the first toplevel declaration in a ".g" file. The BaseVer is a recognized string for a package of features, while extensions modify that package.

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

Keywords are names reserved by the compiler. Users are not permitted to define or use keywords directly as names. An exception is made for atoms. For example, given keyword `import`, users may define `.['import] = ...` or reference `module.['import]`. The set of keywords may vary with the language version declaration.

Proposed keywords:

        import, as                                  modules
        module, abstract, using                     namespace
        anno                                        annotations
        unique, abstract_global_path                special atoms
        with, without                               dict 
        do                                          effects
        let, in, where                              locals

        object, extend, extends, self, mixin        objects
        object_from_spec                            anonymous objects

        if, then, elif, else                        basic conditionals
        match, try, when, try_match                 advanced conditionals
        and, or, not                                comparisons

Keywords implicitly reserve `_keyword`, but it's only meaningful in a few special cases.

## Names and Paths

We accept a subset of C names, mostly restricting use of underscores. A viable regex:

        Part = [a-zA-Z][a-zA-Z0-9]*
        Name = Part('_'Part)*
        Path = Name('.'Name)*

Namespaces are modeled as hierarchical dictionaries, accessed via dotted path, e.g. `foo.bar.baz`. To index the dictionary, we translate each names into an atom, i.e. name `foo` translates to atom `'foo` (see *Atoms*), which is used as a key for a dictionary. Users may similarly quote path suffixes into lists, e.g. `'.foo.bar.baz` evaluates as `['foo, 'bar, 'baz]`. 

In the general case, we also support expression-indexed paths using `.(ListExpr)` or `.[...]` for a literal list. These indices are interpreted such that `.([1, 'two] ++ [3])` is equivalent to `.[1].two.[3]`. The empty list is permitted, e.g. `foo.[]` is equivalent to `foo`, and `foo.[ ].bar` admits spaces, newlines, and comments in names if needed.

Best practice is to avoid expression-indexed paths in module or object namespaces, but it's available as an escape hatch for integration. Users may define `.[Idx] = Def` at the module toplevel. Later access to this name requires `module.[Idx]`. Users may understand `module` as a keyword that aliases the module toplevel namespace, and `self` as the current namespace. At the toplevel scope, `self` and `module` are equivalent.

Special cases: 
- `.path` desugars to `eff:(\api -> api.path)` to support lightweight effects. See *Effects.*
- `^name` (and `^(Expr)`) binds to host scope in context of objects. See *Objects.*

### Introductions and Overrides

When defining names, we'll distinguish introductions versus overrides. An introduction `name = Expr`. An override uses `name := Expr` or `name ::= Update` (rewrites to `name := (Update) _name`). It is an error to introduce a name that is already defined, or to override a name that isn't already defined. This resists ambiguity issues, i.e. a name is introduced with some intention or purpose, and overrides should preserve purpose.

In context of overrides, it is often necessary to reference the prior definition. The `::=` form supports access to prior definitions without repeating names. But for the more general case, I propose a `_` prefix on names, i.e. `_foo` refers to the prior definition of `foo`. Similarly, use of `_module` refers to prior module namespace, or `_self` to the prior object namespace.

*Note:* Deleting definitions isn't recommended or syntactically supported. Better to allocate a fresh, hierarchical module or object namespace then build monotonically. But if users insist `.[] ::= \ prior -> prior without foo` would do the trick. 

### Abstract Definitions

To localize errors, and to simplify analysis of name shadowing, names in use shall be defined or declared. I propose a lightweight declaration for names that we assume to be provided externally:

        abstract Name(, Name)*

Essentially, these declarations build a list of toplevel names that that compilers won't complain about being undefined locally. We don't bother with granularity below the toplevel name.

To share abstract declarations across includes, we'll represent them in our namespace. This might simply be defined in `meta.abstract_names` or similar. The compiler may introduce `abstract env` implicitly.

### Associated Names

In many cases, we'll want to associate one name with another. The proposed convention is a dict named with an `_of` suffix. For example, given a name `foo`, we can also reference `type_of.foo`. The assembler ignores associated names, and I anticipate users mostly work with such names indirectly.

### Final Definitions 

In some cases, it is useful to guard against accidental updates to definitions. The most obvious example is to block accidental updates to an object instance because users should be updating the specification instead. But it's important to preserve the ability to update definitions regardless. 

To this end, we might use a pattern such as defining `final_of.foo = _foo`. Assign a reflection task to verify. 

### Forbidden Shadows

Name shadowing, where a function argument or local variable accidentally masks another name defined or declared in lexical scope, is a common source of subtle bugs. Humans are a lot more flexible about referential context than a lambda calculus, thus easily overlook the error when reading code. To resist this bug, we'll warn on name shadowing by default.

As a special case, we shadow *all* the names by default in `object` or `using` scopes. Instead, users write `^name` to escape the shadowing context. This sort of bulk shadowing seems easier for users to track. 

### Unused Locals

An unused local, e.g. a lambda or let var, will report a warning, but this may be suppressed by use of `_name` when introducing the local.

        # assume foo undefined, bar defined
        let  foo = 42 in foo    # ok, 
        let  foo = 42 in bar    # error (unused foo!)
        let _foo = 42 in foo    # ok (no '_' in rhs!)
        let _foo = 42 in bar    # ok (error suppressed)

Motives for the `_foo` form include TBD code or if it's unclear whether macros will use names. Users may also write just `_` if they know a value won't be used, e.g. `skip _ y = y`. This is much less useful in context of `let _ = 42 in bar`, but still valid.

### Module Metadata

The compiler will generally maintain metadata in `meta.*`. For example, maintaining a list or index of introduced names, a collection of abstract names, or a reverse-lookup index on names. This is also available to users, e.g. so macros can support similar features or as an interaction surface with the compiler. 

It's convenient for 'include'-style imports that compiler state is threaded, so just shove everything into `meta.*`. Avoid hidden state in the compiler.

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

Operators may support limited ad-hoc polymorphism. For example, `>` could compare two numbers, two lists, two tuples. For lists and tuples, we use a lexicographic comparison of elements. Comparing a number to a list, or a list to a tuple, would simply diverge with an error. As a rule, ad-hoc polymorphism must preserve laws or intuitions, e.g. don't use `+` to append lists because it does not preserve commutativity of `+` on numbers.

*Tentative:* Minimal operator precedence, mostly for associative structure. Require parentheses everywhere else.

### Application

Application is essentially expressed as a special whitespace 'operator', i.e. `f x` applies `f` to `x`. The compiler supports some ad hoc polymorphism for application:

- lambda functions, lazy, built-in, evaluate by substitution.
- method objects, `{apply:f,_} x = f x`
- lightweight effects, `(eff:f) x = eff:(\api -> f api x)` 

We'll generally model advanced features (multimethods, keywords, observability, etc.) via 'objects'. 

## Effects

We'll almost directly adopt Haskell's do notation. For aesthetic reasons, we'll support both `var <- op` and `op -> var`. Semicolons can serve as virtual line separators as needed. To support concise, convenient effects without polluting the toplevel namespace, I propose to desugar `.name` to `eff:(\api -> api.name)` and define application to work with effects: `(eff:f) x = eff:(\api -> f api x)`. 

Aesthetically, this should support a direct assembly programming style where we have a column of operations on the left and the occasional label tabbed out to the right. 

        my_loop = do
            .label                      -> loop_start
            .movl 'eax ['ebx, 4]
            ...

Aside from do notation, we'll support the `>>=` composition and `>=>` Kleisli composition, and `=>>` for dropping a unit result.

### Standard Effects

Effect names aren't keywords per se, but the compiler will assume several names for its do notation and operators. 

- `.seq, .r` - corresponds to `>>=` and `return`
- `.alt, .fail, .cut` - pattern matching, try
- `.fix` - recursive do

At the moment, the compiler doesn't touch state or delimited continuations. But as a convention, we might reserve:

- `.set, .get, .put, .take` - for indexed state
  - fail on get/set/take a path that doesn't exist, or put a path that does exist
  - in transactional contexts we can track precisely which paths are observed
  - empty path applies to whole state; can also take/put the whole state
- `.shift, .reset` - indexed continuations

### Recursive Do

Use of fixpoint within do notation is not implicit. It's problematic for it to be implicit because it easily conflicts with shift-reset and features that build upon it. Instead, we'll forward-declare the names we need. We already have a keyword for a similar role in scope of modules and objects: `abstract Name(, Name)*`. We can reuse that for do notation.

        do
            abstract foo
            ... wire foo up, but don't observe foo ...
            op -> foo 
            ... at this point foo is no longer abstract ...

The compiler will leverage `.fix` to capture the name. Haskell already does this with `mdo`, so we can reference that implementation.

### Applicatives

I propose `!>` and `<!` to support applicative style programming. These correspond to Haskell's `<**>` and `<*>` respectively. I despise Haskell's choice of syntax here. Note that `!>` and `<!` correspond to `|>` and `<|` for pure functions. 

        (!>) : Eff a -> Eff (a -> b) -> Eff b   
        (<!) : Eff (a -> b) -> Eff b -> Eff b

We always 'run' these effects from left to right. 

Because `.r` is concise, users can directly write `.r f <! op1 <! op2`. No need for a `<$>` equivalent.

### Alternatives

Direct use of `.alt` is a little awkward. We can introduce an inline operator for this, e.g. `<|>`. In practice, it's probably more convenient to just fork a list of operations.

        (<|>) = .alt

        foo = fork [
          , op1
          , op2
          , op3
          ]

        fork L = match L with
          [x] ++ xs -> match xs with
            [] -> x
            [y] -> .alt x y
            _ -> .alt x (fork xs)
          # fail only if initial list is empty
          [] -> .fail   


## Macros

In context of lazy loading, the compiler must *know* when a macro is being called. Two approaches: declare macros, e.g. a set of macros in `meta.*`, or a distinct invocation syntax. I favor the latter because it also lets readers locally recognize special forms. Proposed syntactic forms:

        @(Expr)                 general form
        @macro_name             short for @(_module.macro_name)

The `@macro_name` form is preferred, but `@(Expr)` form is more general. `Expr` must evaluate to an effect. The compiler provides an effects API to read and write source at flexible levels of abstraction (text or AST). Macros are parameterized in terms of effectfully reading their parameters, e.g. in `@foo arg1 arg2` we expect `@foo` to read its arguments.

There are several structural constraints enforced by the API: 

- macros cannot escape their scope (brackets, braces, parentheses, indentation)
- macros cannot partially read or write an embedded text, whole chunks only
- macros cannot read other macro invocations (instead, awaits macro output)
- macros cannot read comments or count whitespace (whitespace is stretchy)

These are enforced by restrictions on readers and writers, e.g. all reads or writes of parentheses are balanced pairs, and the `#` and `@` characters are processed before macros ever see them. Regarding flexible abstraction, macros may *write* a lazy thunk as an abstract embedded data AST node, which provides a simple means to move data from compile-time to assembly-time.

There is no dedicated syntax for defining macros. It is convenient to define macros within objects: we need `_names` for inheritance and extraction, but we can use normal `name` internally to the object. With careful naming, we can also support an acceptable aesthetics without extraction, e.g. `@table.create`. Eventually, `@macro.rules` might help users define macros directly.

*Aside:* Most conventional use cases for macros evaporate between lazy evaluation and first-class effects, but we still benefit from embedded DSLs or abstracting namespace boilerplate.

## Annotations

        anno : Annotation -> Term -> Term

To express term annotations, I propose keyword `anno`, referring to a built-in function that applies an annotation in context of a term, then returns the term. Annotations are not observable modulo reflection, but may guide performance, debugging, and other use cases. 

The assembler recognizes effectful annotations, of form `eff:(\api -> ...)`. The assembler provides the reflection API, runs the operation to completion, then returns the given term. Depending on the API, the effect may have limited access to term and continuation, e.g. to surgically apply more annotations. If the effect diverges, so does the `anno` expression.

The assembler (or compiler) may recognize other ad hoc annotations such as `accel:'.list.split`. To avoid silent degradation of performance or reasoning, the assembler shall warn about unrecognized annotations. For convenient composition, there shall be an effect to apply more annotations to the term.

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

Pattern matching is permitted in Name position in a few contexts, e.g. do notation and if-let form. See *Pattern Matching* for details. 

## Tagged Data

Tagged data is convenient for modeling extensible variants and detecting assembly-time type errors.

        # literal form
        # no whitespace around ':'
        tag:Data
        [TagExpr]:Data

        # function form
        # :tag = \Data -> tag:Data
        :tag Data
        :[TagExpr] Data

Tagged data is modeled as a singleton dictionary. That is, `tag:Data` is equivalent to `{ tag:Data }` in most contexts. An important exception is updates: tagged data is implicitly frozen, i.e. `anno 'freeze { tag:Data }`. This freeze forbids dict updates (`with`, `without`, `tagged_data.foo = ...`, etc.) unless the dict is thawed (via `anno 'thaw`). 

Unlike dictionaries, dotted paths are not supported for tags. You can write `foo:bar:baz:Data` or ` { foo:(bar:baz:Data) }` or `{ foo.bar:(baz:Data) }` and so on, and these are all the same value, but with different freeze annotations. 

Constructed tags are expressed using `[Expr]:Data`. Although this uses the list form, it's limited to a singleton list. The motive for square brackets is consistency with dotted paths, i.e. `(foo:Data).foo = Data` and `([Expr]:Data).[Expr] = Data)`. 

*Note:* Syntactically, tagged data binds tighter than application, e.g. `fn foo:bar baz` is equivalent to `fn (foo:bar) baz` instead of `fn foo:(bar baz)`.

## Atoms

Atoms are data where the only useful observation is equality.

The unit value `()` is a built-in atom. Tagged unit data, i.e. `tag:()` or `[TagExpr]:()`, serves as a symbolic atom. As shorthand, `'name` rewrites to `["name"]:()`. Note that `'tag` and `tag:()` are distinct atoms: `["tag"]:()` vs. `['tag]:()`. Atoms are convenient for expressing small enums, e.g. `'t` and `'f` for booleans, or `'eax` for registers.

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

Expression-indexed names need some special attention. Users are free to write `{ [0]:"Hello", [1]:"World" }`. In the definitional form, this becomes `{} with { .[0] = "Hello"; .[1] = "World" }` usually multi-line (where braces are unnecessary). 

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

        import "MyFile.md" as binary my_file

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
        import "MyData.sqlite" as binary my_sql
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

I propose to adopt Haskell's use of `\` for lambdas.

        \ x y z -> Expr
        \ x -> \ y -> \ z -> Expr

We'll also support Haskell-style `name args = ...` as a syntactic sugar. 

        name = \ x y z -> Expr
        name x y z = Expr
        name x y z := Expr
        name x y z ::= Update       # name := \ x y z -> (Update) _name

Unlike Haskell, there is no support for pattern matching on lambda or definition arguments. 

## Partial Functions

We can use annotations to indicate known errors or issues.

        anno 'error         Expr        recognized errors
        anno 'TBD           Expr        incomplete definitions
        anno 'deprecated    Expr        transitional code

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

Modules are integrated through 'import' declarations. Everything module-related is consolidated into one declaration. Imports may only appear as toplevel declarations.

        import Expr (as (binary)? Name)? (from Expr)?

        import "Foo.g"                      # without 'as' or 'at' is mixin on toplevel namespace
        import "Bar.g" as b                 # 'as' for default introduction or 'at' for a mixin
        import "BigText.txt" as binary t    # introduces t as a binary, i.e. does not compile t
        import "Qux.g" as q from {
            , rev:Text                      # content hash of containing folder
            , search:[
                , tag:Text                  # tag or branch name for precise cloning
                , url:Text                  # a remote lookup, uses most recent tag
                , url:Text
                ]
            }

In the vast majority of use cases, literal expressions are sufficient.

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
        foo = object (abstract_global_path foo) extends [bar, baz] with 
            def1 = ...
            def2 := ...
        
        # roughly evaluates as
        foo = object_from_spec {
            , name:(abstract_global_path foo)
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

The `extends` and `with` sections are optional, with `spec.deps` and `spec.defs` respectively defaulting to the empty list and const function (`\x _ -> x`). If provided, they cannot be empty. In general, `spec.name` may be any value with equality, e.g. `"foo"`. Toplevel object declarations use `abstract_global_path` to ensure globally unique names, but it's sufficient that we don't reuse a name for two different specs across transitive deps.

To instantiate the object, the compiler applies a linearization algorithm (C3?) to deduplicate and merge components. The compiler uses `spec.name` to distinguish specifications, and asserts (via reflective term annotation) that `spec.name` is not used for two different specs in linearization scope. After specifications are ordered, we apply `spec.defs` to an empty base `{}` then finally introduce `spec` as an implicit final mixin. For consistency and convenience, the compiler exposes an instantiation function via keyword `object_from_spec`, but it isn't anything special.

We also have asyntax `extend Object with ...`, which is analogous to `Dict with ...` but instead updates the specification then re-instantiates the object. As with the `object ...` this also may be used both as declaration and expression. 

        extend foo with
            def1 := ...

        extend Name (as Name)? with Body

In contrast to `object Name ...`, the `extend Object with` variant updates `spec.defs` then lazily rebuilds the object. There is no expression form for this, but see *Mixin Composition* and *Lightweight Extension* below. There is no dedicated syntax for updating `spec.name` or `spec.deps`, and I struggle to think of a use case, but users always have access to `foo ::= \ prior -> prior.spec |> edit_spec |> object_from_spec`. 

Objects support most toplevel declarations. Notable exceptions include `unique` and `import`. Objects do support hierarchical object declarations. In this case, names are generated based on host `spec.name` and path.

*Note:* It's usually an error to update an object as a dict, e.g. `foo.def1 := ...` instead of `extend foo with { def1 := ... }`. To resist accidents, `object_from_spec` shall implicitly `anno 'freeze` the returned dict. Unless users explicitly `anno 'thaw` the dict, regular dict updates diverge with error.

*Note:* If using objects statically, e.g. for macros or toplevel imports, either avoid inheritance or bind it to prior forms, e.g. `object foo extends _bar, _baz`. Otherwise, `_foo.op` generally refers to *future* extensions of `bar` or `baz`. 

### Anonymous Objects

An anonymous object has no name, i.e. `spec.name` is intentionally left undefined. Users may express anonymous objects by use of `_` in name position, e.g. `object _ extends foo, bar with ...` (or minimally, just `object _`). Anonymous objects must not appear within `spec.deps`, otherwise `object_from_spec` will report an error. Hence, anonymous objects do not participate in multiple inheritance. They may participate in mixin composition.

### Abstract Objects

As a special case, if users write `_object` instead of `object`, only `anno 'freeze {spec:Spec}` is returned. Users may instantiate it later via `object_from_spec foo.spec`, or use the object in inheritance. This applies to both object declarations and expressions. 

### Mixin Composition

Introducing operator `&>` (and its mirror `<&`). These operators compose two objects and return an anonymous object. We generally view this as `Obj &> Mixin` such that we 'apply' the mixin to the 'object' (consistent with `|>` and `!>`).

        toAnon o = if (o.spec has name) then _object _ extends o else o

        O &> M = object_from_spec mixed_spec where
            AO = (toAnon O).spec
            AM = (toAnon M).spec
            mixed_spec = spec:{ deps: mixed_deps, defs:mixed_defs }
            mixed_deps = AM.deps ++ AO.deps
            mixed_defs = mix AM.defs AO.defs
            mix c p = \ b s -> c (p b s) s

        M <& O = O &> M

A known weakness is that all definition updates from anonymous objects apply *after* all definition updates from named objects. This may result in some non-intuitive behavior, e.g. when we apply named mixins to anonymous objects and the actual order of overrides is flipped. Also, anonymous definitions are never deduplicated. A consequence is that mixins are safest for introducing names rather than overriding them.

### Explicit Scope

The default scope rule with `self` and `^` is awkward in some contexts, especially mixins. To mitigate this, an optional `as Name` modifier is supported for any object with a `with` section. 

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

*Note:* The name in `as Name` is a local, e.g. use of `as _` is also permitted, or `as _o` to supporess an unused locals warning.

### Lightweight Extensions

I propose keyword `mixin` for lightweight expression of anonymous mixins.

        mixin Body
        _object _ as _ with Body

        mixin as Name Body
        _object _ as Name with Body

This saves a few keystrokes and reduces some line noise. The `as Name` option may be aligned vertically with the `Body`. 

Naturally, we'll apply our mixin upon defining it. It is feasible to embed a mixin on the RHS of a pipe.

        foo = op1 >>= op2 >>= op3 &> mixin
            as op
            A := 42
            B := op4 >>= op.eff >>= op6 &> mixin
                C c = op7 &> mixin 
                    ...

We could tighten this further by compressing `&> mixin`. Current best idea is:

        &Object Body
        Object &> mixin Body

        &Object as Name Body
        Object &> mixin as Name Body

This again shaves off a few horizontal characters and some line noise. The resulting syntax feels clean to me:

        foo = op1 >>= op2 >>= &op3 as o
            A := _o.A + 42 
            B = op4 >>= op5 >>= &op6
                C c := &op7 
                    ...

Lightweight extensions provide a convenient foundation for tuning parameters, keyword arguments with defaults, continuation-passing style, etc.. See *Open Continuations*.

## Booleans

I propose to model booleans as simple atoms.

        't      true
        'f      false

There are no truthy values, e.g. empty list is not falsy. We can support `and, or, not` as keywords, with `and` and `or` acting as infix operators. (I don't like `&&` and `||`.)

We'll support comparisons on numbers `> >= == <> =< <`. Support for `==` and `<>` extend to all equatable values (all values not containing lambdas). 

For dictionaries, we'll add a `Dict has Path`, e.g. `{foo.bar:()} has foo.bar`.

## Conditionals

Big ideas for conditionals.

- *refactoring* - tentative `then?` or `-?>` allows rhs to use effectful rep for conditional behavior, e.g. `.r A` or `.fail`.  This simplifies refactoring. 
- *backtracking* - `try Effect then ...` will run `Effect` assuming context with `.alt, .cut, .fail`. If it fails, we select `else` branch, otherwise the `then` branch.

### If Then Else

        if Cond then A else B

        if C1 then
          A
        else
          B

        if C then A else
        B

        # in general, desugars as
        match when
            GuardClause -> ThenBranch
            _ -> ElseBranch

We'll also support the variation popularized in Python.

        E1 if Cond else E2

Users may write `elif` in place of `else if`. It always has the same meaning. 

Although I write `Cond` above, we'll support any non-branching guard clause from `match when ...`, thus we implicitly also support the popular 'if let' via pattern guards. 

        if Pattern <- Expr when Cond then A else B

### Tentative Commitment

Users may write `then?` to indicate tentative commitment. In context of `if` a `then?` body is expressed as a effect with access to `.alt/.fail/.cut/.seq/.r` and is evaluated with a local compiler-provided handler.

        if C then? .r A else B          # same as if C then A else B
        if C then? .fail else B         # always returns B

This enables users to factor out conditions more flexibly. 

        # snip chunk from middle
        if C1 then E1
        elif C2 then E2
        elif C3 then E3
        elif C4 then E4
        else E5

        if C1 then E1
        elif 't then?
            # can now move this
            if C2 then .r E2
            elif C3 then .r E3
            else .fail
        elif C4 then E4
        else E5

Refactoring isn't convenient or pretty, but it is possible. 

*Note:* The analog for `match` is `-?>`. 

### Try

Users may express `try` behaviors within effectful contexts assuming `.alt/.cut/.fail`. 

        # unit effect
        try Operation then
          Result1
        else
          Result2
        
        # effect with result
        try Operation -> Pattern (when ...) then
            Result1
        else
            Result2

        # in general desugars to
        try_match when
            GuardClause -> ThenBranch
            _ -> ElseBranch

        # roughly implements as:
        .cut (.alt TryThen Else) >>= \x.x

Any non-branching guard clause for `try_match when` is permitted here. Thus, `Operation` may be a boolean, pattern guard, effect, or all three separated by `when` clauses. Regardless, the selected branch is executed in the host, which is an important difference: `if` is a general expression, `try` always describes an effect.

Assuming effectful `Operation`, it may fail or return a result. On failure, we backtrack. If the result is anything other than unit, users *must* capture it via `(Operation -> Pattern)` (or explicitly ignore via `(Op -> _)`), otherwise we'll report an error: it is always an error to drop data implicitly. 

We can use `try` with tentative `then?`. The `then` body receives full access to host effects, running under the implicit `.cut` from `try`. It should ultimately return the decided effect or fail.

### Match

I'll adopt a lot of Haskell's syntax for `match` and its multi-line form. It's the feature of Haskell's syntax that I enjoy the most.

Some differences:

- Analogous to tentative `then?`, we introduce tentative arrow `-?>`
- Guard clauses are indicated by `when`. 
  - Either one clause inline, or many with vertical alignment.
    - May also use braces and semicolons for multi-element inline.
  - use `match when` to skip pattern match, jump directly to guard clauses
- `try_match` is to `match` as `try` is to `if`: 
  - host effects in guard clauses (`{eff:(_), _}`) or tentative `-?>`
  - final result must also be an effect

Example structure:

        match when                  # guard only
          Cond1 ->                  
          (Pattern <- Expr) when    # pattern guards
            Cond2a ->
            Cond2b ->

        try_match when
          Op1 ->
          Cond2 ->
          (Op3 -> Pattern) when
            (Pattern <- Expr) when Cond -> ...

The core pattern matching form:

        match Expr with
            Pattern1 -> 
            Pattern2 when ... -> 
            ...

Logically desugars to `match when` form:

        let e = Expr in
        match when
            (Pattern1 <- e) ->
            (Pattern2 <- e) when ... ->
            ...

### Pattern Matching

Pattern matching is primarily via `match` and `if let`. It is also available to do notations, where a failed match will evaluate to `.fail`. Unlike Haskell, pattern matching isn't supported in function arguments.

        Name                        # match anything (as a local name)
        ()                          # unit
        (Pattern)                   # you can parenthesize patterns
        (Pattern,Pattern)           # eqv. to tuple:(Pattern,Pattern) (also triples, etc.)
        Pattern as Name             # capture pattern target as a local

        {}                          # empty dict
        {d}                         # match dicts and tagged data
        {foo.bar.baz:Pattern, _}    # deep refs
        {(Expr):Pattern, _}         # eval path expr, extract, match Pattern 
        tag:Pattern                 # will also match singleton dicts
        [TagExpr]:Pattern           # eval Expr, extract, match Pattern

        []                          # empty list
        [a,b,c]                     # list of three items
        [x]++_xs                    # we can use append notation in patterns
        _xs++[x]                      
        [x0]++xs++[xN]

        (Applicable -> Pattern)     # view pattern must be parenthesized

        \Name                       # match applicables (\.fn, eff:_, {apply:_,_})
        \.Name                      # match lambdas only

Notes:
- List patterns limited to at most one variable-size element. 
- View patterns use the same structure as a tentative then/arrow, i.e. return optional value. 
- Tag or dict path expressions are *evaluated* in context and used as keys. Only the RHS data is a pattern. 
- `anno 'freeze` won't block full matches on a dict. (Implicit `anno 'thaw` as special case.)

## Loops

As with Haskell, we don't need keywords to support loops. Normal functions will do. Some useful simple loops:

        # foreach [1,2,3] \item-> do Body
        foreach L Action = match L with
            [x]++xs -> (Action x) >>= foreach xs Action
            [] -> .r ()

        untilDone s0 Action = match s0 with
            done:R -> .r R
            _ -> Action s0 >>= \ s1 -> untilDone s1 Action 

In the more general case, mutually recursive loops with tail calls can effectively represent state machines. I recommend such loops are modeled as method objects (instead of `let` groups or toplevel definitions). This makes it easier to extend and reuse the loop. Usefully, we can also expose continuations for overrides (per *Open Continuations*).

## Open Continuations

With the *Lightweight Extensions* syntax for objects, we can support continuation-passing style via extension of abstract method objects. This is another way of passing parameters, more extensible and flexible than lambda arguments. Moreover, it shifts some parameters from horizontal to vertical layout, and avoids some redundancy of reference. The resulting syntax might look a bit like this:

        foo x y = op1 x >>= op2 y >>= &op3 
            A a = op4 x >>=\_-> op5 a >>= &op6 as op
                B := ... op.F ... 
                C c = ...
            D ::= \ prior -> prior + 42

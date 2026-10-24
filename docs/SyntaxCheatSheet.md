# glam ".g" Syntax Cheat Sheet

```
# Every file starts with a language version declaration.
language g0
language g0 with utf8  # BaseVer with Extensions

# '#' to end of line is the only comment form. No multi-line comments.
```

## Definitions & Names

```
# Names: [a-zA-Z][a-zA-Z0-9]* parts, joined by single underscores.
# Active g0 keywords are reserved as bare names and local binders:
# abstract abstract_global_path and as assume at binary declare do else extend
# extends if import in language let match module module_origin object or self
# then try try_match unique using when where with
# Explicit keys may use them: module.where, where:Value, or .['where] = Value.
foo = 42                    # introduce (ERROR if foo already defined)
foo := 43                   # override (ERROR if foo NOT already defined)
foo ::= \ prior -> prior+1  # in-place update; NO defined/undefined check
bar = _foo + 1              # _foo references the PRIOR definition of foo

# Function definition sugar (no pattern matching in args):
add x y = x + y             # same as: add = \ x y -> x + y
add x y := ...              # override forms work too

# Paths index hierarchical dicts:
a.b.c = 7                   # names become atoms: 'a, 'b, 'c as keys
x = foo.[Expr]              # expression-indexed path 
y = foo.(['bar] ++ path)    # .(ListExpr); .([1,'two]++[3]) ≡ .[1].two.[3]
.[Idx] = Def                # toplevel expr-indexed def; access via module.[Idx]

# Declarations:
declare foo, bar            # names expected to be defined, e.g. by a mixin;
                            # the journal reports one left undefined
assume foo, bar             # names defined outside the journal (`:=` may
                            # override them; `=` would conflict)
unique Red, Green, Blue     # introduce unique atoms based on namespace path

# File-private bindings: not module entries; each group recursive within
# itself and seen only by the declarations after it (and their macros).
let helper x = x + 1        # a local: no global or local may share its name
let { a = b; b = 1 }        # braced, or a hanging layout group
 
# Multi-line: continuation lines must be indented; closing }]) may sit at col 0.
big_thing = f arg1
    arg2 arg3               # indented => continuation of same declaration

# Layout rules:
# - The owner line establishes an exclusive floor; continuations stay right.
# - First body member chooses the sibling anchor; inline means hanging layout.
# - A final structural child inherits its owner's floor rather than drifting.
# - Dedent closes a body; only an operator or a `where` may resume it.
# - A leading operator chain aligns at one resumption anchor.
```

## Numbers

```
0    1    42
_42                 # NEGATIVE 42 — prefix '_' is part of the literal
1.234    1.23e_7    # decimal / scientific; e_7 means exponent -7
1_000_000           # internal underscores for legibility (digit both sides)
0xc0de              # hex
0b1001_0110         # binary
1/6                 # exact rational (numbers are unbounded exact rationals)
```

## Atoms, Tagged Data, Dicts

```
()                  # unit, the built-in atom
'eax                # atom: sugar for ["eax"]:()  (NB: 'tag ≠ tag:() )
'.foo.[42]          # quoted path ≡ ['foo, 42]  (any Path)

# `path:` is a prefix, read alike everywhere, in braces or out:
tag:Data            # joint: tags one atom; sugar for { tag:Data }
tag: f x            # spaced: tags the rest of its item, tag:(f x)
(tag:)              # section: constructor \ Data -> tag:Data
:tag                # pun on one bare name: tag:tag
foo.bar:Data        # path-tagged data: sugar for { foo.bar:Data }
(foo.bar:)          # constructor: \ Data -> foo.bar:Data
[KeyA,KeyB]:Data    # computed hierarchical path
['a, x]:Data        # keys are ordinary expressions; 'a evaluates to an atom
([KeyA,KeyB]:)      # corresponding constructor
(PathExpr):Data     # splice a computed list-valued path
((PathExpr):)       # corresponding constructor
# Joint binds tighter than application: g tag:f x ≡ g (tag:f) x. Spaced
# runs to its item's end like a lambda body: tag: f x ≡ tag:(f x), and as an
# argument it is parenthesized: g (tag: f x) or g <| tag: f x.
# tag: |> g and a bare tag: are errors; the section (tag:) constructs.
# A joint colon always tags, even after an operator name: (and:x) is a
# tag; (and x), (x and) and (and) are sections.

{}                  # the empty dict, a value like any other
{foo:1, bar.baz:2}  # literal: members introduced in order; a path defined twice is an error
{foo: f x}          # a spaced tag runs to its item's comma: foo:(f x)
{foo:f x}           # joint, then more: reported while code migrates
{foo: 1, bar:       # a space or line break may follow a tag's `:` ...
    f x}            # ... but never precede it: {foo :x} applies foo to x:x
{:foo, :bar}        # puns: {foo:foo, bar:bar}
{ [0]:'a, [1,2]:'b, ([1] ++ [3,4]):'c } # computed paths, fused by prefix
{ D1, D2 }          # shallow union; ERROR on a shared key
{opt?:[x], none?:[]} # optional members: {opt:x}
{a:{b:1}, a.c:2}    # shallower first: {a:{b:1, c:2}}; (a.b:1) in a literal is a dict member
{(tag:) Value, D}   # a constructor applied, as anywhere
{:tag Value}        # applies the pun tag:tag to Value, as anywhere
{                   # multi-line: leading commas
, name1:Expr1
, name2:Expr2
}
{name1:Expr1,        # opening-line items do not set the later anchor
  name2:Expr2,
  name3:Expr3}

# 'with' updates (same =/:=/::= discipline as toplevel):
d2 = d1 with
    x := 10                 # override existing
    y = 20                  # introduce new
d2h = d1 with x := 10       # inline first member sets hanging sibling column
              y = 20
d3 = d2 with { x := 11; z = 30 } # semicolon-delimited equivalent
d4 = d3 with {}             # explicit no-op update
Dict with                   # capture: _d.x = prior, d.x = result
    alias d                 # an alias is a body's first member; default `_`
    x := _d.x + 1
    y = d.x + a
Dict with                   # object-style scope: _x prior, ^a escapes to host
    alias self
    x := _x + 1
    y = x + ^a
```

## Lists, Tuples, Texts

```
[]   [1]   [1,2,3]
[                   # multi-line lists: leading commas
, 1
, 2
]
[1,2,3,4,           # trailing separator: next member selects anchor
  5,6,7,8]
[1,2                 # leading separators select the same kind of anchor
  ,3,4
  ,5,6]
[make                # ordinary expression continuation within one member
    long_argument,
  next_member]
xs ++ ys            # append (no cons operator; lists are finger-tree ropes)
[x] ++ xs           # "cons" via append; also valid in patterns
list.at n xs         # zero-based lookup, errors out of bounds

(,)                 # empty tuple: tuple:[]
(a,)   (,a)         # singleton tuple: tuple:[a]
(a,b)   (,a,b,)     # tuple; boundary commas are optional
(first,              # first next-line member selects the content anchor
  second,
  third)

# Inside (), [], and {}, commas/semicolons—not indentation—separate members.
# Later member/separator lines align at the first post-opening contribution;
# other lines continue that member expression. Closers use the terminal rule.

"inline text"       # raw — NO escape characters, ever
"""
" first line
" second line       <- '"' + one SP per line; LF-separated, no final LF
# comments/blanks inside are erased
""" |> postprocess  # postprocessing is explicit
```

## Imports

```
import "Foo.g"              # mix into current namespace
import "Bar.g" as b         # introduce b
import "Baz.g" at b         # extend existing b
import "A/B/C.g"            # subfolders ok; NO "../", absolute, or dot-paths
import "data.bin" binary as blob    # raw bytes, not compiled
import 'prelude             # built-in module ('std; also 'math, 'list, 'dict)
import 'math as m

# Target syntax, not yet in the bootstrap:
import as q from {          # remote: content-addressed
    , ref:"Qux.q"
    , rev:"abc123..."
    , search:[ tag:"v1", url:"https://...", url:"https://backup..." ]
    }
```

## Functions, Locals

```
\ x y z -> Expr             # lambda (no argument patterns)
skip _ y = y                # '_' drops an argument
f _unused y = y             # '_' prefix suppresses unused warning
list.map (\x -> x + 1) xs   # an argument is an atom: parenthesize a lambda
op >>= \x -> continue x     # a tail infix operand's body owns the right tail
xs |> list.map (\x -> x + 1) |> finish  # parentheses end the lambda

let x = 1 in x + x          # one-liner
let { x = 1; y = 2 } in x + y # braced ';' group is mutually recursive
let {; x = 1; y = 2; } in x + y # leading/trailing ';' accepted
let {} in Expr              # explicit empty group; equivalent to Expr
let add a b = a + b in add 1 2 # parameters: add = \a b -> a + b
let { y = x; x = 42 } in y  # a group's bindings name each other in any order
go 5 where go n = if n == 0 then 0 else go (n - 1) # local recursion
let x = 1                   # multi-line: no 'in', Body aligns with 'let'
    y = x + 1
x + y
let                         # next-line bindings choose their own anchor
  x = 1
  y = x + 1
x + y

Body where n = d            # post-hoc single binding
Body where { n1 = d1; n2 = d2 } # braced semicolon-separated group
Body where {}               # explicit empty group; equivalent to Body
Body where
  n1 = d1
  n2 = d2
Body where n1 = d1          # inline first binding sets hanging sibling column
           n2 = d2

# `where` scopes the whole definition, statement or arm it ends, never part
# of an expression: it ends open tails, and parentheses scope less.
f x = if x then a else b where b = 1 # scopes all of f's body
f (a where a = 1)                    # a parenthesized expression is a body
if c then (a where a = 1) else b     # `if c then a where ... else b` is an ERROR

# One `where` per definition, statement or arm; its one group is mutually
# recursive. A where binding is a definition, so a `where` on its line scopes it.
Body where x = y where y = 1
Body where x = (y where y = 1)
Body where { x = y; y = x }
Body
  where x = y
  where y = 1               # ERROR: a second `where` for Body

# Dedent closes layout bodies; this `where` scopes the whole definition.
Body with
    member = Inner with
        value = x
  where
    x = replacement

# Naked ';' never groups let/where bindings; use braces or layout.
```

## Interaction Nets (performance escape hatch)

```
# interaction_net constructs an opaque, closed graph with exactly ONE
# exposed port. A raw net is already WHNF and is not directly applicable.

id_net = interaction_net <| do
    .bind -> [fn, result, arg]      # function bind; principal port first
    .wire arg result                # identity: arg flows to result
    .r fn                           # return port wired implicitly

# Provisional arity bridge into ordinary application:
id_fn = net_arity 1 id_net         # expect one Bind, then Data

answer_net = interaction_net <| do
    .data 42 -> [d]
    .r d
answer = net_arity 0 answer_net    # expect Data immediately

# An ordinary function behind Bind consumes exactly ONE argument. A partial
# function returns as ordinary Data and can be applied normally or by another
# explicit Bind.
add left right = left + right
partial_net = interaction_net <| do
    .bind -> [call, argument, result]
    .data add -> [function]
    .data 19 -> [value]
    .wire call function
    .wire argument value
    .r result
partial = net_arity 0 partial_net
sum = partial 23

# Node constructors (introduce ports; principal port is head of list):
    .bind -> [ap, arg, result]      # bind as an application
    .bind -> [fn, result, arg]      # bind as a function: mirrored polarity
    .copy N -> [x0, x1, ..., xN]    # dataflow fan-out, unique instances
    .copy 0 -> [e]                  #   explicitly drop data
    .copy 1 -> [lhs, rhs]           #   tunnel for non-local composition
    .data Expr -> [d]               # embed functions/lists/dicts/numbers
                                    #   (Expr copied logically)

# Wiring (each port must be wired EXACTLY once):
#   each wire joins a port that provides a value to one that consumes it;
#   the returned port must provide one, and every node must be reachable
#   from it. Construction rejects nets that break these rules.
    .wire A B                       # commutative: .wire B A equivalent
                                    # Standard Effects available for
                                    # bookkeeping and backtracking

# Interaction occurs when principal ports connect:
#   bind-bind: join crossed (b1-c2, b2-c1)
#   bind-copy: dup     bind-data: call (else stuck)
#   bind-data(Function): one ordinary value application; another Bind is
#                        required for another argument
#   bind-data(Net): load a logical copy through the raw net's exposed port
#   copy-data: dup     copy-copy: join if same instance, else dup
#   data-data: impossible: both provide, so construction rejects the wire

# Lambda calculus is a design pattern within inets:
#   lambda      = .bind -> [fn, result, arg], wiring arg INTO result
#   application = .bind -> [ap, arg, result], providing arg, extracting result
# A raw net embedded behind Bind is called by lazy cursor-based loading; an
# ordinary Function is never opened as though it were that raw net.
# Data flows BOTH directions (no inherent arg/result distinction),
# but ".g" syntax favors lambdas — use inets to accelerate difficult
# dataflows inside lambda bodies while preserving the lambda interface.
```


## Operators, Pipes, Application

```
arg |> f            # ≡ f arg
f <| arg
f >> g              # \h -> g (f h)   (forward composition)
g << f
(>>= k)             # Haskell-style operator sections
(x < y =< z)        # chained comparison: (x < y) and (y =< z)
# No mixing of opposing directions (>> vs <<, |> vs <|) without parens.
# Distinct arithmetic operators have no precedence: a + (b * c), (a * b) / c.
# `and` and `or` do not mix without parens: A or (B and C).
# Homogeneous `+`, `*`, `and`, and `or` chains remain concise.

# A leading operator establishes an aligned resumption anchor. It may close
# nested layout while preserving the same precedence as one-line syntax:
result = source
  |> configure with
    value := replacement
  |> finish
# Trailing operators remain ordinary incomplete-RHS continuations:
result = source |>
  configure |>
  finish

f x                 # application; ad hoc polymorphism:
                    #   functions; {apply:f,_} x = f x; (eff:f) x
                    #   raw interaction-net values are not applicable
f x <| \y -> body    # an argument is an atom: a tail form follows <|
# Tail forms (prefix if/try, match, do, lambdas, spaced tags, let, using,
# object) run to the end of their region: a closer, `,` `;`, a line end, or a
# separator keyword (then else in where => =>? and a binding's `->`). Never
# to a token that could continue them; after their `}` or dedent, only an
# operator or a keyword may follow, never an argument or path. They are
# never arguments. Parenthesize them elsewhere:
f (match x as {_ => 1}) y          # `f match x as {...} y` is an ERROR
f <| do { a }                      # `f do { a }` is an ERROR
source |> g (do { a }) |> h        # |> and <| do not mix: parenthesize
if c then do { a } else do { b }   # `}` ends the block, as a dedent does
if a then if b then c else d else e # each `if` has an `else`: they pair
if x <- (if a then b else c) and d then e else f  # `and` could continue c
f (if c then a else b)             # after an operand, `if` is postfix: ERROR
                                   #   without parentheses, as for `try`
foo.bar             # member access
foo (.bar)          # apply foo to effect path .bar; `foo .bar` is an ERROR
foo <| .bar         # equivalent alternative without parentheses
```

## Effects & Do Notation

```
.op                 # sugar for eff:(\api -> api.op)
.heap.get Path       # dotted effect path: eff:(\api -> api.heap.get Path)
.heap.set Path Value # blind replacement of shared heap state
.heap.rewrite Path F # commit-ordered lazy rewrite of shared heap state
.heap.try_get Path  # the value where defined; .fail where not (also .try_get, .vol.try_get)
.heap.del Path      # removes, pruning what it empties (also .del, .vol.del)
.heap.vol >>= (\h -> .vol.get h Path)  # volume handles: .vol.new/get/set/rewrite
.promise.new >>= (\p -> .promise.fulfill p.resolver 7)  # {promise, resolver}
.promise.status R   # 'pending, 'fulfilled or 'broken, from resolver R
.movl 'eax 42       # applied effect: eff:(\api -> api.movl 'eax 42)

my_proc = do
    .read 'port -> x        # BOTH name-bind directions are supported:
    y <- .read 'port        #   op -> Name  and  Name <- op
    z = x + y               # lazy pure name binding, no 'let'
    .movl 'eax z            # bare intermediate op must return unit
    .r z                    # final expression is continuation effect

inline = do .r 1            # singleton do
hanging = do x <- .read     # same-line first statement sets sibling column
             y <- .read
             .r [x, y]
braced = do { x <- .r 1; .r x }
empty = do {}               # exactly .r ()
nested = f [do { .r 1 }, do {; .r 2; }]

# Current bootstrap rules:
# - Layout `do` introduces a non-empty newline-delimited block; braces use `;`.
# - An inline first statement establishes a hanging sibling anchor. A
#   next-line first statement freely chooses an anchor above the outer floor.
# - A binding scopes only over later statements; its producer is outside it.
# - `_name` suppresses its unused warning; `op -> _` discards any result.
# - A bare intermediate op uses `=>>` semantics and therefore requires unit.
# - The final statement must express an effect, not a returned value (though
#   it may express an effect to return a value). A lone one still runs as an
#   effect: `do X` is `.r () >>= \_ -> X`, so `do 42` fails when run.
# - A layout do may yield to an aligned leading infix operator after its block.
#   Otherwise it owns the trailing part of its containing expression, and it
#   is never an application's argument: write `f <| do` or `f (do …)`.
# - Braced do opens with `{` on the `do`'s line (on a later line, `{` begins
#   a statement) and ends at `}` as at a dedent: an operator or keyword may
#   follow it, not an argument. One leading/trailing `;` is ignored; interior
#   empty statements and `do {;}` are invalid.
# - `P <- op` and `P = value` bind only when the `<-` or `=` comes before any
#   keyword form or `\` in the statement: `if (a, b) = e then ...` is an
#   operation, `x = if ...` a binding. Guard clauses follow the same rule.
# - In `op -> P`, the `->` is a separator keyword: it ends op's open tails
#   (`else` branch, `let`/`using` body, postfix `if` fallback, lambda body):
#   `if c then .a else .b -> x`.
# - Do bindings currently support irrefutable names, `_`, grouping, irrefutable
#   `P as Q`, scalar literals, fixed quoted paths, list patterns with at most
#   one potentially refutable variable-length segment, computed-path dictionary
#   and quoted-path patterns, static and computed tags, tuples, and the
#   effectful view, predicate, and local-guard patterns, in every binding
#   direction (`P <- op`, `op -> P`, `P = value`).

op1 >>= k           # bind        k1 >=> k2   # Kleisli
op1 =>> op2         # sequence, dropping unit result

# Applicatives (≡ Haskell <**> and <*>):
.r f <! op1 <! op2          # left-assoc
op1 !> op2 !> .r f          # right-assoc; always run left-to-right
# mf <! mx = mf >>= (\f -> mx >>= (\x -> .r (f x)))
# mx !> mf = mx >>= (\x -> mf >>= (\f -> .r (f x)))
# Opposing directions require parentheses.

# Recursive do is explicit; ordinary do never acquires an implicit fixpoint.
do
    declare loop_top, loop_exit
    .jmp loop_top           # may pass the future value without observing it
    .label -> loop_top      # first direct bind fulfills loop_top
    .exit_label -> loop_exit
    .r loop_exit

# Each name has an independent `.fix` through its own fulfillment. Same-line
# names may be reordered by completion without warning. Contained declarations
# nest directly; crossing intervals move the later-ending `.fix` outward and
# warn because its shift/reset scope changed. Missing/duplicate declarations
# and strict premature observations are errors. `_name` suppresses only unused
# warnings; bare `_` cannot be declared because it is inaccessible.
```

## Conditionals & Patterns

```
# Current bootstrap: prefix `if`/`try`, subject `match`/`try_match`,
# guard-only `match when`/`try_match when`, hierarchical choices, and
# tentative `then?`/`=>?` results, plus postfix `if`.
if C then A else B
A if C else B
if (a,b) = Expr and a > b then A else B     # C is guard: patterns + effects + 'and'
if (a or b) and c then A else B     # guards are an `and` chain: `or` needs parens
value if (value, rest) = split input else fallback
\x -> b if C else d                  # \x -> (b if C else d): an open lambda's
                                     #   body takes the trailing postfix if
(\x -> b) if C else d                # a conditional function
# Postfix guards bind within the preceding success value, not the else branch.

match Expr as
    Pattern1 => Result1
    P2 when C2 => R2               # guard clause
    P3 when                        # a `when` ending its line heads nested
        C3a => R3a                 #   choices sharing P3's successful prefix
        C3b => R3b
    P4 when C4a
         and C4b => R4             # guards may continue before the `=>`
    P5 when { C5a => R5a; _ => R5b } # `{` after `when`: nested arms, so
    P6 when ({}) <- Op => R6        #   a dict-pattern guard is parenthesized
    _ => Default

match Expr as { Pattern1 => Result1; _ => Default; }  # `{` on `as`'s line
match Expr as                       # a dict pattern first: on the next line
    {x:value} => value
match Expr as {}                   # error: match exhausted on its source line
match* Expr as ...                  # lazy list of every successful result
match* Expr as {}                   # []

# The first ungrouped `as` belongs to `match`. Pattern arms follow `as`, as
# in `P as Q`; guard arms follow `when`.
# A match is a tail form: `(match X as {...}) y` applies it.
match Dict with { x := 1 } as { {x:value} => value; _ => {}; }

match when                          # no scrutinee: guard-only dispatch
    C1 => R1
    C2 when                         # choices may nest to arbitrary depth
        C2a => R2a
        _ => R2b

try C then A else B                 # effectful variants: return one root .cut
try_match Expr as ...               #    to the host for rollback/state access
try_match when ...                  # guard-only host search
try_match* Expr as ...              # no root cut; ambient handler owns choice
try_match* when ...                 # exhaustion remains .fail

if C then? .r A else B              # tentative choice: branch must
if C then? .fail else B             #   explicitly .r Result or .fail
match Expr as P =>? .r Result       # tentative arrow: direct effect result
try C then? HostOperation else B    # host effects share the same root cut
```

```
# Conditions are pass/fail effects:
X and Y                 # sequence: X must pass before Y
X or Y                  # raw ordered choice, equivalent to .alt X Y

# A branching handler may continue once per successful alternative:
list.pure <| do
    X or Y
    .r Result

# Commit only after a complete path succeeds:
.cut <| do
    X or Y
    LaterGuard
    .r Result

# Parentheses group but do not commit. This commits too early if
# LaterGuard should be able to fall back from X to Y:
(.cut (X or Y)) =>> LaterGuard

# if/match supply an outer cut around their complete choice tree.
# match*/try_match* deliberately omit that cut; match* collects a lazy list.
```

```
# Patterns:
name                # bind
_name               # bind, no unused warning
_                   # drop
P1 as P2            # both views
()                  # unit
{}                  # EMPTY dict only
{d}                 # any dict
{x:P, y:Q, rem}     # partial match, 'rem' captures rest (default {} = exact)
{x:P, {y:Q}}        # run a refutable pattern against the residual dictionary
{x?:P, rem}         # P matches [] where x is absent, [V] where present; never fails on absence
{x?:[]}             # explicitly accept a missing x; {x?:[v]} binds a present one
{a.c:C, a:A}        # deeper paths first: a take removes, pruning what it empties
{:x, :y}            # ≡ {x:x, y:y}; :x alone ≡ {x:x} too
{a.b.c:P, _}        # deep path
{selector:key, [key]:P, rem}       # prior capture selects a computed key
{selector:key, root.[key]:P, rem}  # computed hierarchical component
{path:p, (p):P, rem}               # splice a computed list-valued path
tag:P    'name      # tag:P ≡ tag: P; (tag:) is no pattern
{x: Nat n}          # a spaced payload is complete: tags a predicate,
{x: f -> p}         #   a view, or {x: a as b} both captures
[KeyExpr]:P       # computed singleton tag; prior captures are in scope
[KeyA,KeyB]:P     # computed hierarchical singleton tag
(PathExpr):P      # singleton tag from a computed list-valued path
[]   [a,b,c]
[x]++xs   xs++[x]   [x0]++mid++[xN]     # ONE variable segment max; may refute
[x0]++([y]++ys)++[xN]                   # nested pattern sees middle slice
"foo"    "foo"++rest                    # texts are lists
'.foo.[key].(tail) # computed quoted path; earlier captures are in scope
(,)   (P,)   (P1,P2) # tuple patterns
42   _1.23   1/6    # exact constants
(View -> P)         # view pattern; view runs first
View -> P => Result # whole match-arm view may omit outer parentheses
(P <- View)
(Nat n)             # predicate pattern: pass/fail, captures ORIGINAL input
(Prefix "x-" rest)  # last arg is the pattern
Prefix "x-" rest => Result # complete match arm may omit outer parentheses
(P when Guard)      # local guard
(P as Nat n)  (f -> Nat n)  (Nat n when G)  # also after `as`, `->`, before `when`
# Loosest first: `when`, one view arrow, `as`; then as in expressions:
# `[x] ++ Nat n` is an error (predicate needs parens); a:Nat n ≡ (a:Nat) n
# is an error too: a tag is never a predicate or a view. Write a: Nat n.
```

## Objects

```
object foo extends bar, baz with
    def1 = ...              # names bind to self by default
    def2 := ...             # override inherited def2; _def2 = prior
    def3 = ^a + def1        # ^a steps out of the innermost self (^^a: two),
                            #   host locals too: \v -> object _ with { x = ^v }
    declare def4            # expected by the final object, e.g. from a child
    assume def5             # defined outside any journal, e.g. by a dict

object hanging with def1 = ...
                    def2 = ... # hanging layout is also accepted

object compact with {;      # braced bodies share the recursive vocabulary
  value = 1;
  object nested with { other = 2 };
}
extend compact with { value := 2 }
empty = object () with {}   # explicit empty body

object configured extends choose_parent options, fallback with
    ...                     # each comma-separated parent is an expression

import 'std
object legacy extends object_from_dict optional_defs with
    ...                     # explicit plain-dict to anonymous-object conversion

object foo extends bar with
    alias f                 # local alias, no ^ needed; default `alias self`
    A = f.B + a             # 'a' resolves to host directly now
    B = f.C + 2             # _f.B would be the prior B

Dict with                   # `let` members: private, not entries
    alias d
    let tmp = _d.x + 1      # one group; recursive within itself,
    x := tmp + 2            #   seen only by the members after it

baz = object NameExpr extends foo with ...           # named object expression
qux = object _ extends foo with ...                   # anonymous object expression
abstract object proto with ...                        # declaration: spec only
template = abstract object _ extends proto with ...   # expression: spec only
extend abstract foo with ...                          # update spec, remain abstract
extend foo with                                       # update spec + instantiate
    def1 := ...

# Lightweight extension — any expression, 'with' body is anonymous mixin:
foo = op1 >>= op2 with
    A := 42
    B := op4 >>= op5 with           # nested, with capture
        alias o
        C c = ... o.F ...
    D ::= \ prior -> prior + 1

using Dict in Expr          # treat any dict as temporary object scope;
                            #   outer locals too take ^: `let t = 1 in
                            #   using d in t ++ ^t` reads d.t, then the local
using Dict do Body          # sugar for: using Dict in do Body
```

## Source Macros

```
@macro_name input           # selects prior _module.macro_name, or a file's
                            #   earlier `let macro_name`
@table.create input         # joint static paths only
@grammar input              # object/dict may expose its primary effect as `eff`
@outer @inner input         # inner expands first; each original call runs once

meta.macro.env = {          # immutable parameters available through macro .env
    style:"compact"
}
```

The compiler adds the declared `language` value to `meta.macro.env`.
Macro-bearing declarations are expanded before ordinary parsing. See
[Source Macros](Macros.md) for definition, effects, layout, and staging.

## Annotations and Conventions

```

anno 'TBD Expr              # incomplete
anno 'error Message         # halt evaluation with diagnostic Message
anno context:Frame Expr     # add Frame to msg.context if Expr errors
anno 'deprecated Expr       # valid but warns
anno 'scope_unique Atom     # unique atom (ephemeron pattern)
anno 'compile F             # reduce F's net under fuel, compile it; F unchanged
anno 'jit F                 # as 'compile, at F's first application
anno 'normalize F           # reduce F's net without fuel; F unchanged
anno refl:(.log Msg) Term   # reflection effect before returning Term

# g0 definition initialization adds a shallow source frame:
# {g:{origin:OpaqueOrigin, line:LineNumber, definition:Name}}
# Manual source context may use the same opaque token:
anno context:{origin:module_origin} Expr
# Reflection may explicitly inspect that token with:
.env '.glam.origin.inspect

# Conventions:
type_of.foo = ...           # associated names: '_of' dicts
final_of.foo = _foo         # guard against accidental override
refl.check_invariants = ... # refl.* run automatically as reflection tasks
meta.refl := .r ()          # the namespace's reflection operation, run once
                            # in a task when a definition is first used;
                            # the language declaration installs one that
                            # launches refl.* (here: disabled); state under
                            # [name, 'meta, 'refl]:() on the reflection heap
meta.journal                # compiler metadata lives under meta.*, e.g. the
                            # journal of names introduced or overridden
env                         # implicitly declared; provided on import by host 
module                      # alias for module toplevel namespace
module_origin               # opaque provenance for this source compilation
self                        # current object namespace (= module at toplevel)
abstract_global_path foo.bar
abstract_global_path module.foo.bar  # explicit module scope
```

## Idiomatic Loops (no loop keywords)

```
foreach L Action = match L as
    [x]++xs => Action x >>= foreach xs Action
    [] => .r ()

untilDone s0 Action = match s0 as
    done:R => .r R
    _ => Action s0 >>= \ s1 -> untilDone s1 Action
```

## Putting It Together

```
language g0 
import 'prelude
import "x86.g" as x86

writeln msg = using x86 do
    rodata (^msg ++ [10]) -> msg_loc
    movl 'rdi 1
    movl 'rsi msg_loc
    movl 'rdx (1 + len ^msg)
    syscall

main = do
    .global "_start"
    writeln "Hello, World!"
    .movl 'rax 60
    .xor 'rdi 'rdi
    .syscall

asm.result = x86.mkelf main
```

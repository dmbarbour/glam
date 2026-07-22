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
abstract foo, bar           # names assumed provided externally
unique Red, Green, Blue     # introduce unique atoms based on namespace path
 
# Multi-line: continuation lines must be indented; closing }]) may sit at col 0.
big_thing = f arg1
    arg2 arg3               # indented => continuation of same declaration
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

tag:Data            # tagged data: sugar for { tag:Data }
:tag                # constructor: \ Data -> tag:Data
foo.bar:Data        # path-tagged data: sugar for { foo.bar:Data }
:foo.bar            # constructor: \ Data -> foo.bar:Data
[KeyA,KeyB]:Data    # computed hierarchical path
:[KeyA,KeyB]        # corresponding constructor
(PathExpr):Data     # splice a computed list-valued path
:(PathExpr)         # corresponding constructor
# Colons are tight; tag:f x ≡ (tag:f) x. Use tag:(f x) to tag the call.

{}                  # empty dict; ALSO the 'undefined' value
{foo:1, bar.baz:2}  # literal; paths ok; {foo:{}} ≡ {}
{ [0]:'a, [1,2]:'b, ([1] ++ [3,4]):'c } # computed paths
{ D1, D2 }          # union; ERROR if defined keys overlap
{                   # multi-line: leading commas
, name1:Expr1
, name2:Expr2
}

# 'with' updates (same =/:=/::= discipline as toplevel):
d2 = d1 with
    x := 10                 # override existing
    y = 20                  # introduce new
Dict as d with              # capture: _d.x = prior, d.x = result
    x := _d.x + 1
    y = d.x + a
Dict as self with           # object-style scope: _x prior, ^a escapes to host
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
xs ++ ys            # append (no cons operator; lists are finger-tree ropes)
[x] ++ xs           # "cons" via append; also valid in patterns
list.at n xs         # zero-based element lookup; errors when out of bounds

(,)                 # empty tuple: tuple:[]
(a,)   (,a)         # singleton tuple: tuple:[a]
(a,b)   (,a,b,)     # tuple; boundary commas are optional

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
import 'prelude             # built-in module
import 'trig as t
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

let x = 1 in x + x          # one-liner
let x = 1; y = 2 in x + y   # ';' separates; groups are mutually recursive
let x = 1                   # multi-line: no 'in', Body aligns with 'let'
    y = x + 1
x + y

Body where n1 = d1; n2 = d2 # post-hoc let
Body where
  n1 = d1
  n2 = d2
```

## Interaction Nets (performance escape hatch)

```
# interaction_net constructs an opaque, closed graph with exactly ONE
# exposed port. A raw net is already WHNF and is not directly applicable.

id_net = interaction_net do
    .bind -> [ap, arg, result]      # function node; principal port first
    .wire arg result                # identity: arg flows to result
    .r ap                           # return port wired implicitly

# Provisional arity bridge into ordinary application:
id_fn = net_arity 1 id_net         # expect one Bind, then Data

answer_net = interaction_net do
    .data 42 -> [d]
    .r d
answer = net_arity 0 answer_net    # expect Data immediately

# Node constructors (introduce ports; principal port is head of list):
    .bind -> [ap, arg, result]      # function constructor
    .copy N -> [x0, x1, ..., xN]    # dataflow fan-out, unique instances
    .copy 0 -> [e]                  #   explicitly drop data
    .copy 1 -> [lhs, rhs]           #   tunnel for non-local composition
    .data Expr -> [d]               # embed functions/lists/dicts/numbers
                                    #   (Expr copied logically)

# Wiring (each port must be wired EXACTLY once):
    .wire A B                       # commutative: .wire B A equivalent
                                    # Standard Effects available for
                                    # bookkeeping and backtracking

# Interaction occurs when principal ports connect:
#   bind-bind: join    bind-copy: dup    bind-data: call (else stuck)
#   copy-data: dup     copy-copy: join if same instance, else dup
#   data-data: STUCK — a type error; report and debug

# Lambda calculus is a design pattern within inets:
#   lambda      = .bind copying/wiring arg INTO result
#   application = .bind providing arg, extracting result
# A raw net embedded behind Bind is called by lazy cursor-based loading.
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

f x                 # application; ad hoc polymorphism:
                    #   functions; {apply:f,_} x = f x; (eff:f) x
                    #   raw interaction-net values are not applicable
```

## Effects & Do Notation

```
.op                 # sugar for eff:(\api -> api.op)
.heap.get Path       # dotted effect path: eff:(\api -> api.heap.get Path)
.heap.set Path Value # blind replacement of shared heap state
.heap.rewrite Path F # commit-ordered lazy rewrite of shared heap state
.movl 'eax 42       # applied effect: eff:(\api -> api.movl 'eax 42)

my_proc = do
    .read 'port -> x        # BOTH name-bind directions are supported:
    y <- .read 'port        #   op -> Name  and  Name <- op
    z = x + y               # lazy pure name binding, no 'let'
    .movl 'eax z            # bare intermediate op must return unit
    .r z                    # final expression is continuation effect

# Current bootstrap rules:
# - `do` introduces a non-empty newline-delimited layout block.
# - A binding scopes only over later statements; its producer is outside it.
# - `_name` suppresses its unused warning; `op -> _` discards any result.
# - A bare intermediate op uses `=>>` semantics and therefore requires unit.
# - The final statement must express an effect, not a returned value (though
#   it may express an effect to return a value)
# - The layout block must be the trailing part of its containing expression;
#   in an application it can therefore only be the final argument.
# - Patterns and braced/semicolon blocks are not implemented yet.

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
    abstract loop_top, loop_exit
    .jmp loop_top           # may pass the future value without observing it
    .label -> loop_top      # first direct bind fulfills loop_top
    .exit_label -> loop_exit
    .r loop_exit

# Each name has an independent `.fix` through its own fulfillment. Same-line
# names may be reordered by completion without warning. Contained declarations
# nest directly; crossing intervals move the later-ending `.fix` outward and
# warn because its shift/reset scope changed. Missing/duplicate declarations
# and strict premature observations are errors. `_name` suppresses only unused
# warnings; bare `_` cannot be abstract because it is inaccessible.
```

## Conditionals & Patterns

```
if C then A else B
A if C else B
if (a,b) = Expr and a > b then A else B     # C is guard: patterns + effects + 'and'

match Expr with
    Pattern1 -> Result1
    P2 when C2 -> R2               # guard clause
    P3 when                        # branching guards, nestable
        C3a -> R3a
        C3b -> R3b
    _ -> Default

match when                          # no scrutinee: guard-only dispatch
    C1 -> R1
    C2 -> R2

try C then A else B                 # effectful variants: run in host with
try_match Expr with ...             #    backtracking, state access

if C then? .r A else B              # tentative choice: branch must
if C then? .fail else B             #   explicitly .r Result or .fail
match Expr with P -?> ...           # tentative arrow for match
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
{:x, :y}            # ≡ {x:x, y:y}
{a.b.c:P, _}        # deep path
tag:P    :tag    'name    [KeyA,KeyB]:P    (PathExpr):P
[]   [a,b,c]
[x]++xs   xs++[x]   [x0]++mid++[xN]     # ONE variable segment max
"foo"    "foo"++rest                    # texts are lists
(,)   (P,)   (P1,P2) # tuple patterns
42   _1.23   1/6    # exact constants
(View -> P)         # view pattern — MUST be parenthesized; view runs first
(P <- View)
(Nat n)             # predicate pattern: pass/fail, captures ORIGINAL input
(Prefix "x-" rest)  # last arg is the pattern
(P when Guard)      # local guard
```

## Objects

```
object foo extends bar, baz with
    def1 = ...              # names bind to self by default
    def2 := ...             # override inherited def2; _def2 = prior
    def3 = ^a + def1        # ^a escapes to host scope (^^a two levels)

object foo as f extends bar with    # 'as f': local alias, no ^ needed
    A = f.B + a             # 'a' resolves to host directly now
    B = f.C + 2             # _f.B would be the prior B

baz = object NameExpr extends foo with ...  # named object expression
qux = object _ extends foo with ... # anonymous object expression
abstract object proto with ...      # spec only, no instance
extend foo with                     # update spec + re-instantiate
    def1 := ...

# Lightweight extension — any expression, 'with' body is anonymous mixin:
foo = op1 >>= op2 with
    A := 42
    B := op4 >>= op5 as o with      # nested, with capture
        C c = ... o.F ...
    D ::= \ prior -> prior + 1

using Dict in Expr          # treat any dict as temporary object scope
using Dict do Body          # sugar for: using Dict in do Body
```

## Macros, Annotations, Conventions

```
@macro_name arg1 arg2       # ≡ @(_module.macro_name); effectfully read args
@(Expr) ...                 # general form; Expr must be an effect

anno 'TBD Expr              # incomplete
anno 'error Expr            # known error
anno 'deprecated Expr       # valid but warns
anno 'scope_unique Atom     # unique atom (ephemeron pattern)
anno (.log Msg) Term        # effectful annotation → reflection API

# Conventions:
type_of.foo = ...           # associated names: '_of' dicts
final_of.foo = _foo         # guard against accidental override
refl.check_invariants = ... # refl.* run automatically as reflection tasks
meta.abstract_names         # compiler metadata lives under meta.*
env                         # implicitly abstract; provided on import by host 
module                      # alias for module toplevel namespace
self                        # current object namespace (= module at toplevel)
```

## Idiomatic Loops (no loop keywords)

```
foreach L Action = match L with
    [x]++xs -> Action x >>= foreach xs Action
    [] -> .r ()

untilDone s0 Action = match s0 with
    done:R -> .r R
    _ -> Action s0 >>= \ s1 -> untilDone s1 Action
```

## Putting It Together

```
language g0 
import 'prelude
import "x86.g" as x86

writeln msg = using x86 do
    rodata (msg ++ [10]) -> msg_loc
    movl 'rdi 1
    movl 'rsi msg_loc
    movl 'rdx (1 + len msg)
    syscall

main = do
    .global "_start"
    writeln "Hello, World!"
    .movl 'rax 60
    .xor 'rdi 'rdi
    .syscall

asm.result = x86.mkelf main
```

# Object Spike Notes

This is the implementation plan for the first executable object slice. The
eventual design is larger: objects are dictionaries with `spec:{name,defs,deps}`,
`defs` is a mixin of logical form `\_self self -> _self with ...`, dependency
ordering needs proper linearization, and object scope has host escapes.

For the first spike, keep the surface narrow and observable:

- Parse named toplevel object declarations:

  ```g
  object foo extends bar, baz with
    field = ...
    field2 := ...
    field3 ::= ...
  ```

- Lower object bodies as mixins. Inside an object body, ordinary names resolve
  through final `self`, while `_name` resolves through prior `_self`. Lambda
  locals still shadow these object-scope names.
- Lower a named object declaration approximately as:

  ```g
  foo = object_instance {
    , name:abstract_global_path(foo)
    , deps:[bar.spec, baz.spec]
    , defs:\_self self -> _self with ...
  }
  ```

- Add an internal `ObjectInstance` builtin. It is not exposed through the initial
  `g_syntax` built-in modules yet.
- For this spike, object instantiation may use simple left-to-right dependency
  application and a fixpoint for `self`. Full C3 linearization, spec-name
  deduplication, anonymous objects, abstract objects, `extend`, `as`, `^` host
  escapes, object expressions, `with` expressions, and protection around direct
  `spec` edits are intentionally deferred.
- Add hello-world samples and tests that exercise direct object fields,
  inherited fields, overrides, and `_name` access.

The key semantic constraint to preserve is that an object body is not lowered in
module scope. It has a distinct self scope:

- `name` means `self.name`
- `_name` means `_self.name`
- inherited or earlier same-object definitions are observed lazily through those
  values
- host/module escape syntax is a later addition, not an implicit fallback


## Full Plan

 I reviewed the object sections. The target is larger than one spike: objects are singleton dict instances with spec:{name, defs, deps}, where defs is a mixin roughly \_self
  self -> _self with ..., and instantiation is an open fixpoint over linearized parent specs.

  I’d make the first object spike deliberately narrow but structurally aligned with the target.

  Recommended First Spike

  1. Parse named object declarations only
     Support:

     object foo with
       x = ...
       y := ...
       z ::= ...

     and:

     object child extends parent with
       y := _y ++ "!"

     Defer object expressions, anonymous objects, extend foo with, using, as Name, abstract objects, and full lightweight Expr with ....

  2. Add a real syntax AST node
     Replace the current DeclarationKind::Object placeholder with something like:

     ObjectDecl {
         target: String,
         alias: Option<String>,
         deps: Vec<String>,
         body: Vec<DefinitionDecl>,
     }

     For this spike, require a named target and allow dependency names as ordinary dotted paths.

  3. Lower object bodies as mixins
     Compile the body to a lambda:

     \_self self -> <ordered DictUpdate chain starting from _self>

     Inside this body:
      - name resolves against self.name
      - _name resolves against _self.name
      - lambda locals still shadow normally
      - host escapes with ^name can be either included in this spike or explicitly deferred

     Because we recently simplified names into Name(String) plus Access, this should be tractable by adding a lowering “name scope” parameter rather than special-casing all path
     shapes.

  4. Represent object specs as ordinary values
     Lower:

     object foo extends bar with ...

     roughly to:

     foo = object_instance {
       name: abstract_global_path("foo"),
       deps: [bar.spec],
       defs: \_self self -> ...
     }

     Use abstract_global_path for toplevel object declarations so future linearization can dedupe robustly.

  5. Add an internal object instantiation builtin
     Add something like Builtin::ObjectInstance, initially not exposed through import 'std.

     First implementation can be conservative:
      - evaluate parent specs from deps
      - instantiate parents left-to-right
      - apply each parent defs
      - apply child defs
      - insert final spec
      - use a fixpoint so self references can see the final object

     I would not attempt full C3 in the first spike. But the builtin API should take deps as specs so replacing the simple linearization later does not affect g_syntax.

  6. Explicitly reserve spec behavior for later
     The docs say ordinary dict/object syntax should not casually touch spec. For this spike, I’d avoid enforcing that deeply, but add TODOs and avoid samples that override
     spec.

  7. Tests and samples
     Add samples that produce Hello, World!:

     object hello with
       text = "Hello, World!"

     asm.result = hello.text

     and inheritance:

     object base with
       text = "Hello"

     object hello extends base with
       text := _text ++ ", World!"
     asm.result = hello.text

     Then add focused unit tests for:
      - parsing object headers/body
      - parent override through extends

  Why this path
  I’d defer full as Name and ^ only if we want the spike small. If we include either one, I’d include ^name first, because default object scoping otherwise makes it hard to
  reference host names from object bodies.

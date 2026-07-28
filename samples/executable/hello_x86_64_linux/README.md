# Direct-Assembly Hello World

This sample generates a minimal Linux x86-64 ELF executable. The `.g` source
builds a symbolic instruction stream through the effect API supplied by
`samples/config/direct_assembly.g`; the configuration resolves labels, encodes
the instructions, and wraps the code in an ELF header.

The runner inherits a reusable deterministic state-effect handler, then
extends its `api` object with x86 operations. Only that public API is placed in
`conf.env`; running, sequencing utilities, and the concrete state layout remain
on the handler. Public `.get` and `.set` operate beneath `user_state`, while
cursor selection and instruction streams live beneath protected
`handler` state.

See [`docs/DirectStyleAssembly.md`](../../../docs/DirectStyleAssembly.md) for
the cursor, layout, label, and symbol-publication model exercised here.

This is still only the deterministic state-and-sequencing subset of the
standard effects. Choice (`.alt/.fail/.cut`), fixpoints, and delimited control
remain absent. Adding choice will require the runner outcome to represent zero
or more transactional state results rather than pretending the current
single-outcome state transformer already has those semantics.

The program dynamically allocates entry, exit, and message cursors, then links
their completed fragments in that layout order. It populates the message
fragment before finishing the entry and exit fragments, demonstrating that
construction order does not determine byte order. Labels are captured directly
from cursor boundaries. The `_start` publication determines the ELF entry
address; a trap byte immediately before that label makes the distinction
observable.

Cursor and label identities are opaque to the program. The bootstrap handler
currently implements them with private monotonic tagged integers.

From the repository root:

```sh
GLAM_CONF=samples/config/direct_assembly.g \
  cargo run -- --file samples/executable/hello_x86_64_linux/hello.g \
  > /tmp/glam-hello
chmod +x /tmp/glam-hello
/tmp/glam-hello
```

The executable prints `Hello, World!` followed by a newline.

## Tooling backlog exposed by this sample

- Add a structured handler-failure operation. Cursor-link and symbol conflicts
  currently use `assert_unit` to retain useful context, but consequently end
  with an irrelevant “unit expected” explanation.
- Improve object-parent diagnostics to identify the parent expression, its
  evaluated value kind, and whether it resolved to undefined. This would have
  made an accidental `_api` reference inside an `as x86` scope immediately
  distinguishable from the intended `_x86.api`.
- Add an opt-in effect trace view showing public API dispatch and summarized
  state-path transitions. Protected handler state should remain distinguishable
  from client `user_state`.
- Add contextual pure-evaluation diagnostics, provisionally along the lines of
  `anno context:Context Expr`, so encoding errors can retain instruction,
  cursor, label, and source-stage context.
- Diagnose lazy dependency cycles at the responsible effect/state boundary.
  During development, storing a still-lazy shared payload in handler state and
  observing another property of it later produced only a host stack overflow.
  The sample currently uses `seq` to make the intended demand explicit.
- Repair parser recovery for an empty-dictionary `match` pattern inside a
  layout object body. It is currently misreported as an invalid object header;
  the handler uses an equivalent `if`.
- Add reflection support for inspecting object specifications and their
  linearization without forcing unrelated members.
- Add referential-equality assertions before implementing singleton sections.
- Add a layout view that reports fragment classes and links, offsets, labels,
  publications, and relocations. A disassembler-backed view could then check
  emitted machine code without changing assembly semantics.

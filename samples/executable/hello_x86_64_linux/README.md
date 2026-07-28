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

This is still only the deterministic state-and-sequencing subset of the
standard effects. Choice (`.alt/.fail/.cut`), fixpoints, and delimited control
remain absent. Adding choice will require the runner outcome to represent zero
or more transactional state results rather than pretending the current
single-outcome state transformer already has those semantics.

Labels and write cursors are scope-unique opaque values. The program writes
instructions to the default text cursor, then runs its message subprogram with
the rodata cursor selected. The current ELF layout concatenates those cursors
in text-then-rodata order within one load segment.

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
- Add reflection support for inspecting object specifications and their
  linearization without forcing unrelated members.
- Add referential-equality assertions before implementing singleton sections,
  followed by a layout view that reports cursors, offsets, labels, and
  relocations. A disassembler-backed view could then check emitted machine
  code without changing assembly semantics.

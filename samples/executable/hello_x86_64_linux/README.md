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

The program dynamically allocates an entry cursor, then splits it twice.
Splitting off the message region first and the exit region second establishes
entry-then-exit-then-message layout: each new split is inserted immediately
after the selected region. It populates the message fragment before finishing
the entry and exit fragments, demonstrating that construction order does not
determine byte order. Labels are captured directly from cursor boundaries. The
`_start` publication determines the ELF entry address; a trap byte immediately
before that label makes the distinction observable.

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

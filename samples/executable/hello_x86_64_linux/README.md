# Direct-Assembly Hello World

This sample generates a minimal Linux x86-64 ELF executable. The `.g` source
builds a symbolic instruction stream through the effect API supplied by
`samples/config/direct_assembly.g`; the configuration resolves labels, encodes
the instructions, and wraps the code in an ELF header.

From the repository root:

```sh
GLAM_CONF=samples/config/direct_assembly.g \
  cargo run -- --file samples/executable/hello_x86_64_linux/hello.g \
  > /tmp/glam-hello
chmod +x /tmp/glam-hello
/tmp/glam-hello
```

The executable prints `Hello, World!` followed by a newline.

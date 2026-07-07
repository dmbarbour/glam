# Glam Samples

This directory contains small `.g` programs and fragments used for testing, experimentation, and user education.

These are not a standard library. Glam assemblies do not imply a runtime, and target-specific APIs should live in ordinary modules or future package folders. Samples are here because they are useful shared source texts:

- to exercise parser and assembler behavior,
- to demonstrate language forms as they stabilize,
- to provide compact examples for docs and agent smoke checks.

## Layout

- `syntax/` contains small files focused on the initial `.g` source surface.
- `config/` contains sample configurations, notably `dev.g` is default for container.
- `assembly/` contains source-to-output examples. Early samples may produce raw
  binary text before target libraries exist.
- `invalid/` contains samples that should report diagnostics.
- `packages/` is reserved for package-shaped examples of local module layout.

Prefer small samples with one clear purpose. If a sample is expected to parse or assemble under the current bootstrap, keep it covered by tests.

## Invalid Samples

Invalid samples use a sibling `.expect` file. Each non-empty, non-comment line
has this format:

```text
severity|line|message substring|message substring...
```

For example:

```text
error|1|language|declaration
```

This means an error is expected on line 1, and the diagnostic message must
contain both `language` and `declaration`.

## Running Current Samples

The bootstrap CLI can inspect a source file with:

```sh
cargo run -- --parse samples/syntax/minimal.g
```

The devcontainer sets `GLAM_CONF` to `samples/config/dev.g`. Tests or scripts
that need a specific configuration should still set `GLAM_CONF` explicitly, for
example:

```sh
GLAM_CONF=samples/config/minimal.g cargo test
```

Bare non-option arguments remain reserved for future configured `conf.cli`
rewriting.

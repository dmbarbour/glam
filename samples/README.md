# Glam Samples

This directory contains small `.g` programs and fragments.

Use cases:

- exercise parser and assembler behavior,
- demonstrate language forms as they stabilize,
- compact examples for docs and agent smoke checks.

## Layout

- `syntax/` small files focused on the initial `.g` source surface.
- `config/` configurations; `dev.g` is default for container.
  - common utility functions via `conf.env`
- `assembly/` source to output examples. 
  - early samples may produce raw binary text.
- `invalid/` for testing of diagnostics.

Glam forbids parent-relative paths (`"../"`) in imports, and samples shall not reference remote repos. Thus, sample folders are self-contained. The preference is to keep samples small and focused. To avoid repetition, common utility functions can be written once then provided via configuration (defining `conf.env`). 

## Configuration

Set `GLAM_CONF` in scope to configure for different tests as needed. For example, we could test some assemblies under multiple configurations.

```sh
GLAM_CONF=samples/config/minimal.g cargo test
```

## Expectations

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

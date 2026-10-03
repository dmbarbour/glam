# Glas Assembly (glam)

A high-level language for assembly-level software description. Focus is reproducibility, extensibility, modularity, predictability, and metaprogramming. Assembly targets are supported via libraries and syntactic sugar.

See [overview](docs/Overview.md) for general information.

See the [command-line guide](docs/CLI.md) for assembly commands, configuration,
configured bare commands, and shell completion.

See [samples](samples/README.md) for small `.g` source files used for testing,
experimentation, and user education.

## Building

Install [rustup](https://rustup.rs/). The repository pins its Rust toolchain
in `rust-toolchain.toml`, and the first `cargo` command installs and selects
it automatically. Run `cargo build --release` to build the `glam` binary.
Contributor and agent verification steps live in [`AGENTS.md`](AGENTS.md) and
[`docs/AgentContext.md`](docs/AgentContext.md).



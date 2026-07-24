# Command-Line Interface

This guide describes the command-line interface implemented by the current
Rust bootstrap. The configured interface is intentionally extensible, so a
project or shared configuration can present much friendlier commands than the
fixed bootstrap options.

For Glam source syntax, see [SyntaxCheatSheet.md](SyntaxCheatSheet.md).

## Quick start

Assemble one file and write its `asm.result` binary to a file:

```sh
glam --file program.g >program.bin
```

Pass binary command-line arguments to the assembly after `--`:

```sh
glam --file program.g -- first second >program.bin
```

Multiple inputs are mixed into one module. Earlier inputs override later
inputs, so the first file can specialize definitions supplied by the rest:

```sh
glam --file local.g --file platform.g --file defaults.g >program.bin
```

An inline script is one UTF-8 command-line argument. Its extension selects the
front-end compiler:

```sh
glam --script.g 'asm.result = "hello"' >hello.bin
```

Every assembly needs at least one file or script input. `asm.result` must be a
binary value. Diagnostics go to standard error; the result goes to standard
output. Glam exits unsuccessfully if the result fails or any error diagnostic
is emitted. A valid result may therefore already have been written before
later reasoning reports an error.

## Bootstrap commands

The bootstrap interface is selected whenever the first argument begins with
`-`. With no arguments, Glam prints help. `glam --help` prints the compact
synopsis and `glam --version` prints the bootstrap implementation version.

### Assembly options

| Option | Meaning |
| --- | --- |
| `-f PATH`, `--file PATH` | Add a source file. |
| `-s.EXT TEXT`, `--script.EXT TEXT` | Add an inline UTF-8 source using compiler extension `EXT`. |
| `--manifest PATH` | Write a manifest of local files and the exact bytes consumed. May appear once. |
| `--refl ARG` | Add a reflection-only argument. May be repeated. |
| `--workers N` | Use `N` shared background workers; zero disables sparks. May appear once. |
| `--` | End option parsing; all remaining arguments become `asm.args`. |

`--workers` takes priority over `GLAM_WORKERS`; if neither is present, the
worker count is zero. Configuration parsing, CLI inspection, and completion
always run with zero workers even if the resulting assembly requests workers.

`--refl` arguments are available to reflection as `process.refl_args`, but are
not included in `asm.args`. Arguments after `--` are available to the assembly
as a list of binaries at `asm.args`.

### Source inspection

```sh
glam --parse source.g
glam --parse source.g --quiet
glam --parse source.g --verbose
```

`--parse` runs only the built-in `.g` parser. It does not compile the source or
load imports. Normal output reports diagnostics and a declaration count;
`--quiet` reports only through the exit status, while `--verbose` also lists
the declarations. Inspection output is written to standard output.

### Reproducibility manifests

`--manifest PATH` records every local configuration, assembly, imported source,
and loaded binary used by the command. Each entry contains a platform path,
the `sha256` algorithm name, and the digest of the exact consumed bytes. The
manifest cannot itself be one of the inputs.

Check a manifest without running an assembly:

```sh
glam --check_manifest inputs.manifest
glam --check_manifest inputs.manifest --quiet
```

The check prints each changed or unreadable file to standard output and exits
nonzero when any entry differs. `--quiet` suppresses those lines but preserves
the exit status; it may also precede `--check_manifest`. Relative entries are
resolved from the process working folder, matching manifest creation.

If a local file changes between two reads during an assembly, assembly stops.
If it changes only after all required bytes were consumed, Glam retains the
original digest and reports a warning during the final consistency check.

## Configuration

Configuration is ordinary Glam source mixed into a configuration module. Set
`GLAM_CONF` to a file or list of files:

```sh
GLAM_CONF=project-conf.g glam --file program.g
```

If `GLAM_CONF` is unset, Glam uses an existing file at the platform default:

- Windows: `%APPDATA%\glam\conf.g`
- macOS: `$HOME/Library/Application Support/glam/conf.g`
- other Unix systems: `$XDG_CONFIG_HOME/glam/conf.g`, or
  `$HOME/.config/glam/conf.g` when `XDG_CONFIG_HOME` is unset

If that file does not exist, the configuration is empty. Setting `GLAM_CONF`
to an empty path list also explicitly selects an empty configuration.

On Unix a path list is colon-separated; on Windows it is semicolon-separated.
Empty entries are ignored. Earlier configuration files override later files.

The conventional exported values are:

| Name | Role |
| --- | --- |
| `conf.env` | An object supplied to the assembly as its top-level `env`. The default is empty object. |
| `conf.cli` | A configured parser and command-plan writer for bare commands. Missing means `.fail`. |
| `conf.log` | The configured diagnostic logger. |
| `conf.completion_script.NAME` | An optional completion adapter generator named `NAME`. |

The remaining sections focus on `conf.cli`.

## Configured bare commands

When the first argument does not begin with `-`, Glam runs `conf.cli` over the
entire argument list. There is no fallback to the bootstrap parser and no
recursive rewriting. For example, a configuration may translate
`glam build program.g` into the equivalent of `glam --file program.g`.

A small configuration:

```g

language g0
import 'std

object conf.env

conf.cli =
    .alt
        (.case
            {usage:"build FILE", summary:"Assemble a readable source file"}
            (.read.keyword "build" =>>
             .read.path 'file 'r >>= (\file ->
             .read.end =>>
             .write.file file)))
        (.case
            {usage:"eval SOURCE", summary:"Assemble inline .g source"}
            (.read.keyword "eval" =>>
             .read.text "Glam source" >>= (\source ->
             .read.end =>>
             .write.script "g" source)))
```

The selected `conf.cli` branch must:

- consume every user argument;
- return unit `()`;
- produce at least one file or script input; and
- produce a valid command plan, with at most one manifest and worker count.

Processing by `conf.cli` is single threaded.

### Why `conf.cli` is an effect

Effects express sequencing of commands separately from their interpretations.
In this case, we have the standard effects, but we also have several for
reading command-line arguments and writing commands. By interpreting these
sequences in different ways, we can support rewriting, diagnostics, and 
tab completions.

### Argument readers

Regardless of host, we'll always present arguments as UTF-8 for parsing.

| Effect | Result | Behavior |
| --- | --- | --- |
| `.read.keyword Text` | `()` | Consume one argument exactly equal to `Text`. |
| `.read.text Expectation` | `Text` | Consume any one argument. |
| `.read.token Expectation TokenParser` | parser result | Parse one argument internally. |
| `.read.path Kind Mode` | opaque path | Consume and preflight a platform path. |
| `.read.end` | `()` | Succeed only when every argument is consumed. |

`Expectation` is a short user-facing label used in failures and completions.

Path kinds are `'file`, `'folder`, and `'any`; modes are `'r` and `'w`.
Read mode requires an existing usable target. Write mode accepts a usable
existing target or a missing target with a usable containing folder. These are
non-reserving preflight checks: later file operations remain authoritative.

The returned path is opaque, preserves non-UTF-8 platform paths, and belongs to
one CLI invocation. It can only be passed to a compatible path writer:

- `.write.file` requires `.read.path 'file 'r`;
- `.write.manifest` requires `.read.path 'file 'w`.

### Token parsers

A token parser consumes characters within one argument. `.read.token` requires
the parser to consume that argument completely.

| Effect | Result | Behavior |
| --- | --- | --- |
| `.token.text Text` | `()` | Consume exact literal text. |
| `.token.regex Regex` | `{span:Text}` | Consume a nonempty or empty span matched at the current cursor. |
| `.token.any` | `Text` | Consume one Unicode scalar value. |
| `.token.end` | `()` | Succeed only at the end of the argument. |

`.token.regex` currently uses `regex-lite` syntax and forbids explicit capture
groups. Its `span` field is the whole matched text; the record leaves room for
future capture metadata without changing the reader's outer result shape.
Regex alternatives follow the engine's leftmost-first preference. Literal
token readers can enumerate completion candidates, while a general regex
contributes an expectation but does not enumerate its language.

Token parsing is its own restricted effect context. It has fresh task-local
state for each `.read.token`, isolated from `conf.cli` and from other token
parses. CLI readers and writers, `.env`, `.log`, heap effects, and task effects
cannot be used from inside a token parser.

### Command-plan writers

| Effect | Behavior |
| --- | --- |
| `.write.file Path` | Add the readable file represented by `Path`. |
| `.write.script Extension Text` | Add one UTF-8 inline source. |
| `.write.manifest Path` | Select one manifest output path. |
| `.write.refl_arg Text` | Add one reflection-only argument. |
| `.write.assembly_arg Text` | Add one argument to `asm.args`. |
| `.write.worker_count Number` | Select a supported nonnegative worker count. |

Order is retained within repeated inputs and argument writers. Text argument
writers cannot represent non-UTF-8 arguments; the fixed bootstrap interface
can preserve those platform bytes.

### Explanations and diagnostics

Wrap a parser branch with `.case Explain Parse` to associate structured help
with it without changing the branch's parse semantics:

```g
.case
    {usage:"build FILE", summary:"Assemble one source file"}
    (.read.keyword "build" =>> ...)
```

`Explain` may be plain text or any value. The default failure renderer
recognizes textual `usage`, `summary`, and `details` fields. The original values
remain available as `cli.cases` in the structured diagnostic and are also
attached to library-level completion information. Explanations stay lazy on a
successful command construction.

`.case` does not automatically generate a `--help` command. Configurations
can build their own higher-level case and help functions.

`.log Severity Message` emits a structured diagnostic transactionally. Logs
from the selected branch are published; logs from abandoned branches are not.
Logging does not itself make a parse branch fail.

### Read-only environment

`.env Path` reads immutable host-provided environment data. During CLI parsing,
the useful paths include:

| Path | Value |
| --- | --- |
| `'.process.cli.args` | Original user-provided argument binaries. |
| `'.process.env.[NAME]` | One operating-system environment value; `NAME` is text. |

For example, `.env '.process.env.["HOME"]` reads the platform bytes of
`HOME`. The latent `process.args` and `process.refl_args` values depend on the
CLI parser's final output and should not be observed during CLI parsing.

After selection, reflection sees the original arguments at `process.cli.args`,
the canonical interpreted arguments at `process.args`, and reflection-only
arguments at `process.refl_args`. The assembly receives only the arguments
written for it at `asm.args`.

## Inspecting configured rewriting

Inspect a bare command without running its assembly:

```sh
glam --parse_cli build program.g
```

The output is the canonical bootstrap-style command with each argument labeled
`[N]:`. Embedded newlines continue on lines indented by two spaces:

```text
[1]: --script.g
[2]: asm.result = "hello"
  ++ " world"
```

This is intended for people and deliberately has no escaping convention. For
machine-safe argument boundaries, use NUL-terminated output:

```sh
glam --parse_cli.0 build program.g
```

Both forms load the configuration and run the same `conf.cli` search used by a
real bare command. They do not execute the resulting command, start workers,
or activate the configured logger. The first inspected argument must be bare.

## Completion integration

Glam includes minimal Bash and Zsh adapters. Install one for the current shell
session with:

```sh
eval "$(glam --completion_script bash)"
eval "$(glam --completion_script zsh)"
```

For persistent setup, place the corresponding command in the shell's startup
file. The generated script calls Glam for each request; it is printed, never
installed or sourced automatically.

A configuration can override a built-in adapter or add another binding at
`conf.completion_script.NAME`. It must be a function from this context:

```g
{executable:Binary, protocol:"v0", request:"--completions"}
```

to a binary script. Configured bindings are checked before the built-in `bash`
and `zsh` bindings. A binding name must be nonempty UTF-8 without `.`.

### Completion protocol v0

Other shells and editors can invoke the shell-neutral request directly:

```text
glam --completions v0 MODE BEFORE-N AFTER-N PAYLOAD...
```

`MODE` is:

- `active`: payload is the `BEFORE-N` complete arguments, active prefix, active
  suffix, then the `AFTER-N` complete arguments;
- `absent`: payload contains only the complete arguments before and after an
  insertion point.

Counts are canonical nonnegative decimal integers and payload arity is exact.
Each completion result is a complete replacement argument in platform bytes,
followed by NUL. No candidates is a successful empty response. Diagnostics go
to standard error and failures return a nonzero exit status.

The request does not infer completion mode from shell environment variables.
Adapters are responsible for translating their editor's cursor model into an
explicit prefix and suffix. Completion never executes a command plan or starts
workers. Option-leading commands use bootstrap completion; bare commands use
`conf.cli`. A missing `conf.cli` therefore has no configured candidates.

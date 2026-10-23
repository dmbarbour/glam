//! Restricted parser for structured contents of one UTF-8 command-line token.
//! The library runs it by a pure handler (`pure-handler-token`); here, its
//! `.token.*` requests are registered only so that one escaping `.read.token`
//! is an error.

use glam::reflection::EffectRequestSpec;

pub(in crate::command_line) fn request_specs() -> Vec<EffectRequestSpec<()>> {
    [
        ("text", 1),
        ("regex", 1),
        ("text_span", 0),
        ("any", 0),
        ("end", 0),
    ]
    .into_iter()
    .map(|(name, arity)| {
        EffectRequestSpec::at_path(
            ["token", name],
            ["cli_token_runtime", "v0", "request", name],
            arity,
            (),
        )
    })
    .collect()
}

use regex_lite::{Regex, RegexBuilder};

const MAX_PATTERN_BYTES: usize = 16 * 1024;
const MAX_NESTING: u32 = 64;
const MAX_COMPILED_BYTES: usize = 1024 * 1024;

pub(super) fn compile(pattern: &str) -> Result<Regex, String> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!(
            "`.token.regex` pattern exceeds the supported {MAX_PATTERN_BYTES}-byte limit"
        ));
    }
    let mut builder = RegexBuilder::new(pattern);
    builder
        .nest_limit(MAX_NESTING)
        .size_limit(MAX_COMPILED_BYTES);
    let regex = builder
        .build()
        .map_err(|error| format!("invalid `.token.regex` pattern: {error}"))?;
    if regex.capture_names().count() != 1 {
        return Err("`.token.regex` does not permit capture groups".to_owned());
    }
    Ok(regex)
}

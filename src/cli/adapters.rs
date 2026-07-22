pub const BUILTIN_COMPLETION_SCRIPTS: &[&str] = &["bash", "zsh"];

pub fn builtin_completion_script(name: &str) -> Option<&'static str> {
    match name {
        "bash" => Some(BASH),
        "zsh" => Some(ZSH),
        _ => None,
    }
}

const BASH: &str = r#"# Minimal glam completion adapter for Bash.
_glam_complete() {
    local before_count=$((COMP_CWORD - 1))
    local after_count=$((${#COMP_WORDS[@]} - COMP_CWORD - 1))
    local current=${COMP_WORDS[COMP_CWORD]-}
    local -a before=() after=()
    if ((before_count > 0)); then
        before=("${COMP_WORDS[@]:1:before_count}")
    fi
    if ((after_count > 0)); then
        after=("${COMP_WORDS[@]:COMP_CWORD+1:after_count}")
    fi
    COMPREPLY=()
    while IFS= read -r -d '' candidate; do
        COMPREPLY+=("$candidate")
    done < <(command glam --completions v0 active "$before_count" "$after_count" \
        "${before[@]}" "$current" "" "${after[@]}")
}
complete -F _glam_complete glam
"#;

const ZSH: &str = r#"#compdef glam
# Minimal glam completion adapter for Zsh.
_glam() {
    local -a before after candidates
    local candidate
    integer before_count=$((CURRENT - 2))
    integer after_count=$((${#words} - CURRENT))
    integer index
    for ((index = 2; index < CURRENT; index++)); do
        before+=("${words[index]}")
    done
    for ((index = CURRENT + 1; index <= ${#words}; index++)); do
        after+=("${words[index]}")
    done
    while IFS= read -r -d $'\0' candidate; do
        candidates+=("$candidate")
    done < <(command glam --completions v0 active "$before_count" "$after_count" \
        "${before[@]}" "$PREFIX" "$SUFFIX" "${after[@]}")
    compadd -Q -- "${candidates[@]}"
}
compdef _glam glam
"#;

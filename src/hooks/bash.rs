use crate::config;

/// Generate the bash hook that sources the cache on every directory change.
pub fn generate() -> String {
    let cache = config::cache_path();
    let cache_str = cache.to_string_lossy();

    format!(
        r#"# aitch — bash hook
# Add to ~/.bashrc: eval "$(aitch hook bash)"

_aitch_hook() {{
  if [ -f '{}' ]; then
    source '{}'
  fi
}}

# Prepend to PROMPT_COMMAND
if [[ "$PROMPT_COMMAND" != *"_aitch_hook"* ]]; then
  PROMPT_COMMAND="_aitch_hook;${{PROMPT_COMMAND:-}}"
fi

# Run once on shell startup
_aitch_hook
"#,
        cache_str, cache_str
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_bash_hook() {
        let hook = generate();
        assert!(hook.contains("PROMPT_COMMAND"));
        assert!(hook.contains("_aitch_hook"));
        assert!(hook.contains("source"));
        assert!(hook.contains(".aitch/cache"));
    }
}

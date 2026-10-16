use crate::config;

/// Generate the bash hook that sources the cache on every directory change. With a route, `claude` goes through
/// `aitch launch`, which picks the folder once per start.
pub fn generate(routed: bool) -> String {
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
{}"#,
        cache_str,
        cache_str,
        if routed {
            "\n# A route picks the Claude Code folder at launch\nunalias claude 2>/dev/null || true\nfunction claude { command aitch launch claude -- \"$@\"; }\n"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_bash_hook() {
        let hook = generate(false);
        assert!(hook.contains("PROMPT_COMMAND"));
        assert!(hook.contains("_aitch_hook"));
        assert!(hook.contains("source"));
        assert!(hook.contains(".aitch/cache"));
    }
}

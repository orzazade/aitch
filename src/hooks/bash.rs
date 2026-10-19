use crate::config;
use std::path::Path;

/// Generate the bash hook that sources the cache on every directory change. With a route, aitch's `claude` shim
/// comes first on PATH and picks the folder once per start.
pub fn generate(shim_dir: Option<&Path>) -> String {
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
        super::claude_shim(shim_dir)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_bash_hook() {
        let hook = generate(None);
        assert!(hook.contains("PROMPT_COMMAND"));
        assert!(hook.contains("_aitch_hook"));
        assert!(hook.contains("source"));
        assert!(hook.contains(".aitch/cache"));
    }
}

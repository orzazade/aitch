use crate::config;

/// Generate the zsh hook that sources the cache on every directory change. With a route, `claude` goes through
/// `aitch launch`, which picks the folder once per start.
pub fn generate(routed: bool) -> String {
    let cache = config::cache_path();
    let cache_str = cache.to_string_lossy();

    format!(
        r#"# aitch — zsh hook
# Add to ~/.zshrc: eval "$(aitch hook zsh)"

_aitch_hook() {{
  if [ -f '{}' ]; then
    source '{}'
  fi
}}

autoload -Uz add-zsh-hook
add-zsh-hook chpwd _aitch_hook

# Run once on shell startup
_aitch_hook
{}"#,
        cache_str,
        cache_str,
        if routed {
            "\n# A route picks the Claude Code folder at launch\nunalias claude 2>/dev/null\nfunction claude { command aitch launch claude -- \"$@\"; }\n"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_zsh_hook() {
        let hook = generate(false);
        assert!(hook.contains("add-zsh-hook chpwd"));
        assert!(hook.contains("_aitch_hook"));
        assert!(hook.contains("source"));
        assert!(hook.contains(".aitch/cache"));
    }
}

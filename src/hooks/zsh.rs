use crate::config;

/// Generate the zsh hook that sources the cache on every directory change.
pub fn generate() -> String {
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
"#,
        cache_str, cache_str
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_zsh_hook() {
        let hook = generate();
        assert!(hook.contains("add-zsh-hook chpwd"));
        assert!(hook.contains("_aitch_hook"));
        assert!(hook.contains("source"));
        assert!(hook.contains(".aitch/cache"));
    }
}

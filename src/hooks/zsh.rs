use crate::config;
use std::path::Path;

/// Generate the zsh hook that sources the cache on every directory change. With a route, aitch's `claude` shim
/// comes first on PATH and picks the folder once per start.
pub fn generate(shim_dir: Option<&Path>) -> String {
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
        super::claude_shim(shim_dir)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_valid_zsh_hook() {
        let hook = generate(None);
        assert!(hook.contains("add-zsh-hook chpwd"));
        assert!(hook.contains("_aitch_hook"));
        assert!(hook.contains("source"));
        assert!(hook.contains(".aitch/cache"));
    }
}

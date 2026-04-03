/// Built-in AI provider definitions.
/// Each provider maps to an environment variable that controls its config directory.

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Provider {
    pub key: &'static str,
    pub env_var: &'static str,
    pub name: &'static str,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        key: "claude",
        env_var: "CLAUDE_CONFIG_DIR",
        name: "Claude Code",
    },
    Provider {
        key: "codex",
        env_var: "CODEX_CONFIG_DIR",
        name: "OpenAI Codex",
    },
    Provider {
        key: "gemini",
        env_var: "GEMINI_CONFIG_DIR",
        name: "Google Gemini",
    },
];

pub fn get_provider(key: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.key == key)
}

pub fn get_env_var(key: &str) -> Option<&'static str> {
    get_provider(key).map(|p| p.env_var)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_known_provider() {
        let p = get_provider("claude").unwrap();
        assert_eq!(p.env_var, "CLAUDE_CONFIG_DIR");
        assert_eq!(p.name, "Claude Code");
    }

    #[test]
    fn get_unknown_provider() {
        assert!(get_provider("unknown").is_none());
    }

    #[test]
    fn get_env_var_for_codex() {
        assert_eq!(get_env_var("codex"), Some("CODEX_CONFIG_DIR"));
    }
}

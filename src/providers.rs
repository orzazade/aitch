use std::ffi::OsStr;
use std::path::Path;

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
        env_var: "CODEX_HOME",
        name: "OpenAI Codex",
    },
    Provider {
        key: "gemini",
        env_var: "GEMINI_CLI_HOME",
        name: "Google Gemini",
    },
];

pub fn get_provider(key: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.key == key)
}

pub fn get_env_var(key: &str) -> Option<&'static str> {
    get_provider(key).map(|p| p.env_var)
}

/// The value exported for a provider's folder. GEMINI_CLI_HOME names the folder that holds `.gemini`, so a path
/// to the `.gemini` folder itself is exported as its parent.
pub fn env_value(key: &str, path: &str) -> String {
    let p = Path::new(path);
    match p.parent() {
        Some(parent)
            if key == "gemini"
                && p.file_name() == Some(OsStr::new(".gemini"))
                && !parent.as_os_str().is_empty() =>
        {
            parent.to_string_lossy().to_string()
        }
        _ => path.to_string(),
    }
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
        assert_eq!(get_env_var("codex"), Some("CODEX_HOME"));
    }

    #[test]
    fn get_env_var_for_gemini() {
        assert_eq!(get_env_var("gemini"), Some("GEMINI_CLI_HOME"));
    }

    #[test]
    fn env_value_maps_a_dot_gemini_path_to_its_parent() {
        assert_eq!(env_value("gemini", "/h/work/.gemini"), "/h/work");
        assert_eq!(env_value("gemini", "/h/work/.gemini/"), "/h/work");
        assert_eq!(env_value("gemini", "~/.gemini"), "~");
        assert_eq!(env_value("gemini", "/.gemini"), "/");
        assert_eq!(env_value("gemini", ".gemini"), ".gemini");
        assert_eq!(env_value("gemini", "/h/gemini-work"), "/h/gemini-work");
        assert_eq!(env_value("codex", "/h/.codex"), "/h/.codex");
        assert_eq!(env_value("claude", "/h/work/.gemini"), "/h/work/.gemini");
    }
}

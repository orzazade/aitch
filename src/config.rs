use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[allow(dead_code)]
    #[error("config file not found at {0}")]
    NotFound(PathBuf),
    #[error("failed to read config: {0}")]
    ReadError(#[from] std::io::Error),
    #[error("invalid YAML in config: {0}")]
    ParseError(#[from] serde_yaml::Error),
    #[error("invalid profile name '{0}': must be alphanumeric, hyphens, or underscores")]
    InvalidProfileName(String),
    #[error("invalid path '{0}': contains shell metacharacters")]
    UnsafePath(String),
    #[error("profile '{0}': {1}")]
    BadRoute(String, String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    #[serde(default)]
    pub providers: BTreeMap<String, String>,
    #[serde(default)]
    pub directories: Vec<String>,
    #[serde(default)]
    pub git_remotes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Routes>,
}

/// Launch-time routes, per provider. Only Claude Code has one.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Routes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<Route>,
}

/// Start Claude Code on `primary` while the `quota` command says its account has room, else on the profile's own
/// `providers.claude` dir (the fallback). The org ids are optional login guards, checked against each dir's
/// `.claude.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub primary: String,
    pub quota: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_org: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_org: Option<String>,
}

/// Returns the default config directory: ~/.aitch/
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".aitch")
}

/// Returns the config file path: ~/.aitch/config.yaml
pub fn config_path() -> PathBuf {
    config_dir().join("config.yaml")
}

/// Returns the cache file path: ~/.aitch/cache
pub fn cache_path() -> PathBuf {
    config_dir().join("cache")
}

/// Validate a profile name: alphanumeric, hyphens, underscores only.
pub fn validate_profile_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ConfigError::InvalidProfileName(name.to_string()));
    }
    Ok(())
}

/// Validate a path for shell safety: reject shell metacharacters.
pub fn validate_path(path: &str) -> Result<(), ConfigError> {
    let forbidden = [
        '`', '$', '(', ')', ';', '&', '|', '>', '<', '\n', '\r', '\0',
    ];
    if path.chars().any(|c| forbidden.contains(&c)) {
        return Err(ConfigError::UnsafePath(path.to_string()));
    }
    Ok(())
}

/// Load config from disk. Returns default config if file doesn't exist.
pub fn load_config() -> Result<Config, ConfigError> {
    load_config_from(&config_path())
}

pub fn load_config_from(path: &Path) -> Result<Config, ConfigError> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(Config::default());
    }
    let config: Config = serde_yaml::from_str(&contents)?;
    check_routes(&config)?;
    Ok(config)
}

/// A route needs every field and the profile's own Claude folder to fall back to.
fn check_routes(config: &Config) -> Result<(), ConfigError> {
    for (name, profile) in &config.profiles {
        let Some(route) = profile.route.as_ref().and_then(|r| r.claude.as_ref()) else {
            continue;
        };
        validate_profile_name(name)?;
        let bad = |why: &str| ConfigError::BadRoute(name.clone(), why.to_string());
        for (field, value) in [
            ("primary", Some(&route.primary)),
            ("quota", Some(&route.quota)),
            ("primary_org", route.primary_org.as_ref()),
            ("fallback_org", route.fallback_org.as_ref()),
        ] {
            if value.is_some_and(|v| v.trim().is_empty()) {
                return Err(bad(&format!("route.claude.{field} is empty")));
            }
        }
        if profile
            .providers
            .get("claude")
            .is_none_or(|d| d.trim().is_empty())
        {
            return Err(bad(
                "route.claude needs providers.claude, the dir to fall back to",
            ));
        }
    }
    Ok(())
}

/// Save config to disk. Creates the config directory if needed.
pub fn save_config(config: &Config) -> Result<(), ConfigError> {
    save_config_to(config, &config_path())
}

pub fn save_config_to(config: &Config, path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(config)?;
    fs::write(path, yaml)?;
    Ok(())
}

/// Add or update a profile in the config.
#[allow(dead_code)]
pub fn add_profile(config: &mut Config, name: &str, profile: Profile) -> Result<(), ConfigError> {
    validate_profile_name(name)?;
    for dir in &profile.directories {
        validate_path(dir)?;
    }
    for provider_path in profile.providers.values() {
        validate_path(provider_path)?;
    }
    config.profiles.insert(name.to_string(), profile);
    Ok(())
}

/// Remove a profile from the config. Returns true if it existed.
pub fn remove_profile(config: &mut Config, name: &str) -> bool {
    config.profiles.remove(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_good_profile_names() {
        assert!(validate_profile_name("work").is_ok());
        assert!(validate_profile_name("my-personal").is_ok());
        assert!(validate_profile_name("test_env").is_ok());
        assert!(validate_profile_name("prod123").is_ok());
    }

    #[test]
    fn validate_bad_profile_names() {
        assert!(validate_profile_name("").is_err());
        assert!(validate_profile_name("has space").is_err());
        assert!(validate_profile_name("has;semi").is_err());
        assert!(validate_profile_name("$(inject)").is_err());
    }

    #[test]
    fn validate_safe_paths() {
        assert!(validate_path("/Users/test/Projects").is_ok());
        assert!(validate_path("~/Projects/my project").is_ok());
        assert!(validate_path("/home/user/.claude-work").is_ok());
    }

    #[test]
    fn validate_unsafe_paths() {
        assert!(validate_path("$(rm -rf ~)").is_err());
        assert!(validate_path("`evil`").is_err());
        assert!(validate_path("/tmp;rm -rf /").is_err());
        assert!(validate_path("path|pipe").is_err());
        assert!(validate_path("path&bg").is_err());
    }

    #[test]
    fn load_missing_config_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.yaml");
        let config = load_config_from(&path).unwrap();
        assert!(config.profiles.is_empty());
        assert!(config.default.is_none());
    }

    #[test]
    fn load_empty_config_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, "").unwrap();
        let config = load_config_from(&path).unwrap();
        assert!(config.profiles.is_empty());
    }

    #[test]
    fn roundtrip_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.yaml");

        let mut config = Config {
            default: Some("personal".to_string()),
            ..Default::default()
        };

        let mut profile = Profile::default();
        profile
            .providers
            .insert("claude".to_string(), "~/.claude-work".to_string());
        profile.directories.push("~/Projects/acme".to_string());
        profile.git_remotes.push("github.com/acme/*".to_string());

        add_profile(&mut config, "work", profile).unwrap();
        save_config_to(&config, &path).unwrap();

        let loaded = load_config_from(&path).unwrap();
        assert_eq!(loaded.default, Some("personal".to_string()));
        assert!(loaded.profiles.contains_key("work"));
        assert_eq!(
            loaded.profiles["work"].providers["claude"],
            "~/.claude-work"
        );
    }

    #[test]
    fn malformed_yaml_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.yaml");
        fs::write(&path, "{{invalid yaml: [}").unwrap();
        assert!(load_config_from(&path).is_err());
    }

    #[test]
    fn route_roundtrips_and_is_optional() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.yaml");
        let yaml = "profiles:\n  work:\n    providers: { claude: /w }\n    route:\n      claude:\n        primary: /m\n        primary_org: a\n        fallback_org: b\n        quota: q --room\n  personal: {}\n";
        fs::write(&path, yaml).unwrap();
        let cfg = load_config_from(&path).unwrap();
        let r = cfg.profiles["work"]
            .route
            .as_ref()
            .unwrap()
            .claude
            .as_ref()
            .unwrap();
        assert_eq!((r.primary.as_str(), r.quota.as_str()), ("/m", "q --room"));
        assert!(cfg.profiles["personal"].route.is_none());
        save_config_to(&cfg, &path).unwrap();
        let again = load_config_from(&path).unwrap();
        assert_eq!(
            again.profiles["work"]
                .route
                .as_ref()
                .unwrap()
                .claude
                .as_ref()
                .unwrap()
                .primary_org
                .as_deref(),
            Some("a")
        );
        assert!(!fs::read_to_string(&path)
            .unwrap()
            .contains("personal:\n    route"));
    }

    #[test]
    fn route_with_missing_or_empty_field_fails_at_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.yaml");
        let base = "profiles:\n  work:\n    providers: { claude: /w }\n    route:\n      claude:\n        primary: /m\n";
        fs::write(&path, base).unwrap();
        assert!(load_config_from(&path).is_err(), "quota missing");
        fs::write(&path, format!("{base}        quota: ''\n")).unwrap();
        assert!(load_config_from(&path).is_err(), "quota empty");
        let no_provider =
            base.replace("    providers: { claude: /w }\n", "") + "        quota: q\n";
        fs::write(&path, no_provider).unwrap();
        assert!(load_config_from(&path).is_err(), "no providers.claude");
        fs::write(&path, format!("{base}        quota: q\n")).unwrap();
        assert!(load_config_from(&path).is_ok(), "org guards are optional");
        fs::write(
            &path,
            format!("{base}        quota: q\n        fallback_org: ''\n"),
        )
        .unwrap();
        assert!(load_config_from(&path).is_err(), "org given but empty");
        fs::write(&path, format!("{base}        quota: q\n        typo: 1\n")).unwrap();
        assert!(load_config_from(&path).is_err(), "unknown field");
        // The name becomes part of a file path for the launch.
        let named =
            |n: &str| format!("{base}        quota: q\n").replace("  work:", &format!("  '{n}':"));
        fs::write(&path, named("../../evil")).unwrap();
        assert!(load_config_from(&path).is_err(), "path in the profile name");
        fs::write(&path, named("work-2")).unwrap();
        assert!(load_config_from(&path).is_ok(), "plain name");
    }

    #[test]
    fn add_and_remove_profile() {
        let mut config = Config::default();
        let profile = Profile::default();
        add_profile(&mut config, "test", profile).unwrap();
        assert!(config.profiles.contains_key("test"));
        assert!(remove_profile(&mut config, "test"));
        assert!(!config.profiles.contains_key("test"));
        assert!(!remove_profile(&mut config, "test"));
    }

    #[test]
    fn reject_unsafe_profile_in_add() {
        let mut config = Config::default();
        let mut profile = Profile::default();
        profile
            .providers
            .insert("claude".to_string(), "$(evil)".to_string());
        assert!(add_profile(&mut config, "test", profile).is_err());
    }
}

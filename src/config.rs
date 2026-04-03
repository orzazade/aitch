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
    let forbidden = ['`', '$', '(', ')', ';', '&', '|', '>', '<', '\n', '\r', '\0'];
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
    Ok(config)
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
    for (_, provider_path) in &profile.providers {
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

        let mut config = Config::default();
        config.default = Some("personal".to_string());

        let mut profile = Profile::default();
        profile
            .providers
            .insert("claude".to_string(), "~/.claude-work".to_string());
        profile
            .directories
            .push("~/Projects/appxite".to_string());
        profile
            .git_remotes
            .push("github.com/appxite/*".to_string());

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

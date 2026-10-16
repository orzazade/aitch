use crate::config::{Config, Profile};
use std::path::Path;
use std::process::Command;

/// The rule that matched during resolution.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchRule {
    AitchFile { path: String, profile: String },
    DirectoryMatch { pattern: String },
    GitRemote { remote: String, pattern: String },
    Default,
    None,
}

/// Result of resolving the current directory to a profile.
#[derive(Debug, Clone)]
pub struct ResolveResult {
    pub profile_name: Option<String>,
    pub profile: Option<Profile>,
    pub rule: MatchRule,
}

/// Resolve the current directory to a profile using the priority chain:
/// 1. .aitch file (walk up from pwd to root)
/// 2. Directory prefix match
/// 3. Git remote pattern match
/// 4. Default profile
pub fn resolve(pwd: &str, config: &Config) -> ResolveResult {
    // Priority 1: .aitch file
    if let Some((file_path, profile_name)) = find_aitch_file(pwd) {
        if let Some(profile) = config.profiles.get(&profile_name) {
            return ResolveResult {
                profile_name: Some(profile_name.clone()),
                profile: Some(profile.clone()),
                rule: MatchRule::AitchFile {
                    path: file_path,
                    profile: profile_name,
                },
            };
        }
        // .aitch file references unknown profile, continue to next priority
    }

    // Priority 2: Directory prefix match (case-insensitive for macOS)
    let pwd_lower = pwd.to_lowercase();
    for (name, profile) in &config.profiles {
        for dir in &profile.directories {
            // The folder itself or anything inside it; a sibling that only shares the
            // prefix (acme-ideas for acme) is not a match.
            let dir_lower = expand_tilde(dir).trim_end_matches('/').to_lowercase();
            if pwd_lower == dir_lower || pwd_lower.starts_with(&format!("{}/", dir_lower)) {
                return ResolveResult {
                    profile_name: Some(name.clone()),
                    profile: Some(profile.clone()),
                    rule: MatchRule::DirectoryMatch {
                        pattern: dir.clone(),
                    },
                };
            }
        }
    }

    // Priority 3: Git remote match
    if let Some(remote) = get_git_remote(pwd) {
        for (name, profile) in &config.profiles {
            for pattern in &profile.git_remotes {
                if remote_matches(&remote, pattern) {
                    return ResolveResult {
                        profile_name: Some(name.clone()),
                        profile: Some(profile.clone()),
                        rule: MatchRule::GitRemote {
                            remote: remote.clone(),
                            pattern: pattern.clone(),
                        },
                    };
                }
            }
        }
    }

    // Priority 4: Default profile
    if let Some(default_name) = &config.default {
        if let Some(profile) = config.profiles.get(default_name) {
            return ResolveResult {
                profile_name: Some(default_name.clone()),
                profile: Some(profile.clone()),
                rule: MatchRule::Default,
            };
        }
    }

    // No match
    ResolveResult {
        profile_name: None,
        profile: None,
        rule: MatchRule::None,
    }
}

/// Walk up from pwd looking for a .aitch file.
/// Returns (file_path, profile_name) if found.
pub fn find_aitch_file(pwd: &str) -> Option<(String, String)> {
    let mut dir = Path::new(pwd);
    loop {
        let aitch_file = dir.join(".aitch");
        if aitch_file.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&aitch_file) {
                let profile = contents.trim().to_string();
                if !profile.is_empty() {
                    return Some((aitch_file.to_string_lossy().to_string(), profile));
                }
            }
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => break,
        }
    }
    None
}

/// Get the git remote URL for a directory.
fn get_git_remote(pwd: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(pwd)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Check if a git remote URL matches a pattern.
/// Pattern format: "github.com/org/*" matches any remote containing "github.com/org/"
/// Handles both SSH (github.com:org/) and HTTPS (github.com/org/) formats
/// by normalizing "host:" to "host/" before matching.
fn remote_matches(remote: &str, pattern: &str) -> bool {
    let normalized = normalize_remote(remote);
    let normalized_pattern = normalize_remote(pattern);
    if let Some(prefix) = normalized_pattern.strip_suffix('*') {
        normalized.contains(prefix)
    } else {
        normalized.contains(&normalized_pattern)
    }
}

/// Normalize a git remote URL: replace "host:" with "host/" for SSH URLs.
fn normalize_remote(url: &str) -> String {
    // git@github.com:org/repo.git -> git@github.com/org/repo.git
    if let Some(at_pos) = url.find('@') {
        if let Some(colon_pos) = url[at_pos..].find(':') {
            let colon_abs = at_pos + colon_pos;
            // Only replace if it's not a port number (no // before it)
            if !url[..colon_abs].ends_with('/') {
                let mut normalized = url.to_string();
                normalized.replace_range(colon_abs..colon_abs + 1, "/");
                return normalized;
            }
        }
    }
    url.to_string()
}

/// Expand ~ to home directory in a path string.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Profile};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn test_config() -> Config {
        let mut profiles = BTreeMap::new();

        let mut work = Profile::default();
        work.providers
            .insert("claude".to_string(), "/tmp/claude-work".to_string());
        work.directories.push("/tmp/projects/acme".to_string());
        work.git_remotes.push("github.com/acme/*".to_string());

        let mut personal = Profile::default();
        personal
            .providers
            .insert("claude".to_string(), "/tmp/claude-personal".to_string());
        personal
            .directories
            .push("/tmp/projects/personal".to_string());

        profiles.insert("work".to_string(), work);
        profiles.insert("personal".to_string(), personal);

        Config {
            default: Some("personal".to_string()),
            profiles,
        }
    }

    #[test]
    fn resolve_directory_match() {
        let config = test_config();
        let result = resolve("/tmp/projects/acme/subdir", &config);
        assert_eq!(result.profile_name, Some("work".to_string()));
        assert!(matches!(result.rule, MatchRule::DirectoryMatch { .. }));
    }

    #[test]
    fn resolve_directory_match_case_insensitive() {
        let config = test_config();
        let result = resolve("/tmp/projects/Acme/subdir", &config);
        assert_eq!(result.profile_name, Some("work".to_string()));
        assert!(matches!(result.rule, MatchRule::DirectoryMatch { .. }));
    }

    #[test]
    fn resolve_default_fallback() {
        let config = test_config();
        let result = resolve("/tmp/unrelated/dir", &config);
        assert_eq!(result.profile_name, Some("personal".to_string()));
        assert_eq!(result.rule, MatchRule::Default);
    }

    #[test]
    fn resolve_no_match_no_default() {
        let config = Config {
            default: None,
            profiles: BTreeMap::new(),
        };
        let result = resolve("/tmp/anything", &config);
        assert!(result.profile_name.is_none());
        assert_eq!(result.rule, MatchRule::None);
    }

    #[test]
    fn resolve_aitch_file() {
        let dir = TempDir::new().unwrap();
        let aitch_path = dir.path().join(".aitch");
        std::fs::write(&aitch_path, "work\n").unwrap();

        let config = test_config();
        let result = resolve(dir.path().to_str().unwrap(), &config);
        assert_eq!(result.profile_name, Some("work".to_string()));
        assert!(matches!(result.rule, MatchRule::AitchFile { .. }));
    }

    #[test]
    fn resolve_aitch_file_unknown_profile() {
        let dir = TempDir::new().unwrap();
        let aitch_path = dir.path().join(".aitch");
        std::fs::write(&aitch_path, "nonexistent\n").unwrap();

        let config = test_config();
        let result = resolve(dir.path().to_str().unwrap(), &config);
        // Should fall through to default since profile doesn't exist
        assert_eq!(result.profile_name, Some("personal".to_string()));
    }

    #[test]
    fn remote_matches_wildcard() {
        assert!(remote_matches(
            "git@github.com:acme/repo.git",
            "github.com/acme/*"
        ));
        assert!(remote_matches(
            "https://github.com/acme/repo.git",
            "github.com/acme/*"
        ));
        assert!(!remote_matches(
            "git@github.com:other/repo.git",
            "github.com/acme/*"
        ));
    }

    #[test]
    fn remote_matches_exact() {
        assert!(remote_matches(
            "git@github.com:org/repo.git",
            "github.com/org/repo"
        ));
    }

    #[test]
    fn find_aitch_file_in_subdir() {
        let dir = TempDir::new().unwrap();
        let sub = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join(".aitch"), "work").unwrap();

        let result = find_aitch_file(sub.to_str().unwrap());
        assert!(result.is_some());
        let (_, profile) = result.unwrap();
        assert_eq!(profile, "work");
    }

    #[test]
    fn directory_match_stops_at_a_folder_boundary() {
        let config = test_config();
        let work = |p: &str| resolve(p, &config).profile_name.as_deref() == Some("work");
        assert!(work("/tmp/projects/acme"));
        assert!(work("/tmp/projects/acme/svc-quote"));
        assert!(work("/tmp/projects/ACME/svc-quote"));
        // A sibling folder that only shares the prefix is not inside it.
        assert!(!work("/tmp/projects/acme-ideas"));
        assert!(!work("/tmp/projects/acmenotes/x"));
    }

    #[test]
    fn expand_tilde_works() {
        let expanded = expand_tilde("~/Projects/test");
        assert!(!expanded.starts_with("~/"));
        assert!(expanded.ends_with("/Projects/test"));
    }
}

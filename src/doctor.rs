//! `aitch doctor`: every Claude Code config folder aitch knows, with its login, whether it carries what its source
//! sets (MCP servers, answers), and which MCP servers that need a sign-in have one in it. Logins are read into memory
//! only; nothing secret is printed.
use crate::config::Config;
use crate::resolve::expand_tilde;
use crate::route;
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Json = serde_json::Value;

/// Claude Code's key for a server's sign-in in the login's `mcpOAuth`: the name and the first 16 hex of sha256 of
/// `{"type","url","headers"}` as JSON (headers `{}` when absent).
pub fn oauth_key(name: &str, server: &Json) -> String {
    let headers = match server.get("headers") {
        None | Some(Json::Null) => serde_json::json!({}),
        Some(h) => h.clone(),
    };
    let id = serde_json::json!({"type": server["type"], "url": server["url"], "headers": headers});
    let hash = hex::encode(Sha256::digest(id.to_string().as_bytes()));
    format!("{name}|{}", &hash[..16])
}

/// Whether Claude Code signs in to this server with OAuth: a remote (http, sse) one with no credential header of its
/// own (a name with "auth", "key" or "token" in it) and no headers helper. Any other header leaves it to OAuth.
pub fn needs_oauth(server: &Json) -> bool {
    let remote = matches!(server["type"].as_str(), Some("http" | "sse"));
    let own_auth = server["headers"].as_object().is_some_and(|h| {
        h.keys().any(|k| {
            let k = k.to_ascii_lowercase();
            ["auth", "key", "token"].iter().any(|w| k.contains(w))
        })
    });
    remote && !own_auth && server.get("headersHelper").is_none()
}

/// The keychain item Claude Code keeps a folder's login under: named after CLAUDE_CONFIG_DIR, no suffix when unset.
fn keychain_service(dir: &Path, unset: bool) -> String {
    if unset {
        return "Claude Code-credentials".into();
    }
    let hash = hex::encode(Sha256::digest(dir.as_os_str().as_encoded_bytes()));
    format!("Claude Code-credentials-{}", &hash[..8])
}

/// The login Claude Code keeps for `dir`, parsed, in memory only: the macOS keychain item, else the folder's
/// `.credentials.json`. None: no login stored. `unset`: the folder runs with CLAUDE_CONFIG_DIR unset.
fn credentials(dir: &Path, unset: bool) -> Result<Option<Json>, String> {
    // No keychain item: Claude Code then keeps the login in the file.
    if let Some(c) = keychain(dir, unset)? {
        return Ok(Some(c));
    }
    let file = dir.join(".credentials.json");
    match fs::read_to_string(&file) {
        Ok(t) => parse(&t, &file.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", file.display())),
    }
}

/// The folder's macOS keychain item, parsed, in memory only; None when there is none (or not on macOS).
fn keychain(dir: &Path, unset: bool) -> Result<Option<Json>, String> {
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    let svc = keychain_service(dir, unset);
    let out = Command::new("security")
        .args(["find-generic-password", "-s", &svc, "-w"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run security: {e}"))?;
    match out.status.code() {
        Some(0) => parse(&String::from_utf8_lossy(&out.stdout), &svc),
        // 44: no such item.
        Some(44) => Ok(None),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!(
                "security: {}",
                err.lines().next().unwrap_or("failed")
            ))
        }
    }
}

fn parse(text: &str, what: &str) -> Result<Option<Json>, String> {
    serde_json::from_str::<Json>(text.trim())
        .map(Some)
        .map_err(|e| format!("{what} is not JSON ({e})"))
}

/// A token field that holds something.
fn has(v: &Json, field: &str) -> bool {
    v[field].as_str().is_some_and(|s| !s.is_empty())
}

/// The folders to check: every profile's claude folder and route primary, then every `~/.claude*` folder linked to
/// one of them (or with broken links). aitch starts the default folder only unset (route::set_config_dir), so one
/// path is one login item.
fn folders(cfg: &Config, home: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    let add = |out: &mut Vec<PathBuf>, dir: PathBuf| {
        if !out.iter().any(|d| route::same_config(d, &dir)) {
            out.push(dir);
        }
    };
    for p in cfg.profiles.values() {
        if let Some(v) = p.providers.get("claude") {
            if v.trim().is_empty() {
                add(&mut out, route::default_dir());
            } else {
                add(&mut out, PathBuf::from(expand_tilde(v)));
            }
        }
        if let Some(r) = p.route.as_ref().and_then(|r| r.claude.as_ref()) {
            add(&mut out, PathBuf::from(expand_tilde(&r.primary)));
        }
    }
    let mut found: Vec<PathBuf> = fs::read_dir(home)
        .map(|r| r.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    found.sort();
    for d in found {
        let name = d.file_name().map(|n| n.to_string_lossy().to_string());
        let real_dir = fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir());
        if !real_dir || !name.is_some_and(|n| n.starts_with(".claude")) {
            continue;
        }
        let known = |s: &Path| out.iter().any(|k| route::same_dir(k, s));
        match route::source_of(&d) {
            Ok(Some(s)) if known(&s) => add(&mut out, d),
            Err(_) => add(&mut out, d),
            _ => {}
        }
    }
    out
}

/// Print the report; the number of problems found.
pub fn run(cfg: &Config) -> usize {
    let home = dirs::home_dir().unwrap_or_default();
    let mut problems = 0;
    let mut bad = |line: String| {
        problems += 1;
        line.red().bold().to_string()
    };
    for dir in folders(cfg, &home) {
        let source = route::source_of(&dir);
        let role = match &source {
            Ok(None) => "source".to_string(),
            Ok(Some(s)) => format!("linked to {}", s.display()),
            Err(e) => bad(format!("LINKS BROKEN: {e}")),
        };
        println!("{} ({role})", dir.display());
        let own_read = read_json(&route::config_file(&dir));
        if let Err(e) = &own_read {
            println!("  settings: {}", bad(format!("CANNOT READ: {e}")));
        }
        let own = own_read.clone().unwrap_or(Json::Null);
        let login = credentials(&dir, route::is_default_dir(&dir));
        let line = match &login {
            Ok(Some(c))
                if has(&c["claudeAiOauth"], "accessToken")
                    || has(&c["claudeAiOauth"], "refreshToken") =>
            {
                let org = own["oauthAccount"]["organizationUuid"]
                    .as_str()
                    .unwrap_or("?");
                format!(
                    "signed in (org {})",
                    org.chars().take(8).collect::<String>()
                )
            }
            Ok(_) => bad("NOT SIGNED IN (start claude on this folder and /login)".into()),
            Err(e) => bad(format!("CANNOT READ THE LOGIN: {e}")),
        };
        println!("  login: {line}");
        // A login stored while something set CLAUDE_CONFIG_DIR to this path: a keychain item of its own (macOS).
        if cfg!(target_os = "macos") && route::is_default_dir(&dir) {
            if let Ok(Some(c)) = keychain(&dir, false) {
                if has(&c["claudeAiOauth"], "accessToken")
                    || has(&c["claudeAiOauth"], "refreshToken")
                {
                    println!(
                        "  {}",
                        format!(
                            "note: another login is kept for CLAUDE_CONFIG_DIR={}; aitch starts this folder unset and does not use it",
                            dir.display()
                        )
                        .yellow()
                    );
                }
            }
        }
        // A linked folder should carry its source's servers: check the sign-ins against those.
        let mut servers = own_read;
        if let Ok(Some(src)) = &source {
            let line = match route::sync_preview(src, &dir) {
                Ok(s) if s.servers.is_empty() => {
                    let n = own["mcpServers"].as_object().map_or(0, |m| m.len());
                    format!("in sync ({n})")
                }
                Ok(s) => bad(format!(
                    "NOT IN SYNC: {} (aitch launch or aitch link copies them)",
                    s.servers.join(" ")
                )),
                Err(e) => bad(format!("CANNOT COMPARE: {e}")),
            };
            println!("  MCP servers: {line}");
            let line = match route::sync_preview(src, &dir) {
                Ok(s) if s.answers == 0 => "in sync".to_string(),
                Ok(s) => {
                    let n = s.answers;
                    bad(format!(
                        "{n} answer{} not copied yet (aitch launch or aitch link copies them)",
                        if n == 1 { "" } else { "s" }
                    ))
                }
                Err(e) => bad(format!("CANNOT COMPARE: {e}")),
            };
            println!("  answers: {line}");
            servers = read_json(&route::config_file(src));
        }
        let servers = match servers {
            Ok(j) => j,
            Err(e) => {
                println!("  MCP sign-in: {}", bad(format!("CANNOT CHECK: {e}")));
                continue;
            }
        };
        let oauth = login
            .as_ref()
            .ok()
            .and_then(|c| c.as_ref())
            .map(|c| c["mcpOAuth"].clone())
            .unwrap_or(Json::Null);
        // User-scope servers, then each project's local ones (named with their project).
        let mut all: Vec<(String, &str, &Json)> = vec![];
        for (name, s) in servers["mcpServers"].as_object().into_iter().flatten() {
            all.push((name.clone(), name, s));
        }
        for (k, p) in servers["projects"].as_object().into_iter().flatten() {
            for (name, s) in p["mcpServers"].as_object().into_iter().flatten() {
                all.push((format!("{name} ({k})"), name, s));
            }
        }
        let mut signs = vec![];
        for (label, name, s) in all {
            if !needs_oauth(s) {
                continue;
            }
            let entry = &oauth[oauth_key(name, s).as_str()];
            if has(entry, "accessToken") || has(entry, "refreshToken") {
                signs.push(format!("{label}: signed in"));
            } else {
                signs.push(bad(format!(
                    "{label}: NEEDS SIGN-IN (in a session on this folder: /mcp)"
                )));
            }
        }
        if signs.is_empty() {
            println!("  MCP sign-in: none needed");
        } else {
            println!("  MCP sign-in: {}", signs.join("; "));
        }
    }
    problems
}

/// A settings file, parsed; the error names the file and the place, never its content.
fn read_json(path: &Path) -> Result<Json, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not JSON ({e})", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_key_is_the_one_claude_code_files_the_sign_in_under() {
        // The rule was checked against real login items of Claude Code 2.1.309; key order is type, url, headers.
        let s = serde_json::json!({"type": "http", "url": "https://c.example/mcp"});
        assert_eq!(oauth_key("notes", &s), "notes|32928f708bfba17d");
        let with =
            serde_json::json!({"url": "https://c.example/mcp", "type": "http", "headers": {}});
        assert_eq!(oauth_key("notes", &with), "notes|32928f708bfba17d");
    }

    #[test]
    fn only_remote_servers_without_their_own_auth_sign_in() {
        assert!(needs_oauth(
            &serde_json::json!({"type": "http", "url": "u"})
        ));
        assert!(needs_oauth(&serde_json::json!({"type": "sse", "url": "u"})));
        assert!(!needs_oauth(&serde_json::json!({"command": "x"})));
        assert!(!needs_oauth(
            &serde_json::json!({"type": "stdio", "command": "x"})
        ));
        assert!(!needs_oauth(
            &serde_json::json!({"type": "http", "url": "u", "headers": {"authorization": "Bearer x"}})
        ));
        assert!(!needs_oauth(
            &serde_json::json!({"type": "http", "url": "u", "headersHelper": "h"})
        ));
        // A key in a header of another name is its own sign-in too.
        assert!(!needs_oauth(
            &serde_json::json!({"type": "http", "url": "u", "headers": {"X-API-Key": "k"}})
        ));
        // A header that carries no key leaves the sign-in to OAuth: report it, do not pass it.
        assert!(needs_oauth(
            &serde_json::json!({"type": "http", "url": "u", "headers": {"X-Trace": "1"}})
        ));
    }

    #[test]
    fn the_login_item_is_named_after_the_folder_as_claude_code_names_it() {
        assert_eq!(
            keychain_service(Path::new("/x"), true),
            "Claude Code-credentials"
        );
        let h = hex::encode(Sha256::digest(b"/Users/a/.claude-max"));
        assert_eq!(
            keychain_service(Path::new("/Users/a/.claude-max"), false),
            format!("Claude Code-credentials-{}", &h[..8])
        );
    }
}

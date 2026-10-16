//! Launch-time routing: a profile with a route starts Claude Code on its primary config dir while the quota command
//! says that account has room, and on the profile's own (fallback) dir otherwise. Unknown is never room.
use crate::config::Route;
use crate::resolve::expand_tilde;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// What the primary dir takes from the fallback dir by symlink: the rules, never the login.
pub const LINKS: &[&str] = &[
    "CLAUDE.md",
    "settings.json",
    "agents",
    "skills",
    "commands",
    "hooks",
    "output-styles",
    "plugins",
    "projects",
];

/// How long the quota command may take before its answer counts as unknown.
pub const QUOTA_TIMEOUT: Duration = Duration::from_secs(3);

/// The quota command's answer to "does the primary account have room", with its reason.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Room(String),
    Full(String),
    Unknown(String),
}

/// The folder a launch uses and why. `loud` marks an answer that is not a plain room/full verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub dir: PathBuf,
    pub primary: bool,
    pub reason: String,
    pub loud: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Go(Choice),
    /// The fallback dir holds the wrong login: launching at all would hide it.
    Refuse(String),
}

/// Run the quota command (split on whitespace, `~` expanded in the program) and read its verdict.
/// Room only on exit 0 with "room"; full only on exit 3 with "full"; anything else is unknown.
pub fn read_verdict(cmd: &str, timeout: Duration) -> Verdict {
    let mut words = cmd.split_whitespace();
    let Some(prog) = words.next() else {
        return Verdict::Unknown("quota command is empty".into());
    };
    let prog = expand_tilde(prog);
    let mut child = match Command::new(&prog)
        .args(words)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Verdict::Unknown(format!("cannot run {prog}: {e}")),
    };
    let stdout = reader(child.stdout.take());
    let stderr = reader(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Verdict::Unknown(format!("{prog} took over {} ms", timeout.as_millis()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => return Verdict::Unknown(format!("waiting for {prog}: {e}")),
        }
    };
    // A child of the command can keep the pipes open after it exits: the deadline covers reading them too.
    let late = || Verdict::Unknown(format!("{prog} took over {} ms", timeout.as_millis()));
    let left = || deadline.saturating_duration_since(Instant::now());
    let Ok(out) = stdout.recv_timeout(left()) else {
        return late();
    };
    let Ok(err) = stderr.recv_timeout(left()) else {
        return late();
    };
    let code = match status.code() {
        Some(c) => c,
        None => return Verdict::Unknown(format!("{prog} was killed by a signal")),
    };
    let parsed: Option<serde_json::Value> = serde_json::from_str(out.trim()).ok();
    let Some(json) = parsed.filter(|v| v.is_object()) else {
        let why = err
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no JSON on stdout");
        return Verdict::Unknown(format!("exit {code}: {why}"));
    };
    let mut reason = json["reason"].as_str().unwrap_or("").to_string();
    // An exit code outside the contract is a failure of the command: its stderr says why.
    if ![0, 3, 4].contains(&code) {
        if let Some(line) = err.lines().find(|l| !l.trim().is_empty()) {
            reason = format!("{reason} (stderr: {line})");
        }
    }
    match (code, json["verdict"].as_str()) {
        (0, Some("room")) => Verdict::Room(reason),
        (3, Some("full")) => Verdict::Full(reason),
        (_, Some("unknown")) => Verdict::Unknown(format!("exit {code}: {reason}")),
        (_, Some(v)) => Verdict::Unknown(format!("exit {code} with verdict {v}: {reason}")),
        (_, None) => Verdict::Unknown(format!("exit {code}: no verdict in the JSON")),
    }
}

fn reader<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_string(&mut s);
        }
        let _ = tx.send(s);
    });
    rx
}

fn claude_json(dir: &Path) -> Result<serde_json::Value, String> {
    let path = dir.join(".claude.json");
    let text =
        fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", path.display()))
}

/// The organization the folder's Claude Code login belongs to (`.claude.json` `oauthAccount.organizationUuid`).
pub fn org_of(dir: &Path) -> Result<String, String> {
    claude_json(dir)?["oauthAccount"]["organizationUuid"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "{} is not logged in (no oauthAccount.organizationUuid)",
                dir.display()
            )
        })
}

/// Check that every entry of LINKS that exists in `from` is a symlink in `to` pointing at it.
pub fn check_links(from: &Path, to: &Path) -> Result<(), String> {
    for name in LINKS {
        let src = from.join(name);
        if fs::symlink_metadata(&src).is_err() {
            continue;
        }
        let dst = to.join(name);
        match fs::read_link(&dst) {
            Ok(_) if links_to(&dst, &src) => {}
            Ok(target) => {
                return Err(format!(
                    "{} points at {}, not {}",
                    dst.display(),
                    target.display(),
                    src.display()
                ))
            }
            Err(_) => {
                return Err(format!(
                    "{} is missing or not a link to {}",
                    dst.display(),
                    src.display()
                ))
            }
        }
    }
    Ok(())
}

/// Whether `dst` is a symlink that resolves to the same file as `src` (a dangling link does not).
fn links_to(dst: &Path, src: &Path) -> bool {
    let is_link = fs::symlink_metadata(dst).is_ok_and(|m| m.file_type().is_symlink());
    is_link && matches!((fs::canonicalize(dst), fs::canonicalize(src)), (Ok(a), Ok(b)) if a == b)
}

/// Create `to` and the LINKS symlinks into `from`. Idempotent; never replaces anything that is not the right link.
/// Returns the links it created.
pub fn link(from: &Path, to: &Path) -> Result<Vec<String>, String> {
    if !from.is_dir() {
        return Err(format!("{} is not a folder", from.display()));
    }
    // A link stores its target as written, and a relative one would resolve from inside `to`.
    let from = &std::path::absolute(from).map_err(|e| format!("{}: {e}", from.display()))?;
    // The links would land in whatever folder it points to. A trailing '/' makes the check follow the link.
    let raw = to.as_os_str().as_bytes();
    let end = raw.iter().rposition(|&b| b != b'/').map_or(1, |i| i + 1);
    if fs::symlink_metadata(OsStr::from_bytes(&raw[..end]))
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "{} is a symlink; give a real folder or a new path",
            to.display()
        ));
    }
    fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    let mut made = vec![];
    for name in LINKS {
        let src = from.join(name);
        if fs::symlink_metadata(&src).is_err() {
            continue;
        }
        let dst = to.join(name);
        match fs::read_link(&dst) {
            Ok(_) if links_to(&dst, &src) => continue,
            _ if fs::symlink_metadata(&dst).is_ok() => {
                return Err(format!(
                    "{} exists and is not a link to {}; move it away and run again",
                    dst.display(),
                    src.display()
                ))
            }
            _ => {}
        }
        symlink(&src, &dst).map_err(|e| format!("cannot link {}: {e}", dst.display()))?;
        made.push(name.to_string());
    }
    Ok(made)
}

/// Write `{"mcpServers": …}` from `fallback_dir/.claude.json` to `out`, mode 0600, replacing it atomically.
pub fn write_mcp(fallback_dir: &Path, out: &Path) -> Result<(), String> {
    let json = claude_json(fallback_dir)?;
    let servers = match &json["mcpServers"] {
        serde_json::Value::Null => serde_json::json!({}),
        v @ serde_json::Value::Object(_) => v.clone(),
        _ => {
            return Err(format!(
                "mcpServers in {}/.claude.json is not an object",
                fallback_dir.display()
            ))
        }
    };
    let body = serde_json::to_string(&serde_json::json!({ "mcpServers": servers }))
        .map_err(|e| e.to_string())?;
    let dir = out.parent().ok_or("MCP file has no folder")?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let tmp = out.with_extension(format!("tmp{}", std::process::id()));
    // A leftover file of the same name would keep its old mode: start from a new one.
    let _ = fs::remove_file(&tmp);
    let write = || -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(body.as_bytes())?;
        fs::rename(&tmp, out)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot write {}: {e}", out.display())
    })
}

/// Pick the folder for one launch. Guards first (logins, links, MCP), then the quota verdict.
pub fn decide(
    route: &Route,
    fallback_dir: &Path,
    primary_dir: &Path,
    mcp_out: &Path,
    verdict: impl FnOnce() -> Verdict,
) -> Decision {
    if let Some(want) = &route.fallback_org {
        if let Err(e) = login_is(fallback_dir, want) {
            return Decision::Refuse(format!(
                "{e}; run /logout and /login there with the account it is meant for"
            ));
        }
    }
    let fallback = |reason: String, loud: bool| {
        Decision::Go(Choice {
            dir: fallback_dir.to_path_buf(),
            primary: false,
            reason,
            loud,
        })
    };
    let guards = match &route.primary_org {
        Some(want) => login_is(primary_dir, want),
        None => Ok(()),
    }
    .and_then(|_| check_links(fallback_dir, primary_dir));
    if let Err(e) = guards {
        return fallback(format!("NOT USING THE PRIMARY DIR: {e}"), true);
    }
    match verdict() {
        Verdict::Room(reason) => match write_mcp(fallback_dir, mcp_out) {
            Ok(()) => Decision::Go(Choice {
                dir: primary_dir.to_path_buf(),
                primary: true,
                reason,
                loud: false,
            }),
            Err(e) => fallback(format!("NOT USING THE PRIMARY DIR: {e}"), true),
        },
        Verdict::Full(reason) => fallback(format!("quota full: {reason}"), false),
        Verdict::Unknown(reason) => fallback(
            format!("QUOTA UNKNOWN: {reason}. Not using the primary dir."),
            true,
        ),
    }
}

/// The folder's login must belong to organization `want`.
fn login_is(dir: &Path, want: &str) -> Result<(), String> {
    let org = org_of(dir)?;
    if org == want {
        Ok(())
    } else {
        Err(format!(
            "{} is logged in to organization {org}, not {want}",
            dir.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use tempfile::TempDir;

    const FALLBACK_ORG: &str = "org-fallback";
    const PRIMARY_ORG: &str = "org-primary";

    struct Setup {
        _tmp: TempDir,
        root: PathBuf,
        fallback: PathBuf,
        primary: PathBuf,
        mcp: PathBuf,
        route: Route,
    }

    fn login(dir: &Path, org: &str) {
        fs::create_dir_all(dir).unwrap();
        let json = format!(
            r#"{{"oauthAccount":{{"organizationUuid":"{org}"}},"mcpServers":{{"github":{{"command":"gh-mcp"}}}}}}"#
        );
        fs::write(dir.join(".claude.json"), json).unwrap();
    }

    /// A fallback dir with the rules, a primary dir linked to it, both logged in to the right accounts.
    fn setup() -> Setup {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let fallback = root.join("fallback");
        let primary = root.join("primary");
        login(&fallback, FALLBACK_ORG);
        fs::write(fallback.join("CLAUDE.md"), "rules").unwrap();
        fs::write(fallback.join("settings.json"), "{}").unwrap();
        fs::create_dir_all(fallback.join("skills")).unwrap();
        fs::create_dir_all(fallback.join("projects")).unwrap();
        link(&fallback, &primary).unwrap();
        login(&primary, PRIMARY_ORG);
        let route = Route {
            primary: primary.to_string_lossy().to_string(),
            primary_org: Some(PRIMARY_ORG.to_string()),
            fallback_org: Some(FALLBACK_ORG.to_string()),
            quota: "unused".to_string(),
        };
        let mcp = root.join("run").join("mcp.json");
        Setup {
            _tmp: tmp,
            root,
            fallback,
            primary,
            mcp,
            route,
        }
    }

    fn go(s: &Setup, v: Verdict) -> Choice {
        match decide(&s.route, &s.fallback, &s.primary, &s.mcp, || v) {
            Decision::Go(c) => c,
            Decision::Refuse(r) => panic!("refused: {r}"),
        }
    }

    /// A quota command that prints `out` and exits with `code`.
    fn fake_quota(dir: &Path, out: &str, code: i32) -> String {
        let path = dir.join(format!("quota-{code}-{}", out.len()));
        fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' '{out}'\nexit {code}\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        format!("{} quota --room", path.display())
    }

    // 1. room, guards pass → primary, the quota command's reason on the line.
    #[test]
    fn room_picks_primary() {
        let s = setup();
        let c = go(&s, Verdict::Room("7d 43%, 5h 9%".into()));
        assert!(c.primary);
        assert_eq!(c.dir, s.primary);
        assert!(!c.loud);
        assert!(c.reason.contains("7d 43%, 5h 9%"));
    }

    // 2. full → fallback, plain line.
    #[test]
    fn full_picks_work_quietly() {
        let s = setup();
        let c = go(&s, Verdict::Full("7d 100%".into()));
        assert!(!c.primary);
        assert_eq!(c.dir, s.fallback);
        assert!(!c.loud);
        assert!(c.reason.contains("7d 100%"));
    }

    // 5. unknown → fallback, loud.
    #[test]
    fn unknown_picks_work_loudly() {
        let s = setup();
        let c = go(&s, Verdict::Unknown("no sample for 2 h".into()));
        assert!(!c.primary);
        assert!(c.loud);
        assert!(c.reason.contains("no sample for 2 h"));
    }

    // 6. G1: the fallback dir logged in to another account → refuse, and the quota command never runs.
    #[test]
    fn fallback_dir_on_wrong_account_refuses() {
        let s = setup();
        login(&s.fallback, PRIMARY_ORG);
        let mut asked = false;
        let d = decide(&s.route, &s.fallback, &s.primary, &s.mcp, || {
            asked = true;
            Verdict::Room(String::new())
        });
        assert!(
            matches!(d, Decision::Refuse(ref r) if r.contains(PRIMARY_ORG)),
            "{d:?}"
        );
        assert!(!asked);
    }

    #[test]
    fn fallback_dir_without_login_refuses() {
        let s = setup();
        fs::remove_file(s.fallback.join(".claude.json")).unwrap();
        let d = decide(&s.route, &s.fallback, &s.primary, &s.mcp, || {
            Verdict::Room(String::new())
        });
        assert!(matches!(d, Decision::Refuse(_)), "{d:?}");
    }

    // 7. G2: the primary dir not logged in, or logged in to another account → fallback, loud.
    #[test]
    fn primary_dir_without_login_picks_fallback() {
        let s = setup();
        fs::remove_file(s.primary.join(".claude.json")).unwrap();
        let c = go(&s, Verdict::Room(String::new()));
        assert!(!c.primary && c.loud, "{c:?}");
    }

    #[test]
    fn primary_dir_on_wrong_account_picks_fallback() {
        let s = setup();
        login(&s.primary, FALLBACK_ORG);
        let c = go(&s, Verdict::Room(String::new()));
        assert!(!c.primary && c.loud, "{c:?}");
        assert!(c.reason.contains(FALLBACK_ORG));
    }

    // Without org ids there is no login guard: a dir with no login is left for Claude Code to ask about.
    #[test]
    fn org_guards_are_optional() {
        let mut s = setup();
        s.route.primary_org = None;
        s.route.fallback_org = None;
        fs::remove_file(s.fallback.join(".claude.json")).unwrap();
        fs::write(s.fallback.join(".claude.json"), "{}").unwrap();
        fs::remove_file(s.primary.join(".claude.json")).unwrap();
        let c = go(&s, Verdict::Room(String::new()));
        assert!(c.primary && !c.loud, "{c:?}");
    }

    // 8. G3: a link turned into a plain file, or pointing elsewhere → fallback, naming it.
    #[test]
    fn replaced_link_picks_fallback() {
        let s = setup();
        fs::remove_file(s.primary.join("settings.json")).unwrap();
        fs::write(s.primary.join("settings.json"), "{}").unwrap();
        let c = go(&s, Verdict::Room(String::new()));
        assert!(!c.primary && c.loud, "{c:?}");
        assert!(c.reason.contains("settings.json"));
    }

    #[test]
    fn link_pointing_elsewhere_picks_fallback() {
        let s = setup();
        fs::remove_file(s.primary.join("CLAUDE.md")).unwrap();
        fs::write(s.root.join("other.md"), "personal").unwrap();
        symlink(s.root.join("other.md"), s.primary.join("CLAUDE.md")).unwrap();
        let c = go(&s, Verdict::Room(String::new()));
        assert!(!c.primary && c.loud, "{c:?}");
        assert!(c.reason.contains("CLAUDE.md"));
    }

    #[test]
    fn missing_link_picks_fallback() {
        let s = setup();
        fs::remove_file(s.primary.join("skills")).unwrap();
        let c = go(&s, Verdict::Room(String::new()));
        assert!(!c.primary && c.loud, "{c:?}");
        assert!(c.reason.contains("skills"));
    }

    // G4: mcpServers in the fallback dir's .claude.json unusable → fallback.
    #[test]
    fn mcp_servers_not_an_object_picks_fallback() {
        let s = setup();
        let json = format!(
            r#"{{"oauthAccount":{{"organizationUuid":"{FALLBACK_ORG}"}},"mcpServers":[1]}}"#
        );
        fs::write(s.fallback.join(".claude.json"), json).unwrap();
        let c = go(&s, Verdict::Room(String::new()));
        assert!(!c.primary && c.loud, "{c:?}");
    }

    // 10. MCP file: 0600, exactly the fallback dir's servers, rewritten each launch.
    #[test]
    fn primary_launch_writes_mcp_file() {
        let s = setup();
        go(&s, Verdict::Room(String::new()));
        let text = fs::read_to_string(&s.mcp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["github"]["command"], "gh-mcp");
        let mode = fs::metadata(&s.mcp).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let json = format!(
            r#"{{"oauthAccount":{{"organizationUuid":"{FALLBACK_ORG}"}},"mcpServers":{{"ado":{{"command":"ado"}}}}}}"#
        );
        fs::write(s.fallback.join(".claude.json"), json).unwrap();
        go(&s, Verdict::Room(String::new()));
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&s.mcp).unwrap()).unwrap();
        assert!(v["mcpServers"].get("github").is_none());
        assert_eq!(v["mcpServers"]["ado"]["command"], "ado");
    }

    #[test]
    fn link_is_idempotent_and_skips_missing_sources() {
        let s = setup();
        assert!(link(&s.fallback, &s.primary).unwrap().is_empty());
        // agents does not exist in the fallback dir, so it is neither linked nor required.
        assert!(!s.primary.join("agents").exists());
        assert!(check_links(&s.fallback, &s.primary).is_ok());
        assert_eq!(
            fs::read_link(s.primary.join("CLAUDE.md")).unwrap(),
            s.fallback.join("CLAUDE.md")
        );
    }

    #[test]
    fn link_never_replaces_a_real_file() {
        let s = setup();
        fs::remove_file(s.primary.join("settings.json")).unwrap();
        fs::write(s.primary.join("settings.json"), "{\"mine\":1}").unwrap();
        let err = link(&s.fallback, &s.primary).unwrap_err();
        assert!(err.contains("settings.json"), "{err}");
        assert_eq!(
            fs::read_to_string(s.primary.join("settings.json")).unwrap(),
            "{\"mine\":1}"
        );
    }

    #[test]
    fn link_refuses_a_destination_that_is_a_symlink() {
        let s = setup();
        let elsewhere = s.root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let to = s.root.join("alt");
        symlink(&elsewhere, &to).unwrap();
        let err = link(&s.fallback, &to).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(
            fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "wrote through the link"
        );
    }

    // 1-5 on the reader, with a fake quota command.
    #[test]
    fn reader_room_on_exit_0() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"room","reason":"7d 43%"}"#, 0);
        assert_eq!(
            read_verdict(&cmd, QUOTA_TIMEOUT),
            Verdict::Room("7d 43%".into())
        );
    }

    #[test]
    fn reader_full_on_exit_3() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"full","reason":"7d 100%"}"#, 3);
        assert_eq!(
            read_verdict(&cmd, QUOTA_TIMEOUT),
            Verdict::Full("7d 100%".into())
        );
    }

    fn unknown_containing(v: Verdict, part: &str) {
        match v {
            Verdict::Unknown(r) => assert!(r.contains(part), "{r:?} lacks {part:?}"),
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn reader_unknown_on_exit_4() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"unknown","reason":"no sample"}"#, 4);
        unknown_containing(read_verdict(&cmd, QUOTA_TIMEOUT), "no sample");
    }

    #[test]
    fn reader_room_with_nonzero_exit_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"room","reason":"x"}"#, 1);
        unknown_containing(read_verdict(&cmd, QUOTA_TIMEOUT), "exit 1");
    }

    #[test]
    fn reader_unexpected_exit_shows_stderr_even_with_json() {
        let t = TempDir::new().unwrap();
        let path = t.path().join("quota");
        fs::write(
            &path,
            "#!/bin/sh\necho '{\"verdict\":\"room\",\"reason\":\"x\"}'\necho 'db locked' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        unknown_containing(
            read_verdict(&path.to_string_lossy(), QUOTA_TIMEOUT),
            "db locked",
        );
    }

    #[test]
    fn reader_full_with_exit_0_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"full","reason":"x"}"#, 0);
        unknown_containing(read_verdict(&cmd, QUOTA_TIMEOUT), "full");
    }

    #[test]
    fn reader_missing_command_is_unknown() {
        unknown_containing(
            read_verdict("/nonexistent/quota-tool quota --room", QUOTA_TIMEOUT),
            "/nonexistent/quota-tool",
        );
    }

    #[test]
    fn reader_not_json_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), "not json", 0);
        unknown_containing(read_verdict(&cmd, QUOTA_TIMEOUT), "exit 0");
    }

    #[test]
    fn reader_json_without_verdict_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"reason":"x"}"#, 0);
        unknown_containing(read_verdict(&cmd, QUOTA_TIMEOUT), "verdict");
    }

    #[test]
    fn reader_shows_stderr_when_stdout_has_no_json() {
        let t = TempDir::new().unwrap();
        let path = t.path().join("q");
        fs::write(&path, "#!/bin/sh\necho 'store missing' >&2\nexit 4\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        unknown_containing(
            read_verdict(&path.to_string_lossy(), QUOTA_TIMEOUT),
            "store missing",
        );
    }

    #[test]
    fn reader_child_holding_the_pipe_is_unknown_in_time() {
        let t = TempDir::new().unwrap();
        let path = t.path().join("bg");
        fs::write(
            &path,
            "#!/bin/sh\nsleep 5 &\necho '{\"verdict\":\"room\",\"reason\":\"\"}'\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let start = std::time::Instant::now();
        unknown_containing(
            read_verdict(&path.to_string_lossy(), Duration::from_millis(300)),
            "took",
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn dangling_link_fails_the_check() {
        let s = setup();
        fs::remove_file(s.primary.join("CLAUDE.md")).unwrap();
        symlink("fallback/CLAUDE.md", s.primary.join("CLAUDE.md")).unwrap();
        assert!(check_links(&s.fallback, &s.primary).is_err());
    }

    #[test]
    fn reader_slow_command_is_unknown() {
        let t = TempDir::new().unwrap();
        let path = t.path().join("slow");
        fs::write(&path, "#!/bin/sh\nexec sleep 5\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let start = std::time::Instant::now();
        unknown_containing(
            read_verdict(&path.to_string_lossy(), Duration::from_millis(300)),
            "took",
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn link_refuses_a_symlink_destination_written_with_a_slash() {
        let s = setup();
        let elsewhere = s.root.join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, s.root.join("alt")).unwrap();
        let err = link(&s.fallback, &s.root.join("alt/")).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(
            fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "wrote through the link"
        );
    }
}

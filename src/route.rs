//! Launch-time routing: a profile with a route starts Claude Code on its primary config dir while the quota command
//! says that account has room, and on the profile's own (fallback) dir otherwise. Unknown is never room.
use crate::config::Route;
use crate::resolve::expand_tilde;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// The command gets AITCH_PRIMARY set to the primary dir it is asked about.
pub fn read_verdict(cmd: &str, primary: &Path, timeout: Duration) -> Verdict {
    let mut words = cmd.split_whitespace();
    let Some(prog) = words.next() else {
        return Verdict::Unknown("quota command is empty".into());
    };
    let prog = expand_tilde(prog);
    let mut child = match Command::new(&prog)
        .args(words)
        .env("AITCH_PRIMARY", primary)
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

/// Claude Code's default config folder, the one it uses when CLAUDE_CONFIG_DIR is unset.
pub fn default_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude")
}

/// Whether two paths name the same folder (both must exist).
pub fn same_dir(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// The file Claude Code keeps a folder's settings and login in: `.claude.json` inside it, but for the default folder
/// `~/.claude.json`, beside it.
pub fn config_file(dir: &Path) -> PathBuf {
    if same_dir(dir, &default_dir()) {
        dirs::home_dir().unwrap_or_default().join(".claude.json")
    } else {
        dir.join(".claude.json")
    }
}

fn claude_json(dir: &Path) -> Result<serde_json::Value, String> {
    let path = config_file(dir);
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

/// Write `{"mcpServers": …}` from `fallback_dir/.claude.json` to `out`, mode 0600, replacing it atomically: the
/// user-scope servers, then the local-scope ones filed under `folders` (the launch folder's project, see
/// `project_key`), which win on a shared name as they do in Claude Code.
pub fn write_mcp(fallback_dir: &Path, folders: &[String], out: &Path) -> Result<(), String> {
    let json = claude_json(fallback_dir)?;
    let mut servers = serde_json::Map::new();
    let local = folders.iter().map(|f| {
        (
            format!("projects[{f}].mcpServers"),
            &json["projects"][f.as_str()]["mcpServers"],
        )
    });
    for (key, v) in std::iter::once(("mcpServers".to_string(), &json["mcpServers"])).chain(local) {
        match v {
            serde_json::Value::Null => {}
            serde_json::Value::Object(m) => servers.extend(m.clone()),
            _ => {
                return Err(format!(
                    "{key} in {}/.claude.json is not an object",
                    fallback_dir.display()
                ))
            }
        }
    }
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

/// The folder Claude Code files a launch folder's local state under: the nearest folder above it (or itself) holding
/// `.git` (for a linked worktree, its main repo), unless that is the home folder; else the launch folder.
pub fn project_key(cwd: &Path, home: &Path) -> String {
    let root = cwd
        .ancestors()
        .find(|d| fs::symlink_metadata(d.join(".git")).is_ok())
        .map(main_worktree)
        .filter(|d| fs::canonicalize(d).ok() != fs::canonicalize(home).ok());
    root.unwrap_or_else(|| cwd.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// A linked worktree's `.git` file names `<repo>/.git/worktrees/<name>`, whose `commondir` leads to the repo's `.git`.
/// Any other `.git` (a folder, a submodule's file, a worktree of a bare repo) keeps `dir`.
fn main_worktree(dir: &Path) -> PathBuf {
    let common = fs::read_to_string(dir.join(".git"))
        .ok()
        .and_then(|s| s.strip_prefix("gitdir:").map(|g| dir.join(g.trim())))
        .and_then(|g| {
            let c = fs::read_to_string(g.join("commondir")).ok()?;
            fs::canonicalize(g.join(c.trim())).ok()
        });
    common
        .filter(|c| c.file_name() == Some(OsStr::new(".git")))
        .and_then(|c| c.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| dir.to_path_buf())
}

/// Mark as trusted in `to` every folder that `from` trusts and that is one of `folders` or a parent of one, so the
/// launch does not ask "trust this folder?" again. Writes `to/.claude.json` only when that changes something, keeps
/// its other fields and its mode. Returns the folders it marked.
pub fn share_trust(
    from: &Path,
    to: &Path,
    folders: &[String],
    backups: &Path,
) -> Result<Vec<String>, String> {
    if let Err(e) = fs::symlink_metadata(config_file(from)) {
        if e.kind() == std::io::ErrorKind::NotFound {
            return Ok(vec![]);
        }
    }
    let src = claude_json(from)?;
    let Some(trusted) = src["projects"].as_object() else {
        return Ok(vec![]);
    };
    let covers = |key: &str| {
        let key = key.trim_end_matches('/');
        !key.is_empty()
            && folders
                .iter()
                .any(|f| f == key || f.starts_with(&format!("{key}/")))
    };
    let wanted: Vec<&String> = trusted
        .iter()
        .filter(|(k, v)| v["hasTrustDialogAccepted"] == true && covers(k))
        .map(|(k, _)| k)
        .collect();
    let what = to.join(".claude.json").display().to_string();
    let marked = edit_claude_json(to, backups, &|dst| {
        let todo: Vec<String> = wanted
            .iter()
            .filter(|k| {
                dst.get("projects")
                    .and_then(|p| p.get(k.as_str()))
                    .and_then(|p| p.get("hasTrustDialogAccepted"))
                    != Some(&Json::Bool(true))
            })
            .map(|k| k.to_string())
            .collect();
        if todo.is_empty() {
            return Ok(None);
        }
        let projects = object_in(dst, "projects", &what)?;
        for k in &todo {
            object_in(projects, k, &what)?.insert("hasTrustDialogAccepted".into(), true.into());
        }
        Ok(Some(todo))
    })?;
    Ok(marked.unwrap_or_default())
}

type Json = serde_json::Value;
type Object = serde_json::Map<String, Json>;

/// The object under `key` in `map`, added empty when missing; an error naming the file when it is something else.
fn object_in<'a>(map: &'a mut Object, key: &str, file: &str) -> Result<&'a mut Object, String> {
    map.entry(key.to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{key} in {file} is not an object"))
}

/// Take the lock Claude Code changes `dir/.claude.json` under (a folder); wait a little for it, never take it over.
/// Returns the lock to remove when done.
fn take_lock(dir: &Path) -> Result<PathBuf, String> {
    let lock = dir.join(".claude.json.lock");
    let mut tries = 0;
    while let Err(e) = fs::create_dir(&lock) {
        tries += 1;
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(format!("cannot take {}: {e}", lock.display()));
        }
        if tries == 50 {
            let age = fs::metadata(&lock)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            // Claude Code renews its lock while it holds it: an old one was left by a process that died.
            return Err(if age > Duration::from_secs(60) {
                format!(
                    "{} is {} s old, maybe stale; remove it if no Claude Code is writing there",
                    lock.display(),
                    age.as_secs()
                )
            } else {
                format!("{} is held by another process", lock.display())
            });
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(lock)
}

/// Change `dir/.claude.json` through `edit`, which returns None when it changes nothing; then nothing is written. A
/// write happens under Claude Code's lock: read the file again there, back it up into `backups`, and replace it
/// atomically, same mode, only if nobody wrote it meanwhile, else try again. A missing file starts as `{}` and is
/// created with mode 0600. Never writes through a symlink.
fn edit_claude_json<R>(
    dir: &Path,
    backups: &Path,
    edit: &dyn Fn(&mut Object) -> Result<Option<R>, String>,
) -> Result<Option<R>, String> {
    let path = dir.join(".claude.json");
    // A look without the lock first: most launches change nothing and need not wait for it.
    if edit(&mut read_current(&path)?.0)?.is_none() {
        return Ok(None);
    }
    let lock = take_lock(dir)?;
    let result = (|| {
        for _ in 0..3 {
            let (mut json, before) = read_current(&path)?;
            let Some(r) = edit(&mut json)? else {
                return Ok(None);
            };
            if replace(&path, &json, before.as_ref(), backups)? {
                return Ok(Some(r));
            }
        }
        Err(format!(
            "{} kept changing while aitch wrote it; not written",
            path.display()
        ))
    })();
    let _ = fs::remove_dir(&lock);
    result
}

/// The file as an object, and its metadata and text as read (None: there is no file).
type Current = (Object, Option<(fs::Metadata, String)>);

fn read_current(path: &Path) -> Result<Current, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Object::new(), None)),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; not writing through it",
            path.display()
        ));
    }
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    match serde_json::from_str(&text) {
        Ok(Json::Object(m)) => Ok((m, Some((meta, text)))),
        Ok(_) => Err(format!("{} is not a JSON object", path.display())),
        Err(e) => Err(format!("{} is not JSON: {e}", path.display())),
    }
}

/// Replace `path` with `json` if it is still as `before` found it (None: still missing), backing up the old text
/// first. False when it changed meanwhile: another writer that skipped the lock keeps its version.
fn replace(
    path: &Path,
    json: &Object,
    before: Option<&(fs::Metadata, String)>,
    backups: &Path,
) -> Result<bool, String> {
    let body = serde_json::to_string_pretty(json).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("json.aitch{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let write = || -> std::io::Result<bool> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        if let Some((meta, _)) = before {
            f.set_permissions(meta.permissions())?;
        }
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        let same = match (before, fs::symlink_metadata(path)) {
            (None, Err(e)) => e.kind() == std::io::ErrorKind::NotFound,
            (Some((meta, _)), Ok(now)) => {
                now.modified()? == meta.modified()?
                    && now.len() == meta.len()
                    && now.ino() == meta.ino()
            }
            _ => false,
        };
        if !same {
            return Ok(false);
        }
        if let Some((_, text)) = before {
            backup(backups, path, text)?;
        }
        fs::rename(&tmp, path)?;
        Ok(true)
    };
    let result = write();
    if !matches!(result, Ok(true)) {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// How many backups of one folder's `.claude.json` are kept.
const BACKUPS_KEPT: usize = 10;

/// Save `text`, the content of `path` before a write, into `backups` (mode 0600); keep the newest BACKUPS_KEPT of
/// that file.
fn backup(backups: &Path, path: &Path, text: &str) -> std::io::Result<()> {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    fs::create_dir_all(backups)?;
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().trim_start_matches('.').to_string())
        .unwrap_or_default();
    let id = hex::encode(Sha256::digest(path.as_os_str().as_bytes()));
    let tag = format!("{folder}-{}.claude.json.", &id[..8]);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let name = format!(
        "{tag}{ms:013}-{}-{:04}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(backups.join(name))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    let mut old: Vec<PathBuf> = fs::read_dir(backups)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&tag))
        .map(|e| e.path())
        .collect();
    old.sort();
    for p in old.iter().rev().skip(BACKUPS_KEPT) {
        let _ = fs::remove_file(p);
    }
    Ok(())
}

/// What `sync_config` changed: MCP server names (`+` added, `~` changed, `-` removed; a local one with its project)
/// and how many answers it copied.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Synced {
    pub servers: Vec<String>,
    pub answers: usize,
}

impl Synced {
    /// One line: "MCP servers +a -b, 2 answers".
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if !self.servers.is_empty() {
            parts.push(format!("MCP servers {}", self.servers.join(" ")));
        }
        if self.answers > 0 {
            let s = if self.answers == 1 { "" } else { "s" };
            parts.push(format!("{} answer{s}", self.answers));
        }
        parts.join(", ")
    }
}

/// The per-project answers to Claude Code's start-up questions a linked folder takes from its source: folder trust
/// and CLAUDE.md imports from outside the folder. Only a yes is copied, never a no.
const ANSWERS: &[&str] = &[
    "hasTrustDialogAccepted",
    "hasClaudeMdExternalIncludesApproved",
    "hasClaudeMdExternalIncludesWarningShown",
];

/// Per-project lists of approved project (`.mcp.json`) servers: the source's names are added, none removed.
const APPROVALS: &[&str] = &["enabledMcpjsonServers"];

/// The folder whose rules `dir` links to (see `link`): None when it links into no config folder (a source itself),
/// an error when its links point into more than one or miss one. A config folder is one with a `.claude.json`, so a
/// source whose CLAUDE.md is a link into a dotfiles repo is still a source.
pub fn source_of(dir: &Path) -> Result<Option<PathBuf>, String> {
    let mut found: Option<PathBuf> = None;
    for name in LINKS {
        let at = dir.join(name);
        let Ok(target) = fs::read_link(&at) else {
            continue;
        };
        let Some(folder) = dir
            .join(target)
            .parent()
            .and_then(|p| fs::canonicalize(p).ok())
        else {
            continue;
        };
        if !config_file(&folder).exists() {
            continue;
        }
        match &found {
            Some(f) if *f != folder => {
                return Err(format!(
                    "{} links into more than one folder ({} and {})",
                    dir.display(),
                    f.display(),
                    folder.display()
                ))
            }
            _ => found = Some(folder),
        }
    }
    let Some(src) = found else {
        return Ok(None);
    };
    check_links(&src, dir)?;
    Ok(Some(src))
}

/// Make `map[key]` the servers `want` names (an object, or null for none); report each name that changes.
fn mirror(
    map: &mut Object,
    key: &str,
    want: &Json,
    scope: &str,
    file: &str,
    report: &mut Vec<String>,
) -> Result<(), String> {
    let empty = Object::new();
    let objects = |v: Option<&Json>, at: &str| match v {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Object(m)) => Ok(Some(m.clone())),
        Some(_) => Err(format!("{key}{scope} in {at} is not an object")),
    };
    let want = objects(Some(want), "the source")?;
    let have = objects(map.get(key), file)?;
    let (w, h) = (
        want.as_ref().unwrap_or(&empty),
        have.as_ref().unwrap_or(&empty),
    );
    let before = report.len();
    for (name, v) in w {
        match h.get(name) {
            None => report.push(format!("+{name}{scope}")),
            Some(old) if old != v => report.push(format!("~{name}{scope}")),
            _ => {}
        }
    }
    for name in h.keys().filter(|n| !w.contains_key(*n)) {
        report.push(format!("-{name}{scope}"));
    }
    if report.len() > before {
        match want {
            Some(m) => map.insert(key.to_string(), Json::Object(m)),
            None => map.remove(key),
        };
    }
    Ok(())
}

/// Make `to/.claude.json` carry what `from/.claude.json` sets for every folder linked to it: the same MCP servers (user
/// and local scope, mirrored) and every yes to a start-up question (ANSWERS, APPROVALS). Never touches anything else,
/// the login included. Creates the file (mode 0600) when it is missing; backs it up into `backups` before every write.
pub fn sync_config(from: &Path, to: &Path, backups: &Path) -> Result<Synced, String> {
    sync(from, to, Some(backups))
}

/// What `sync_config` would change now, writing nothing.
pub fn sync_preview(from: &Path, to: &Path) -> Result<Synced, String> {
    sync(from, to, None)
}

fn sync(from: &Path, to: &Path, backups: Option<&Path>) -> Result<Synced, String> {
    let src = claude_json(from)?;
    let file = to.join(".claude.json").display().to_string();
    let none = Object::new();
    let src_projects = match &src["projects"] {
        Json::Null => &none,
        Json::Object(m) => m,
        _ => {
            return Err(format!(
                "projects in {}/.claude.json is not an object",
                from.display()
            ))
        }
    };
    let edit = |dst: &mut Object| {
        let mut out = Synced::default();
        mirror(
            dst,
            "mcpServers",
            &src["mcpServers"],
            "",
            &file,
            &mut out.servers,
        )?;
        let dst_projects = object_in(dst, "projects", &file)?;
        let mut keys: Vec<String> = src_projects.keys().cloned().collect();
        keys.extend(
            dst_projects
                .keys()
                .filter(|k| !src_projects.contains_key(*k))
                .cloned(),
        );
        for k in keys {
            let sp = src_projects.get(&k).unwrap_or(&Json::Null);
            let dp = dst_projects.get(&k).unwrap_or(&Json::Null);
            let local = |v: &Json| v["mcpServers"].as_object().is_some_and(|m| !m.is_empty());
            let yes: Vec<&str> = ANSWERS
                .iter()
                .copied()
                .filter(|a| sp[*a] == true && dp[*a] != true)
                .collect();
            let approve: Vec<(&str, Vec<Json>)> = APPROVALS
                .iter()
                .filter_map(|a| {
                    let have = dp[*a].as_array();
                    let new: Vec<Json> = sp[*a]
                        .as_array()?
                        .iter()
                        .filter(|n| !have.is_some_and(|h| h.contains(n)))
                        .cloned()
                        .collect();
                    (!new.is_empty()).then_some((*a, new))
                })
                .collect();
            if !local(sp) && !local(dp) && yes.is_empty() && approve.is_empty() {
                continue;
            }
            let scope = format!(" ({k})");
            let dp = object_in(dst_projects, &k, &file)?;
            mirror(
                dp,
                "mcpServers",
                &sp["mcpServers"],
                &scope,
                &file,
                &mut out.servers,
            )?;
            for a in yes {
                dp.insert(a.to_string(), true.into());
                out.answers += 1;
            }
            for (a, new) in approve {
                out.answers += new.len();
                let list = dp
                    .entry(a.to_string())
                    .or_insert_with(|| serde_json::json!([]));
                list.as_array_mut()
                    .ok_or_else(|| format!("{a}{scope} in {file} is not a list"))?
                    .extend(new);
            }
        }
        Ok((out != Synced::default()).then_some(out))
    };
    let done = match backups {
        Some(b) => edit_claude_json(to, b, &edit)?,
        None => edit(&mut read_current(&to.join(".claude.json"))?.0)?,
    };
    Ok(done.unwrap_or_default())
}

/// Pick the folder for one launch. Guards first (logins, links, MCP), then the quota verdict.
pub fn decide(
    route: &Route,
    fallback_dir: &Path,
    primary_dir: &Path,
    folders: &[String],
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
        Verdict::Room(reason) => match write_mcp(fallback_dir, folders, mcp_out) {
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
pub fn login_is(dir: &Path, want: &str) -> Result<(), String> {
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
        match decide(&s.route, &s.fallback, &s.primary, &[], &s.mcp, || v) {
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
        // Run through sh, not exec'd: Linux refuses to exec a file that a forked test thread still holds open.
        format!("/bin/sh {} quota --room", path.display())
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
        let d = decide(&s.route, &s.fallback, &s.primary, &[], &s.mcp, || {
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
        let d = decide(&s.route, &s.fallback, &s.primary, &[], &s.mcp, || {
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
            read_verdict(&cmd, Path::new("/p"), QUOTA_TIMEOUT),
            Verdict::Room("7d 43%".into())
        );
    }

    #[test]
    fn reader_full_on_exit_3() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"full","reason":"7d 100%"}"#, 3);
        assert_eq!(
            read_verdict(&cmd, Path::new("/p"), QUOTA_TIMEOUT),
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
        unknown_containing(
            read_verdict(&cmd, Path::new("/p"), QUOTA_TIMEOUT),
            "no sample",
        );
    }

    #[test]
    fn reader_room_with_nonzero_exit_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"room","reason":"x"}"#, 1);
        unknown_containing(read_verdict(&cmd, Path::new("/p"), QUOTA_TIMEOUT), "exit 1");
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
            read_verdict(
                &format!("/bin/sh {}", path.display()),
                Path::new("/p"),
                QUOTA_TIMEOUT,
            ),
            "db locked",
        );
    }

    #[test]
    fn reader_full_with_exit_0_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"verdict":"full","reason":"x"}"#, 0);
        unknown_containing(read_verdict(&cmd, Path::new("/p"), QUOTA_TIMEOUT), "full");
    }

    #[test]
    fn reader_missing_command_is_unknown() {
        unknown_containing(
            read_verdict(
                "/nonexistent/quota-tool quota --room",
                Path::new("/p"),
                QUOTA_TIMEOUT,
            ),
            "/nonexistent/quota-tool",
        );
    }

    #[test]
    fn reader_not_json_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), "not json", 0);
        unknown_containing(read_verdict(&cmd, Path::new("/p"), QUOTA_TIMEOUT), "exit 0");
    }

    #[test]
    fn reader_json_without_verdict_is_unknown() {
        let t = TempDir::new().unwrap();
        let cmd = fake_quota(t.path(), r#"{"reason":"x"}"#, 0);
        unknown_containing(
            read_verdict(&cmd, Path::new("/p"), QUOTA_TIMEOUT),
            "verdict",
        );
    }

    #[test]
    fn reader_shows_stderr_when_stdout_has_no_json() {
        let t = TempDir::new().unwrap();
        let path = t.path().join("q");
        fs::write(&path, "#!/bin/sh\necho 'store missing' >&2\nexit 4\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        unknown_containing(
            read_verdict(
                &format!("/bin/sh {}", path.display()),
                Path::new("/p"),
                QUOTA_TIMEOUT,
            ),
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
            read_verdict(
                &format!("/bin/sh {}", path.display()),
                Path::new("/p"),
                Duration::from_millis(300),
            ),
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
            read_verdict(
                &format!("/bin/sh {}", path.display()),
                Path::new("/p"),
                Duration::from_millis(300),
            ),
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

    fn write_json(path: &Path, v: serde_json::Value) {
        fs::write(path, serde_json::to_string(&v).unwrap()).unwrap();
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn mcp_file_adds_the_folders_local_servers() {
        let s = setup();
        write_json(
            &s.fallback.join(".claude.json"),
            serde_json::json!({
                "mcpServers": {"github": {"command": "user"}, "ado": {"command": "ado"}},
                "projects": {
                    "/r/svc": {"mcpServers": {"github": {"command": "local"}, "db": {"command": "db"}}},
                    "/r/other": {"mcpServers": {"x": {"command": "x"}}}
                }
            }),
        );
        write_mcp(&s.fallback, &["/r/svc".to_string()], &s.mcp).unwrap();
        let v = &read_json(&s.mcp)["mcpServers"];
        assert_eq!(v["github"]["command"], "local", "local scope wins: {v}");
        assert_eq!(v["db"]["command"], "db");
        assert_eq!(v["ado"]["command"], "ado");
        assert!(v.get("x").is_none(), "another folder's server: {v}");
    }

    #[test]
    fn trust_follows_the_folder_and_its_parents() {
        let s = setup();
        let from = s.fallback.join(".claude.json");
        let to = s.primary.join(".claude.json");
        let mut f = read_json(&from);
        f["projects"] = serde_json::json!({
            "/r/svc": {"hasTrustDialogAccepted": true},
            "/r": {"hasTrustDialogAccepted": true},
            "/r/svc-ideas": {"hasTrustDialogAccepted": true},
            "/q": {"hasTrustDialogAccepted": true},
            "/r/svc/deep": {"hasTrustDialogAccepted": false}
        });
        write_json(&from, f);
        let mut t = read_json(&to);
        t["projects"] = serde_json::json!({"/r/svc": {"allowedTools": ["Read"]}});
        write_json(&to, t);
        fs::set_permissions(&to, fs::Permissions::from_mode(0o666)).unwrap();

        let mut got = share_trust(
            &s.fallback,
            &s.primary,
            &["/r/svc/deep".to_string()],
            &s.root.join("backups"),
        )
        .unwrap();
        got.sort();
        assert_eq!(got, ["/r", "/r/svc"]);
        let t = read_json(&to);
        assert_eq!(t["projects"]["/r/svc"]["hasTrustDialogAccepted"], true);
        assert_eq!(
            t["projects"]["/r/svc"]["allowedTools"][0], "Read",
            "other fields kept"
        );
        assert_eq!(t["projects"]["/r"]["hasTrustDialogAccepted"], true);
        assert!(
            t["projects"].get("/r/svc-ideas").is_none(),
            "a sibling is not a parent"
        );
        assert!(t["projects"].get("/q").is_none());
        assert_eq!(
            t["oauthAccount"]["organizationUuid"], PRIMARY_ORG,
            "login kept"
        );
        assert_eq!(
            fs::metadata(&to).unwrap().permissions().mode() & 0o777,
            0o666
        );

        let before = fs::read_to_string(&to).unwrap();
        assert!(share_trust(
            &s.fallback,
            &s.primary,
            &["/r/svc/deep".to_string()],
            &s.root.join("backups")
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            fs::read_to_string(&to).unwrap(),
            before,
            "no change, no write"
        );
    }

    #[test]
    fn trust_never_writes_through_a_symlinked_claude_json() {
        let s = setup();
        let from = s.fallback.join(".claude.json");
        let mut f = read_json(&from);
        f["projects"] = serde_json::json!({"/r": {"hasTrustDialogAccepted": true}});
        write_json(&from, f);
        let real = s.root.join("real.json");
        fs::rename(s.primary.join(".claude.json"), &real).unwrap();
        symlink(&real, s.primary.join(".claude.json")).unwrap();
        assert!(share_trust(
            &s.fallback,
            &s.primary,
            &["/r".to_string()],
            &s.root.join("backups")
        )
        .is_err());
        assert!(fs::symlink_metadata(s.primary.join(".claude.json"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn project_key_is_the_git_root_like_claude_code() {
        let s = setup();
        let repo = s.root.join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("a/b")).unwrap();
        let plain = s.root.join("plain/dir");
        fs::create_dir_all(&plain).unwrap();
        let key = |p: &Path| project_key(p, &s.root.join("home"));
        assert_eq!(key(&repo.join("a/b")), repo.to_string_lossy());
        assert_eq!(key(&repo), repo.to_string_lossy());
        assert_eq!(key(&plain), plain.to_string_lossy());
        // A home folder that is a git repo (dotfiles) is not a project root.
        let home = s.root.join("dot");
        fs::create_dir_all(home.join(".git")).unwrap();
        fs::create_dir_all(home.join("notes")).unwrap();
        assert_eq!(
            project_key(&home.join("notes"), &home),
            home.join("notes").to_string_lossy()
        );
    }

    #[test]
    fn trust_waits_for_a_held_lock_and_gives_up_without_writing() {
        let s = setup();
        let from = s.fallback.join(".claude.json");
        let mut f = read_json(&from);
        f["projects"] = serde_json::json!({"/r": {"hasTrustDialogAccepted": true}});
        write_json(&from, f);
        let lock = s.primary.join(".claude.json.lock");
        fs::create_dir(&lock).unwrap();
        let before = fs::read_to_string(s.primary.join(".claude.json")).unwrap();
        let err = share_trust(
            &s.fallback,
            &s.primary,
            &["/r".to_string()],
            &s.root.join("backups"),
        )
        .unwrap_err();
        assert!(err.contains("lock"), "{err}");
        assert_eq!(
            fs::read_to_string(s.primary.join(".claude.json")).unwrap(),
            before
        );
        assert!(lock.is_dir(), "someone else's lock was removed");
    }

    #[test]
    fn trust_from_a_dir_without_a_login_file_shares_nothing() {
        let s = setup();
        fs::remove_file(s.fallback.join(".claude.json")).unwrap();
        assert!(share_trust(
            &s.fallback,
            &s.primary,
            &["/r".to_string()],
            &s.root.join("backups")
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn project_key_sees_a_symlinked_home() {
        let s = setup();
        let dot = s.root.join("dot");
        fs::create_dir_all(dot.join(".git")).unwrap();
        fs::create_dir_all(dot.join("notes")).unwrap();
        let home = s.root.join("home-link");
        symlink(&dot, &home).unwrap();
        let notes = dot.join("notes");
        assert_eq!(project_key(&notes, &home), notes.to_string_lossy());
    }

    #[test]
    fn an_old_lock_is_named_as_maybe_stale() {
        let s = setup();
        let from = s.fallback.join(".claude.json");
        let mut f = read_json(&from);
        f["projects"] = serde_json::json!({"/r": {"hasTrustDialogAccepted": true}});
        write_json(&from, f);
        let lock = s.primary.join(".claude.json.lock");
        fs::create_dir(&lock).unwrap();
        assert!(Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(&lock)
            .status()
            .unwrap()
            .success());
        let err = share_trust(
            &s.fallback,
            &s.primary,
            &["/r".to_string()],
            &s.root.join("backups"),
        )
        .unwrap_err();
        assert!(err.contains("stale") && err.contains("remove"), "{err}");
        assert!(lock.is_dir());
    }

    #[test]
    fn a_bad_local_mcp_entry_is_named() {
        let s = setup();
        write_json(
            &s.fallback.join(".claude.json"),
            serde_json::json!({"projects": {"/r": {"mcpServers": ["x"]}}}),
        );
        let err = write_mcp(&s.fallback, &["/r".to_string()], &s.mcp).unwrap_err();
        assert!(err.contains("projects") && err.contains("/r"), "{err}");
    }

    #[test]
    fn project_key_of_a_linked_worktree_is_its_main_repo() {
        let s = setup();
        let main = s.root.join("main");
        let meta = main.join(".git/worktrees/wt");
        fs::create_dir_all(&meta).unwrap();
        fs::write(meta.join("commondir"), "../..\n").unwrap();
        let wt = s.root.join("elsewhere/wt");
        fs::create_dir_all(wt.join("sub")).unwrap();
        fs::write(wt.join(".git"), format!("gitdir: {}\n", meta.display())).unwrap();
        let main = fs::canonicalize(&main).unwrap();
        assert_eq!(
            project_key(&wt.join("sub"), &s.root),
            main.to_string_lossy()
        );
        // A .git file without commondir (a submodule) keeps its own folder.
        let module = s.root.join("module");
        fs::create_dir_all(&module).unwrap();
        fs::create_dir_all(s.root.join("super/.git/modules/m")).unwrap();
        fs::write(module.join(".git"), "gitdir: ../super/.git/modules/m\n").unwrap();
        assert_eq!(project_key(&module, &s.root), module.to_string_lossy());
        // A worktree of a bare repo has no `.git` folder to stand under: it keeps its own folder.
        let bare = s.root.join("bare/repo.git/worktrees/b");
        fs::create_dir_all(&bare).unwrap();
        fs::write(bare.join("commondir"), "../..\n").unwrap();
        let bwt = s.root.join("bwt");
        fs::create_dir_all(&bwt).unwrap();
        fs::write(bwt.join(".git"), format!("gitdir: {}\n", bare.display())).unwrap();
        assert_eq!(project_key(&bwt, &s.root), bwt.to_string_lossy());
    }

    #[test]
    fn sharing_trust_keeps_the_order_of_keys() {
        let s = setup();
        let from = s.fallback.join(".claude.json");
        let mut f = read_json(&from);
        f["projects"] = serde_json::json!({"/r": {"hasTrustDialogAccepted": true}});
        write_json(&from, f);
        let to = s.primary.join(".claude.json");
        fs::write(
            &to,
            r#"{"zeta": 1, "alpha": {"y": 1, "b": 2}, "projects": {}}"#,
        )
        .unwrap();
        share_trust(
            &s.fallback,
            &s.primary,
            &["/r".to_string()],
            &s.root.join("backups"),
        )
        .unwrap();
        let body = fs::read_to_string(&to).unwrap();
        let at = |k: &str| body.find(k).unwrap();
        assert!(
            at("zeta") < at("alpha") && at("\"y\"") < at("\"b\""),
            "{body}"
        );
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn backups_of(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .map(|r| {
                r.map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[test]
    fn source_of_a_linked_folder_is_the_folder_it_links_to() {
        let s = setup();
        let fallback = fs::canonicalize(&s.fallback).unwrap();
        assert_eq!(source_of(&s.primary).unwrap(), Some(fallback));
        assert_eq!(
            source_of(&s.fallback).unwrap(),
            None,
            "a source links nothing"
        );
        // Links into two folders, or one missing, are not a linked folder: say so, do not guess.
        let other = s.root.join("other");
        login(&other, "org-other");
        fs::write(other.join("CLAUDE.md"), "x").unwrap();
        fs::remove_file(s.primary.join("CLAUDE.md")).unwrap();
        symlink(other.join("CLAUDE.md"), s.primary.join("CLAUDE.md")).unwrap();
        let err = source_of(&s.primary).unwrap_err();
        assert!(err.contains("more than one folder"), "{err}");
        fs::remove_file(s.primary.join("CLAUDE.md")).unwrap();
        let err = source_of(&s.primary).unwrap_err();
        assert!(err.contains("CLAUDE.md"), "{err}");
    }

    #[test]
    fn a_source_with_a_link_into_a_dotfiles_folder_is_still_a_source() {
        let s = setup();
        let dots = s.root.join("dotfiles");
        fs::create_dir_all(&dots).unwrap();
        fs::write(dots.join("CLAUDE.md"), "mine").unwrap();
        fs::remove_file(s.fallback.join("CLAUDE.md")).unwrap();
        symlink(dots.join("CLAUDE.md"), s.fallback.join("CLAUDE.md")).unwrap();
        assert_eq!(source_of(&s.fallback).unwrap(), None);
    }

    #[test]
    fn sync_mirrors_the_user_servers_and_keeps_the_login() {
        let s = setup();
        let backups = s.root.join("backups");
        write_json(
            &s.fallback.join(".claude.json"),
            serde_json::json!({"oauthAccount": {"organizationUuid": FALLBACK_ORG},
                "mcpServers": {"github": {"command": "gh-mcp", "args": ["new"]},
                               "cortex": {"type": "http", "url": "https://c/mcp"}}}),
        );
        write_json(
            &s.primary.join(".claude.json"),
            serde_json::json!({"oauthAccount": {"organizationUuid": PRIMARY_ORG, "emailAddress": "p@x"},
                "mcpServers": {"github": {"command": "gh-mcp"}, "old": {"command": "o"}},
                "numStartups": 7}),
        );
        let mut got = sync_config(&s.fallback, &s.primary, &backups).unwrap();
        got.servers.sort();
        assert_eq!(got.servers, ["+cortex", "-old", "~github"]);
        let t = read_json(&s.primary.join(".claude.json"));
        assert_eq!(
            t["mcpServers"],
            read_json(&s.fallback.join(".claude.json"))["mcpServers"]
        );
        assert_eq!(
            t["oauthAccount"]["organizationUuid"], PRIMARY_ORG,
            "login kept"
        );
        assert_eq!(t["oauthAccount"]["emailAddress"], "p@x");
        assert_eq!(t["numStartups"], 7, "other keys kept");
    }

    #[test]
    fn sync_copies_every_yes_and_never_a_no() {
        let s = setup();
        let backups = s.root.join("backups");
        write_json(
            &s.fallback.join(".claude.json"),
            serde_json::json!({"projects": {
                "/a": {"hasTrustDialogAccepted": true, "hasClaudeMdExternalIncludesApproved": true,
                       "hasClaudeMdExternalIncludesWarningShown": true, "enabledMcpjsonServers": ["x", "y"],
                       "lastCost": 5, "allowedTools": ["Bash"]},
                "/b": {"hasTrustDialogAccepted": false}}}),
        );
        write_json(
            &s.primary.join(".claude.json"),
            serde_json::json!({"oauthAccount": {"organizationUuid": PRIMARY_ORG}, "projects": {
                "/a": {"enabledMcpjsonServers": ["y", "z"]},
                "/b": {"hasTrustDialogAccepted": true}}}),
        );
        let got = sync_config(&s.fallback, &s.primary, &backups).unwrap();
        assert_eq!(got.answers, 4, "{got:?}");
        let t = read_json(&s.primary.join(".claude.json"));
        let a = &t["projects"]["/a"];
        assert_eq!(a["hasTrustDialogAccepted"], true);
        assert_eq!(a["hasClaudeMdExternalIncludesApproved"], true);
        assert_eq!(a["hasClaudeMdExternalIncludesWarningShown"], true);
        assert_eq!(
            a["enabledMcpjsonServers"],
            serde_json::json!(["y", "z", "x"])
        );
        assert!(
            a.get("lastCost").is_none() && a.get("allowedTools").is_none(),
            "only answers: {a}"
        );
        assert_eq!(
            t["projects"]["/b"]["hasTrustDialogAccepted"], true,
            "a yes here is kept"
        );
    }

    #[test]
    fn sync_mirrors_the_local_servers_of_every_project() {
        let s = setup();
        let backups = s.root.join("backups");
        write_json(
            &s.fallback.join(".claude.json"),
            serde_json::json!({"mcpServers": {}, "projects": {"/repo": {"mcpServers": {"db": {"command": "db"}}}}}),
        );
        write_json(
            &s.primary.join(".claude.json"),
            serde_json::json!({"projects": {"/old": {"mcpServers": {"gone": {"command": "g"}}, "lastCost": 1}}}),
        );
        let mut got = sync_config(&s.fallback, &s.primary, &backups).unwrap();
        got.servers.sort();
        assert_eq!(got.servers, ["+db (/repo)", "-gone (/old)"]);
        let t = read_json(&s.primary.join(".claude.json"));
        assert_eq!(t["projects"]["/repo"]["mcpServers"]["db"]["command"], "db");
        assert!(t["projects"]["/old"].get("mcpServers").is_none());
        assert_eq!(t["projects"]["/old"]["lastCost"], 1);
    }

    #[test]
    fn sync_keeps_the_mode_and_the_order_and_backs_up_before_writing() {
        let s = setup();
        let backups = s.root.join("backups");
        let to = s.primary.join(".claude.json");
        fs::write(&to, r#"{"zeta": 1, "alpha": {"y": 1, "b": 2}}"#).unwrap();
        fs::set_permissions(&to, fs::Permissions::from_mode(0o644)).unwrap();
        sync_config(&s.fallback, &s.primary, &backups).unwrap();
        let body = fs::read_to_string(&to).unwrap();
        let at = |k: &str| body.find(k).unwrap();
        assert!(
            at("zeta") < at("alpha") && at("\"y\"") < at("\"b\""),
            "{body}"
        );
        assert_eq!(mode(&to), 0o644);
        let b = backups_of(&backups);
        assert_eq!(b.len(), 1, "{b:?}");
        let saved = backups.join(&b[0]);
        assert_eq!(
            fs::read_to_string(&saved).unwrap(),
            r#"{"zeta": 1, "alpha": {"y": 1, "b": 2}}"#
        );
        assert_eq!(mode(&saved), 0o600);
        // Nothing to change: no write, no backup.
        let before = fs::metadata(&to).unwrap();
        assert_eq!(
            sync_config(&s.fallback, &s.primary, &backups).unwrap(),
            Synced::default()
        );
        assert_eq!(
            fs::metadata(&to).unwrap().modified().unwrap(),
            before.modified().unwrap()
        );
        assert_eq!(backups_of(&backups).len(), 1);
    }

    #[test]
    fn sync_keeps_the_last_ten_backups_of_a_folder() {
        let s = setup();
        let backups = s.root.join("backups");
        for i in 0..12 {
            write_json(
                &s.fallback.join(".claude.json"),
                serde_json::json!({"mcpServers": {"s": {"command": format!("c{i}")}}}),
            );
            sync_config(&s.fallback, &s.primary, &backups).unwrap();
        }
        assert_eq!(backups_of(&backups).len(), 10);
    }

    #[test]
    fn sync_creates_a_missing_file_private_and_without_a_login() {
        let s = setup();
        let backups = s.root.join("backups");
        let to = s.root.join("fresh");
        link(&s.fallback, &to).unwrap();
        let _ = fs::remove_file(to.join(".claude.json"));
        write_json(
            &s.fallback.join(".claude.json"),
            serde_json::json!({"oauthAccount": {"organizationUuid": FALLBACK_ORG},
                "mcpServers": {"cortex": {"type": "http", "url": "https://c/mcp"}},
                "projects": {"/r": {"hasTrustDialogAccepted": true}}}),
        );
        let got = sync_config(&s.fallback, &to, &backups).unwrap();
        assert_eq!(got.servers, ["+cortex"]);
        let t = read_json(&to.join(".claude.json"));
        assert!(t.get("oauthAccount").is_none(), "no login copied: {t}");
        assert_eq!(t["projects"]["/r"]["hasTrustDialogAccepted"], true);
        assert_eq!(mode(&to.join(".claude.json")), 0o600);
        assert!(backups_of(&backups).is_empty(), "nothing to back up");
    }

    #[test]
    fn sync_never_writes_through_a_symlink_or_past_a_held_lock() {
        let s = setup();
        let backups = s.root.join("backups");
        write_json(
            &s.fallback.join(".claude.json"),
            serde_json::json!({"mcpServers": {"new": {"command": "n"}}}),
        );
        let lock = s.primary.join(".claude.json.lock");
        fs::create_dir(&lock).unwrap();
        let before = fs::read_to_string(s.primary.join(".claude.json")).unwrap();
        let err = sync_config(&s.fallback, &s.primary, &backups).unwrap_err();
        assert!(err.contains("lock"), "{err}");
        assert_eq!(
            fs::read_to_string(s.primary.join(".claude.json")).unwrap(),
            before
        );
        assert!(lock.is_dir());
        fs::remove_dir(&lock).unwrap();
        let real = s.root.join("real.json");
        fs::rename(s.primary.join(".claude.json"), &real).unwrap();
        symlink(&real, s.primary.join(".claude.json")).unwrap();
        let err = sync_config(&s.fallback, &s.primary, &backups).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(fs::read_to_string(&real).unwrap(), before);
    }

    #[test]
    fn sync_from_a_source_without_a_file_is_an_error_not_an_empty_mirror() {
        let s = setup();
        fs::remove_file(s.fallback.join(".claude.json")).unwrap();
        let before = fs::read_to_string(s.primary.join(".claude.json")).unwrap();
        let err = sync_config(&s.fallback, &s.primary, &s.root.join("b")).unwrap_err();
        assert!(err.contains(".claude.json"), "{err}");
        assert_eq!(
            fs::read_to_string(s.primary.join(".claude.json")).unwrap(),
            before
        );
    }

    #[test]
    fn sharing_trust_backs_up_too() {
        let s = setup();
        let backups = s.root.join("backups");
        let from = s.fallback.join(".claude.json");
        let mut f = read_json(&from);
        f["projects"] = serde_json::json!({"/r": {"hasTrustDialogAccepted": true}});
        write_json(&from, f);
        share_trust(&s.fallback, &s.primary, &["/r".to_string()], &backups).unwrap();
        assert_eq!(backups_of(&backups).len(), 1);
    }

    #[test]
    fn only_a_missing_login_file_means_nothing_to_share() {
        let s = setup();
        let not_a_dir = s.root.join("file");
        fs::write(&not_a_dir, "").unwrap();
        assert!(share_trust(
            &not_a_dir,
            &s.primary,
            &["/r".to_string()],
            &s.root.join("backups")
        )
        .is_err());
    }
}

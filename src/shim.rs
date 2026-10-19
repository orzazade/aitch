//! The `claude` shim: the hook links `~/.aitch/bin/claude` to aitch and puts that folder first on PATH, so `claude`
//! routes in any shell or terminal, including one that replaces shell functions named `claude` (cmux does). A lookup
//! of `claude` on PATH can then lead back to aitch, or through a wrapper that runs `claude` again, so the real one is
//! found by skipping those.
use crate::config;
use std::env;
use std::ffi::{CString, OsString};
use std::fmt;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The exec chain aitch began: its pid, then `:` and the hex bytes of each `claude` file it started (resolved, so a
/// relative PATH entry or a later `cd` cannot change which file it names). exec keeps the pid, so a wrapper aitch
/// started that runs `claude` again by exec comes back to aitch with the same pid.
const CHAIN: &str = "AITCH_CLAUDE_CHAIN";

/// No `claude` to start. main exits 127 for it, as a shell does for a command it cannot find.
#[derive(Debug)]
pub struct NoClaude;

impl fmt::Display for NoClaude {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "no claude found on PATH besides aitch itself and the claude files it already tried"
        )
    }
}

impl std::error::Error for NoClaude {}

/// Make `~/.aitch/bin/claude` a link to this aitch and return the folder. The link is swapped in by rename, so
/// shells starting together never see it missing.
pub fn install() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = config::config_dir().join("bin");
    let link = dir.join("claude");
    let me = env::current_exe()?;
    if fs::read_link(&link).ok() == Some(me.clone()) {
        return Ok(dir);
    }
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".claude.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(&me, &tmp)?;
    fs::rename(&tmp, &link)?;
    Ok(dir)
}

/// The `claude` files already started in this process's chain, or None when aitch did not begin one here.
pub fn tried() -> Result<Option<Vec<PathBuf>>, Box<dyn std::error::Error>> {
    let Ok(chain) = env::var(CHAIN) else {
        return Ok(None);
    };
    let mut parts = chain.split(':');
    if parts.next() != Some(&std::process::id().to_string()) {
        return Ok(None);
    }
    let paths = parts
        .map(|h| hex::decode(h).map(|b| PathBuf::from(OsString::from_vec(b))))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{} is damaged: {}", CHAIN, e))?;
    Ok(Some(paths))
}

/// A command for the next real `claude` after `tried` (see `next`), carrying the chain with it added.
pub fn command(tried: &[PathBuf]) -> Result<Command, Box<dyn std::error::Error>> {
    let (target, real) = next(tried)?;
    let mut chain = std::process::id().to_string();
    for p in tried.iter().chain([&real]) {
        chain.push(':');
        chain.push_str(&hex::encode(p.as_os_str().as_bytes()));
    }
    let mut cmd = Command::new(target);
    cmd.env(CHAIN, chain);
    Ok(cmd)
}

/// The first `claude` on PATH this user can run that is not aitch itself and not in `tried`, as an absolute path and
/// as the file it resolves to. cmux's shims are skipped too once cmux's wrapper has run in this process: it sets
/// CMUX_CLAUDE_PID to its pid before it execs the claude it found, and exec keeps the pid. Before that (a shell where
/// PATH reaches the shim first), they are not, so the wrapper still runs.
fn next(tried: &[PathBuf]) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let me = fs::canonicalize(env::current_exe()?)?;
    let cwd = env::current_dir()?;
    let skip_cmux = env::var("CMUX_CLAUDE_PID").is_ok_and(|p| p == std::process::id().to_string());
    let cmux_named = env::var_os("CMUX_CLAUDE_WRAPPER_SHIM").map(PathBuf::from);
    let path = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path) {
        // An empty entry is the current folder, as in a shell; joining makes every candidate absolute.
        let candidate = cwd.join(dir).join("claude");
        let Ok(real) = fs::canonicalize(&candidate) else {
            continue;
        };
        if !fs::metadata(&real).is_ok_and(|m| m.is_file())
            || !runnable(&real)
            || real == me
            || tried.contains(&real)
        {
            continue;
        }
        if skip_cmux && is_cmux_shim(&candidate, &real, cmux_named.as_deref()) {
            continue;
        }
        return Ok((candidate, real));
    }
    Err(Box::new(NoClaude))
}

/// Whether this user may run `file`, as the shell's own lookup decides.
fn runnable(file: &Path) -> bool {
    CString::new(file.as_os_str().as_bytes())
        .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0)
}

/// cmux's own test for its claude shims (cmux-claude-wrapper, `cmux_claude_wrapper_is_self_or_shim`): the file it
/// names in CMUX_CLAUDE_WRAPPER_SHIM, or a `claude` in a `cmux-cli-shims` folder or one level below it.
fn is_cmux_shim(candidate: &Path, real: &Path, named: Option<&Path>) -> bool {
    if named.is_some_and(|n| fs::canonicalize(n).ok().as_deref() == Some(real)) {
        return true;
    }
    let shims = |p: Option<&Path>| p.and_then(Path::file_name) == Some("cmux-cli-shims".as_ref());
    let parent = candidate.parent();
    shims(parent) || shims(parent.and_then(Path::parent))
}

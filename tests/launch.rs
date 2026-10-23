//! `aitch launch` end to end: a temp HOME with a config, a fake `claude` that prints what it got, and a fake quota
//! command that follows the quota command contract.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const AITCH: &str = env!("CARGO_BIN_EXE_aitch");

/// Writing a script and starting a process never overlap: a child forked while another test thread has a script
/// open for writing holds that file, and Linux then refuses to exec it ("Text file busy").
static FORK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn fork_lock() -> std::sync::MutexGuard<'static, ()> {
    FORK.lock().unwrap_or_else(|e| e.into_inner())
}

trait Locked {
    fn locked_spawn(&mut self) -> std::io::Result<std::process::Child>;
    fn locked_output(&mut self) -> std::io::Result<Output>;
    fn locked_status(&mut self) -> std::io::Result<std::process::ExitStatus>;
}

impl Locked for Command {
    fn locked_spawn(&mut self) -> std::io::Result<std::process::Child> {
        let _g = fork_lock();
        self.spawn()
    }

    fn locked_output(&mut self) -> std::io::Result<Output> {
        use std::process::Stdio;
        self.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .locked_spawn()?
            .wait_with_output()
    }

    fn locked_status(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.locked_spawn()?.wait()
    }
}

struct Env {
    _tmp: TempDir,
    home: PathBuf,
    bin: PathBuf,
    work: PathBuf,
    primary: PathBuf,
    acme: PathBuf,
    personal: PathBuf,
}

fn script(path: &Path, body: &str) {
    let _g = fork_lock();
    fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn login(dir: &Path, org: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"organizationUuid":"{org}"}},"mcpServers":{{"github":{{"command":"gh"}}}}}}"#),
    )
    .unwrap();
}

impl Env {
    /// `quota` is the fake quota command's body. The fallback and primary dirs are logged in and linked.
    fn new(quota: &str) -> Env {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let bin = home.join("bin");
        let work = home.join(".claude-work");
        let primary = home.join(".claude-alt");
        let acme = home.join("Projects/acme/svc");
        let personal = home.join("Projects/personal/x");
        for d in [&bin, &acme, &personal] {
            fs::create_dir_all(d).unwrap();
        }
        login(&work, "org-fallback");
        fs::write(work.join("CLAUDE.md"), "rules").unwrap();
        fs::write(work.join("settings.json"), "{}").unwrap();
        script(
            &bin.join("claude"),
            "echo \"DIR=${CLAUDE_CONFIG_DIR-unset}\"\necho \"CONNECTORS=${ENABLE_CLAUDEAI_MCP_SERVERS-unset}\"\nfor a in \"$@\"; do echo \"ARG=$a\"; done\nexit ${FAKE_EXIT:-0}\n",
        );
        script(&bin.join("quota"), quota);
        let env = Env {
            _tmp: tmp,
            home,
            bin,
            work,
            primary,
            acme,
            personal,
        };
        let out = env.run(&env.home, &["link", &env.work_s(), &env.primary_s()]);
        assert!(out.status.success(), "link: {}", text(&out.stderr));
        login(&env.primary, "org-primary");
        env.write_config(true);
        env
    }

    fn work_s(&self) -> String {
        self.work.to_string_lossy().to_string()
    }

    fn primary_s(&self) -> String {
        self.primary.to_string_lossy().to_string()
    }

    fn write_config(&self, with_route: bool) {
        let route = if with_route {
            format!(
                "    route:\n      claude:\n        primary: {}\n        primary_org: org-primary\n        fallback_org: org-fallback\n        quota: {} --room\n",
                self.primary_s(),
                self.bin.join("quota").display()
            )
        } else {
            String::new()
        };
        let yaml = format!(
            "default: personal\nprofiles:\n  personal:\n    providers: {{ claude: '' }}\n    directories: [{}]\n  work:\n    providers: {{ claude: {} }}\n    directories: [{}]\n{route}",
            self.home.join("Projects/personal").display(),
            self.work_s(),
            self.home.join("Projects/acme").display(),
        );
        fs::create_dir_all(self.home.join(".aitch")).unwrap();
        fs::write(self.home.join(".aitch/config.yaml"), yaml).unwrap();
    }

    fn cmd(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(AITCH);
        c.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("NO_COLOR", "1")
            .env("CLAUDE_CONFIG_DIR", "/inherited");
        c
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.cmd(cwd, args).locked_output().unwrap()
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

const ROOM: &str = "echo '{\"verdict\":\"room\",\"reason\":\"7d 43%, 5h 9%\"}'\n";
const FULL: &str = "echo '{\"verdict\":\"full\",\"reason\":\"7d 100%\"}'\nexit 3\n";
const UNKNOWN: &str = "echo '{\"verdict\":\"unknown\",\"reason\":\"no sample for 2 h\"}'\nexit 4\n";

#[test]
fn room_launches_on_primary_with_args_unchanged() {
    let e = Env::new(ROOM);
    let out = e.run(
        &e.acme,
        &[
            "launch",
            "claude",
            "--",
            "--resume",
            "abc",
            "two words",
            "--help",
        ],
    );
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{se}");
    assert!(so.contains(&format!("DIR={}\n", e.primary_s())), "{so}");
    // One word: claude's --mcp-config takes values until the next flag, so a separate path would eat the prompt.
    let mcp = e.home.join(".aitch/run/work-mcp.json");
    assert!(
        so.contains(&format!("ARG=--mcp-config={}\n", mcp.display())),
        "{so}"
    );
    assert!(!so.contains("ARG=--mcp-config\n"), "{so}");
    // The primary account's own claude.ai connectors stay out; the fallback dir's servers come in by file.
    assert!(so.contains("CONNECTORS=false\n"), "{so}");
    assert!(
        so.contains("ARG=--resume\nARG=abc\nARG=two words\nARG=--help\n"),
        "{so}"
    );
    assert!(se.contains("aitch: work → primary"), "{se}");
    assert!(se.contains("7d 43%, 5h 9%"), "{se}");
}

#[test]
fn full_launches_on_work_with_a_plain_line() {
    let e = Env::new(FULL);
    let out = e.run(&e.acme, &["launch", "claude", "--", "-p", "hi"]);
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.work_s())), "{so}");
    assert!(!so.contains("--mcp-config"), "{so}");
    assert!(so.contains("CONNECTORS=unset\n"), "{so}");
    assert!(se.contains("aitch: work → fallback"), "{se}");
    assert!(se.contains("7d 100%") && !se.contains("UNKNOWN"), "{se}");
}

#[test]
fn unknown_launches_on_work_loudly() {
    let e = Env::new(UNKNOWN);
    let out = e.run(&e.acme, &["launch", "claude"]);
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.work_s())), "{so}");
    assert!(
        se.contains("QUOTA UNKNOWN") && se.contains("no sample for 2 h"),
        "{se}"
    );
}

/// A third folder linked from the fallback one and logged in to its own organization.
fn second_primary(e: &Env) -> PathBuf {
    let alt = e.home.join(".claude-alt2");
    let out = e.run(
        &e.home,
        &["link", &e.work_s(), &alt.to_string_lossy().to_string()],
    );
    assert!(out.status.success(), "link: {}", text(&out.stderr));
    login(&alt, "org-other");
    alt
}

#[test]
fn primary_option_launches_on_the_named_folder_and_tells_the_quota_command() {
    let e = Env::new(
        "echo \"{\\\"verdict\\\":\\\"room\\\",\\\"reason\\\":\\\"asked for $AITCH_PRIMARY\\\"}\"\n",
    );
    let alt = second_primary(&e);
    let out = e.run(
        &e.acme,
        &[
            "launch",
            "claude",
            "--primary",
            &alt.to_string_lossy(),
            "--",
            "x",
        ],
    );
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{se}");
    assert!(so.contains(&format!("DIR={}\n", alt.display())), "{so}");
    assert!(
        so.contains("ARG=--mcp-config=") && so.contains("ARG=x\n"),
        "{so}"
    );
    // Its login is not primary_org: the caller picked the account, so that guard is not the one to apply.
    assert!(
        se.contains(&format!("work → primary ({})", alt.display())),
        "{se}"
    );
    assert!(se.contains(&format!("asked for {}", alt.display())), "{se}");
}

#[test]
fn the_quota_command_is_told_the_route_primary_folder() {
    let e = Env::new(
        "echo \"{\\\"verdict\\\":\\\"room\\\",\\\"reason\\\":\\\"asked for $AITCH_PRIMARY\\\"}\"\n",
    );
    let out = e.run(&e.acme, &["launch", "claude"]);
    let se = text(&out.stderr);
    assert!(se.contains(&format!("asked for {}", e.primary_s())), "{se}");
}

#[test]
fn primary_option_still_needs_the_links_and_room() {
    let e = Env::new(FULL);
    let alt = second_primary(&e);
    let out = e.run(
        &e.acme,
        &["launch", "claude", "--primary", &alt.to_string_lossy()],
    );
    let so = text(&out.stdout);
    assert!(so.contains(&format!("DIR={}\n", e.work_s())), "{so}");

    let e = Env::new(ROOM);
    let alt = second_primary(&e);
    fs::remove_file(alt.join("CLAUDE.md")).unwrap();
    fs::write(alt.join("CLAUDE.md"), "other rules").unwrap();
    let out = e.run(
        &e.acme,
        &["launch", "claude", "--primary", &alt.to_string_lossy()],
    );
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.work_s())), "{so}");
    assert!(se.contains("NOT USING THE PRIMARY DIR"), "{se}");
}

#[test]
fn primary_option_without_a_route_refuses_a_folder_not_linked_to_the_profiles_own() {
    let e = Env::new(ROOM);
    let alt = second_primary(&e);
    let out = e.run(
        &e.personal,
        &["launch", "claude", "--primary", &alt.to_string_lossy()],
    );
    assert!(!out.status.success());
    assert!(text(&out.stdout).is_empty(), "claude ran");
    assert!(
        text(&out.stderr).contains("or a folder linked to it"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn primary_option_with_a_broken_config_refuses() {
    let e = Env::new(ROOM);
    let alt = second_primary(&e);
    fs::write(e.home.join(".aitch/config.yaml"), "profiles: [").unwrap();
    let out = e.run(
        &e.acme,
        &["launch", "claude", "--primary", &alt.to_string_lossy()],
    );
    assert!(!out.status.success());
    assert!(text(&out.stdout).is_empty(), "claude ran");
    assert!(
        text(&out.stderr).contains("the config does not load"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn primary_option_takes_a_home_path_and_refuses_an_empty_or_relative_one() {
    let e = Env::new(ROOM);
    let alt = second_primary(&e);
    let out = e.run(
        &e.acme,
        &["launch", "claude", "--primary", "~/.claude-alt2", "--", "x"],
    );
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{se}");
    assert!(so.contains(&format!("DIR={}\n", alt.display())), "{so}");
    for bad in ["", ".", "../.claude-alt2"] {
        let out = e.run(&e.acme, &["launch", "claude", "--primary", bad]);
        assert!(!out.status.success(), "{bad:?}");
        assert!(text(&out.stdout).is_empty(), "{bad:?}: claude ran");
        assert!(
            text(&out.stderr).contains("--primary needs an absolute folder"),
            "{bad:?}: {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn primary_option_on_a_folder_with_no_login_refuses() {
    let e = Env::new(ROOM);
    let alt = e.home.join(".claude-alt3");
    let out = e.run(
        &e.home,
        &["link", &e.work_s(), &alt.to_string_lossy().to_string()],
    );
    assert!(out.status.success(), "link: {}", text(&out.stderr));
    let out = e.run(
        &e.acme,
        &["launch", "claude", "--primary", &alt.to_string_lossy()],
    );
    assert!(!out.status.success());
    assert!(text(&out.stdout).is_empty(), "claude ran");
    assert!(
        text(&out.stderr).contains("not launching claude")
            && text(&out.stderr).contains(".claude-alt3"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn exit_code_of_claude_comes_back() {
    let e = Env::new(ROOM);
    let out = e
        .cmd(&e.acme, &["launch", "claude"])
        .env("FAKE_EXIT", "7")
        .locked_output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn profile_without_route_never_runs_the_quota_command() {
    let e = Env::new("touch \"$HOME/asked\"\necho '{\"verdict\":\"room\",\"reason\":\"\"}'\n");
    let out = e.run(&e.personal, &["launch", "claude", "--", "x"]);
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{se}");
    assert!(
        so.contains("DIR=/inherited\n") && so.contains("ARG=x\n"),
        "{so}"
    );
    assert!(se.is_empty(), "{se}");
    assert!(!e.home.join("asked").exists());
}

#[test]
fn work_folder_on_wrong_account_refuses_to_launch() {
    let e = Env::new(ROOM);
    login(&e.work, "org-primary");
    let out = e.run(&e.acme, &["launch", "claude"]);
    assert!(!out.status.success());
    assert!(text(&out.stdout).is_empty(), "claude ran");
    assert!(
        text(&out.stderr).contains("org-primary"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn hook_puts_the_claude_shim_on_path_only_with_a_route() {
    let e = Env::new(ROOM);
    for shell in ["zsh", "bash"] {
        let hook = text(&e.run(&e.home, &["hook", shell]).stdout);
        assert!(hook.contains("/.aitch/bin"), "{shell}: {hook}");
        // A shell function named claude is replaced by terminals that wrap claude (cmux does at the first prompt).
        assert!(!hook.contains("function claude"), "{shell}: {hook}");
        let check = Command::new(shell)
            .arg("-n")
            .arg("-c")
            .arg(&hook)
            .locked_output()
            .unwrap();
        assert!(
            check.status.success(),
            "{shell} -n: {}",
            text(&check.stderr)
        );
    }
    e.write_config(false);
    for shell in ["zsh", "bash"] {
        let hook = text(&e.run(&e.home, &["hook", shell]).stdout);
        assert!(!hook.contains("claude"), "{shell}: {hook}");
    }
}

#[test]
fn plain_claude_after_the_hook_is_the_shim_and_routes() {
    let e = Env::new(ROOM);
    let aitch_dir = Path::new(AITCH).parent().unwrap();
    let shim = e.home.join(".aitch/bin/claude");
    for (shell, kind) in [("bash", "type -P claude"), ("zsh", "whence -p claude")] {
        let out = Command::new(shell)
            .arg("-c")
            .arg(format!(
                "eval \"$(aitch hook {shell})\"; {kind}; claude --resume 'a b'"
            ))
            .current_dir(&e.acme)
            .env("HOME", &e.home)
            .env(
                "PATH",
                format!("{}:{}:/usr/bin:/bin", aitch_dir.display(), e.bin.display()),
            )
            .env("NO_COLOR", "1")
            .locked_output()
            .unwrap();
        let so = text(&out.stdout);
        assert!(
            so.starts_with(&format!("{}\n", shim.display())),
            "{shell}: {so} {}",
            text(&out.stderr)
        );
        assert!(
            so.contains(&format!("DIR={}\n", e.primary_s())),
            "{shell}: {so}"
        );
        assert!(so.contains("ARG=--resume\nARG=a b\n"), "{shell}: {so}");
    }
}

/// A folder with `claude` linked to aitch, as the hook makes it.
fn shim_in(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let link = dir.join("claude");
    std::os::unix::fs::symlink(AITCH, &link).unwrap();
    link
}

impl Env {
    /// Run `claude` through a shim at `program` with only `path` (then /usr/bin:/bin) on PATH.
    fn claude(&self, program: &Path, path: &[&Path], args: &[&str]) -> Command {
        let dirs: Vec<String> = path.iter().map(|p| p.display().to_string()).collect();
        let mut c = Command::new(program);
        c.args(args)
            .current_dir(&self.acme)
            .env("HOME", &self.home)
            .env("PATH", format!("{}:/usr/bin:/bin", dirs.join(":")))
            .env("NO_COLOR", "1")
            .env("CLAUDE_CONFIG_DIR", "/inherited")
            .env_remove("CMUX_CLAUDE_WRAPPER_SHIM")
            .env_remove("AITCH_CLAUDE_CHAIN");
        c
    }
}

/// Output, or a panic after 20 s: a launch loop must fail the test, not hang it.
fn within_20s(c: &mut Command) -> Output {
    use std::process::Stdio;
    let child = c
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .locked_spawn()
        .unwrap();
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output()));
    match rx.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok(out) => out.unwrap(),
        Err(_) => {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            panic!("claude did not finish in 20 s: a launch loop");
        }
    }
}

const COUNTED_ROOM: &str =
    "echo x >> \"$HOME/asked\"\necho '{\"verdict\":\"room\",\"reason\":\"7d 43%\"}'\n";

fn quota_runs(e: &Env) -> usize {
    fs::read_to_string(e.home.join("asked"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn claude_through_the_shim_routes() {
    let e = Env::new(COUNTED_ROOM);
    let shim = shim_in(&e.home.join("shim"));
    let out =
        within_20s(&mut e.claude(&shim, &[&e.home.join("shim"), &e.bin], &["--resume", "a b"]));
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{se}");
    assert!(
        so.contains(&format!("DIR={}\n", e.primary_s())),
        "{so} {se}"
    );
    assert!(so.contains("CONNECTORS=false\n"), "{so}");
    assert!(so.contains("ARG=--resume\nARG=a b\n"), "{so}");
    assert!(se.contains("aitch: work → primary"), "{se}");
    assert_eq!(quota_runs(&e), 1);
}

#[test]
fn the_shim_skips_aitch_itself_and_cmux_shims() {
    let e = Env::new(ROOM);
    let shim = shim_in(&e.home.join("shim"));
    // Another link to aitch, cmux's per-tab shim (found by its path) and a shim cmux names in its environment.
    let again = e.home.join("again");
    shim_in(&again);
    let tab = e.home.join(".cmuxterm/cmux-cli-shims/ABC");
    let named = e.home.join("named");
    for d in [&tab, &named] {
        fs::create_dir_all(d).unwrap();
        script(&d.join("claude"), "echo CMUX\nexit 9\n");
    }
    // As cmux's wrapper starts the claude it found: CMUX_CLAUDE_PID is its pid, which exec keeps.
    let as_cmux = "export CMUX_CLAUDE_PID=$$; exec \"$0\" \"$@\"";
    let out = within_20s(
        e.claude(
            Path::new("/bin/sh"),
            &[&e.home.join("shim"), &again, &tab, &named, &e.bin],
            &["-c", as_cmux, &shim.to_string_lossy()],
        )
        .env("CMUX_CLAUDE_WRAPPER_SHIM", named.join("claude")),
    );
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert!(!so.contains("CMUX"), "{so}");
    assert!(so.contains(&format!("DIR={}\n", e.primary_s())), "{so}");
}

#[test]
fn a_shell_that_reaches_the_shim_first_still_runs_cmux_wrapper_once() {
    let e = Env::new(COUNTED_ROOM);
    let shim = shim_in(&e.home.join("shim"));
    // cmux's per-tab shim acting as cmux's wrapper: it adds its settings and starts the claude its lookup finds.
    let tab = e.home.join(".cmuxterm/cmux-cli-shims/ABC");
    fs::create_dir_all(&tab).unwrap();
    script(
        &tab.join("claude"),
        &format!(
            "echo CMUX-WRAPPER\nexport CMUX_CLAUDE_PID=$$\nexec '{}' --settings cmux \"$@\"\n",
            shim.display()
        ),
    );
    let out = within_20s(&mut e.claude(&shim, &[&e.home.join("shim"), &tab, &e.bin], &["x"]));
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert_eq!(so.matches("CMUX-WRAPPER").count(), 1, "{so}");
    assert!(so.contains(&format!("DIR={}\n", e.primary_s())), "{so}");
    assert_eq!(so.matches("ARG=--settings\n").count(), 1, "{so}");
    assert_eq!(so.matches("ARG=--mcp-config=").count(), 1, "{so}");
    assert!(so.ends_with("ARG=x\n"), "{so}");
    assert_eq!(quota_runs(&e), 1);
}

#[test]
fn the_shim_fails_loudly_when_no_real_claude_is_on_path() {
    let e = Env::new(COUNTED_ROOM);
    let shim = shim_in(&e.home.join("shim"));
    let out = within_20s(&mut e.claude(&shim, &[&e.home.join("shim")], &[]));
    let se = text(&out.stderr);
    assert_eq!(out.status.code(), Some(127), "{se}");
    assert!(se.contains("no claude found on PATH"), "{se}");
}

#[test]
fn the_shim_passes_the_exit_code_of_claude_through() {
    let e = Env::new(ROOM);
    let shim = shim_in(&e.home.join("shim"));
    let out = within_20s(
        e.claude(&shim, &[&e.home.join("shim"), &e.bin], &[])
            .env("FAKE_EXIT", "7"),
    );
    assert_eq!(out.status.code(), Some(7), "{}", text(&out.stderr));
}

#[test]
fn a_wrapper_that_runs_claude_again_does_not_loop_or_route_twice() {
    let e = Env::new(COUNTED_ROOM);
    let shim = shim_in(&e.home.join("shim"));
    // Like cmux's wrapper: it runs `claude` from PATH again, which finds the shim.
    let wrap = e.home.join("wrap");
    fs::create_dir_all(&wrap).unwrap();
    script(&wrap.join("claude"), "exec claude \"$@\"\n");
    let out = within_20s(&mut e.claude(&shim, &[&e.home.join("shim"), &wrap, &e.bin], &["x"]));
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.primary_s())), "{so}");
    assert_eq!(so.matches("ARG=--mcp-config=").count(), 1, "{so}");
    assert!(so.ends_with("ARG=x\n"), "{so}");
    assert_eq!(quota_runs(&e), 1);
}

#[test]
fn a_shell_with_the_old_claude_function_routes_once() {
    let e = Env::new(COUNTED_ROOM);
    let aitch_dir = Path::new(AITCH).parent().unwrap();
    let shim_dir = e.home.join(".aitch/bin");
    shim_in(&shim_dir);
    // What `aitch hook` printed before the shim; shells started then keep it.
    let old = "function claude { command aitch launch claude -- \"$@\"; }";
    let out = within_20s(
        Command::new("bash")
            .arg("-c")
            .arg(format!("{old}; claude x"))
            .current_dir(&e.acme)
            .env("HOME", &e.home)
            .env(
                "PATH",
                format!(
                    "{}:{}:{}:/usr/bin:/bin",
                    shim_dir.display(),
                    aitch_dir.display(),
                    e.bin.display()
                ),
            )
            .env("NO_COLOR", "1")
            .env_remove("AITCH_CLAUDE_CHAIN"),
    );
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.primary_s())), "{so}");
    assert_eq!(so.matches("ARG=--mcp-config=").count(), 1, "{so}");
    assert_eq!(quota_runs(&e), 1);
}

#[test]
fn the_hook_moves_an_inherited_shim_folder_back_to_the_front() {
    let e = Env::new(ROOM);
    let aitch_dir = Path::new(AITCH).parent().unwrap();
    let shim_dir = e.home.join(".aitch/bin");
    shim_in(&shim_dir);
    // A nested shell inherits the shim folder, then its startup files put another claude ahead of it.
    let path = format!(
        "{}:{}:{}:/usr/bin:/bin",
        e.bin.display(),
        shim_dir.display(),
        aitch_dir.display()
    );
    for shell in ["bash", "zsh"] {
        let out = within_20s(
            Command::new(shell)
                .arg("-c")
                .arg(format!(
                    "eval \"$(aitch hook {shell})\"; echo \"P=$PATH\"; claude x"
                ))
                .current_dir(&e.acme)
                .env("HOME", &e.home)
                .env("PATH", &path)
                .env("NO_COLOR", "1")
                .env_remove("AITCH_CLAUDE_CHAIN"),
        );
        let so = text(&out.stdout);
        assert!(
            so.contains(&format!(
                "P={}:{}:{}:/usr/bin:/bin\n",
                shim_dir.display(),
                e.bin.display(),
                aitch_dir.display()
            )),
            "{shell}: {so}"
        );
        assert!(
            so.contains(&format!("DIR={}\n", e.primary_s())),
            "{shell}: {so}"
        );
    }
}

#[test]
fn a_wrapper_in_a_folder_named_with_a_newline_does_not_loop() {
    let e = Env::new(COUNTED_ROOM);
    let shim = shim_in(&e.home.join("shim"));
    // A newline once split the chain's entries. (macOS refuses names that are not UTF-8, the other case.)
    let wrap = e.home.join("wr\nap");
    fs::create_dir_all(&wrap).unwrap();
    script(&wrap.join("claude"), "exec claude \"$@\"\n");
    let out = within_20s(&mut e.claude(&shim, &[&e.home.join("shim"), &wrap, &e.bin], &["x"]));
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert!(so.ends_with("ARG=x\n"), "{so}");
    assert_eq!(quota_runs(&e), 1);
}

#[test]
fn a_wrapper_that_changes_folder_still_reaches_the_next_claude() {
    let e = Env::new(ROOM);
    let shim = shim_in(&e.home.join("shim"));
    // Relative PATH entry `bin`: from pkg it is pkg/bin, whose claude moves up a folder, where it is the real one.
    let pkg = e.home.join("pkg");
    fs::create_dir_all(pkg.join("bin")).unwrap();
    script(&pkg.join("bin/claude"), "cd ..\nexec claude \"$@\"\n");
    let out = within_20s(
        e.claude(&shim, &[&e.home.join("shim"), Path::new("bin")], &["x"])
            .current_dir(&pkg),
    );
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert!(so.ends_with("ARG=x\n"), "{so}");
}

#[test]
fn an_empty_path_entry_finds_claude_in_the_current_folder() {
    let e = Env::new(ROOM);
    let shim = shim_in(&e.home.join("shim"));
    let out = within_20s(
        e.claude(&shim, &[&e.home.join("shim"), Path::new("")], &["x"])
            .current_dir(&e.bin),
    );
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert!(so.ends_with("ARG=x\n"), "{so}");
}

#[test]
fn the_shim_skips_a_claude_this_user_cannot_run() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new(ROOM);
    let shim = shim_in(&e.home.join("shim"));
    // Runnable by group and others, not by its owner (this user).
    let locked = e.home.join("locked");
    fs::create_dir_all(&locked).unwrap();
    script(&locked.join("claude"), "echo LOCKED\n");
    fs::set_permissions(locked.join("claude"), fs::Permissions::from_mode(0o055)).unwrap();
    let out = within_20s(&mut e.claude(&shim, &[&e.home.join("shim"), &locked, &e.bin], &["x"]));
    let so = text(&out.stdout);
    assert!(out.status.success(), "{so} {}", text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.primary_s())), "{so}");
}

#[test]
fn bad_config_still_installs_the_base_hook() {
    let e = Env::new(ROOM);
    let cfg = e.home.join(".aitch/config.yaml");
    let bad = fs::read_to_string(&cfg)
        .unwrap()
        .replace("primary:", "typo:");
    fs::write(&cfg, bad).unwrap();
    for shell in ["zsh", "bash"] {
        let out = e.run(&e.home, &["hook", shell]);
        let hook = text(&out.stdout);
        assert!(
            hook.contains("_aitch_hook") && hook.contains("source"),
            "{shell}: {hook}"
        );
        assert!(!hook.contains("claude"), "{shell}: {hook}");
        assert!(!text(&out.stderr).is_empty(), "{shell}: no warning");
    }
}

#[test]
fn launch_resolves_the_logical_directory_like_the_hook() {
    let e = Env::new("touch \"$HOME/asked\"\necho '{\"verdict\":\"room\",\"reason\":\"\"}'\n");
    // A personal folder that is a symlink into a routed folder: the hook sees $PWD (personal), so must launch.
    let link = e.home.join("Projects/personal/link");
    std::os::unix::fs::symlink(&e.acme, &link).unwrap();
    let out = e
        .cmd(&link, &["launch", "claude"])
        .env("PWD", &link)
        .locked_output()
        .unwrap();
    assert!(
        text(&out.stdout).contains("DIR=/inherited\n"),
        "{}",
        text(&out.stdout)
    );
    assert!(!e.home.join("asked").exists());
}

#[test]
fn hook_shim_is_safe_from_an_existing_alias() {
    let e = Env::new(ROOM);
    for shell in ["zsh", "bash"] {
        let hook = text(&e.run(&e.home, &["hook", shell]).stdout);
        assert!(hook.contains("unalias claude"), "{shell}: {hook}");
    }
}

#[test]
fn link_with_relative_paths_makes_working_links() {
    let e = Env::new(ROOM);
    let out = e.run(&e.home, &["link", ".claude-work", "rel"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(e.home.join("rel/CLAUDE.md").exists(), "dangling link");
    // Run again: nothing to do, and still no error.
    let again = e.run(&e.home, &["link", ".claude-work", "rel"]);
    assert!(again.status.success(), "{}", text(&again.stderr));
}

#[test]
fn bad_config_warns_and_launches_with_the_inherited_dir() {
    let e = Env::new(ROOM);
    let cfg = e.home.join(".aitch/config.yaml");
    let bad = fs::read_to_string(&cfg)
        .unwrap()
        .replace("primary:", "typo:");
    fs::write(&cfg, bad).unwrap();
    let out = e.run(&e.acme, &["launch", "claude", "--", "x"]);
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(
        so.contains("DIR=/inherited\n") && so.contains("ARG=x\n"),
        "{so}"
    );
    assert!(se.contains("config") && se.contains("typo"), "{se}");
}

#[test]
fn which_uses_the_logical_directory_like_the_hook() {
    let e = Env::new(ROOM);
    let link = e.home.join("Projects/personal/link");
    std::os::unix::fs::symlink(&e.acme, &link).unwrap();
    let out = e
        .cmd(&link, &["which"])
        .env("PWD", &link)
        .locked_output()
        .unwrap();
    assert!(
        text(&out.stdout).contains("personal"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn hook_survives_errexit_in_both_shells() {
    let e = Env::new(ROOM);
    for (shell, errexit) in [("bash", "set -e"), ("zsh", "setopt err_exit")] {
        let script = format!("{errexit}; eval \"$('{AITCH}' hook {shell})\"; echo reached");
        let run = Command::new(shell)
            .arg("-c")
            .arg(&script)
            .current_dir(&e.acme)
            .env("HOME", &e.home)
            .env("PATH", format!("{}:/usr/bin:/bin", e.bin.display()))
            .locked_output()
            .unwrap();
        assert!(
            text(&run.stdout).contains("reached"),
            "{shell}: {}",
            text(&run.stderr)
        );
    }
}

#[test]
fn hook_rebuilds_the_cache_from_a_hand_edited_config() {
    let e = Env::new(ROOM);
    let cache = e.home.join(".aitch/cache");
    let _ = fs::remove_file(&cache);
    let out = e.run(&e.home, &["hook", "zsh"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let body = fs::read_to_string(&cache).expect("hook wrote no cache");
    assert!(body.contains(&e.work_s()), "{body}");
}

#[test]
fn shells_starting_together_all_get_the_hook() {
    let e = Env::new(ROOM);
    let _ = fs::remove_file(e.home.join(".aitch/cache"));
    let kids: Vec<_> = (0..8)
        .map(|_| e.cmd(&e.home, &["hook", "zsh"]).locked_spawn().unwrap())
        .collect();
    for k in kids {
        let out = k.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
}

#[test]
fn hook_survives_a_remote_sed_cannot_read_under_errexit() {
    let e = Env::new(ROOM);
    let repo = e.home.join("Projects/other/bad");
    fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&std::ffi::OsStr]| {
        assert!(Command::new("git")
            .args(args)
            .current_dir(&repo)
            .locked_status()
            .unwrap()
            .success())
    };
    git(&["init".as_ref(), "-q".as_ref()]);
    use std::os::unix::ffi::OsStrExt;
    let url = std::ffi::OsStr::from_bytes(b"git@host:\xff\xfe/x");
    git(&["remote".as_ref(), "add".as_ref(), "origin".as_ref(), url]);
    assert!(e.run(&e.home, &["hook", "bash"]).status.success());
    let run = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -e; source '{}'; echo reached",
            e.home.join(".aitch/cache").display()
        ))
        .current_dir(&repo)
        .env("PWD", &repo)
        .env("LC_ALL", "en_US.UTF-8")
        .locked_output()
        .unwrap();
    assert!(
        text(&run.stdout).contains("reached"),
        "{}",
        text(&run.stderr)
    );
}

#[test]
fn mcp_subcommand_runs_on_the_fallback_dir_without_the_quota_command() {
    let e = Env::new(&format!("touch \"$HOME/asked\"\n{ROOM}"));
    let out = e.run(
        &e.acme,
        &["launch", "claude", "--", "mcp", "add", "db", "--", "db-mcp"],
    );
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.work_s())), "{so}");
    assert!(!so.contains("--mcp-config"), "{so}");
    assert!(
        so.contains("ARG=mcp\nARG=add\nARG=db\nARG=--\nARG=db-mcp\n"),
        "{so}"
    );
    assert!(!e.home.join("asked").exists(), "quota command ran");
    assert!(se.contains("→ fallback"), "{se}");
}

#[test]
fn a_primary_launch_trusts_a_folder_the_fallback_dir_trusts() {
    let e = Env::new(ROOM);
    let json = |d: &Path| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(d.join(".claude.json")).unwrap()).unwrap()
    };
    let mut f = json(&e.work);
    f["projects"] =
        serde_json::json!({ e.acme.to_string_lossy(): {"hasTrustDialogAccepted": true} });
    fs::write(e.work.join(".claude.json"), f.to_string()).unwrap();
    let out = e.run(&e.acme, &["launch", "claude"]);
    assert!(text(&out.stdout).contains(&format!("DIR={}\n", e.primary_s())));
    let p = json(&e.primary);
    assert_eq!(
        p["projects"][e.acme.to_string_lossy().as_ref()]["hasTrustDialogAccepted"],
        true,
        "{p}"
    );
}

#[test]
fn primary_launch_in_a_repo_subfolder_gets_the_repo_local_servers() {
    let e = Env::new(ROOM);
    fs::create_dir_all(e.acme.join(".git")).unwrap();
    let sub = e.acme.join("src/deep");
    fs::create_dir_all(&sub).unwrap();
    let mut f: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(e.work.join(".claude.json")).unwrap()).unwrap();
    f["projects"] = serde_json::json!({
        e.acme.to_string_lossy(): {"mcpServers": {"db": {"command": "db"}}},
        sub.to_string_lossy(): {"mcpServers": {"old": {"command": "old"}}}
    });
    fs::write(e.work.join(".claude.json"), f.to_string()).unwrap();
    let out = e.run(&sub, &["launch", "claude"]);
    assert!(text(&out.stdout).contains(&format!("DIR={}\n", e.primary_s())));
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(e.home.join(".aitch/run/work-mcp.json")).unwrap())
            .unwrap();
    let servers = &mcp["mcpServers"];
    assert!(
        servers.get("db").is_some() && servers.get("github").is_some(),
        "{servers}"
    );
    assert!(servers.get("old").is_none(), "{servers}");
}

#[test]
fn mcp_subcommand_still_checks_the_fallback_login() {
    let e = Env::new(ROOM);
    login(&e.work, "someone-else");
    let out = e.run(&e.acme, &["launch", "claude", "--", "mcp", "list"]);
    assert!(!out.status.success());
    assert!(!text(&out.stdout).contains("DIR="), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("not launching"),
        "{}",
        text(&out.stderr)
    );
}

fn json_of(d: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(d.join(".claude.json")).unwrap()).unwrap()
}

fn set_json(d: &Path, v: serde_json::Value) {
    fs::write(d.join(".claude.json"), v.to_string()).unwrap();
}

/// The personal profile's own folder (the default ~/.claude, logged in) and a folder linked to it with its own login.
fn personal_max(e: &Env) -> (PathBuf, PathBuf) {
    let own = e.home.join(".claude");
    login(&own, "org-personal");
    // The default folder keeps its settings beside it, in ~/.claude.json.
    std::os::unix::fs::symlink(own.join(".claude.json"), e.home.join(".claude.json")).unwrap();
    fs::write(own.join("CLAUDE.md"), "mine").unwrap();
    fs::write(own.join("settings.json"), "{}").unwrap();
    let max = e.home.join(".claude-max");
    let out = e.run(
        &e.home,
        &["link", &own.to_string_lossy(), &max.to_string_lossy()],
    );
    assert!(out.status.success(), "link: {}", text(&out.stderr));
    login(&max, "org-max");
    (own, max)
}

const CORTEX: &str = "https://c.example/mcp";
/// Claude Code's key for CORTEX's sign-in: the name and the first 16 hex of sha256 of {"type","url","headers"}.
const CORTEX_KEY: &str = "cortex|32928f708bfba17d";

#[test]
fn link_gives_a_new_folder_the_servers_and_answers_of_its_source() {
    let e = Env::new(ROOM);
    let mut f = json_of(&e.work);
    f["mcpServers"]["cortex"] = serde_json::json!({"type": "http", "url": CORTEX});
    f["projects"] = serde_json::json!({ e.acme.to_string_lossy(): {
        "hasTrustDialogAccepted": true, "hasClaudeMdExternalIncludesApproved": true}});
    set_json(&e.work, f.clone());
    let fresh = e.home.join(".claude-new");
    let out = e.run(&e.home, &["link", &e.work_s(), &fresh.to_string_lossy()]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let t = json_of(&fresh);
    assert_eq!(t["mcpServers"], f["mcpServers"], "{t}");
    let p = &t["projects"][e.acme.to_string_lossy().as_ref()];
    assert_eq!(p["hasTrustDialogAccepted"], true, "{t}");
    assert_eq!(p["hasClaudeMdExternalIncludesApproved"], true, "{t}");
    assert!(t.get("oauthAccount").is_none(), "login copied: {t}");
    assert!(
        text(&out.stdout).contains("+cortex"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn every_launch_brings_its_linked_folder_in_line_with_the_source() {
    let e = Env::new(ROOM);
    let mut f = json_of(&e.work);
    f["mcpServers"]["cortex"] = serde_json::json!({"type": "http", "url": CORTEX});
    set_json(&e.work, f.clone());
    let out = e.run(&e.acme, &["launch", "claude"]);
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(so.contains(&format!("DIR={}\n", e.primary_s())), "{so}");
    assert_eq!(json_of(&e.primary)["mcpServers"], f["mcpServers"]);
    assert_eq!(
        json_of(&e.primary)["oauthAccount"]["organizationUuid"],
        "org-primary"
    );
    assert!(se.contains("synced from") && se.contains("+cortex"), "{se}");
    // A backup of the file as it was, under ~/.aitch/backups.
    assert_eq!(
        fs::read_dir(e.home.join(".aitch/backups")).unwrap().count(),
        1
    );
    // A folder named by CLAUDE_CONFIG_DIR with no route in play is brought in line too.
    f["mcpServers"]["more"] = serde_json::json!({"command": "m"});
    set_json(&e.work, f.clone());
    let out = e
        .cmd(&e.personal, &["launch", "claude"])
        .env("CLAUDE_CONFIG_DIR", &e.primary)
        .locked_output()
        .unwrap();
    assert!(text(&out.stdout).contains(&format!("DIR={}\n", e.primary_s())));
    assert_eq!(json_of(&e.primary)["mcpServers"], f["mcpServers"]);
}

#[test]
fn a_launch_says_loudly_when_it_cannot_sync() {
    let e = Env::new(ROOM);
    let mut f = json_of(&e.work);
    f["mcpServers"]["cortex"] = serde_json::json!({"type": "http", "url": CORTEX});
    set_json(&e.work, f);
    fs::create_dir(e.primary.join(".claude.json.lock")).unwrap();
    let out = e.run(&e.acme, &["launch", "claude"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("not synced"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn primary_option_without_a_route_starts_a_folder_linked_to_the_profiles_own() {
    let e = Env::new(ROOM);
    let (own, max) = personal_max(&e);
    let mut f = json_of(&own);
    f["mcpServers"]["cortex"] = serde_json::json!({"type": "http", "url": CORTEX});
    set_json(&own, f.clone());
    fs::write(e.bin.join("quota"), "#!/bin/sh\ntouch \"$HOME/asked\"\n").unwrap();
    let out = e.run(
        &e.personal,
        &[
            "launch",
            "claude",
            "--primary",
            &max.to_string_lossy(),
            "--",
            "x",
        ],
    );
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{se}");
    assert!(so.contains(&format!("DIR={}\n", max.display())), "{so}");
    assert!(
        so.contains("ARG=x\n") && !so.contains("--mcp-config"),
        "{so}"
    );
    assert!(so.contains("CONNECTORS=unset\n"), "{so}");
    assert_eq!(json_of(&max)["mcpServers"], f["mcpServers"]);
    assert!(!e.home.join("asked").exists(), "quota command ran");
    assert!(se.contains("aitch: personal →"), "{se}");
}

#[test]
fn primary_option_naming_the_profiles_own_default_folder_starts_it_as_the_hook_does() {
    let e = Env::new(ROOM);
    let (own, _) = personal_max(&e);
    // The default folder runs with CLAUDE_CONFIG_DIR unset: set, Claude Code reads another login item.
    let out = e.run(
        &e.personal,
        &["launch", "claude", "--primary", &own.to_string_lossy()],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("DIR=unset\n"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn mcp_subcommand_on_a_linked_folder_runs_on_its_source() {
    let e = Env::new(ROOM);
    let (_, max) = personal_max(&e);
    let out = e.run(
        &e.personal,
        &[
            "launch",
            "claude",
            "--primary",
            &max.to_string_lossy(),
            "--",
            "mcp",
            "list",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("DIR=unset\n"),
        "{}",
        text(&out.stdout)
    );
    let out = e
        .cmd(&e.personal, &["launch", "claude", "--", "mcp", "list"])
        .env("CLAUDE_CONFIG_DIR", &e.primary)
        .locked_output()
        .unwrap();
    assert!(
        text(&out.stdout).contains(&format!("DIR={}\n", e.work_s())),
        "{}",
        text(&out.stdout)
    );
}

/// A fake `security` (macOS) and `.credentials.json` files (elsewhere) with these blobs per folder.
fn fake_logins(e: &Env, blobs: &[(&Path, Option<&str>, &str)]) {
    use sha2::{Digest, Sha256};
    let mut cases = String::new();
    for (dir, env, blob) in blobs {
        let svc = match env {
            None => "Claude Code-credentials".to_string(),
            Some(v) => format!(
                "Claude Code-credentials-{}",
                &hex::encode(Sha256::digest(v.as_bytes()))[..8]
            ),
        };
        cases.push_str(&format!("  '{svc}') printf '%s\\n' '{blob}' ;;\n"));
        fs::write(dir.join(".credentials.json"), blob).unwrap();
    }
    script(
        &e.bin.join("security"),
        &format!("case \"$3\" in\n{cases}  *) echo 'The specified item could not be found in the keychain.' >&2; exit 44 ;;\nesac\n"),
    );
}

#[test]
fn doctor_lists_each_folder_with_its_login_servers_and_sign_ins_and_never_a_token() {
    let e = Env::new(ROOM);
    let (own, max) = personal_max(&e);
    let mut f = json_of(&own);
    f["mcpServers"]["cortex"] = serde_json::json!({"type": "http", "url": CORTEX});
    f["projects"] = serde_json::json!({"/r": {"hasTrustDialogAccepted": true}});
    set_json(&own, f);
    let signed = format!(
        r#"{{"claudeAiOauth":{{"accessToken":"SECRET-TOKEN-123"}},"mcpOAuth":{{"{CORTEX_KEY}":{{"accessToken":"SECRET-TOKEN-123","refreshToken":"SECRET-R"}}}}}}"#
    );
    let bare = r#"{"claudeAiOauth":{"accessToken":"SECRET-TOKEN-123"}}"#;
    fake_logins(
        &e,
        &[
            (&own, None, &signed),
            (&max, Some(&max.to_string_lossy()), bare),
            (&e.work, Some(&e.work_s()), bare),
            (&e.primary, Some(&e.primary_s()), bare),
        ],
    );
    let out = e.run(&e.home, &["doctor"]);
    let (so, se) = (text(&out.stdout), text(&out.stderr));
    assert!(!so.contains("SECRET") && !se.contains("SECRET"), "{so}{se}");
    assert_eq!(out.status.code(), Some(1), "{so}{se}");
    let o = section_of(&so, &own);
    assert!(
        o.contains("source") && o.contains("signed in") && o.contains("cortex: signed in"),
        "{o}"
    );
    let m = section_of(&so, &max);
    assert!(m.contains(&format!("linked to {}", own.display())), "{m}");
    assert!(m.contains("login: signed in"), "{m}");
    assert!(m.contains("NOT IN SYNC") && m.contains("+cortex"), "{m}");
    assert!(m.contains("1 answer"), "{m}");
    assert!(m.contains("cortex: NEEDS SIGN-IN"), "{m}");
    let p = section_of(&so, &e.primary);
    assert!(p.contains(&format!("linked to {}", e.work_s())), "{p}");
    // After a launch the folder is in sync; the sign-in is still missing, so still a problem.
    let out = e.run(
        &e.personal,
        &["launch", "claude", "--primary", &max.to_string_lossy()],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = e.run(&e.home, &["doctor"]);
    let so = text(&out.stdout);
    let m = section_of(&so, &max);
    assert!(
        m.contains("MCP servers: in sync") && m.contains("answers: in sync"),
        "{m}"
    );
    assert!(m.contains("cortex: NEEDS SIGN-IN"), "{m}");
    assert_eq!(out.status.code(), Some(1));
}

fn section_of(so: &str, dir: &Path) -> String {
    let head = format!("{} ", dir.display());
    let at = so.find(&head).unwrap_or_else(|| panic!("no {head}: {so}"));
    let rest = &so[at..];
    let end = rest[1..].find("\n/").map_or(rest.len(), |i| i + 1);
    rest[..end].to_string()
}

#[test]
fn doctor_says_when_a_folder_has_no_login() {
    let e = Env::new(ROOM);
    let (own, max) = personal_max(&e);
    let bare = r#"{"claudeAiOauth":{"accessToken":"SECRET-TOKEN-123"}}"#;
    fake_logins(&e, &[(&own, None, bare)]);
    let _ = (&max, &own);
    let out = e.run(&e.home, &["doctor"]);
    let so = text(&out.stdout);
    let m = section_of(&so, &max);
    assert!(m.contains("login: NOT SIGNED IN"), "{m}");
    assert_eq!(out.status.code(), Some(1));
}

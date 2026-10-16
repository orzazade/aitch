//! `aitch launch` end to end: a temp HOME with a config, a fake `claude` that prints what it got, and a fake quota
//! command that follows the quota command contract.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const AITCH: &str = env!("CARGO_BIN_EXE_aitch");

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
        self.cmd(cwd, args).output().unwrap()
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

#[test]
fn exit_code_of_claude_comes_back() {
    let e = Env::new(ROOM);
    let out = e
        .cmd(&e.acme, &["launch", "claude"])
        .env("FAKE_EXIT", "7")
        .output()
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
fn hook_defines_claude_only_with_a_route() {
    let e = Env::new(ROOM);
    for shell in ["zsh", "bash"] {
        let hook = text(&e.run(&e.home, &["hook", shell]).stdout);
        assert!(hook.contains("function claude {"), "{shell}: {hook}");
        let check = Command::new(shell)
            .arg("-n")
            .arg("-c")
            .arg(&hook)
            .output()
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
        assert!(!hook.contains("function claude"), "{shell}: {hook}");
    }
}

#[test]
fn shell_function_routes_through_launch() {
    let e = Env::new(ROOM);
    let aitch_dir = Path::new(AITCH).parent().unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg("eval \"$(aitch hook bash)\"; claude --resume 'a b'")
        .current_dir(&e.acme)
        .env("HOME", &e.home)
        .env(
            "PATH",
            format!("{}:{}:/usr/bin:/bin", aitch_dir.display(), e.bin.display()),
        )
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let so = text(&out.stdout);
    assert!(
        so.contains(&format!("DIR={}\n", e.primary_s())),
        "{so} {}",
        text(&out.stderr)
    );
    assert!(so.contains("ARG=--resume\nARG=a b\n"), "{so}");
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
        .output()
        .unwrap();
    assert!(
        text(&out.stdout).contains("DIR=/inherited\n"),
        "{}",
        text(&out.stdout)
    );
    assert!(!e.home.join("asked").exists());
}

#[test]
fn hook_function_is_safe_from_an_existing_alias() {
    let e = Env::new(ROOM);
    for shell in ["zsh", "bash"] {
        let hook = text(&e.run(&e.home, &["hook", shell]).stdout);
        assert!(hook.contains("unalias claude"), "{shell}: {hook}");
        assert!(hook.contains("function claude {"), "{shell}: {hook}");
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
    let out = e.cmd(&link, &["which"]).env("PWD", &link).output().unwrap();
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
            .output()
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
        .map(|_| e.cmd(&e.home, &["hook", "zsh"]).spawn().unwrap())
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
            .status()
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
        .output()
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

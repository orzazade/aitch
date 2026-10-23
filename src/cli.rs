use crate::cache;
use crate::config::{self, Config, Profile};
use crate::hooks;
use crate::providers;
use crate::resolve;
use crate::route;
use crate::shim;
use colored::Colorize;
use std::env;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Initialize aitch: create config directory and default config.
pub fn init() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = config::config_path();
    if config_path.exists() {
        println!(
            "{} Config already exists at {}",
            "warning:".yellow().bold(),
            config_path.display()
        );
        return Ok(());
    }

    let config = Config::default();
    config::save_config(&config)?;
    cache::write_cache(&config)?;

    println!(
        "{} Created {}",
        "done:".green().bold(),
        config_path.display()
    );
    println!();
    println!("Next steps:");
    println!("  aitch add work --dir ~/Projects/work --claude ~/.claude-work");
    println!("  aitch add personal --dir ~/Projects/personal --claude ~/.claude-personal");
    println!("  eval \"$(aitch hook zsh)\"  # add to ~/.zshrc");

    Ok(())
}

/// Add a directory mapping to a profile.
pub fn add(
    profile_name: &str,
    dir: Option<&str>,
    git: Option<&str>,
    claude: Option<&str>,
    codex: Option<&str>,
    gemini: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    config::validate_profile_name(profile_name)?;

    let mut cfg = config::load_config()?;
    let profile = cfg
        .profiles
        .entry(profile_name.to_string())
        .or_insert_with(Profile::default);

    if let Some(d) = dir {
        config::validate_path(d)?;
        if !profile.directories.contains(&d.to_string()) {
            profile.directories.push(d.to_string());
        }
    }

    if let Some(g) = git {
        if !profile.git_remotes.contains(&g.to_string()) {
            profile.git_remotes.push(g.to_string());
        }
    }

    if let Some(c) = claude {
        config::validate_path(c)?;
        profile
            .providers
            .insert("claude".to_string(), c.to_string());
    }

    if let Some(c) = codex {
        config::validate_path(c)?;
        profile.providers.insert("codex".to_string(), c.to_string());
    }

    if let Some(g) = gemini {
        config::validate_path(g)?;
        profile
            .providers
            .insert("gemini".to_string(), g.to_string());
    }

    config::save_config(&cfg)?;
    cache::write_cache(&cfg)?;

    println!(
        "{} Updated profile '{}'",
        "done:".green().bold(),
        profile_name.bold()
    );

    Ok(())
}

/// Set the default profile.
pub fn set_default(profile_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = config::load_config()?;
    if !cfg.profiles.contains_key(profile_name) {
        return Err(format!("profile '{}' does not exist", profile_name).into());
    }
    cfg.default = Some(profile_name.to_string());
    config::save_config(&cfg)?;
    cache::write_cache(&cfg)?;

    println!(
        "{} Default profile set to '{}'",
        "done:".green().bold(),
        profile_name.bold()
    );
    Ok(())
}

/// List all profiles and their mappings.
pub fn list() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::load_config()?;

    if cfg.profiles.is_empty() {
        println!("No profiles configured. Run 'aitch add <name>' to create one.");
        return Ok(());
    }

    for (name, profile) in &cfg.profiles {
        let is_default = cfg.default.as_deref() == Some(name.as_str());
        let marker = if is_default { " (default)" } else { "" };

        println!("{}{}", name.bold(), marker.dimmed());

        if !profile.providers.is_empty() {
            println!("  {}:", "providers".dimmed());
            for (key, path) in &profile.providers {
                let env_var = providers::get_env_var(key).unwrap_or("?");
                println!(
                    "    {} = {}",
                    env_var.cyan(),
                    providers::env_value(key, path)
                );
            }
        }

        if !profile.directories.is_empty() {
            println!("  {}:", "directories".dimmed());
            for d in &profile.directories {
                println!("    {}", d);
            }
        }

        if !profile.git_remotes.is_empty() {
            println!("  {}:", "git remotes".dimmed());
            for r in &profile.git_remotes {
                println!("    {}", r);
            }
        }

        println!();
    }

    Ok(())
}

/// Remove a profile.
pub fn remove(profile_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = config::load_config()?;

    if !config::remove_profile(&mut cfg, profile_name) {
        return Err(format!("profile '{}' does not exist", profile_name).into());
    }

    // Clear default if it was this profile
    if cfg.default.as_deref() == Some(profile_name) {
        cfg.default = None;
    }

    config::save_config(&cfg)?;
    cache::write_cache(&cfg)?;

    println!(
        "{} Removed profile '{}'",
        "done:".green().bold(),
        profile_name.bold()
    );
    Ok(())
}

/// Show the active profile for the current directory.
pub fn status() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::load_config()?;
    let pwd = logical_pwd()?;
    let result = resolve::resolve(&pwd, &cfg);

    match result.profile_name {
        Some(name) => {
            println!("{} {}", "Profile:".bold(), name.green().bold());
            if let Some(profile) = &result.profile {
                for (key, path) in &profile.providers {
                    let env_var = providers::get_env_var(key).unwrap_or("?");
                    println!("  {} = {}", env_var.cyan(), providers::env_value(key, path));
                }
            }
        }
        None => {
            println!("{}", "No active profile".yellow());
        }
    }

    Ok(())
}

/// Show which rule matched and the full resolution trace.
pub fn which() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::load_config()?;
    let pwd = logical_pwd()?;
    let result = resolve::resolve(&pwd, &cfg);

    println!("{} {}", "Directory:".dimmed(), pwd);

    match &result.rule {
        resolve::MatchRule::AitchFile { path, profile } => {
            println!(
                "{} {} (from .aitch file at {})",
                "Profile:".bold(),
                profile.green().bold(),
                path.dimmed()
            );
        }
        resolve::MatchRule::DirectoryMatch { pattern } => {
            println!(
                "{} {} (directory match: {})",
                "Profile:".bold(),
                result.profile_name.as_deref().unwrap_or("?").green().bold(),
                pattern.dimmed()
            );
        }
        resolve::MatchRule::GitRemote { remote, pattern } => {
            println!(
                "{} {} (git remote: {} matched {})",
                "Profile:".bold(),
                result.profile_name.as_deref().unwrap_or("?").green().bold(),
                remote.dimmed(),
                pattern.dimmed()
            );
        }
        resolve::MatchRule::Default => {
            println!(
                "{} {} (default)",
                "Profile:".bold(),
                result.profile_name.as_deref().unwrap_or("?").green().bold()
            );
        }
        resolve::MatchRule::None => {
            println!("{}", "No matching profile".yellow());
        }
    }

    if let Some(profile) = &result.profile {
        println!();
        println!("{}:", "Providers".bold());
        for (key, path) in &profile.providers {
            let env_var = providers::get_env_var(key).unwrap_or("?");
            println!("  {} = {}", env_var.cyan(), providers::env_value(key, path));
        }
    }

    Ok(())
}

/// Output the shell hook for the given shell.
pub fn hook(shell: &str) -> Result<(), Box<dyn std::error::Error>> {
    // A broken config must not cost the base hook: it sources the cache built from the last good config.
    let cfg = config::load_config();
    let routed = cfg.as_ref().is_ok_and(|c| {
        c.profiles
            .values()
            .any(|p| p.route.as_ref().is_some_and(|r| r.claude.is_some()))
    });
    let generate = match shell {
        "zsh" => hooks::zsh::generate,
        "bash" => hooks::bash::generate,
        _ => return Err(format!("unsupported shell: '{}' (use zsh or bash)", shell).into()),
    };
    let shim_dir = if routed {
        match shim::install() {
            Ok(dir) => Some(dir),
            Err(e) => {
                eprintln!(
                    "{}",
                    format!(
                        "aitch: claude shim not installed, claude starts without its route: {}",
                        e
                    )
                    .red()
                    .bold()
                );
                None
            }
        }
    } else {
        None
    };
    print!("{}", generate(shim_dir.as_deref()));
    // A hand-edited config reaches the hook in every new shell, so the hook and `aitch launch` read the same rules.
    cache::write_cache(&cfg?)?;
    Ok(())
}

/// Start `provider` for the current directory. A profile with a route picks its folder here, once, and says which
/// and why on stderr; any other profile runs the provider with the environment the shell hook already set.
pub fn launch(
    provider: &str,
    primary: Option<&str>,
    args: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if provider != "claude" {
        return Err(format!("launch supports claude only, not '{}'", provider).into());
    }
    start_claude(primary, args)
}

/// Run as `claude` through the hook's shim. A start aitch already routed comes back here when a wrapper it started
/// runs `claude` again; that goes on to the next claude as it is.
pub fn claude_shim(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match shim::tried()? {
        Some(tried) => {
            let mut cmd = shim::command(&tried)?;
            let err = cmd.args(args).exec();
            Err(cannot_run(&cmd, err))
        }
        None => start_claude(None, args),
    }
}

fn cannot_run(cmd: &Command, err: std::io::Error) -> Box<dyn std::error::Error> {
    format!(
        "cannot run {}: {}",
        cmd.get_program().to_string_lossy(),
        err
    )
    .into()
}

fn start_claude(primary: Option<&str>, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd = shim::command(&[])?;
    // A broken config must not stop claude: run it with the folder the hook's cache (last good config) set.
    let cfg = match config::load_config() {
        Ok(c) => c,
        Err(e) if primary.is_some() => {
            return Err(format!(
                "--primary needs the config, and the config does not load: {}",
                e
            )
            .into())
        }
        Err(e) => {
            eprintln!(
                "{}",
                format!("aitch: config error, no route used: {}", e)
                    .red()
                    .bold()
            );
            let err = cmd.args(args).exec();
            return Err(cannot_run(&cmd, err));
        }
    };
    let pwd = logical_pwd()?;
    let result = resolve::resolve(&pwd, &cfg);
    let routed = result.profile.as_ref().and_then(|p| {
        let r = p.route.as_ref()?.claude.as_ref()?;
        Some((r, p.providers.get("claude")?))
    });
    // The caller picked the account, so no org check; but the folder is its full path and has a login, or Claude
    // Code would read it from wherever it starts and ask to sign in there.
    // Claude Code names the login item after the exact string, so "~/.claude-max/" would look for another login.
    let primary = primary.map(|p| {
        let p = resolve::expand_tilde(p);
        match p.trim_end_matches('/') {
            "" => p,
            t => t.to_string(),
        }
    });
    if let Some(p) = &primary {
        if !Path::new(p).is_absolute() {
            return Err(format!("--primary needs an absolute folder, not {:?}", p).into());
        }
        route::org_of(Path::new(p)).map_err(|e| format!("not launching claude: {}", e))?;
    }
    if let Some((r, fallback)) = routed {
        // A named primary stands in for the route's for this launch.
        let named = primary.map(|p| config::Route {
            primary: p,
            primary_org: None,
            ..r.clone()
        });
        let r = named.as_ref().unwrap_or(r);
        let name = result.profile_name.as_deref().unwrap_or("?");
        let fallback_dir = resolve::expand_tilde(fallback);
        let primary_dir = resolve::expand_tilde(&r.primary);
        let mcp = config::config_dir()
            .join("run")
            .join(format!("{}-mcp.json", name));
        // The launch folder as the shell names it and as the disk does: Claude Code keys its per-folder state by one.
        let mut folders = vec![pwd.clone()];
        if let Ok(p) = env::current_dir() {
            let p = p.to_string_lossy().to_string();
            if p != pwd {
                folders.push(p);
            }
        }
        // Local-scope MCP servers are filed under the git root, as Claude Code does.
        let home = dirs::home_dir().unwrap_or_default();
        let project = route::project_key(&env::current_dir()?, &home);
        let choice = if args.first().is_some_and(|a| a == "mcp") {
            // `claude mcp` edits the servers; primary launches read them from the fallback dir.
            if let Some(want) = &r.fallback_org {
                route::login_is(Path::new(&fallback_dir), want)
                    .map_err(|e| format!("not launching claude: {}", e))?;
            }
            route::Choice {
                dir: fallback_dir.clone().into(),
                primary: false,
                reason:
                    "claude mcp edits the fallback dir's servers, which primary launches use too"
                        .into(),
                loud: false,
            }
        } else {
            match route::decide(
                r,
                Path::new(&fallback_dir),
                Path::new(&primary_dir),
                std::slice::from_ref(&project),
                &mcp,
                || route::read_verdict(&r.quota, Path::new(&primary_dir), route::QUOTA_TIMEOUT),
            ) {
                route::Decision::Go(c) => c,
                route::Decision::Refuse(why) => {
                    return Err(format!("not launching claude: {}", why).into())
                }
            }
        };
        let (which, shown) = if choice.primary {
            ("primary", r.primary.as_str())
        } else {
            ("fallback", fallback.as_str())
        };
        let reason = if choice.loud {
            choice.reason.red().bold().to_string()
        } else {
            choice.reason.clone()
        };
        eprintln!("aitch: {} → {} ({}): {}", name, which, shown, reason);
        let other = if choice.primary {
            &fallback_dir
        } else {
            &primary_dir
        };
        match route::share_trust(
            Path::new(other),
            &choice.dir,
            &folders,
            &config::config_dir().join("backups"),
        ) {
            Ok(marked) => {
                for f in marked {
                    eprintln!(
                        "aitch: {} is trusted in {} too, as in {}",
                        f,
                        choice.dir.display(),
                        other
                    );
                }
            }
            Err(e) => eprintln!(
                "{}",
                format!("aitch: folder trust not shared: {}", e).yellow()
            ),
        }
        sync_linked(&choice.dir);
        route::set_config_dir(&mut cmd, &choice.dir);
        if choice.primary {
            // One word: --mcp-config takes values up to the next flag and would eat the user's first arguments.
            cmd.arg(format!("--mcp-config={}", mcp.display()));
            // The primary account's own claude.ai connectors are not part of the fallback dir's setup.
            cmd.env("ENABLE_CLAUDEAI_MCP_SERVERS", "false");
        }
    } else if let Some(p) = &primary {
        // No route: the caller names a folder of this profile's account family, its own or one linked to it.
        let name = result.profile_name.as_deref().unwrap_or("?");
        let value = result
            .profile
            .as_ref()
            .and_then(|p| p.providers.get("claude"))
            .ok_or("--primary needs a profile with a claude folder for this directory")?;
        let own = if value.trim().is_empty() {
            route::default_dir()
        } else {
            PathBuf::from(resolve::expand_tilde(value))
        };
        let p = Path::new(p);
        if !route::same_dir(p, &own) {
            match route::source_of(p) {
                Ok(Some(s)) if route::same_dir(&s, &own) => {}
                Ok(_) => {
                    return Err(format!(
                        "--primary {} is not {} or a folder linked to it (aitch link {} {})",
                        p.display(),
                        own.display(),
                        own.display(),
                        p.display()
                    )
                    .into())
                }
                Err(e) => return Err(format!("not launching claude: {}", e).into()),
            }
        }
        let dir = if args.first().is_some_and(|a| a == "mcp") {
            // The servers are kept in the profile's folder, named as the profile names it (Claude Code names the login
            // item after that string); in its own source if the profile's folder is itself linked.
            let src = if route::same_dir(p, &own) {
                route::source_of(&own)
                    .map_err(|e| format!("not launching claude: {}", e))?
                    .unwrap_or_else(|| own.clone())
            } else {
                own.clone()
            };
            if !route::same_dir(&src, p) {
                eprintln!(
                    "aitch: claude mcp runs on {}, whose servers {} takes",
                    src.display(),
                    p.display()
                );
            }
            src
        } else {
            sync_linked(p);
            p.to_path_buf()
        };
        eprintln!("aitch: {} → {} (named by --primary)", name, dir.display());
        route::set_config_dir(&mut cmd, &dir);
    } else if let Some(d) = env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
    {
        // The folder the shell set: a linked one gets its source's servers and answers, and `claude mcp` edits them
        // in the source, where they are kept.
        match route::source_of(&d) {
            Ok(Some(src)) if args.first().is_some_and(|a| a == "mcp") => {
                eprintln!(
                    "aitch: claude mcp runs on {}, whose servers {} takes",
                    src.display(),
                    d.display()
                );
                route::set_config_dir(&mut cmd, &src);
            }
            _ => {
                sync_linked(&d);
                route::set_config_dir(&mut cmd, &d);
            }
        }
    }
    let err = cmd.args(args).exec();
    Err(cannot_run(&cmd, err))
}

/// Bring linked folder `dir` in line with its source (route::sync_config) and say what changed. A failure is loud
/// but does not stop the launch: Claude Code then asks its questions itself.
fn sync_linked(dir: &Path) {
    let fail = |e: String| {
        eprintln!(
            "{}",
            format!("aitch: {} not synced: {}", dir.display(), e)
                .red()
                .bold()
        )
    };
    let src = match route::source_of(dir) {
        Ok(Some(s)) => s,
        Ok(None) => return,
        Err(e) => return fail(e),
    };
    match route::sync_config(&src, dir, &config::config_dir().join("backups")) {
        Ok(s) if s == route::Synced::default() => {}
        Ok(s) => eprintln!(
            "aitch: {} synced from {}: {}",
            dir.display(),
            src.display(),
            s.summary()
        ),
        Err(e) => fail(e),
    }
}

/// The directory as the shell sees it ($PWD, symlinks kept), like the hook's cache; else the physical one.
fn logical_pwd() -> std::io::Result<String> {
    let physical = env::current_dir()?;
    if let Ok(pwd) = env::var("PWD") {
        if Path::new(&pwd).is_absolute()
            && std::fs::canonicalize(&pwd).ok() == physical.canonicalize().ok()
        {
            return Ok(pwd);
        }
    }
    Ok(physical.to_string_lossy().to_string())
}

/// Create a config folder that shares another folder's rules by symlink.
pub fn link(from: &str, to: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (from, to) = (resolve::expand_tilde(from), resolve::expand_tilde(to));
    let made = route::link(Path::new(&from), Path::new(&to))?;
    route::check_links(Path::new(&from), Path::new(&to))?;
    let synced = route::sync_config(
        Path::new(&from),
        Path::new(&to),
        &config::config_dir().join("backups"),
    )
    .map_err(|e| format!("linked, but not synced: {}", e))?;
    if synced != route::Synced::default() {
        println!(
            "{} {} synced: {}",
            "done:".green().bold(),
            to,
            synced.summary()
        );
    }
    if made.is_empty() {
        println!(
            "{} {} already links every rule from {}",
            "done:".green().bold(),
            to,
            from
        );
    } else {
        println!(
            "{} linked {} in {} to {}",
            "done:".green().bold(),
            made.join(", "),
            to,
            from
        );
    }
    Ok(())
}

/// Report every config folder (see doctor::run); an error when it found a problem.
pub fn doctor() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = config::load_config()?;
    match crate::doctor::run(&cfg) {
        0 => {
            println!("{} no problems", "aitch doctor:".green().bold());
            Ok(())
        }
        n => Err(format!(
            "aitch doctor found {} problem{}",
            n,
            if n == 1 { "" } else { "s" }
        )
        .into()),
    }
}

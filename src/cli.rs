use crate::cache;
use crate::config::{self, Config, Profile};
use crate::hooks;
use crate::providers;
use crate::resolve;
use colored::Colorize;
use std::env;

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
        profile.providers.insert("claude".to_string(), c.to_string());
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
                println!("    {} = {}", env_var.cyan(), path);
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
    let pwd = env::current_dir()?.to_string_lossy().to_string();
    let result = resolve::resolve(&pwd, &cfg);

    match result.profile_name {
        Some(name) => {
            println!("{} {}", "Profile:".bold(), name.green().bold());
            if let Some(profile) = &result.profile {
                for (key, path) in &profile.providers {
                    let env_var = providers::get_env_var(key).unwrap_or("?");
                    println!("  {} = {}", env_var.cyan(), path);
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
    let pwd = env::current_dir()?.to_string_lossy().to_string();
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
            println!("  {} = {}", env_var.cyan(), path);
        }
    }

    Ok(())
}

/// Output the shell hook for the given shell.
pub fn hook(shell: &str) -> Result<(), Box<dyn std::error::Error>> {
    match shell {
        "zsh" => print!("{}", hooks::zsh::generate()),
        "bash" => print!("{}", hooks::bash::generate()),
        _ => return Err(format!("unsupported shell: '{}' (use zsh or bash)", shell).into()),
    }
    Ok(())
}

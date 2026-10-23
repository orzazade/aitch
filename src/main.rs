mod cache;
mod cli;
mod config;
mod doctor;
mod hooks;
mod providers;
mod resolve;
mod route;
mod shim;

use clap::{Parser, Subcommand};
use colored::Colorize;
use std::ffi::{OsStr, OsString};
use std::path::Path;

#[derive(Parser)]
#[command(name = "aitch", version, about = "Universal AI account switcher")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize aitch: create ~/.aitch/ config directory
    Init,

    /// Add or update a profile
    Add {
        /// Profile name (e.g., "work", "personal")
        name: String,

        /// Map a directory to this profile
        #[arg(long)]
        dir: Option<String>,

        /// Map a git remote pattern to this profile (e.g., "github.com/org/*")
        #[arg(long)]
        git: Option<String>,

        /// Claude Code config directory for this profile
        #[arg(long)]
        claude: Option<String>,

        /// OpenAI Codex config directory for this profile
        #[arg(long)]
        codex: Option<String>,

        /// Gemini CLI home for this profile: the folder that holds .gemini
        #[arg(long)]
        gemini: Option<String>,
    },

    /// Set the default profile
    Default {
        /// Profile name to set as default
        name: String,
    },

    /// List all profiles and their mappings
    List,

    /// Remove a profile
    Remove {
        /// Profile name to remove
        name: String,
    },

    /// Show the active profile for the current directory
    Status,

    /// Show which rule matched the current directory and why
    Which,

    /// Output the shell hook (add to .zshrc or .bashrc)
    Hook {
        /// Shell type: zsh or bash
        shell: String,
    },

    /// Start a provider with the folder its route picks for this directory (as `claude` through the hook's shim does)
    Launch {
        /// Provider to start: claude
        provider: String,

        /// Use this config folder as the route's primary for this launch (its links and quota checks still apply)
        #[arg(long)]
        primary: Option<String>,

        /// Arguments passed to the provider unchanged (after --)
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Check every Claude Code config folder: login, MCP servers and answers shared from its source, MCP sign-ins
    Doctor,

    /// Make <to> a config folder with its own login and the rules of <from> by symlink
    Link {
        /// The folder whose rules are shared (e.g. ~/.claude-work)
        from: String,

        /// The folder to create (e.g. ~/.claude-alt)
        to: String,
    },
}

fn main() {
    // The hook's shim links `claude` to aitch: run that way, it is claude with the route applied.
    let mut argv = std::env::args_os();
    if argv
        .next()
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_name)
        == Some(OsStr::new("claude"))
    {
        let args: Result<Vec<String>, _> = argv.map(OsString::into_string).collect();
        let result = match args {
            Ok(args) => cli::claude_shim(&args),
            Err(a) => Err(format!("argument is not UTF-8: {:?}", a).into()),
        };
        exit_on(result);
    }

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init => cli::init(),
        Commands::Add {
            name,
            dir,
            git,
            claude,
            codex,
            gemini,
        } => cli::add(
            &name,
            dir.as_deref(),
            git.as_deref(),
            claude.as_deref(),
            codex.as_deref(),
            gemini.as_deref(),
        ),
        Commands::Default { name } => cli::set_default(&name),
        Commands::List => cli::list(),
        Commands::Remove { name } => cli::remove(&name),
        Commands::Status => cli::status(),
        Commands::Which => cli::which(),
        Commands::Hook { shell } => cli::hook(&shell),
        Commands::Launch {
            provider,
            primary,
            args,
        } => cli::launch(&provider, primary.as_deref(), &args),
        Commands::Link { from, to } => cli::link(&from, &to),
        Commands::Doctor => cli::doctor(),
    };

    exit_on(result);
}

fn exit_on(result: Result<(), Box<dyn std::error::Error>>) {
    if let Err(e) = result {
        eprintln!("{}: {}", "error".red().bold(), e);
        std::process::exit(if e.is::<shim::NoClaude>() { 127 } else { 1 });
    }
}

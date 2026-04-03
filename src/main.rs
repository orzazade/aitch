mod cache;
mod cli;
mod config;
mod hooks;
mod providers;
mod resolve;

use clap::{Parser, Subcommand};
use colored::Colorize;

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

        /// Google Gemini config directory for this profile
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
}

fn main() {
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
    };

    if let Err(e) = result {
        eprintln!("{}: {}", "error".red().bold(), e);
        std::process::exit(1);
    }
}

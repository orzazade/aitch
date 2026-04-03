# aitch

Universal AI account switcher. Auto-activate the right AI accounts per directory.

Walk into any project directory. The right Claude, Codex, and Gemini accounts activate. No commands, no thinking.

## Install

```bash
cargo install aitch
```

Or with Homebrew:

```bash
brew install orzazade/tap/aitch
```

## Quick Start

```bash
# Initialize
aitch init

# Add profiles
aitch add work --dir ~/Projects/work --claude ~/.claude-work
aitch add personal --dir ~/Projects/personal --claude ~/.claude-personal

# Set default fallback
aitch default personal

# Map git orgs (auto-detect for new repos)
aitch add work --git "github.com/mycompany/*"

# Add shell hook to ~/.zshrc
echo 'eval "$(aitch hook zsh)"' >> ~/.zshrc

# Verify
cd ~/Projects/work/some-repo
aitch which
# Profile: work (directory match: ~/Projects/work)
#   CLAUDE_CONFIG_DIR = ~/.claude-work
```

## How It Works

aitch uses a resolution chain to determine which profile to activate:

1. `.aitch` file — drop a file containing a profile name in any repo
2. **Directory match** — prefix match against configured directories
3. **Git remote** — match against git remote URL patterns
4. **Default profile** — fallback when nothing else matches

The shell hook runs on every `cd` and sources a pre-compiled cache file. Static matches resolve in <1ms. Git remote detection (~3-5ms) only runs for unmatched directories.

## Commands

| Command | Description |
|---------|-------------|
| `aitch init` | Create ~/.aitch/ config directory |
| `aitch add <profile>` | Add or update a profile |
| `aitch default <profile>` | Set the default fallback profile |
| `aitch list` | Show all profiles and mappings |
| `aitch remove <profile>` | Remove a profile |
| `aitch status` | Show active profile for current directory |
| `aitch which` | Show which rule matched and why |
| `aitch hook zsh` | Output zsh hook (for .zshrc) |
| `aitch hook bash` | Output bash hook (for .bashrc) |

## Config

Config lives at `~/.aitch/config.yaml`:

```yaml
default: personal

profiles:
  work:
    providers:
      claude: ~/.claude-work
      codex: ~/.codex-work
    directories:
      - ~/Projects/work
    git_remotes:
      - "github.com/mycompany/*"

  personal:
    providers:
      claude: ~/.claude-personal
    directories:
      - ~/Projects/personal
    git_remotes:
      - "github.com/myusername/*"
```

## Supported Providers

| Provider | Environment Variable |
|----------|---------------------|
| Claude Code | `CLAUDE_CONFIG_DIR` |
| OpenAI Codex | `CODEX_CONFIG_DIR` |
| Google Gemini | `GEMINI_CONFIG_DIR` |

## Team Usage

Drop a `.aitch` file in your repo root:

```bash
echo "work" > .aitch
git add .aitch
git commit -m "chore: add aitch profile"
```

Anyone with aitch installed and a "work" profile gets the right accounts automatically.

## License

MIT

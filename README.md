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
3. **Git remote** — match against git remote URL patterns. A pattern such as `github.com/org/*` (or
   `github.com:org/*`) matches HTTPS and SSH (`git@github.com:org/repo`) clones alike
4. **Default profile** — fallback when nothing else matches

A `.aitch` file that names a profile you don't have is skipped, and the walk goes on to the folders above it.

The shell hook runs on every `cd` and sources a pre-compiled cache file. `aitch` commands rebuild it, and so does
every new shell, so after editing `config.yaml` by hand, open a new shell. Static matches resolve in <1ms. Git remote detection (~3-5ms) only runs for unmatched directories.

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
| `aitch link <from> <to>` | Make `<to>` a Claude Code config dir that shares `<from>`'s rules by symlink |
| `aitch launch claude -- <args>` | Start Claude Code with the dir a route picks (the hook's `claude` calls this) |

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

## Routing by quota (Claude Code)

A profile can start Claude Code on a second account while that account has quota left, and on the profile's own
account otherwise. The choice is made once, when `claude` starts; a running session is never switched.

```yaml
profiles:
  work:
    providers:
      claude: ~/.claude-work         # the fallback dir
    directories:
      - ~/Projects/work
    route:
      claude:
        primary: ~/.claude-alt       # used while the quota command says "room"
        quota: ~/bin/my-quota --room # see the contract below
        primary_org: <org id>        # optional: the login the primary dir must hold
        fallback_org: <org id>       # optional: the login the fallback dir must hold
```

Set it up once:

```bash
aitch link ~/.claude-work ~/.claude-alt         # share the fallback dir's rules
CLAUDE_CONFIG_DIR=~/.claude-alt command claude  # then /login with the second account
```

`aitch link` creates the primary dir with symlinks to the fallback dir's `CLAUDE.md`, `settings.json`, `agents`,
`skills`, `commands`, `hooks`, `output-styles`, `plugins` and `projects` (each one that exists). The login
(`.claude.json` and the keychain entry) stays separate. It never replaces a file that is not the right link. MCP
servers live next to the login, so on each launch in the primary dir aitch passes the fallback dir's `mcpServers`
with `--mcp-config` (a file under `~/.aitch/run/`, mode 0600, rewritten on each launch; it stays after the launch
because aitch replaces itself with Claude Code, and it holds the same servers as the fallback dir's `.claude.json`), and sets
`ENABLE_CLAUDEAI_MCP_SERVERS=false` so the second account's own claude.ai connectors stay out.

Claude Code keeps per-folder trust in each dir's `.claude.json`. If the other dir of the route already trusts the launch
folder or a folder above it, aitch marks the same folders as trusted in the dir it picked, so you are not asked again. It writes
`.claude.json` only when that changes something, under the lock Claude Code uses for it. `claude mcp …` runs on the
fallback dir (put `mcp` first, before any flag), since primary launches read their servers from there: user scope,
and local scope for the launch folder's project (its git repo root, as Claude Code files it).

With a route, the shell hook defines a `claude` function that calls `aitch launch claude`. It prints one line to
stderr on every launch, saying which dir it picked and why:

```
aitch: work → primary (~/.claude-alt): 7d 43%, 5h 9%, sample 1 min old
aitch: work → fallback (~/.claude-work): quota full: 7d 100%
aitch: work → fallback (~/.claude-work): QUOTA UNKNOWN: exit 4: no sample. Not using the primary dir.
```

The fallback dir is used when the quota answer is anything but a clear "room": the command fails or is missing,
takes longer than 3 seconds, or prints something unexpected. It is also used when a link in the primary dir was
replaced or points elsewhere, or when the primary dir's login is not `primary_org`. If the fallback dir's login is
not `fallback_org`, aitch does not start Claude Code at all. Anything that runs the `claude` binary directly gets
the hook's plain `CLAUDE_CONFIG_DIR`, which is the fallback dir. If the config does not load, `claude` still starts with that
plain value, after a red warning.

### Quota command contract

Any tool can be the quota command. aitch runs it without a shell (the string is split on spaces, `~` is expanded)
and reads one JSON line from stdout:

```json
{"verdict": "room", "reason": "7d 43%, 5h 9%, sample 1 min old"}
```

| Exit | `verdict` | aitch uses |
|------|-----------|------------|
| 0 | `room` | the primary dir |
| 3 | `full` | the fallback dir |
| 4 | `unknown` | the fallback dir, and says so loudly |
| any other, or no valid JSON | — | the fallback dir, loudly (it shows the first stderr line) |

`reason` is printed as is, so keep it to one short line. Other fields are ignored.

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

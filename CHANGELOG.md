# Changelog

## 0.2.1 — 2026-10-20

### Fixed

- The `codex` and `gemini` providers now switch those tools. aitch exported `CODEX_CONFIG_DIR` and
  `GEMINI_CONFIG_DIR`, which neither tool reads; it now exports `CODEX_HOME` and `GEMINI_CLI_HOME`.
- `GEMINI_CLI_HOME` names the folder that holds `.gemini`, so a `gemini:` value that ends in `.gemini` is exported as
  the folder above it. `aitch list`, `status` and `which` show the value that is exported.

## 0.2.0 — 2026-10-17

### Added

- **Routing by quota (Claude Code).** A profile can set `route.claude` with a `primary` config dir and a `quota`
  command. With a route, the shell hook defines a `claude` function that calls `aitch launch claude`, which starts
  Claude Code on the primary dir while the quota command says "room" and on the profile's own dir otherwise. One
  line on stderr says which dir and why. Optional `primary_org` and `fallback_org` check each dir's login. See
  "Routing by quota" in the README.
- `aitch link <from> <to>`: make a Claude Code config dir that shares another one's rules by symlink.
- `aitch launch claude -- <args>`: what the hook's `claude` function calls.
- On a primary launch, the fallback dir's MCP servers (user scope, and local scope for the launch folder's git
  repo, or a worktree's main repo) are passed with `--mcp-config`, and the primary account's claude.ai connectors stay out.
- At launch, a folder (or a folder above it) that the other dir of the route trusts is marked trusted in the dir
  the launch picked, so Claude Code does not ask again.
- `claude mcp …` (with `mcp` first) in a routed folder runs on the fallback dir, where primary launches read the
  servers from.

### Changed

- The hook and `aitch which`/`status`/`launch` follow the same rules (a test runs the hook's shell code in bash
  and zsh against them):
  - a `.aitch` file naming a profile you don't have is skipped, and the walk goes on upward;
  - SSH clones (`git@github.com:org/repo`) match `github.com/org/*` patterns in the hook too, and
    `github.com:org/*` patterns match HTTPS and SSH clones.
- Every new shell rebuilds the hook's cache from `config.yaml`, so hand edits take effect in new shells.
- `aitch which` and `status` use the folder as the shell names it (symlinks kept), like the hook.

### Fixed

- Directory matching is case-insensitive, stops at a folder boundary (`acme` does not match
  `acme-ideas`), and an empty provider value unsets the variable.
- The hook no longer ends shell startup under `set -e`.

## 0.1.0

First release.

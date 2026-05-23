# dotr specification

## Goal

`dotr` is a Rust-native filesystem-to-Git backup tool for personal configuration files.

It backs up selected files and directories from the local machine into a Git repository,
can restore them later, and can run as a daemon that reacts to filesystem changes.

The core design is intentionally simpler than a full dotfiles manager:

- No dependency on chezmoi, yadm, or a shell backup script.
- No `dot_` or `private_` source filename encoding.
- No templating in v0.
- Package-manager and editor-extension inventories are supported only through
  explicit `custom_backup` commands and generated files.
- Copy-based backup by default, not symlink-based management.

`dotr` should feel like: configure paths, run `dotr backup`, optionally run
`dotr watch`, and trust that the repository contains a restorable copy.

## Assumptions

- The primary use case is backing up user-owned configuration under `$HOME`,
  plus occasional absolute paths outside `$HOME`.
- The backup repository is private, but private does not mean safe for raw secrets.
- The backup and restore logic is implemented in Rust. Git transport may initially
  call the `git` executable so existing SSH credentials keep working; this is not
  a shell backup script and should be isolated behind a Git backend interface.
- Files are small enough for direct copy and hashing. Very large files should be
  excluded by policy rather than optimized around in v0.
- Restore is explicit. `dotr watch` should never restore.

## Repository layout

The repository stores `dotr` data directly at the repository root:

```text
dotr.toml
files/
  home/
    .zshrc
    .config/
      nvim/
        init.lua
  absolute/
    Library/
      example/
        hello/
          world
metadata/
  index.json
```

Path mapping is direct and reversible:

```text
~/.zshrc
=> files/home/.zshrc

~/.config/nvim/init.lua
=> files/home/.config/nvim/init.lua

/Library/example/hello/world
=> files/absolute/Library/example/hello/world
```

`files/home` maps to `$HOME`.

`files/absolute` maps to `/`.

`metadata/index.json` stores metadata that cannot be represented reliably
by copied file contents alone.

## Initialization

`dotr init` creates a new backup repository or prepares an existing Git
repository for `dotr`.

Examples:

```text
dotr init ~/code/dotbackup
dotr init . --with-defaults
dotr init . --no-git
dotr init ~/dotbackup --with-defaults --set-default
```

Behavior:

1. If the target directory does not exist, create it.
2. If the target directory is not a Git repository and `--no-git` is not set,
   run `git init`.
3. Create `dotr.toml` unless it already exists.
4. Create `files/home`, `files/absolute`, and `metadata`.
5. Create an empty `metadata/index.json`.
6. Create `recipients.txt` when encryption is enabled.
7. Create or update a conservative `.gitignore` for dotr lock files, temp files,
   and logs, without ignoring `files`.

`dotr init` must not migrate the current dotfiles repository layout in v0. It
creates a fresh `dotr.toml`, `files/`, and `metadata/` layout and leaves any existing chezmoi, yadm, or custom
dotfiles structure untouched.

`--set-default` writes a machine-local user config at
`~/.config/dotr/config.toml`:

```toml
default_repo = "/Users/alice/dotbackup"
```

The user config is not committed to the backup repository.

## Repository discovery

Commands that operate on an existing repository resolve the repository in this
order:

1. `--repo` or `-C`.
2. `DOTR_REPO`.
3. Walk upward from the current directory until `dotr.toml` is found.
4. `default_repo` in `~/.config/dotr/config.toml`.

If none of these resolves a repository, `dotr` fails with a message explaining
how to pass `--repo` or set a default.

`--with-defaults` may create a generic starter config with common shell, Git,
SSH, GPG, editor, prompt, terminal, Homebrew, and VS Code config paths. It must
not include machine-specific personal paths.

```toml
[[path]]
src = "~/.zshrc"

[[path]]
src = "~/.gitconfig"

[[path]]
src = "~/.ssh/config"

[[path]]
src = "~/.config/nvim"
```

The full starter path set is:

```text
~/.bash_profile
~/.bashrc
~/.profile
~/.zprofile
~/.zshenv
~/.zshrc
~/.zpreztorc
~/.inputrc
~/.editorconfig
~/.gitconfig
~/.gitignore_global
~/.ssh/config
~/.gnupg/gpg.conf
~/.gnupg/gpg-agent.conf
~/.tmux.conf
~/.vimrc
~/.ideavimrc
~/.config/git
~/.config/fish
~/.config/nvim
~/.config/helix
~/.config/starship.toml
~/.config/alacritty
~/.config/ghostty
~/.config/kitty
~/.config/wezterm
~/.config/bat
~/.config/direnv
~/.cargo/config.toml
~/.config/atuin
~/.config/fastfetch
~/.config/fresh
~/.config/gh
~/.config/gh-dash
~/.config/jj
~/.jjconfig.toml
~/.config/karabiner
~/.config/lvim
~/.config/mise
~/.config/openspeak
~/.config/ripasso
~/.config/yazi
~/.config/zed
~/.config/zellij
~/.warp
~/.hammerspoon
~/.config/homebrew/Brewfile
~/Library/Application Support/Code/User/settings.json
~/Library/Application Support/Code/User/keybindings.json
~/Library/Application Support/Code/User/tasks.json
~/Library/Application Support/Code/User/snippets
~/.config/Code/User/settings.json
~/.config/Code/User/keybindings.json
~/.config/Code/User/tasks.json
~/.config/Code/User/snippets
~/.config/vscode/extensions.txt
```

Some starter paths are include-limited. For example, `~/.config/gh` includes
`config.yml` but not `hosts.yml`. VS Code uses explicit user settings,
keybindings, tasks, snippets, and generated extension-list paths instead of the
whole `User` directory.

## Configuration

`dotr.toml` is committed to the repository.

Example:

```toml
[repository]
root = "/Users/yukang/code/dotfiles"
store = "."

[[path]]
src = "~/.zshrc"

[[path]]
src = "~/.gitconfig"

[[path]]
src = "~/.ssh/config"

[[path]]
src = "~/.config/nvim"
include = [
  "init.lua",
  "lua/**",
]

[[path]]
src = "~/.config/some-app"
include = [
  "config.toml",
  "assets/**",
]
include_binary_file = true

[[path]]
src = "/Library/example/hello/world"

[[custom_backup]]
name = "homebrew"
backup_command = "if command -v brew >/dev/null 2>&1; then mkdir -p ~/.config/homebrew && brew bundle dump --file ~/.config/homebrew/Brewfile --force; else echo 'dotr: skipping homebrew backup; brew not found' >&2; fi"
restore_command = "if command -v brew >/dev/null 2>&1 && [ -f ~/.config/homebrew/Brewfile ]; then brew bundle --file ~/.config/homebrew/Brewfile; else echo 'dotr: skipping homebrew restore; brew or Brewfile not found' >&2; fi"

[[custom_backup.path]]
src = "~/.config/homebrew/Brewfile"

[watch]
enabled = true
debounce_secs = 30
min_backup_interval_secs = 900

[daemon]
log_path = "~/.local/state/dotr/dotr-watch.log"
log_level = "info"

[git]
auto_commit = true
auto_push = true
commit_message = "chore(dotr): automated backup"

[encryption]
backend = "age"
recipients_file = "recipients.txt"
identity = "~/.config/dotr/age.key"

[policy]
max_file_size = "20MiB"
```

Paths support `~` expansion. Environment-variable expansion is not in v0 unless
there is a concrete need.

## Include and exclude policy

Each configured `[[path]]` may add local include/exclude rules:

```toml
[[path]]
src = "~/.codex"
include = [
  "AGENTS.md",
  "RTK.md",
  "config.toml",
  "rules/**",
  "skills/**",
]
exclude = [
  "skills/.system/**",
]
```

`include` patterns are relative to `src`. When `include` is present, only
matching files are stored; directories are traversed but not stored as metadata
entries. If `follow_symlink = false`, matching symlinks are stored as symlink
metadata.

Binary files are skipped by default. A path can opt in with
`include_binary_file = true` when binary assets are intentional. Include and
exclude rules still apply before the binary-file policy, so the recommended
shape for mixed application directories is a narrow `include` list plus this
binary opt-in.

Global default excludes are always applied:

```text
**/.git/**
**/.DS_Store
**/__pycache__/**
**/.cache/**
**/cache
**/cache/**
**/caches
**/caches/**
**/.tmp/**
**/tmp
**/tmp/**
**/temp
**/temp/**
**/log
**/log/**
**/logs
**/logs/**
**/sessions
**/sessions/**
**/archived_sessions
**/archived_sessions/**
**/browser/sessions
**/browser/sessions/**
**/shell_snapshots
**/shell_snapshots/**
**/worktrees
**/worktrees/**
**/targets
**/targets/**
**/target
**/target/**
**/generated_images
**/generated_images/**
**/ambient-suggestions
**/ambient-suggestions/**
**/node_repl
**/node_repl/**
**/vendor_imports
**/vendor_imports/**
**/plugins/cache
**/plugins/cache/**
**/node_modules/**
**/.venv/**
**/venv/**
**/.env
**/.env.*
**/auth.json
**/credentials.json
**/*.pem
**/*.key
**/*.db
**/*.sqlite
**/*.sqlite-*
**/*.sqlite3
**/*.sqlite3-*
**/*.log
**/*.pyc
**/*.tmp
**/*.tmp-*
**/.*.tmp-*
**/*.bak
**/*.bak.*
**/.*.bak
**/.*.bak*
**/references/llms*.md
```

These defaults are conservative and can be loosened only with an explicit
per-path allow rule in a later version. v0 does not need allow-rule overrides.

## Backup behavior

`dotr backup` performs one complete backup pass.

Algorithm:

1. Load `dotr.toml`.
2. Run each configured `custom_backup.backup_command`, unless this is a dry run.
3. Resolve configured source paths from both `[[path]]` and
   `[[custom_backup.path]]`.
4. Walk files under each source path.
5. Apply default and per-path excludes.
6. Apply per-path includes.
7. Skip binary files unless the path sets `include_binary_file = true`.
8. Reject paths that cannot be mapped safely into `files`.
9. Compare source files with current backup files.
10. Copy changed or new files into `files`.
11. Remove backed-up files whose source files disappeared, unless
   `--no-delete` is set.
12. Remove orphan files under `files/` that are not present in the current
   metadata result, unless `--no-delete` is set.
13. Write `metadata/index.json`.
14. If files changed and Git auto-commit is enabled, commit and optionally push.

Comparison uses content hashing, not only mtime. For unencrypted files,
mtime-only rewrites with identical content preserve the previous `index.json`
entry and should not count as changes. Durable metadata changes such as mode or
executable-bit changes are still recorded. Encrypted files use size and mtime as
the conservative change signal because plaintext hashes are not stored.

`dotr backup` prints progress updates to stderr while it scans configured
sources, checks deletions, writes metadata, and runs optional Git steps.

`dotr backup --dry-run` prints planned additions, updates, deletions, encrypted
updates, custom backup commands, and Git actions without writing.

`[[custom_backup]]` is for generated inventories such as Homebrew `Brewfile`
and VS Code extension lists. It may define `backup_command`, `restore_command`,
and nested `[[custom_backup.path]]` entries. The nested paths use the same
schema as top-level `[[path]]`.

## Add and remove behavior

`dotr add PATH` resolves `PATH` relative to the current directory, writes a new
`[[path]]` entry to `dotr.toml` if it is not already configured, and then runs
one backup pass. Paths under `$HOME` are stored in config with `~`.

`dotr remove PATH` removes the matching configured `[[path]]` entry from
`dotr.toml` and then runs one backup pass with deletion enabled, so entries that
are no longer covered by config are removed from `files/` and
`metadata/index.json`.

## Metadata

`index.json` stores one entry per backed-up path:

```json
{
  "version": 1,
  "entries": [
    {
      "source": "~/.zshrc",
      "stored": "files/home/.zshrc",
      "kind": "file",
      "sha256": "...",
      "mode": 420,
      "executable": false,
      "encrypted": false
    }
  ]
}
```

v0 metadata requirements:

- File vs directory vs symlink.
- Unix mode bits, at least executable bit.
- SHA-256 of plaintext for unencrypted files.
- Encryption flag and encrypted blob hash for encrypted files.

Ownership, ACLs, extended attributes, and quarantine attributes are out of scope
for v0.

## Symlinks

`dotr` follows symlinks by default because its backup model is copy-based.

Default behavior:

- Follow symlinks and store the target contents at the symlink path.
- Follow symlinked directories during traversal.
- Apply the same include, exclude, size, binary, and encryption policies to the
  followed target contents.

Per-path opt-out:

```toml
[[path]]
src = "~/.config/some-linked-app"
follow_symlink = false
```

When `follow_symlink = false`:

- Back up symlinks as symlink metadata, not as target file contents.
- Store the symlink target in `index.json`.
- Restore symlinks as symlinks.

Safety rules:

- Restore must not follow an existing destination symlink and overwrite its target.
  It should replace the symlink itself only after showing the action in dry-run.

## Encryption

Secret files are encrypted per file, not bundled into one archive.

Example v0 config:

```toml
[[path]]
src = "~/.ssh/config"
encrypt = true

[[path]]
src = "~/.config/some-app/token.json"
encrypt = true
```

Encrypted files are stored with an `.age` suffix:

```text
~/.config/some-app/token.json
=> files/home/.config/some-app/token.json.age
```

The plaintext path remains in `index.json`, but plaintext content does not.

Age concepts:

- `recipient`: public key used to encrypt.
- `identity`: private key used to decrypt.

The identity file must never be committed. `dotr doctor` must fail if any file
under the repository contains `AGE-SECRET-KEY-1`.

Age encryption is part of v0. Use a Rust age crate or a narrow encryption
backend boundary. Do not invent a custom crypto format.

For encrypted files, v0 stores only the encrypted blob hash in `index.json`.
It must not store plaintext hashes for encrypted files.

## Restore behavior

`dotr restore` restores from `files` to the original paths.

Default restore is conservative:

- Dry-run by default unless `--apply` is passed.
- Refuse to overwrite a destination file that differs from the backup unless
  `--force` is passed.
- Refuse absolute-path restores unless `--allow-absolute` is passed.
- Create parent directories as needed.
- Restore executable bits.
- Restore symlinks as symlinks.

Restore examples:

```text
dotr restore --dry-run
dotr restore --apply ~/.config/nvim
dotr restore --apply --allow-absolute /Library/example/hello/world
```

After matching files are restored, `dotr restore --apply` runs matching
`custom_backup.restore_command` entries. A custom restore command matches when
no target filter is provided or when a target filter overlaps one of the
custom backup paths. Dry-run restore prints the command without executing it.

Absolute path restore follows the stricter path:

- Backing up absolute paths is allowed.
- Restoring absolute paths is always dry-run unless `--apply` is present.
- Restoring absolute paths is refused unless `--allow-absolute` is also present.

Path traversal protection is mandatory:

- Stored paths must normalize under `files/home` or
  `files/absolute`.
- `..` components in stored paths are invalid.
- Restore must not write outside `$HOME` or `/` mapping roots because of symlink
  traversal.

## Watch mode

`dotr watch` runs as a long-lived process.

Behavior:

1. Watch configured source paths.
2. Ignore events matching exclude rules.
3. Debounce bursts of changes.
4. Acquire a process lock.
5. Run the same Rust backup pipeline as `dotr backup`.
6. Enforce `min_backup_interval_secs` for Git commits.
7. Log actions and failures.

The watcher must ignore changes inside the backup repository itself unless the
repository is explicitly listed as a source path.

`dotr watch` should be boring. It should never restore, never prompt, and never
make policy decisions not already present in config.

## Daemon mode

`dotr daemon` is the portable wrapper around `dotr watch`.

Commands:

```text
dotr daemon start
dotr daemon stop
dotr daemon restart
dotr daemon status
```

Behavior:

- `start` resolves the repository, writes dotr's own user-level daemon config,
  records the current executable path, and spawns `dotr --repo <repo> watch` in
  the background.
- `start` reads `[daemon].log_path` and `[daemon].log_level` from `dotr.toml`;
  omitted values default to `~/.local/state/dotr/dotr-watch.log` and `info`.
- Watch-triggered backups are scoped to changed paths after debounce. Scope
  outside `metadata/index.json` entries are preserved, scoped deletions only
  remove entries under the changed path, and only matching custom backup
  commands run.
- `stop` reads the pid file and sends `SIGTERM`.
- `restart` runs `stop`, then uses the same repository resolution and config
  refresh behavior as `start`.
- `status` reports whether the daemon config exists and whether the recorded
  pid is running.
- v0 does not write systemd units, launchd plists, or other OS-specific service
  files.

Daemon logs are timestamp-prefixed structured lines. Each structured dotr log
entry starts with an ISO 8601 timestamp in the system time zone, followed by a
tab and a JSON payload. `~` in daemon log paths expands to the user's home
directory. Relative daemon log paths resolve from the dotr repository root.
`log_level` supports `error`, `warn`, `info`, `debug`, and `trace`. stdout and
stderr both append to the same `log_path`.

## Git behavior

Git integration is optional but enabled for the main use case.

Commands:

```text
dotr backup --no-git
dotr backup --commit
dotr backup --commit --push
```

Commit rules:

- Commit only when backup files or metadata changed.
- Do not commit unrelated repository changes by default.
- If unrelated changes exist, fail with a clear message unless
  `git.include_unrelated = true`.
- Commit message should include timestamp and a short change summary.
- If push fails, keep the local commit and report the failure.

Implementation note:

- v0 may shell out to `git` for commit and push.
- Backup, restore, scanning, diffing, encryption decisions, and watch scheduling
  must stay in Rust.
- Keep Git behind a trait so a future `gix` backend is possible.

## CLI

Initial commands:

```text
dotr init
dotr init [TARGET] [--set-default]
dotr --repo ~/dotbackup backup
dotr -C ~/dotbackup status
dotr add ~/.config/yazi
dotr remove ~/.config/yazi
dotr backup [--dry-run] [--no-delete] [--no-git] [--commit] [--push]
dotr status
dotr restore [--dry-run] [--apply] [--force] [--allow-absolute] [PATH...]
dotr watch
dotr daemon start
dotr daemon stop
dotr daemon restart
dotr daemon status
dotr doctor
dotr repo
dotr config set default_repo ~/dotbackup
```

`dotr status` reports:

- Sources missing.
- Files changed since last backup.
- Files excluded by policy.
- Files that would be deleted from backup.
- Git dirty state for managed backup files.

`dotr doctor` checks:

- Config parses.
- Source paths exist or are allowed to be missing.
- Backup repository exists and is writable.
- Exclude rules compile.
- Age identity exists if encrypted restore is requested.
- Age private keys are not present in the repository.
- Suspicious secret-like files are either encrypted or excluded.

## v0 success criteria

v0 is successful when all of these are true:

1. `dotr init` creates a Git repository, `dotr.toml`,
   `files/home`, `files/absolute`, and
   `metadata/index.json`.
2. `dotr init` does not migrate or rewrite any existing dotfiles/chezmoi layout.
3. A starter config contains common shell, Git, SSH, editor, prompt, terminal,
   Homebrew, and VS Code paths, and does not contain machine-specific personal
   paths.
4. A config with `/Library/example/hello/world` maps to
   `files/absolute/Library/example/hello/world`.
5. Excluded files are not copied.
6. Re-running `dotr backup` with no source changes produces no file changes and
   no commit.
7. Editing a managed file causes exactly that stored file and metadata to change.
8. An encrypted path stores only `.age` content in `files` and can be
   restored with the configured age identity.
9. `dotr restore --dry-run` shows intended actions without writing.
10. `dotr restore --apply ~/.config/nvim` restores only paths under
    `~/.config/nvim`.
11. Absolute-path restore is refused unless both `--apply` and
    `--allow-absolute` are present.
12. `dotr watch` coalesces multiple save events into one backup.
13. `dotr watch` ignores backup repository changes unless explicitly configured
    to watch the repository.
14. A backup repository containing `AGE-SECRET-KEY-1` fails `dotr doctor`.
15. Custom backup commands can generate package/editor inventories before
    backup and can run restore commands after matching files are restored.
16. Tests cover init, home-relative mapping, absolute mapping, excludes,
    deletion, encryption, symlink handling, watch debounce, and dry-run restore
    safety.

## Non-goals for v0

- Chezmoi compatibility.
- yadm compatibility.
- Migrating the current dotfiles repository layout.
- Template rendering.
- Browser/application state backup.
- Conflict resolution UI.
- Cross-machine profile transforms.
- Automatic secret classification beyond simple guardrails.
- Cloud storage integration outside Git.

## Open questions

- Should Git commit/push be part of `backup` by default, or should `watch` use
  a separate commit interval?
- Should deleted source files delete backup files by default, or should deletion
  require an explicit pruning mode?
- Should v0 preserve symlink targets exactly, including absolute targets, or
  reject absolute symlink targets during restore?

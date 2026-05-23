# dotr

`dotr` is a small Rust-native backup tool for personal configuration files.

It copies selected files and directories into a Git repository, keeps a metadata
index for restore, supports per-file age encryption, and can watch configured
paths for changes.

The design is intentionally simpler than a full dotfiles manager:

- no chezmoi or yadm dependency
- no symlink-based config management
- no filename encoding such as `dot_foo`
- no templating in v0
- restore is always explicit

## Build

```sh
cargo build
```

For local development:

```sh
cargo run -- --help
```

## Quick Start

Create or prepare a backup repository:

```sh
dotr init ~/dotbackup --with-defaults --set-default
```

Edit `dotr.toml` and choose the paths you want to back up:

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

Run a backup:

```sh
dotr backup
```

Preview a restore:

```sh
dotr restore --dry-run ~/.config/nvim
```

Apply a restore:

```sh
dotr restore --apply ~/.config/nvim
```

After `--set-default`, `dotr backup`, `dotr restore`, and other commands can be
run from any directory.

## Repository Discovery

`dotr` resolves the repository in this order:

1. `--repo` or `-C`
2. `DOTR_REPO`
3. walking upward from the current directory until `dotr.toml` is found
4. `default_repo` in `~/.config/dotr/config.toml`

Examples:

```sh
dotr --repo ~/dotbackup backup
dotr -C ~/dotbackup status
DOTR_REPO=~/dotbackup dotr doctor
dotr config set default_repo ~/dotbackup
dotr repo
```

The user-level config is machine-local and should not be committed:

```toml
default_repo = "/Users/alice/dotbackup"
```

## Repository Layout

`dotr` stores managed data directly at the repository root:

```text
dotr.toml
files/
  home/
  absolute/
metadata/
  index.json
```

Home paths are stored under `files/home`:

```text
~/.zshrc
=> files/home/.zshrc
```

Absolute paths outside `$HOME` are stored under `files/absolute`:

```text
/Library/example/hello/world
=> files/absolute/Library/example/hello/world
```

## Configuration

Example `dotr.toml`:

```toml
[[path]]
src = "~/.config/nvim"
include = [
  "init.lua",
  "lua/**",
  "after/**",
]
exclude = [
  "**/swap/**",
  "**/.netrwhist",
]

[[path]]
src = "~/.config/some-app"
include = [
  "config.toml",
  "assets/**",
]
include_binary_file = true

[[path]]
src = "~/.gitconfig"

[[path]]
src = "~/.config/some-app/token.json"
encrypt = true

[watch]
enabled = true
debounce_secs = 30
min_backup_interval_secs = 900

[daemon]
stdout_log = "~/.local/state/dotr/dotr-watch.log"
stderr_log = "~/.local/state/dotr/dotr-watch.err.log"

[git]
auto_commit = true
auto_push = false
commit_message = "chore(dotr): automated backup"

[encryption]
backend = "age"
recipients_file = "recipients.txt"
identity = "~/.config/dotr/age.key"

[policy]
max_file_size = "20MiB"
```

Default excludes are always applied for common caches, logs, local databases,
sessions, build outputs, temporary files, environment files, private keys, and
similar files.

For application directories that mix durable config with state, prefer
`include` over backing up the whole tree:

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

`include` patterns are relative to `src`. Directory entries are not stored when
`include` is present; parent directories are recreated as needed during backup
and restore.

Binary files are skipped by default. If a configured path intentionally needs
binary assets, set `include_binary_file = true` on that `[[path]]`; include and
exclude rules still apply first.

Symlinks are followed by default so copy-based backups capture the target
contents at the configured path. To preserve symlink metadata instead, opt out
per path:

```toml
[[path]]
src = "~/.config/some-linked-app"
follow_symlink = false
```

## Commands

Initialize a repo:

```sh
dotr init [TARGET] [--with-defaults] [--no-git] [--force] [--set-default]
```

`--with-defaults` writes a generic starter `dotr.toml` for common shell, Git,
SSH, GPG, editor, prompt, and terminal config paths. It is intentionally
conservative: it does not include broad application state directories or
machine-specific personal paths.

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

The generated config contains more entries than the abbreviated example above.
It only creates starter config entries. It does not migrate existing dotfiles,
chezmoi state, yadm state, or any current repository layout.

Current starter paths:

```text
~/.bash_profile
~/.bashrc
~/.profile
~/.zprofile
~/.zshenv
~/.zshrc
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
```

Run one backup pass:

```sh
dotr backup [--dry-run] [--no-delete] [--no-git] [--commit] [--push]
```

`dotr backup` prints progress updates to stderr while it scans configured
sources, checks deletions, writes metadata, and runs optional Git steps.

Add a source path and immediately back it up:

```sh
dotr add ~/.config/yazi
```

Remove a source path and immediately delete its backed-up files:

```sh
dotr remove ~/.config/yazi
```

Show pending backup changes without writing:

```sh
dotr status
```

Restore files:

```sh
dotr restore [--dry-run] [--apply] [--force] [--allow-absolute] [TARGETS]...
```

Run the filesystem watcher:

```sh
dotr watch
```

Start and control the background watcher:

```sh
dotr daemon start
dotr daemon status
dotr daemon stop
```

`dotr daemon start` resolves the repository, writes a user-level daemon config
that points at the resolved repository and the current `dotr` executable, then
launches `dotr --repo <repo> watch` in the background. It does not install a
systemd unit, launchd plist, or any OS-specific service file. The daemon records
a pid file under `~/.local/state/dotr`; `stop` sends the process `SIGTERM`.

Daemon logs default to:

```text
~/.local/state/dotr/dotr-watch.log
~/.local/state/dotr/dotr-watch.err.log
```

Override them in `dotr.toml`:

```toml
[daemon]
stdout_log = "~/.local/state/dotr/watch.log"
stderr_log = "logs/watch.err.log"
```

`~` expands to the user's home directory. Relative paths are resolved from the
dotr repository root. After changing these paths, restart the daemon with
`dotr daemon stop` and `dotr daemon start`.

Check the repository and config:

```sh
dotr doctor
```

Print the resolved repository:

```sh
dotr repo
```

Set the user-level default repository:

```sh
dotr config set default_repo ~/dotbackup
```

## Encryption

Set `encrypt = true` on a path to store it as an age-encrypted `.age` file.

```toml
[[path]]
src = "~/.ssh/config"
encrypt = true
```

`recipients_file` contains public age recipients used for backup encryption.
`identity` points to the private age identity used for restore decryption.

Do not commit the age identity. `dotr doctor` fails if it finds
`AGE-SECRET-KEY-1` anywhere inside the repository.

## Restore Safety

Restore is dry-run by default unless `--apply` is passed.

`dotr` refuses to overwrite differing destination files unless `--force` is
passed.

Absolute-path restore is intentionally stricter. To restore paths outside
`$HOME`, both flags are required:

```sh
dotr restore --apply --allow-absolute /Library/example/hello/world
```

## Development

Run tests:

```sh
cargo test
```

Run clippy:

```sh
cargo clippy --all-targets -- -D warnings
```

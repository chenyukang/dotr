# dotr

`dotr` is a small Rust-native backup tool for personal configuration files.

It copies selected files and directories into a Git repository, keeps a metadata
index for restore, supports per-file age encryption, and can watch configured
paths for changes.

## Install

From crates.io:

```sh
cargo install dotr-cli
```

The published crate is `dotr-cli`, but the installed command is `dotr`.

macOS or Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/chenyukang/dotr/main/install.sh | sh
```

Windows PowerShell:

```powershell
iwr https://raw.githubusercontent.com/chenyukang/dotr/main/install.ps1 -UseB | iex
```

Install a specific release:

```sh
curl -fsSL https://raw.githubusercontent.com/chenyukang/dotr/main/install.sh | DOTR_VERSION=v0.1.0 sh
```

By default the installer writes to `~/.local/bin`. Override with
`DOTR_INSTALL_DIR` if you prefer another directory.

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
[[path_set]]
base = "~"
items = [
  ".zshrc",
  ".gitconfig",
  ".ssh/config",
  ".config/nvim",
]
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
DOTR_REPO=~/dotbackup dotr check
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
metadata/
  index.json
```

Home paths are stored under `files/home`:

```text
~/.zshrc
=> files/home/.zshrc
```

Absolute paths outside `$HOME` are stored under `files/root`:

```text
/Library/example/hello/world
=> files/root/Library/example/hello/world
```

`files/root` is created only after the first absolute path is backed up.

## Configuration

Example `dotr.toml`:

```toml
[[path_set]]
base = "~"
items = [
  ".gitconfig",
  { src = ".config/nvim", include = ["init.lua", "lua/**", "after/**"] },
  { src = ".config/some-app", include = ["config.toml", "assets/**"], include_binary_file = true },
  { src = ".local/state/app/activity.log", force = true },
]

[[path]]
src = "~/.config/some-app/token.json"
encrypt = true

[[custom_backup]]
name = "homebrew"
backup = "brew bundle dump --file ~/.config/homebrew/Brewfile --force"
restore = "brew bundle --file ~/.config/homebrew/Brewfile"
paths = ["~/.config/homebrew/Brewfile"]

[watch]
enabled = true
debounce_secs = 30
min_backup_interval_secs = 900

[daemon]
log_path = "~/.local/state/dotr/dotr-watch.log"
log_level = "info"

[git]
auto_commit = true
auto_push = false
commit_message = "chore(dotr): automated backup"

[encryption]
backend = "age"
recipients_file = "recipients"
identity = "~/.config/dotr/identity"

[policy]
max_file_size = "20MiB"
```

Default excludes are always applied for common caches, logs, local databases,
sessions, build outputs, temporary files, environment files, private keys, and
similar files.

For application directories that mix durable config with state, prefer
`include` over backing up the whole tree:

```toml
[[path_set]]
base = "~"
items = [
  { src = ".codex", include = [
    "AGENTS.md",
    "RTK.md",
    "config.toml",
    "rules/**",
    "skills/**",
  ], exclude = ["skills/.system/**"] },
]
```

`[[path_set]]` is a compact form for many related paths. String items are
equivalent to `{ src = "..." }`; table items accept the same fields as
`[[path]]`. Relative `src` values are joined with `base`; `~` and absolute paths
ignore `base`. `include` patterns are relative to `src`. Directory entries are
not stored when `include` is present; parent directories are recreated as needed
during backup and restore.

Binary files are skipped by default. If a configured path intentionally needs
binary assets, set `include_binary_file = true` on that `[[path]]`; include and
exclude rules still apply first.

Use `force = true` only for explicit files or directories that must bypass
dotr's default policy. It ignores default excludes, binary detection, and
`max_file_size` for that configured path. Explicit `include` and `exclude`
rules still apply.

Symlinks are followed by default so copy-based backups capture the target
contents at the configured path. To preserve symlink metadata instead, opt out
per path:

```toml
[[path]]
src = "~/.config/some-linked-app"
follow_symlink = false
```

For generated inventories such as Homebrew packages or VS Code extensions, use
`[[custom_backup]]`. During `dotr backup`, `backup` runs before the listed
custom paths are scanned. During `dotr restore --apply`, `restore` runs after
matching files are restored. Dry runs print the commands without executing
them. The older `backup_command`, `restore_command`, and
`[[custom_backup.path]]` keys remain supported for existing configs.

```toml
[[custom_backup]]
name = "vscode"
backup = '''
if command -v code >/dev/null 2>&1; then
  mkdir -p ~/.config/vscode
  code --list-extensions > ~/.config/vscode/extensions.txt
else
  echo 'dotr: skipping VS Code extension backup; code not found' >&2
fi
'''
restore = '''
if command -v code >/dev/null 2>&1 && [ -f ~/.config/vscode/extensions.txt ]; then
  xargs -n 1 code --install-extension < ~/.config/vscode/extensions.txt
else
  echo 'dotr: skipping VS Code extension restore; code or extensions.txt not found' >&2
fi
'''
paths = ["~/.config/vscode/extensions.txt"]
path_sets = [
  { base = "~/Library/Application Support/Code/User", items = [
    "settings.json",
    "keybindings.json",
    "tasks.json",
    "snippets",
  ] },
]
```

## Commands

Initialize a repo:

```sh
dotr init [TARGET] [--with-defaults] [--no-git] [--force] [--set-default]
```

`--with-defaults` writes a generic starter `dotr.toml` for common shell, Git,
SSH, GPG, editor, prompt, terminal, Homebrew, and VS Code config paths. It is
intentionally conservative: it does not include broad application state
directories or machine-specific personal paths.

```toml
[[path_set]]
base = "~"
items = [
  ".zshrc",
  ".gitconfig",
  ".ssh/config",
  ".config/nvim",
]
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

Some starter entries use `include` rules. For example, `~/.config/gh` backs up
`config.yml` but avoids `hosts.yml`. VS Code backs up explicit user settings,
keybindings, tasks, snippets, and the generated extension list instead of the
whole `User` directory.

Generate encryption key material:

```sh
dotr keygen [--force]
```

This writes `~/.config/dotr/identity`, writes `recipients` in the dotr
repository, and updates `[encryption]` in `dotr.toml`. Existing key files are
not overwritten unless you confirm with `y` or pass `--force`.

Run one backup pass:

```sh
dotr backup [--dry-run] [--no-delete] [--no-git] [--commit] [--push]
```

`dotr backup` prints progress updates to stderr while it scans configured
sources, checks deletions, writes metadata, and runs optional Git steps.

Add a source path and immediately back it up:

```sh
dotr add ~/.config/yazi
dotr add --encrypt ~/.npmrc
dotr add --force /Library/Logs/MCXTools.log
```

`PATH` may be a file or directory. Use `--encrypt` for secret-bearing paths.
Use `--force` for intentional files that default policy would normally skip,
such as `.log`, binary, or oversized files.
If encryption is not configured yet, `dotr add --encrypt` fails before editing
`dotr.toml` and prints the `[encryption]` snippet plus the key setup commands.
The backup pass is scoped to the added path.
If the scoped backup would store nothing, `dotr add` fails without editing
`dotr.toml`, prints the skip reason, and gives a `dotr add --force PATH` hint.

Remove a source path and immediately delete its backed-up files:

```sh
dotr remove ~/.config/yazi
```

The cleanup pass is scoped to the removed path.

Show pending backup changes without writing:

```sh
dotr status
```

Restore files:

```sh
dotr restore [--dry-run] [--apply] [--force] [--allow-absolute] [-o PATH] [--diff] [TARGETS]...
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
dotr daemon restart
```

`dotr daemon start` resolves the repository, writes a user-level daemon config
that points at the resolved repository and the current `dotr` executable, then
launches `dotr --repo <repo> watch` in the background. It does not install a
systemd unit, launchd plist, or any OS-specific service file. The daemon records
a pid file under `~/.local/state/dotr`; `stop` sends the process `SIGTERM`.

The watcher filters filesystem events to configured paths and runs scoped
backups for the changed files or directories. Scoped backups preserve
`metadata/index.json` entries outside the changed scope and only run matching
custom backup commands.

Daemon logs default to:

```text
~/.local/state/dotr/dotr-watch.log
```

Override the path and level in `dotr.toml`:

```toml
[daemon]
log_path = "~/.local/state/dotr/dotr-watch.log"
log_level = "info"
```

`~` expands to the user's home directory. Relative paths are resolved from the
dotr repository root. After changing these paths, restart the daemon with
`dotr daemon restart`.

`log_level` supports `error`, `warn`, `info`, `debug`, and `trace`. The daemon
redirects both stdout and stderr to the same `log_path`, so structured dotr logs
and unexpected process output stay together. Structured dotr log entries start
with an ISO 8601 timestamp in the system time zone, followed by a tab and a JSON
payload.

Check the repository and config:

```sh
dotr check
```

`dotr doctor` remains available as a compatibility alias.

Print the resolved repository:

```sh
dotr repo
```

Set the user-level default repository:

```sh
dotr config set default_repo ~/dotbackup
```

Generate age key material for encrypted backups:

```sh
cd ~/dotbackup
dotr keygen
```

## Encryption

Set `encrypt = true` on a path to store it as an age-encrypted `.age` file.
`dotr` uses its built-in Rust age implementation for key generation, backup,
and restore, so the `age` command does not need to be installed.

Generate key material once:

```sh
cd ~/dotbackup
dotr keygen
```

This creates `~/.config/dotr/identity`, creates `recipients` in the dotr
repository, and writes the `[encryption]` section in `dotr.toml`.

If either key file already exists, `dotr keygen` asks before overwriting it.
Only an exact `y` confirmation continues. Use `dotr keygen --force` to overwrite
without prompting.

Treat `~/.config/dotr/identity` like a password/private key: do not commit it to
the backup repository. The `recipients` file contains the public age recipient
and is safe to commit.

`dotr keygen` writes this config:

```toml
[encryption]
backend = "age"
recipients_file = "recipients"
identity = "~/.config/dotr/identity"
```

Then mark only the sensitive paths that should be encrypted:

```toml
[[path]]
src = "~/.ssh/config"
encrypt = true
```

When any configured path has `encrypt = true`, `dotr check` verifies that
`recipients_file` exists and contains valid age recipients.

Do not commit the age identity. `dotr check` fails if it finds
`AGE-SECRET-KEY-1` anywhere inside the repository.

Good candidates for encryption are small config files that contain tokens,
hostnames, internal URLs, account names, or API endpoints. Avoid backing up raw
private keys such as `~/.ssh/id_rsa` or GnuPG private key material unless you
have a separate recovery and rotation plan.

If a secret was already committed in plaintext, enabling `encrypt = true` only
protects future backup files. Rewrite Git history or rotate the exposed secret
before publishing the repository.

## Restore Safety

Restore is dry-run by default unless `--apply` is passed.

`dotr` refuses to overwrite differing destination files unless `--force` is
passed.

Use `-o` / `--output` to write exactly one restored file to another path. This
is useful for temporarily decrypting a file to inspect it, and it does not
require `--apply` because it never writes to the original destination:

```sh
dotr restore -o /tmp/ssh-config ~/.ssh/config
```

Use `--diff` to compare what file restore would change without writing:

```sh
dotr restore --diff ~/.ssh/config
```

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

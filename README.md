# dotr

`dotr` is a Rust-native backup tool for personal configuration files.

It copies selected files and directories into a Git repository, keeps metadata
for restore, supports explicit per-file age encryption, and can watch configured
paths for changes.

## Installation

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

Build from source:

```sh
cargo build
cargo run -- --help
```

## Repository Layout

Create or prepare a backup repository:

```sh
dotr init ~/dotbackup --with-defaults --set-default
```

After `--set-default`, `dotr backup`, `dotr restore`, and other commands can be
run from any directory.

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

[[custom_backup]]
name = "homebrew"
backup = "brew bundle dump --file ~/.config/homebrew/Brewfile --force"
restore = "brew bundle --file ~/.config/homebrew/Brewfile"
paths = ["~/.config/homebrew/Brewfile"]

[watch]
debounce_secs = 30
backup_interval_secs = 300

[daemon]
log_path = "~/.local/state/dotr/dotr-watch.log"
log_level = "info"

[git]
auto_commit = true
auto_push = false
commit_message = "chore(dotr): automated backup"

[policy]
max_file_size = "20MiB"
```

`[[path_set]]` is a compact form for many related paths. String items are
equivalent to `{ src = "..." }`; table items accept the same fields as
`[[path]]`. Relative `src` values are joined with `base`; `~` and absolute paths
ignore `base`. `include` patterns are relative to `src`.

Default excludes skip common caches, logs, local databases, sessions, build
outputs, temporary files, environment files, private keys, and similar files.
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

Binary files are skipped by default. If a configured path intentionally needs
binary assets, set `include_binary_file = true`. Use `force = true` only for
explicit files or directories that must bypass default excludes, binary
detection, and `max_file_size`; explicit `include` and `exclude` rules still
apply.

Symlinks are followed by default so copy-based backups capture the target
contents at the configured path. To preserve symlink metadata instead:

```toml
[[path]]
src = "~/.config/some-linked-app"
follow_symlink = false
```

For generated inventories such as Homebrew packages or VS Code extensions, use
`[[custom_backup]]`. During `dotr backup`, `backup` runs before the listed
custom paths are scanned. During `dotr restore --apply`, `restore` runs after
matching files are restored. Dry runs print the commands without executing
them.

## Common Commands

Initialize a repo:

```sh
dotr init [TARGET] [--with-defaults] [--no-git] [--force] [--set-default]
```

`--with-defaults` writes a generic starter `dotr.toml` for common shell, Git,
SSH, GPG, editor, prompt, terminal, Homebrew, and VS Code config paths. It does
not migrate existing dotfiles, chezmoi state, yadm state, or any current
repository layout.

Run one backup pass:

```sh
dotr backup [--dry-run] [--no-delete] [--no-git] [--commit] [--push]
```

`dotr backup` prints progress updates to stderr while it scans configured
sources, checks deletions, writes metadata, and runs optional Git steps.
When Git auto-commit is enabled, dotr stages and commits only dotr-managed
paths: `dotr.toml`, `files/`, `metadata/`, `recipients`, and `.gitignore`.

Add a source path and immediately back it up:

```sh
dotr add ~/.config/yazi
dotr add --encrypt ~/.npmrc
dotr add --force /Library/Logs/MCXTools.log
```

`PATH` may be a file or directory. Use `--encrypt` for secret-bearing paths.
Use `--force` for intentional files that default policy would normally skip,
such as `.log`, binary, or oversized files.

If the scoped backup would store nothing, `dotr add` fails without editing
`dotr.toml`, prints the skip reason, and gives a `dotr add --force PATH` hint.

Remove a source path and immediately delete its backup:

```sh
dotr remove ~/.config/yazi
```

Show pending backup changes without writing:

```sh
dotr status
```

Restore files:

```sh
dotr restore [--dry-run] [--apply] [--force] [--allow-absolute] [-o PATH] [--diff] [TARGETS]...
```

Restore is dry-run by default unless `--apply` is passed. `dotr` refuses to
overwrite differing destination files unless `--force` is passed.

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

Watch configured paths:

```sh
dotr watch
```

Control the background watcher:

```sh
dotr daemon start
dotr daemon status
dotr daemon restart
dotr daemon stop
```

`dotr daemon start` resolves the repository, writes a user-level daemon config
that points at the resolved repository and current `dotr` executable, then
launches `dotr --repo <repo> watch` in the background. It does not install a
systemd unit, launchd plist, or any OS-specific service file.

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

`log_level` supports `error`, `warn`, `info`, `debug`, and `trace`. The daemon
redirects both stdout and stderr to the same `log_path`. Structured dotr log
entries start with an ISO 8601 timestamp in the system time zone, followed by a
tab and a JSON payload.

Check the repository and config:

```sh
dotr check
```

`dotr doctor` remains available as a compatibility alias.

Resolve or set the repository:

```sh
dotr repo
dotr --repo ~/dotbackup backup
dotr -C ~/dotbackup status
DOTR_REPO=~/dotbackup dotr check
dotr config set default_repo ~/dotbackup
```

Repository resolution order:

1. `--repo` or `-C`
2. `DOTR_REPO`
3. walking upward from the current directory until `dotr.toml` is found
4. `default_repo` in `~/.config/dotr/config.toml`

The user-level config is machine-local and should not be committed:

```toml
default_repo = "/Users/alice/dotbackup"
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

```sh
dotr add --encrypt ~/.ssh/config
dotr add --encrypt ~/.npmrc
```

Equivalent manual config:

```toml
[[path]]
src = "~/.ssh/config"
encrypt = true
```

When any configured path has `encrypt = true`, `dotr check` verifies that
`recipients_file` exists and contains valid age recipients.

Good candidates for encryption are small config files that contain tokens,
hostnames, internal URLs, account names, or API endpoints. Avoid backing up raw
private keys such as `~/.ssh/id_rsa` or GnuPG private key material unless you
have a separate recovery and rotation plan.

If a secret was already committed in plaintext, enabling `encrypt = true` only
protects future backup files. Rewrite Git history or rotate the exposed secret
before publishing the repository.

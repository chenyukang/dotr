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
dotr init ~/code/dotbackup --with-defaults
cd ~/code/dotbackup
```

Edit `backup/dotr.toml` and choose the paths you want to back up:

```toml
[[path]]
src = "~/.codex"

[[path]]
src = "~/.agents"

[[path]]
src = "~/.hermes"

[[path]]
src = "~/code/bin"
```

Run a backup:

```sh
dotr backup
```

Preview a restore:

```sh
dotr restore --dry-run ~/.codex
```

Apply a restore:

```sh
dotr restore --apply ~/.codex
```

## Repository Layout

`dotr` stores all managed data under `backup/`:

```text
backup/
  dotr.toml
  files/
    home/
    absolute/
  metadata/
    index.json
```

Home paths are stored under `backup/files/home`:

```text
~/.codex/AGENTS.md
=> backup/files/home/.codex/AGENTS.md
```

Absolute paths outside `$HOME` are stored under `backup/files/absolute`:

```text
/Library/example/hello/world
=> backup/files/absolute/Library/example/hello/world
```

## Configuration

Example `backup/dotr.toml`:

```toml
[[path]]
src = "~/.codex"
exclude = [
  "**/sessions/**",
  "**/logs/**",
  "**/plugins/cache/**",
]

[[path]]
src = "~/code/bin"

[[path]]
src = "~/.config/some-app/token.json"
encrypt = true

[watch]
enabled = true
debounce_secs = 30
min_backup_interval_secs = 900

[git]
auto_commit = true
auto_push = false
commit_message = "chore(dotr): automated backup"

[encryption]
backend = "age"
recipients_file = "backup/recipients.txt"
identity = "~/.config/dotr/age.key"

[policy]
max_file_size = "20MiB"
```

Default excludes are always applied for common caches, logs, local databases,
environment files, private keys, and similar files.

## Commands

Initialize a repo:

```sh
dotr init [TARGET] [--with-defaults] [--no-git] [--force]
```

`--with-defaults` writes a starter `backup/dotr.toml` with these paths:

```toml
[[path]]
src = "~/.codex"

[[path]]
src = "~/.agents"

[[path]]
src = "~/.hermes"

[[path]]
src = "~/code/bin"
```

It only creates starter config entries. It does not migrate existing dotfiles,
chezmoi state, yadm state, or any current repository layout.

Run one backup pass:

```sh
dotr backup [--dry-run] [--no-delete] [--no-git] [--commit] [--push]
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

Check the repository and config:

```sh
dotr doctor
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

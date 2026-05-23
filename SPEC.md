# dotr specification

## Goal

`dotr` is a Rust-native filesystem-to-Git backup tool for personal configuration files.

It backs up selected files and directories from the local machine into a Git repository,
can restore them later, and can run as a daemon that reacts to filesystem changes.

The core design is intentionally simpler than a full dotfiles manager:

- No dependency on chezmoi, yadm, or a shell backup script.
- No `dot_` or `private_` source filename encoding.
- No templating in v0.
- No package-manager backup in v0.
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

The repository stores `dotr` data under one directory:

```text
backup/
  dotr.toml
  files/
    home/
      .codex/
        AGENTS.md
      code/
        bin/
          restart-codex.sh
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
~/.codex/AGENTS.md
=> backup/files/home/.codex/AGENTS.md

~/code/bin/restart-codex.sh
=> backup/files/home/code/bin/restart-codex.sh

/Library/example/hello/world
=> backup/files/absolute/Library/example/hello/world
```

`backup/files/home` maps to `$HOME`.

`backup/files/absolute` maps to `/`.

`backup/metadata/index.json` stores metadata that cannot be represented reliably
by copied file contents alone.

## Initialization

`dotr init` creates a new backup repository or prepares an existing Git
repository for `dotr`.

Examples:

```text
dotr init ~/code/dotbackup
dotr init . --with-defaults
dotr init . --no-git
```

Behavior:

1. If the target directory does not exist, create it.
2. If the target directory is not a Git repository and `--no-git` is not set,
   run `git init`.
3. Create `backup/dotr.toml` unless it already exists.
4. Create `backup/files/home`, `backup/files/absolute`, and `backup/metadata`.
5. Create an empty `backup/metadata/index.json`.
6. Create `backup/recipients.txt` when encryption is enabled.
7. Create or update a conservative `.gitignore` for dotr lock files, temp files,
   and logs, without ignoring `backup/files`.

`dotr init` must not migrate the current dotfiles repository layout in v0. It
creates a fresh `backup/` tree and leaves any existing chezmoi, yadm, or custom
dotfiles structure untouched.

`--with-defaults` may create a starter config for this machine:

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

## Configuration

`backup/dotr.toml` is committed to the repository.

Example:

```toml
[repository]
root = "/Users/yukang/code/dotfiles"
store = "backup"

[[path]]
src = "~/.codex"

[[path]]
src = "~/.agents"

[[path]]
src = "~/.hermes"

[[path]]
src = "~/code/bin"

[[path]]
src = "/Library/example/hello/world"

[watch]
enabled = true
debounce_secs = 30
min_backup_interval_secs = 900

[git]
auto_commit = true
auto_push = true
commit_message = "chore(dotr): automated backup"

[encryption]
backend = "age"
recipients_file = "backup/recipients.txt"
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
exclude = [
  "**/sessions/**",
  "**/logs/**",
  "**/plugins/cache/**",
]
```

Global default excludes are always applied:

```text
**/.git/**
**/.DS_Store
**/__pycache__/**
**/node_modules/**
**/.venv/**
**/venv/**
**/.env
**/.env.*
**/*.pem
**/*.key
**/*.db
**/*.sqlite
**/*.log
**/*.pyc
**/references/llms*.md
```

These defaults are conservative and can be loosened only with an explicit
per-path allow rule in a later version. v0 does not need allow-rule overrides.

## Backup behavior

`dotr backup` performs one complete backup pass.

Algorithm:

1. Load `backup/dotr.toml`.
2. Resolve configured source paths.
3. Walk files under each source path.
4. Apply default and per-path excludes.
5. Reject paths that cannot be mapped safely into `backup/files`.
6. Compare source files with current backup files.
7. Copy changed or new files into `backup/files`.
8. Remove backed-up files whose source files disappeared, unless
   `--no-delete` is set.
9. Write `backup/metadata/index.json`.
10. If files changed and Git auto-commit is enabled, commit and optionally push.

Comparison uses content hashing, not only mtime. Metadata-only changes are still
recorded in `index.json` and should count as changes.

`dotr backup --dry-run` prints planned additions, updates, deletions, encrypted
updates, and Git actions without writing.

## Metadata

`index.json` stores one entry per backed-up path:

```json
{
  "version": 1,
  "entries": [
    {
      "source": "~/.codex/AGENTS.md",
      "stored": "files/home/.codex/AGENTS.md",
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

`dotr` must not blindly follow symlinks.

Default behavior:

- Back up symlinks as symlink metadata, not as target file contents.
- Store the symlink target in `index.json`.
- Restore symlinks as symlinks.

Safety rules:

- A symlink inside a configured source must not cause `dotr` to read files outside
  the configured source unless a future explicit `follow_symlinks = true` exists.
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
=> backup/files/home/.config/some-app/token.json.age
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

`dotr restore` restores from `backup/files` to the original paths.

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
dotr restore --apply ~/.codex
dotr restore --apply --allow-absolute /Library/example/hello/world
```

Absolute path restore follows the stricter path:

- Backing up absolute paths is allowed.
- Restoring absolute paths is always dry-run unless `--apply` is present.
- Restoring absolute paths is refused unless `--allow-absolute` is also present.

Path traversal protection is mandatory:

- Stored paths must normalize under `backup/files/home` or
  `backup/files/absolute`.
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
dotr backup [--dry-run] [--no-delete] [--no-git] [--commit] [--push]
dotr status
dotr restore [--dry-run] [--apply] [--force] [--allow-absolute] [PATH...]
dotr watch
dotr doctor
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

1. `dotr init` creates a Git repository, `backup/dotr.toml`,
   `backup/files/home`, `backup/files/absolute`, and
   `backup/metadata/index.json`.
2. `dotr init` does not migrate or rewrite any existing dotfiles/chezmoi layout.
3. A config with `~/.codex`, `~/.agents`, `~/.hermes`, and `~/code/bin` backs up
   to `backup/files/home/...` without chezmoi.
4. A config with `/Library/example/hello/world` maps to
   `backup/files/absolute/Library/example/hello/world`.
5. Excluded files are not copied.
6. Re-running `dotr backup` with no source changes produces no file changes and
   no commit.
7. Editing a managed file causes exactly that stored file and metadata to change.
8. An encrypted path stores only `.age` content in `backup/files` and can be
   restored with the configured age identity.
9. `dotr restore --dry-run` shows intended actions without writing.
10. `dotr restore --apply ~/.codex` restores only paths under `~/.codex`.
11. Absolute-path restore is refused unless both `--apply` and
    `--allow-absolute` are present.
12. `dotr watch` coalesces multiple save events into one backup.
13. `dotr watch` ignores backup repository changes unless explicitly configured
    to watch the repository.
14. A backup repository containing `AGE-SECRET-KEY-1` fails `dotr doctor`.
15. Tests cover init, home-relative mapping, absolute mapping, excludes,
    deletion, encryption, symlink handling, watch debounce, and dry-run restore
    safety.

## Non-goals for v0

- Chezmoi compatibility.
- yadm compatibility.
- Migrating the current dotfiles repository layout.
- Template rendering.
- Package manager backup.
- VS Code extension backup.
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

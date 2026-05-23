use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use globset::GlobSet;
use walkdir::WalkDir;

use crate::{
    config::{Config, default_exclude_set, globset_from_patterns, index_path},
    encryption,
    environment::Environment,
    git::{CommandGit, GitBackend},
    hash::{sha256_bytes, sha256_file},
    index::{EntryKind, Index, IndexEntry},
    paths::{absolutize, ensure_safe_relative, source_to_stored},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupOptions {
    pub dry_run: bool,
    pub no_delete: bool,
    pub no_git: bool,
    pub commit: bool,
    pub push: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReport {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub encrypted: usize,
    pub actions: Vec<String>,
}

impl BackupReport {
    pub fn changed(&self) -> bool {
        self.added + self.updated + self.deleted > 0
    }
}

pub fn run(repo_root: &Path, env: &Environment, options: &BackupOptions) -> Result<BackupReport> {
    let config = Config::load(repo_root)?;
    run_with_config(repo_root, env, &config, options, &CommandGit)
}

pub fn run_with_config(
    repo_root: &Path,
    env: &Environment,
    config: &Config,
    options: &BackupOptions,
    git: &impl GitBackend,
) -> Result<BackupReport> {
    let store_dir = config.store_dir(repo_root);
    let index_file = index_path(&store_dir);
    let previous = Index::read(&index_file)?;
    let default_excludes = default_exclude_set()?;
    let max_file_size = config.max_file_size_bytes()?;
    let recipients = if config.has_encrypted_paths() && !options.dry_run {
        let recipients_file = config
            .encryption
            .recipients_file
            .as_deref()
            .context("encrypted paths require encryption.recipients_file")?;
        let recipients_path = encryption::resolve_recipients_file(repo_root, recipients_file);
        Some(encryption::load_recipients(&recipients_path)?)
    } else {
        None
    };

    let mut report = BackupReport::default();
    let mut current = BTreeMap::<String, IndexEntry>::new();

    for path_config in &config.paths {
        let source = absolutize(&env.expand_tilde(&path_config.src), repo_root);
        if !source.exists() {
            report.skipped += 1;
            report
                .actions
                .push(format!("skip missing source {}", source.display()));
            continue;
        }

        let local_excludes = globset_from_patterns(path_config.exclude.iter().map(String::as_str))?;
        if source.is_dir() && !source.symlink_metadata()?.file_type().is_symlink() {
            for entry in WalkDir::new(&source)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    !is_excluded(entry.path(), &source, &default_excludes, &local_excludes)
                })
            {
                let entry =
                    entry.with_context(|| format!("failed to walk {}", source.display()))?;
                process_entry(
                    env,
                    &store_dir,
                    &previous,
                    &mut current,
                    &mut report,
                    entry.path(),
                    path_config.encrypt,
                    max_file_size,
                    recipients.as_deref(),
                    options.dry_run,
                )?;
            }
        } else if is_excluded(&source, &source, &default_excludes, &local_excludes) {
            report.skipped += 1;
            report
                .actions
                .push(format!("skip excluded {}", source.display()));
        } else {
            process_entry(
                env,
                &store_dir,
                &previous,
                &mut current,
                &mut report,
                &source,
                path_config.encrypt,
                max_file_size,
                recipients.as_deref(),
                options.dry_run,
            )?;
        }
    }

    handle_deletions(
        &store_dir,
        &previous,
        &mut current,
        &mut report,
        options.no_delete,
        options.dry_run,
    )?;

    if !options.dry_run {
        let index = Index {
            version: 1,
            entries: current.into_values().collect(),
        };
        index.write(&index_file)?;

        let should_commit = !options.no_git
            && report.changed()
            && (options.commit || config.git.auto_commit || options.push || config.git.auto_push);
        if should_commit {
            let message = format!(
                "{} ({})",
                config.git.commit_message,
                change_summary(&report)
            );
            git.commit_backup(repo_root, &message, config.git.include_unrelated)?;
        }

        let should_push =
            !options.no_git && report.changed() && (options.push || config.git.auto_push);
        if should_push {
            git.push(repo_root)?;
        }
    }

    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn process_entry(
    env: &Environment,
    store_dir: &Path,
    previous: &Index,
    current: &mut BTreeMap<String, IndexEntry>,
    report: &mut BackupReport,
    source: &Path,
    encrypted: bool,
    max_file_size: u64,
    recipients: Option<&[age::x25519::Recipient]>,
    dry_run: bool,
) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("failed to read metadata for {}", source.display()))?;
    let file_type = metadata.file_type();
    let mode = unix_mode(&metadata);

    if file_type.is_file() && metadata.len() > max_file_size {
        report.skipped += 1;
        report.actions.push(format!(
            "skip oversized {} ({} bytes)",
            source.display(),
            metadata.len()
        ));
        return Ok(());
    }

    let stored = source_to_stored(source, env, encrypted && file_type.is_file())?;
    ensure_safe_relative(&stored.relative)?;
    let stored_key = stored.as_index_path();
    let previous_entry = previous.by_stored(&stored_key);
    let mut index_entry = IndexEntry {
        source: env.display_source(source),
        stored: stored_key.clone(),
        kind: kind_for_metadata(&metadata),
        sha256: None,
        mode,
        executable: mode.is_some_and(|mode| mode & 0o111 != 0),
        encrypted: encrypted && file_type.is_file(),
        symlink_target: None,
        size: None,
        modified_unix_nanos: None,
    };

    if file_type.is_dir() {
        if !dry_run {
            fs::create_dir_all(store_dir.join(&stored.relative))?;
        }
        record_change(previous_entry, &index_entry, report, "directory");
        current.insert(stored_key, index_entry);
        return Ok(());
    }

    if file_type.is_symlink() {
        let target = fs::read_link(source)
            .with_context(|| format!("failed to read symlink {}", source.display()))?;
        index_entry.symlink_target = Some(target.to_string_lossy().into_owned());
        if !dry_run {
            remove_if_exists(&store_dir.join(&stored.relative))?;
        }
        record_change(previous_entry, &index_entry, report, "symlink");
        current.insert(stored_key, index_entry);
        return Ok(());
    }

    if !file_type.is_file() {
        report.skipped += 1;
        report
            .actions
            .push(format!("skip unsupported file type {}", source.display()));
        return Ok(());
    }

    index_entry.size = Some(metadata.len());
    index_entry.modified_unix_nanos = modified_unix_nanos(&metadata);

    if encrypted {
        let unchanged = previous_entry.is_some_and(|prev| {
            prev.kind == index_entry.kind
                && prev.encrypted
                && prev.mode == index_entry.mode
                && prev.executable == index_entry.executable
                && prev.size == index_entry.size
                && prev.modified_unix_nanos == index_entry.modified_unix_nanos
                && store_dir.join(&prev.stored).exists()
        });

        if unchanged {
            index_entry.sha256 = previous_entry.and_then(|prev| prev.sha256.clone());
            report.unchanged += 1;
            current.insert(stored_key, index_entry);
            return Ok(());
        }

        report.encrypted += 1;
        if !dry_run {
            let recipients = recipients.context("encrypted paths require age recipients")?;
            let plaintext =
                fs::read(source).with_context(|| format!("failed to read {}", source.display()))?;
            let ciphertext = encryption::encrypt_bytes(&plaintext, recipients)?;
            let target = store_dir.join(&stored.relative);
            write_bytes(&target, &ciphertext)?;
            index_entry.sha256 = Some(sha256_bytes(&ciphertext));
        } else {
            index_entry.sha256 = previous_entry.and_then(|prev| prev.sha256.clone());
        }

        record_change(previous_entry, &index_entry, report, "encrypted file");
        current.insert(stored_key, index_entry);
        return Ok(());
    }

    index_entry.sha256 =
        Some(sha256_file(source).with_context(|| format!("failed to hash {}", source.display()))?);
    let unchanged =
        previous_entry == Some(&index_entry) && store_dir.join(&stored.relative).exists();

    if unchanged {
        report.unchanged += 1;
    } else {
        if !dry_run {
            copy_file(source, &store_dir.join(&stored.relative))?;
        }
        record_change(previous_entry, &index_entry, report, "file");
    }

    current.insert(stored_key, index_entry);
    Ok(())
}

fn handle_deletions(
    store_dir: &Path,
    previous: &Index,
    current: &mut BTreeMap<String, IndexEntry>,
    report: &mut BackupReport,
    no_delete: bool,
    dry_run: bool,
) -> Result<()> {
    let current_keys = current.keys().cloned().collect::<BTreeSet<_>>();
    let mut stale = previous
        .entries
        .iter()
        .filter(|entry| !current_keys.contains(&entry.stored))
        .cloned()
        .collect::<Vec<_>>();
    stale.sort_by_key(|entry| std::cmp::Reverse(entry.stored.len()));

    for entry in stale {
        if no_delete {
            current.insert(entry.stored.clone(), entry);
            continue;
        }

        ensure_safe_relative(Path::new(&entry.stored))?;
        report.deleted += 1;
        report.actions.push(format!("delete {}", entry.stored));
        if !dry_run {
            remove_if_exists(&store_dir.join(&entry.stored))?;
        }
    }

    Ok(())
}

fn is_excluded(path: &Path, source_root: &Path, default: &GlobSet, local: &GlobSet) -> bool {
    let rel = path.strip_prefix(source_root).unwrap_or(path);
    default.is_match(path)
        || default.is_match(rel)
        || local.is_match(path)
        || local.is_match(rel)
        || path
            .file_name()
            .is_some_and(|file_name| default.is_match(file_name) || local.is_match(file_name))
}

fn record_change(
    previous_entry: Option<&IndexEntry>,
    next_entry: &IndexEntry,
    report: &mut BackupReport,
    noun: &str,
) {
    match previous_entry {
        None => {
            report.added += 1;
            report
                .actions
                .push(format!("add {noun} {}", next_entry.source));
        }
        Some(prev) if prev != next_entry => {
            report.updated += 1;
            report
                .actions
                .push(format!("update {noun} {}", next_entry.source));
        }
        Some(_) => report.unchanged += 1,
    }
}

fn kind_for_metadata(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    }
}

fn unix_mode(metadata: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn modified_unix_nanos(metadata: &fs::Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_if_exists(path)?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

fn copy_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_if_exists(target)?;
    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            target.display()
        )
    })?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
    }
}

fn change_summary(report: &BackupReport) -> String {
    format!(
        "{} add, {} update, {} delete",
        report.added, report.updated, report.deleted
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs as unix_fs;

    use age::secrecy::ExposeSecret;
    use tempfile::tempdir;

    use super::*;
    use crate::{config::PathConfig, init};

    fn env_for(home: &Path) -> Environment {
        Environment::new(home.to_path_buf()).unwrap()
    }

    fn repo_with_config(home: &Path, config: &Config) -> tempfile::TempDir {
        let repo = tempdir().unwrap();
        init::run(&init::InitOptions {
            target: repo.path().to_path_buf(),
            with_defaults: false,
            no_git: true,
            force: false,
        })
        .unwrap();
        let toml = toml::to_string_pretty(config).unwrap();
        fs::write(repo.path().join("backup/dotr.toml"), toml).unwrap();
        fs::create_dir_all(home).unwrap();
        repo
    }

    #[test]
    fn backs_up_home_file_and_noops_on_second_run() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(home.join(".codex/AGENTS.md"), "rules").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.codex".to_string(),
            exclude: Vec::new(),
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let first = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();
        assert!(first.added >= 1);
        assert_eq!(
            fs::read_to_string(repo.path().join("backup/files/home/.codex/AGENTS.md")).unwrap(),
            "rules"
        );

        let second = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();
        assert_eq!(second.added + second.updated + second.deleted, 0);
    }

    #[test]
    fn excludes_default_secret_like_files() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("app")).unwrap();
        fs::write(home.join("app/config.toml"), "ok").unwrap();
        fs::write(home.join("app/.env"), "token=secret").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/app".to_string(),
            exclude: Vec::new(),
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(
            repo.path()
                .join("backup/files/home/app/config.toml")
                .exists()
        );
        assert!(!repo.path().join("backup/files/home/app/.env").exists());
    }

    #[test]
    fn deletes_stale_backup_files_by_default() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/tool"), "one").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/bin".to_string(),
            exclude: Vec::new(),
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let options = BackupOptions {
            no_git: true,
            ..BackupOptions::default()
        };
        run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();
        fs::remove_file(home.join("bin/tool")).unwrap();
        let report = run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();

        assert!(report.deleted >= 1);
        assert!(!repo.path().join("backup/files/home/bin/tool").exists());
    }

    #[test]
    fn encrypted_backup_writes_age_file_without_plaintext() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/token.json"), "secret-token").unwrap();

        let repo = tempdir().unwrap();
        init::run(&init::InitOptions {
            target: repo.path().to_path_buf(),
            with_defaults: false,
            no_git: true,
            force: false,
        })
        .unwrap();

        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        fs::write(
            repo.path().join("backup/recipients.txt"),
            recipient.to_string(),
        )
        .unwrap();

        let mut config = Config::default();
        config.encryption.recipients_file = Some("backup/recipients.txt".to_string());
        config.paths.push(PathConfig {
            src: "~/.config/app/token.json".to_string(),
            exclude: Vec::new(),
            encrypt: true,
        });
        fs::write(
            repo.path().join("backup/dotr.toml"),
            toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let env = env_for(home);
        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        let encrypted = fs::read(
            repo.path()
                .join("backup/files/home/.config/app/token.json.age"),
        )
        .unwrap();
        assert!(
            !encrypted
                .windows(b"secret-token".len())
                .any(|window| window == b"secret-token")
        );

        let index = Index::read(&repo.path().join("backup/metadata/index.json")).unwrap();
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.source.ends_with("token.json"))
            .unwrap();
        assert!(entry.encrypted);
        assert!(entry.sha256.is_some());
        assert_ne!(entry.sha256.as_deref(), Some("secret-token"));

        fs::write(
            repo.path().join("identity.txt"),
            identity.to_string().expose_secret(),
        )
        .unwrap();
    }

    #[test]
    fn symlinks_are_recorded_as_symlinks_not_followed() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("links")).unwrap();
        fs::write(home.join("outside"), "outside").unwrap();
        unix_fs::symlink(home.join("outside"), home.join("links/current")).unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/links".to_string(),
            exclude: Vec::new(),
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(!repo.path().join("backup/files/home/links/current").exists());
        let index = Index::read(&repo.path().join("backup/metadata/index.json")).unwrap();
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.source.ends_with("links/current"))
            .unwrap();
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert!(entry.symlink_target.is_some());
    }
}

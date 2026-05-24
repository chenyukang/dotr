use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use similar::{ChangeTag, TextDiff};

use crate::{
    config::{Config, index_path},
    custom_backup, encryption,
    environment::Environment,
    hash::sha256_bytes,
    index::{EntryKind, Index, IndexEntry},
    paths::{ensure_safe_relative, is_stored_absolute, stored_index_to_target},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreOptions {
    pub dry_run: bool,
    pub apply: bool,
    pub force: bool,
    pub allow_absolute: bool,
    pub output: Option<PathBuf>,
    pub diff: bool,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub restored: usize,
    pub planned: usize,
    pub skipped: usize,
    pub actions: Vec<String>,
    pub diffs: Vec<String>,
}

pub fn run(repo_root: &Path, env: &Environment, options: &RestoreOptions) -> Result<RestoreReport> {
    let config = Config::load(repo_root)?;
    run_with_config(repo_root, env, &config, options)
}

pub fn run_with_config(
    repo_root: &Path,
    env: &Environment,
    config: &Config,
    options: &RestoreOptions,
) -> Result<RestoreReport> {
    if options.dry_run && options.apply {
        bail!("restore --dry-run and --apply cannot be used together");
    }
    if options.diff && options.apply {
        bail!("restore --diff and --apply cannot be used together");
    }
    if options.diff && options.output.is_some() {
        bail!("restore --diff and --output cannot be used together");
    }

    let store_dir = config.store_dir(repo_root);
    let index = Index::read(&index_path(&store_dir))?;
    let target_filters = options
        .targets
        .iter()
        .map(|raw| absolute_filter(repo_root, env, raw))
        .collect::<Vec<_>>();
    let entries = index
        .entries
        .iter()
        .filter(|entry| {
            target_filters.is_empty()
                || target_filters
                    .iter()
                    .any(|filter| entry_matches(env, entry, filter))
        })
        .collect::<Vec<_>>();
    if options.output.is_some() && (entries.len() != 1 || entries[0].kind != EntryKind::File) {
        bail!("restore --output requires exactly one file target");
    }

    let dry_run = options.dry_run || (!options.apply && options.output.is_none());
    let needs_identities =
        (!dry_run || options.diff) && entries.iter().any(|entry| entry.encrypted);
    let identities = if needs_identities {
        let identity = config
            .encryption
            .identity
            .as_deref()
            .context("encrypted restore requires encryption.identity")?;
        let identity_path = encryption::resolve_identity_file(env, identity);
        Some(encryption::load_identities(&identity_path)?)
    } else {
        None
    };

    let mut report = RestoreReport::default();
    for entry in entries {
        ensure_safe_relative(Path::new(&entry.stored))?;
        let absolute_restore = is_stored_absolute(&entry.stored);
        if !dry_run && options.output.is_none() && absolute_restore && !options.allow_absolute {
            bail!(
                "refusing to restore absolute path {}; pass --allow-absolute with --apply",
                entry.source
            );
        }

        let (_, default_target) = stored_index_to_target(&entry.stored, env)?;
        let target = options.output.as_deref().unwrap_or(&default_target);
        let action = format!("restore {} -> {}", entry.stored, target.display());
        if options.diff {
            diff_entry(
                &store_dir,
                entry,
                target,
                identities.as_deref(),
                &mut report,
            )?;
            continue;
        }

        if dry_run {
            report.planned += 1;
            report.actions.push(format!("would {action}"));
            continue;
        }

        restore_entry(
            &store_dir,
            entry,
            target,
            identities.as_deref(),
            options.force,
        )?;
        report.restored += 1;
        report.actions.push(action);
    }

    if options.output.is_none() {
        custom_backup::run_restore_commands(
            config,
            repo_root,
            env,
            dry_run,
            &options.targets,
            &mut report.actions,
        )?;
    }

    Ok(report)
}

fn diff_entry(
    store_dir: &Path,
    entry: &IndexEntry,
    target: &Path,
    identities: Option<&[age::x25519::Identity]>,
    report: &mut RestoreReport,
) -> Result<()> {
    if entry.kind != EntryKind::File {
        report.skipped += 1;
        report
            .actions
            .push(format!("skip diff non-file {}", entry.source));
        return Ok(());
    }

    let incoming = entry_bytes(store_dir, entry, identities)?;
    let existing = match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_file() => fs::read(target)
            .with_context(|| format!("failed to read existing {}", target.display()))?,
        Ok(_) => {
            report.skipped += 1;
            report
                .actions
                .push(format!("skip diff non-file target {}", target.display()));
            return Ok(());
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", target.display()));
        }
    };

    report.planned += 1;
    if existing == incoming {
        report.actions.push(format!(
            "diff {} -> {}: no changes",
            entry.stored,
            target.display()
        ));
        return Ok(());
    }

    match render_text_diff(target, &entry.stored, &existing, &incoming) {
        Some(diff) => report.diffs.push(diff),
        None => {
            report.skipped += 1;
            report.actions.push(format!(
                "skip diff binary or non-UTF-8 {} -> {}",
                entry.stored,
                target.display()
            ));
        }
    }

    Ok(())
}

fn render_text_diff(
    target: &Path,
    stored: &str,
    existing: &[u8],
    incoming: &[u8],
) -> Option<String> {
    let existing = std::str::from_utf8(existing).ok()?;
    let incoming = std::str::from_utf8(incoming).ok()?;
    let diff = TextDiff::from_lines(existing, incoming);
    let mut rendered = format!("--- {}\n+++ {}\n", target.display(), stored);

    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        rendered.push_str(sign);
        rendered.push_str(&change.to_string());
    }

    Some(rendered)
}

fn restore_entry(
    store_dir: &Path,
    entry: &IndexEntry,
    target: &Path,
    identities: Option<&[age::x25519::Identity]>,
    force: bool,
) -> Result<()> {
    match entry.kind {
        EntryKind::Directory => {
            ensure_can_write(target, None, force)?;
            fs::create_dir_all(target)
                .with_context(|| format!("failed to create directory {}", target.display()))?;
        }
        EntryKind::Symlink => {
            let symlink_target = entry
                .symlink_target
                .as_deref()
                .context("symlink index entry is missing symlink_target")?;
            ensure_can_write(target, None, force)?;
            if force {
                remove_if_exists(target)?;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            create_symlink(Path::new(symlink_target), target)?;
        }
        EntryKind::File => {
            let bytes = entry_bytes(store_dir, entry, identities)?;

            ensure_can_write(target, Some(&bytes), force)?;
            if force {
                remove_if_exists(target)?;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(target, &bytes)
                .with_context(|| format!("failed to write {}", target.display()))?;
            restore_mode(target, entry.mode)?;
        }
    }

    Ok(())
}

fn entry_bytes(
    store_dir: &Path,
    entry: &IndexEntry,
    identities: Option<&[age::x25519::Identity]>,
) -> Result<Vec<u8>> {
    let source = store_dir.join(&entry.stored);
    if entry.encrypted {
        let identities = identities.context("encrypted restore requires age identities")?;
        let ciphertext =
            fs::read(&source).with_context(|| format!("failed to read {}", source.display()))?;
        encryption::decrypt_bytes(&ciphertext, identities)
    } else {
        fs::read(&source).with_context(|| format!("failed to read {}", source.display()))
    }
}

fn ensure_can_write(target: &Path, incoming: Option<&[u8]>, force: bool) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(target) else {
        return Ok(());
    };

    if metadata.file_type().is_symlink() {
        if force {
            return Ok(());
        }
        bail!(
            "refusing to overwrite existing symlink {}",
            target.display()
        );
    }

    if metadata.is_file() {
        if let Some(bytes) = incoming {
            let existing = fs::read(target)
                .with_context(|| format!("failed to read existing {}", target.display()))?;
            if sha256_bytes(&existing) == sha256_bytes(bytes) {
                return Ok(());
            }
        }

        if !force {
            bail!("refusing to overwrite differing file {}", target.display());
        }
    }

    if metadata.is_dir() && incoming.is_some() && !force {
        bail!("refusing to overwrite directory {}", target.display());
    }

    Ok(())
}

fn entry_matches(env: &Environment, entry: &IndexEntry, filter: &Path) -> bool {
    let source_target = env.expand_tilde(&entry.source);
    source_target.starts_with(filter)
}

fn absolute_filter(repo_root: &Path, env: &Environment, raw: &str) -> PathBuf {
    let expanded = env.expand_tilde(raw);
    if expanded.is_absolute() {
        expanded
    } else {
        repo_root.join(expanded)
    }
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

fn restore_mode(path: &Path, mode: Option<u32>) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(mode) = mode {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .with_context(|| format!("failed to set mode on {}", path.display()))?;
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        let _ = mode;
    }

    Ok(())
}

fn create_symlink(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target)
            .with_context(|| format!("failed to create symlink {}", target.display()))
    }

    #[cfg(not(unix))]
    {
        let _ = source;
        let _ = target;
        bail!("symlink restore is only supported on unix platforms in v0")
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;

    use age::secrecy::ExposeSecret;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        backup::{self, BackupOptions},
        config::{CustomBackupConfig, PathConfig},
        init,
    };

    fn prepare_repo(home: &Path, config: &Config) -> tempfile::TempDir {
        let repo = tempdir().unwrap();
        init::run(&init::InitOptions {
            target: repo.path().to_path_buf(),
            with_defaults: false,
            no_git: true,
            force: false,
        })
        .unwrap();
        fs::write(
            repo.path().join("dotr.toml"),
            toml::to_string_pretty(config).unwrap(),
        )
        .unwrap();
        fs::create_dir_all(home).unwrap();
        repo
    }

    #[test]
    fn dry_run_restore_does_not_write() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/nvim")).unwrap();
        fs::write(home.join(".config/nvim/init.lua"), "rules").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/nvim".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(home.join(".config/nvim/init.lua")).unwrap();

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                dry_run: true,
                apply: false,
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert!(report.planned > 0);
        assert!(!home.join(".config/nvim/init.lua").exists());
    }

    #[test]
    fn restore_rejects_dry_run_with_apply() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let config = Config::default();
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();

        let err = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                dry_run: true,
                apply: true,
                ..RestoreOptions::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("--dry-run and --apply"));
    }

    #[test]
    fn scoped_restore_only_restores_matching_target() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/nvim")).unwrap();
        fs::create_dir_all(home.join(".config/fish")).unwrap();
        fs::write(home.join(".config/nvim/init.lua"), "nvim").unwrap();
        fs::write(home.join(".config/fish/config.fish"), "fish").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/nvim".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        config.paths.push(PathConfig {
            src: "~/.config/fish".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();
        let options = BackupOptions {
            no_git: true,
            ..BackupOptions::default()
        };
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &options,
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(home.join(".config/nvim/init.lua")).unwrap();
        fs::remove_file(home.join(".config/fish/config.fish")).unwrap();

        run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                apply: true,
                targets: vec!["~/.config/nvim".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(home.join(".config/nvim/init.lua")).unwrap(),
            "nvim"
        );
        assert!(!home.join(".config/fish/config.fish").exists());
    }

    #[cfg(unix)]
    #[test]
    fn restore_runs_matching_custom_restore_command_after_files() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/generated")).unwrap();
        fs::write(home.join(".config/generated/state.txt"), "state").unwrap();

        let mut config = Config::default();
        config.custom_backups.push(CustomBackupConfig {
            name: "generated".to_string(),
            backup_command: None,
            restore_command: Some("printf restored > ~/.custom-restored".to_string()),
            paths: vec![PathConfig {
                src: "~/.config/generated/state.txt".to_string(),
                include: Vec::new(),
                exclude: Vec::new(),
                follow_symlink: true,
                include_binary_file: false,
                force: false,
                encrypt: false,
            }],
            ..CustomBackupConfig::default()
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(home.join(".config/generated/state.txt")).unwrap();

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                apply: true,
                targets: vec!["~/.config/generated".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(home.join(".config/generated/state.txt")).unwrap(),
            "state"
        );
        assert_eq!(
            fs::read_to_string(home.join(".custom-restored")).unwrap(),
            "restored"
        );
        assert!(
            report
                .actions
                .iter()
                .any(|action| action.starts_with("run custom restore generated:"))
        );
    }

    #[test]
    fn dry_run_restore_reports_custom_restore_without_running_it() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let mut config = Config::default();
        config.custom_backups.push(CustomBackupConfig {
            name: "generated".to_string(),
            backup_command: None,
            restore_command: Some("printf restored > ~/.custom-restored".to_string()),
            paths: vec![PathConfig {
                src: "~/.config/generated/state.txt".to_string(),
                include: Vec::new(),
                exclude: Vec::new(),
                follow_symlink: true,
                include_binary_file: false,
                force: false,
                encrypt: false,
            }],
            ..CustomBackupConfig::default()
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                dry_run: true,
                targets: vec!["~/.config/generated".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert!(!home.join(".custom-restored").exists());
        assert!(
            report
                .actions
                .iter()
                .any(|action| action.starts_with("would run custom restore generated:"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn absolute_restore_requires_explicit_flag() {
        let home_dir = tempdir().unwrap();
        let abs_root = tempdir().unwrap();
        let source = abs_root.path().join("Library/example/hello/world");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "abs").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: source.to_string_lossy().into_owned(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = prepare_repo(home_dir.path(), &config);
        let env = Environment::new(home_dir.path().to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(&source).unwrap();

        let err = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                apply: true,
                targets: vec![source.to_string_lossy().into_owned()],
                ..RestoreOptions::default()
            },
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("refusing to restore absolute path")
        );
    }

    #[test]
    fn restores_encrypted_file_with_identity() {
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
        fs::write(
            repo.path().join("recipients.txt"),
            identity.to_public().to_string(),
        )
        .unwrap();
        let identity_path = repo.path().join("identity.txt");
        fs::write(&identity_path, identity.to_string().expose_secret()).unwrap();

        let mut config = Config::default();
        config.encryption.recipients_file = Some("recipients.txt".to_string());
        config.encryption.identity = Some(identity_path.to_string_lossy().into_owned());
        config.paths.push(PathConfig {
            src: "~/.config/app/token.json".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: true,
        });
        fs::write(
            repo.path().join("dotr.toml"),
            toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(home.join(".config/app/token.json")).unwrap();

        run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                apply: true,
                targets: vec!["~/.config/app/token.json".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(home.join(".config/app/token.json")).unwrap(),
            "secret-token"
        );
    }

    #[test]
    fn output_restores_encrypted_file_without_apply() {
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
        fs::write(
            repo.path().join("recipients"),
            identity.to_public().to_string(),
        )
        .unwrap();
        let identity_path = repo.path().join("identity");
        fs::write(&identity_path, identity.to_string().expose_secret()).unwrap();

        let mut config = Config::default();
        config.encryption.recipients_file = Some("recipients".to_string());
        config.encryption.identity = Some(identity_path.to_string_lossy().into_owned());
        config.paths.push(PathConfig {
            src: "~/.config/app/token.json".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: true,
        });
        fs::write(
            repo.path().join("dotr.toml"),
            toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(home.join(".config/app/token.json")).unwrap();
        let output = home.join("preview-token");

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                output: Some(output.clone()),
                targets: vec!["~/.config/app/token.json".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&output).unwrap(), "secret-token");
        assert!(!home.join(".config/app/token.json").exists());
        assert_eq!(report.restored, 1);
    }

    #[test]
    fn output_dry_run_does_not_write() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/config.toml"), "value").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/app/config.toml".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        let output = home.join("preview-config");

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                dry_run: true,
                output: Some(output.clone()),
                targets: vec!["~/.config/app/config.toml".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert!(!output.exists());
        assert_eq!(report.planned, 1);
    }

    #[test]
    fn output_requires_exactly_one_file_target() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/one.toml"), "one").unwrap();
        fs::write(home.join(".config/app/two.toml"), "two").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/app".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();

        let err = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                output: Some(home.join("preview")),
                targets: vec!["~/.config/app".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("exactly one file target"));
    }

    #[test]
    fn diff_reports_planned_file_changes_without_writing() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/config.toml"), "new = true\n").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/app/config.toml".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::write(home.join(".config/app/config.toml"), "new = false\n").unwrap();

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                diff: true,
                targets: vec!["~/.config/app/config.toml".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(home.join(".config/app/config.toml")).unwrap(),
            "new = false\n"
        );
        assert_eq!(report.planned, 1);
        let diff = report.diffs.join("\n");
        assert!(diff.contains("-new = false"));
        assert!(diff.contains("+new = true"));
    }

    #[test]
    fn encrypted_restore_rejects_invalid_identity_file() {
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
        fs::write(
            repo.path().join("recipients.txt"),
            identity.to_public().to_string(),
        )
        .unwrap();
        let identity_path = repo.path().join("identity.txt");
        fs::write(&identity_path, identity.to_string().expose_secret()).unwrap();

        let mut config = Config::default();
        config.encryption.recipients_file = Some("recipients.txt".to_string());
        config.encryption.identity = Some(identity_path.to_string_lossy().into_owned());
        config.paths.push(PathConfig {
            src: "~/.config/app/token.json".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: true,
        });
        fs::write(
            repo.path().join("dotr.toml"),
            toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(home.join(".config/app/token.json")).unwrap();
        fs::write(&identity_path, "not-an-age-identity").unwrap();

        let err = run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                apply: true,
                targets: vec!["~/.config/app/token.json".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("invalid age identity"));
    }

    #[cfg(unix)]
    #[test]
    fn restores_symlink_itself() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("links")).unwrap();
        unix_fs::symlink("/tmp/target", home.join("links/current")).unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/links".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: false,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = prepare_repo(home, &config);
        let env = Environment::new(home.to_path_buf()).unwrap();
        backup::run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &crate::git::CommandGit,
        )
        .unwrap();
        fs::remove_file(home.join("links/current")).unwrap();

        run_with_config(
            repo.path(),
            &env,
            &config,
            &RestoreOptions {
                apply: true,
                targets: vec!["~/links/current".to_string()],
                ..RestoreOptions::default()
            },
        )
        .unwrap();

        let target = fs::read_link(home.join("links/current")).unwrap();
        assert_eq!(target, PathBuf::from("/tmp/target"));
    }
}

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{
    backup::{self, BackupOptions, BackupReport},
    config::{Config, PathConfig},
    encryption,
    environment::Environment,
    git::{CommandGit, GitBackend},
    paths::absolutize,
    progress::{BackupProgress, NoopProgress},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOptions {
    pub path: PathBuf,
    pub encrypt: bool,
    pub force: bool,
    pub no_git: bool,
    pub commit: bool,
    pub push: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveOptions {
    pub path: PathBuf,
    pub no_git: bool,
    pub commit: bool,
    pub push: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManageReport {
    pub source: String,
    pub config_changed: bool,
    pub backup: BackupReport,
}

pub fn add(
    repo_root: &Path,
    cwd: &Path,
    env: &Environment,
    options: &AddOptions,
) -> Result<ManageReport> {
    let mut progress = NoopProgress;
    add_with_progress(repo_root, cwd, env, options, &mut progress)
}

pub fn add_with_progress(
    repo_root: &Path,
    cwd: &Path,
    env: &Environment,
    options: &AddOptions,
    progress: &mut impl BackupProgress,
) -> Result<ManageReport> {
    let source = resolve_cli_path(&options.path, cwd, env);
    if !source.exists() {
        bail!("path does not exist: {}", source.display());
    }

    let mut config = Config::load(repo_root)?;
    if options.encrypt {
        ensure_encryption_ready(repo_root, &config)?;
    }
    let config_changed = ensure_path_config(
        &mut config,
        repo_root,
        env,
        &source,
        options.encrypt,
        options.force,
    );
    let validation = validate_add_would_backup(repo_root, env, &config, &source)?;
    if !backup_stored_anything(&validation) {
        bail!(
            "{}",
            add_failed_message(&validation, env, &source, options.force)
        );
    }

    if config_changed {
        config.write(repo_root)?;
    }

    let backup = run_backup(
        repo_root,
        env,
        &config,
        &ScopedBackupOptions {
            scope: &source,
            no_git: true,
            commit: false,
            push: false,
        },
        progress,
    )?;
    if !backup_stored_anything(&backup) {
        bail!(
            "{}",
            add_failed_message(&backup, env, &source, options.force)
        );
    }
    finish_git(
        repo_root,
        &config,
        &backup,
        options.no_git,
        options.commit,
        options.push,
        progress,
    )?;
    Ok(ManageReport {
        source: env.display_source(&source),
        config_changed,
        backup,
    })
}

pub fn remove(
    repo_root: &Path,
    cwd: &Path,
    env: &Environment,
    options: &RemoveOptions,
) -> Result<ManageReport> {
    let mut progress = NoopProgress;
    remove_with_progress(repo_root, cwd, env, options, &mut progress)
}

pub fn remove_with_progress(
    repo_root: &Path,
    cwd: &Path,
    env: &Environment,
    options: &RemoveOptions,
    progress: &mut impl BackupProgress,
) -> Result<ManageReport> {
    let source = resolve_cli_path(&options.path, cwd, env);
    let mut config = Config::load(repo_root)?;
    let removed = remove_path_config(&mut config, repo_root, env, &source);
    if !removed {
        bail!("path is not configured: {}", env.display_source(&source));
    }

    config.write(repo_root)?;
    let backup = run_backup(
        repo_root,
        env,
        &config,
        &ScopedBackupOptions {
            scope: &source,
            no_git: options.no_git,
            commit: options.commit,
            push: options.push,
        },
        progress,
    )?;
    Ok(ManageReport {
        source: env.display_source(&source),
        config_changed: true,
        backup,
    })
}

fn ensure_path_config(
    config: &mut Config,
    repo_root: &Path,
    env: &Environment,
    source: &Path,
    encrypt: bool,
    force: bool,
) -> bool {
    for path in &mut config.paths {
        if config_path_matches(path, repo_root, env, source) {
            let mut changed = false;
            if force && !path.force {
                path.force = true;
                changed = true;
            }
            if encrypt && !path.encrypt {
                path.encrypt = true;
                changed = true;
            }
            return changed;
        }
    }

    if config.path_sets.iter().any(|set| {
        set.expand()
            .iter()
            .any(|path| config_path_matches(path, repo_root, env, source))
    }) {
        if !force && !encrypt {
            return false;
        }

        for set in &mut config.path_sets {
            set.remove_matching(repo_root, env, source);
        }
        config.path_sets.retain(|set| !set.items.is_empty());
    }

    config.paths.push(PathConfig {
        src: env.display_source(source),
        include: Vec::new(),
        exclude: Vec::new(),
        follow_symlink: true,
        include_binary_file: false,
        force,
        encrypt,
    });
    true
}

fn ensure_encryption_ready(repo_root: &Path, config: &Config) -> Result<()> {
    if config.encryption.backend != "age" {
        bail!(
            "dotr add --encrypt only supports age encryption; set [encryption]\nbackend = \"age\""
        );
    }

    let Some(recipients_file) = config.encryption.recipients_file.as_deref() else {
        bail!(
            "dotr add --encrypt requires encryption.recipients_file.\n\
Run `dotr keygen` in your dotr repository to generate key material and write:\n\n\
[encryption]\n\
backend = \"age\"\n\
recipients_file = \"recipients\"\n\
identity = \"~/.config/dotr/identity\""
        );
    };

    let recipients_path = encryption::resolve_recipients_file(repo_root, recipients_file);
    if !recipients_path.is_file() {
        let identity = config
            .encryption
            .identity
            .as_deref()
            .unwrap_or("~/.config/dotr/identity");
        bail!(
            "dotr add --encrypt requires age recipients file: {}\n\
Run `dotr keygen` in your dotr repository to create it.\n\
If you intentionally keep the configured identity, recreate recipients with:\n\
  age-keygen -y {} > {}",
            recipients_path.display(),
            identity,
            recipients_path.display()
        );
    }

    let recipients = encryption::load_recipients(&recipients_path).with_context(|| {
        format!(
            "failed to load age recipients from {} for dotr add --encrypt",
            recipients_path.display()
        )
    })?;
    if recipients.is_empty() {
        bail!(
            "dotr add --encrypt requires at least one age recipient in {}",
            recipients_path.display()
        );
    }

    Ok(())
}

fn remove_path_config(
    config: &mut Config,
    repo_root: &Path,
    env: &Environment,
    source: &Path,
) -> bool {
    let before = config.paths.len();
    config
        .paths
        .retain(|path| !config_path_matches(path, repo_root, env, source));
    let removed_from_paths = before != config.paths.len();
    let mut removed_from_sets = false;
    for set in &mut config.path_sets {
        removed_from_sets |= set.remove_matching(repo_root, env, source);
    }
    config.path_sets.retain(|set| !set.items.is_empty());
    removed_from_paths || removed_from_sets
}

fn config_path_matches(
    path_config: &PathConfig,
    repo_root: &Path,
    env: &Environment,
    source: &Path,
) -> bool {
    let configured = absolutize(&env.expand_tilde(&path_config.src), repo_root);
    normalize_path(&configured) == normalize_path(source)
}

struct ScopedBackupOptions<'a> {
    scope: &'a Path,
    no_git: bool,
    commit: bool,
    push: bool,
}

fn run_backup(
    repo_root: &Path,
    env: &Environment,
    config: &Config,
    options: &ScopedBackupOptions<'_>,
    progress: &mut impl BackupProgress,
) -> Result<BackupReport> {
    backup::run_with_config_and_progress(
        repo_root,
        env,
        config,
        &BackupOptions {
            dry_run: false,
            no_delete: false,
            no_git: options.no_git,
            commit: options.commit,
            push: options.push,
            scopes: vec![options.scope.to_path_buf()],
        },
        &CommandGit,
        progress,
    )
}

fn validate_add_would_backup(
    repo_root: &Path,
    env: &Environment,
    config: &Config,
    source: &Path,
) -> Result<BackupReport> {
    let mut progress = NoopProgress;
    backup::run_with_config_and_progress(
        repo_root,
        env,
        config,
        &BackupOptions {
            dry_run: true,
            no_delete: true,
            no_git: true,
            commit: false,
            push: false,
            scopes: vec![source.to_path_buf()],
        },
        &CommandGit,
        &mut progress,
    )
}

fn finish_git(
    repo_root: &Path,
    config: &Config,
    backup: &BackupReport,
    no_git: bool,
    commit: bool,
    push: bool,
    progress: &mut impl BackupProgress,
) -> Result<()> {
    if no_git {
        return Ok(());
    }

    let wants_commit = commit || config.git.auto_commit || push || config.git.auto_push;
    if wants_commit {
        let message = format!(
            "{} ({})",
            config.git.commit_message,
            backup::change_summary(backup)
        );
        progress.phase("committing git changes");
        CommandGit.commit_backup(repo_root, &message, config.git.include_unrelated)?;
    }

    if push || config.git.auto_push {
        progress.phase("pushing git changes");
        CommandGit.push(repo_root)?;
    }

    Ok(())
}

fn backup_stored_anything(backup: &BackupReport) -> bool {
    backup.added + backup.updated + backup.unchanged > 0
}

fn add_failed_message(
    backup: &BackupReport,
    env: &Environment,
    source: &Path,
    force: bool,
) -> String {
    let source_display = env.display_source(source);
    let reason = backup
        .actions
        .iter()
        .find(|action| action.starts_with("skip "))
        .map(|action| format!(" Reason: {action}."))
        .unwrap_or_default();

    let hint = if force {
        " Check the skip reason above and adjust explicit include/exclude rules or file permissions."
            .to_string()
    } else {
        format!(
            " If you intentionally want to back it up, run `dotr add --force {}`.",
            shell_arg(&source_display)
        )
    };

    format!(
        "path was not added to dotr.toml because no files would be backed up for {source_display}.{reason}{hint}"
    )
}

fn shell_arg(value: &str) -> String {
    if value == "~" {
        return value.to_string();
    }

    if let Some(rest) = value.strip_prefix("~/") {
        return if is_shell_safe(rest) {
            value.to_string()
        } else {
            format!("~/{}", single_quote(rest))
        };
    }

    if is_shell_safe(value) {
        value.to_string()
    } else {
        single_quote(value)
    }
}

fn is_shell_safe(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            matches!(
                byte,
                b'A'..=b'Z'
                    | b'a'..=b'z'
                    | b'0'..=b'9'
                    | b'/'
                    | b'.'
                    | b'_'
                    | b'-'
                    | b':'
                    | b','
                    | b'+'
                    | b'='
                    | b'@'
                    | b'%'
            )
        })
}

fn single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn resolve_cli_path(raw: &Path, cwd: &Path, env: &Environment) -> PathBuf {
    let expanded = env.expand_tilde(&raw.to_string_lossy());
    normalize_path(&absolutize(&expanded, cwd))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::{
        backup,
        config::{Config, PathSetConfig, PathSetItem, path_config},
        init,
    };

    fn env_for(home: &Path) -> Environment {
        Environment::new(home.to_path_buf()).unwrap()
    }

    fn prepare_repo() -> tempfile::TempDir {
        let repo = tempdir().unwrap();
        init::run(&init::InitOptions {
            target: repo.path().to_path_buf(),
            with_defaults: false,
            no_git: true,
            force: false,
        })
        .unwrap();
        repo
    }

    #[test]
    fn shell_arg_preserves_home_expansion() {
        assert_eq!(shell_arg("~/logs/app.log"), "~/logs/app.log");
        assert_eq!(
            shell_arg("~/Library/Application Support/Code"),
            "~/'Library/Application Support/Code'"
        );
        assert_eq!(
            shell_arg("/tmp/dotr demo/app.log"),
            "'/tmp/dotr demo/app.log'"
        );
    }

    #[test]
    fn add_writes_config_and_runs_backup() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/config.toml"), "ok").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);

        let report = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.config/app/config.toml"),
                encrypt: false,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert!(report.config_changed);
        assert_eq!(report.source, "~/.config/app/config.toml");
        assert!(report.backup.added >= 1);
        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/.config/app/config.toml")).unwrap(),
            "ok"
        );

        let config = Config::load(repo.path()).unwrap();
        assert_eq!(config.paths.len(), 1);
        assert_eq!(config.paths[0].src, "~/.config/app/config.toml");
    }

    #[test]
    fn add_accepts_directory_paths() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/config.toml"), "ok").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);

        let report = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.config/app"),
                encrypt: false,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert!(report.config_changed);
        assert_eq!(report.source, "~/.config/app");
        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/.config/app/config.toml")).unwrap(),
            "ok"
        );
    }

    #[test]
    fn add_runs_backup_only_for_added_path() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/existing")).unwrap();
        fs::create_dir_all(home.join(".config/new")).unwrap();
        fs::write(home.join(".config/existing/config.toml"), "existing").unwrap();
        fs::write(home.join(".config/new/config.toml"), "new").unwrap();
        let repo = prepare_repo();
        let mut config = Config::load(repo.path()).unwrap();
        config
            .paths
            .push(path_config("~/.config/existing/config.toml"));
        config.write(repo.path()).unwrap();
        let env = env_for(home);

        let report = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.config/new/config.toml"),
                encrypt: false,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert_eq!(report.backup.visited, 1);
        assert!(
            repo.path()
                .join("files/home/.config/new/config.toml")
                .is_file()
        );
        assert!(
            !repo
                .path()
                .join("files/home/.config/existing/config.toml")
                .exists()
        );
    }

    #[test]
    fn add_force_updates_existing_path_and_backs_up_default_excluded_file() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("logs")).unwrap();
        fs::write(home.join("logs/app.log"), "important log").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);
        let options = AddOptions {
            path: PathBuf::from("~/logs/app.log"),
            encrypt: false,
            force: false,
            no_git: true,
            commit: false,
            push: false,
        };

        let err = add(repo.path(), home, &env, &options).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("path was not added to dotr.toml"));
        assert!(message.contains("skip excluded"));
        assert!(message.contains("dotr add --force ~/logs/app.log"));
        assert!(Config::load(repo.path()).unwrap().paths.is_empty());
        assert!(!repo.path().join("files/home/logs/app.log").exists());

        let forced = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                force: true,
                ..options
            },
        )
        .unwrap();

        assert!(forced.config_changed);
        assert_eq!(forced.backup.visited, 1);
        assert!(repo.path().join("files/home/logs/app.log").is_file());

        let config = Config::load(repo.path()).unwrap();
        assert_eq!(config.paths.len(), 1);
        assert_eq!(config.paths[0].src, "~/logs/app.log");
        assert!(config.paths[0].force);
    }

    #[test]
    fn add_can_mark_path_encrypted() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/token"), "secret-token").unwrap();
        let repo = prepare_repo();
        let mut config = Config::load(repo.path()).unwrap();
        config.encryption.recipients_file = Some("recipients".to_string());
        config.write(repo.path()).unwrap();
        let identity = age::x25519::Identity::generate();
        fs::write(
            repo.path().join("recipients"),
            identity.to_public().to_string(),
        )
        .unwrap();
        let env = env_for(home);

        let report = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.config/app/token"),
                encrypt: true,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert!(report.config_changed);
        assert!(
            repo.path()
                .join("files/home/.config/app/token.age")
                .is_file()
        );
        assert!(!repo.path().join("files/home/.config/app/token").exists());

        let config = Config::load(repo.path()).unwrap();
        assert_eq!(config.paths.len(), 1);
        assert_eq!(config.paths[0].src, "~/.config/app/token");
        assert!(config.paths[0].encrypt);
    }

    #[test]
    fn add_encrypt_requires_encryption_config() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".npmrc"), "token").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);

        let err = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.npmrc"),
                encrypt: true,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("dotr add --encrypt requires encryption.recipients_file"));
        assert!(message.contains("Run `dotr keygen`"));
        assert!(message.contains("recipients_file = \"recipients\""));
        assert!(Config::load(repo.path()).unwrap().paths.is_empty());
    }

    #[test]
    fn add_encrypt_reports_missing_recipients_file() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".npmrc"), "token").unwrap();
        let repo = prepare_repo();
        let mut config = Config::load(repo.path()).unwrap();
        config.encryption.recipients_file = Some("recipients".to_string());
        config.encryption.identity = Some("~/.config/dotr/identity".to_string());
        config.write(repo.path()).unwrap();
        let env = env_for(home);

        let err = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.npmrc"),
                encrypt: true,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("requires age recipients file"));
        assert!(message.contains("Run `dotr keygen`"));
        assert!(Config::load(repo.path()).unwrap().paths.is_empty());
    }

    #[test]
    fn add_existing_path_does_not_duplicate_config() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".zshrc"), "zsh").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);
        let options = AddOptions {
            path: PathBuf::from("~/.zshrc"),
            encrypt: false,
            force: false,
            no_git: true,
            commit: false,
            push: false,
        };

        add(repo.path(), home, &env, &options).unwrap();
        let report = add(repo.path(), home, &env, &options).unwrap();

        assert!(!report.config_changed);
        let config = Config::load(repo.path()).unwrap();
        assert_eq!(config.paths.len(), 1);
    }

    #[test]
    fn add_existing_path_from_path_set_does_not_duplicate_config() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".zshrc"), "zsh").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);
        let mut config = Config::load(repo.path()).unwrap();
        config.path_sets.push(PathSetConfig {
            base: Some("~".to_string()),
            items: vec![PathSetItem::Src(".zshrc".to_string())],
        });
        config.write(repo.path()).unwrap();

        let report = add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.zshrc"),
                encrypt: false,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert!(!report.config_changed);
        let config = Config::load(repo.path()).unwrap();
        assert!(config.paths.is_empty());
        assert_eq!(config.path_sets[0].items.len(), 1);
    }

    #[test]
    fn remove_deletes_config_and_backup_files() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".zshrc"), "zsh").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);

        add(
            repo.path(),
            home,
            &env,
            &AddOptions {
                path: PathBuf::from("~/.zshrc"),
                encrypt: false,
                force: false,
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();
        assert!(repo.path().join("files/home/.zshrc").exists());

        let report = remove(
            repo.path(),
            home,
            &env,
            &RemoveOptions {
                path: PathBuf::from("~/.zshrc"),
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert!(report.config_changed);
        assert!(report.backup.deleted >= 1);
        assert!(!repo.path().join("files/home/.zshrc").exists());
        assert!(Config::load(repo.path()).unwrap().paths.is_empty());
    }

    #[test]
    fn remove_runs_backup_only_for_removed_path() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".zshrc"), "zsh").unwrap();
        fs::write(home.join(".gitconfig"), "git").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);
        let mut config = Config::load(repo.path()).unwrap();
        config.paths.push(path_config("~/.zshrc"));
        config.paths.push(path_config("~/.gitconfig"));
        config.write(repo.path()).unwrap();
        backup::run(
            repo.path(),
            &env,
            &backup::BackupOptions {
                no_git: true,
                ..backup::BackupOptions::default()
            },
        )
        .unwrap();
        assert!(repo.path().join("files/home/.zshrc").is_file());
        assert!(repo.path().join("files/home/.gitconfig").is_file());

        let report = remove(
            repo.path(),
            home,
            &env,
            &RemoveOptions {
                path: PathBuf::from("~/.zshrc"),
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert_eq!(report.backup.visited, 0);
        assert_eq!(report.backup.unchanged, 0);
        assert_eq!(report.backup.deleted, 1);
        assert!(!repo.path().join("files/home/.zshrc").exists());
        assert!(repo.path().join("files/home/.gitconfig").is_file());
    }

    #[test]
    fn remove_deletes_path_set_item() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".zshrc"), "zsh").unwrap();
        fs::write(home.join(".gitconfig"), "git").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);
        let mut config = Config::load(repo.path()).unwrap();
        config.path_sets.push(PathSetConfig {
            base: Some("~".to_string()),
            items: vec![
                PathSetItem::Src(".zshrc".to_string()),
                PathSetItem::Src(".gitconfig".to_string()),
            ],
        });
        config.write(repo.path()).unwrap();

        let report = remove(
            repo.path(),
            home,
            &env,
            &RemoveOptions {
                path: PathBuf::from("~/.zshrc"),
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap();

        assert!(report.config_changed);
        let config = Config::load(repo.path()).unwrap();
        assert_eq!(config.path_sets[0].items.len(), 1);
        assert_eq!(config.path_configs()[0].src, "~/.gitconfig");
    }

    #[test]
    fn remove_unconfigured_path_fails() {
        let home_dir = tempdir().unwrap();
        let repo = prepare_repo();
        let env = env_for(home_dir.path());

        let err = remove(
            repo.path(),
            home_dir.path(),
            &env,
            &RemoveOptions {
                path: PathBuf::from("~/.missing"),
                no_git: true,
                commit: false,
                push: false,
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("path is not configured"));
    }
}

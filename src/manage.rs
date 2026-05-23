use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

use crate::{
    backup::{self, BackupOptions, BackupReport},
    config::{Config, PathConfig},
    environment::Environment,
    paths::absolutize,
    progress::{BackupProgress, NoopProgress},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOptions {
    pub path: PathBuf,
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
    let config_changed = ensure_path_config(&mut config, repo_root, env, &source);
    if config_changed {
        config.write(repo_root)?;
    }

    let backup = run_backup(
        repo_root,
        env,
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
        options.no_git,
        options.commit,
        options.push,
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
) -> bool {
    if config
        .paths
        .iter()
        .any(|path| config_path_matches(path, repo_root, env, source))
    {
        return false;
    }

    config.paths.push(PathConfig {
        src: env.display_source(source),
        include: Vec::new(),
        exclude: Vec::new(),
        follow_symlink: true,
        include_binary_file: false,
        encrypt: false,
    });
    true
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
    before != config.paths.len()
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

fn run_backup(
    repo_root: &Path,
    env: &Environment,
    no_git: bool,
    commit: bool,
    push: bool,
    progress: &mut impl BackupProgress,
) -> Result<BackupReport> {
    backup::run_with_progress(
        repo_root,
        env,
        &BackupOptions {
            dry_run: false,
            no_delete: false,
            no_git,
            commit,
            push,
        },
        progress,
    )
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
    use crate::{config::Config, init};

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
    fn add_existing_path_does_not_duplicate_config() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::write(home.join(".zshrc"), "zsh").unwrap();
        let repo = prepare_repo();
        let env = env_for(home);
        let options = AddOptions {
            path: PathBuf::from("~/.zshrc"),
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

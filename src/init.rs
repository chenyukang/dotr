use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{
    config::{CONFIG_FILE_NAME, Config, DEFAULT_STORE_DIR, config_path},
    git::{CommandGit, GitBackend, is_git_repo},
    index::Index,
};

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub target: PathBuf,
    pub with_defaults: bool,
    pub no_git: bool,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub repo_root: PathBuf,
    pub created_config: bool,
    pub initialized_git: bool,
}

impl InitOptions {
    pub fn new(target: PathBuf) -> Self {
        Self {
            target,
            with_defaults: false,
            no_git: false,
            force: false,
        }
    }
}

pub fn run(options: &InitOptions) -> Result<InitReport> {
    run_with_git(options, &CommandGit)
}

pub fn run_with_git(options: &InitOptions, git: &impl GitBackend) -> Result<InitReport> {
    let repo_root = absolute_target(&options.target)?;
    fs::create_dir_all(&repo_root)
        .with_context(|| format!("failed to create {}", repo_root.display()))?;

    let mut initialized_git = false;
    if !options.no_git && !is_git_repo(&repo_root) {
        git.init(&repo_root)?;
        initialized_git = true;
    }

    let store_dir = repo_root.join(DEFAULT_STORE_DIR);
    fs::create_dir_all(store_dir.join("files/home"))?;
    fs::create_dir_all(store_dir.join("files/absolute"))?;
    fs::create_dir_all(store_dir.join("metadata"))?;

    let cfg_path = config_path(&repo_root);
    let created_config = options.force || !cfg_path.exists();
    if created_config {
        let config = Config::starter(options.with_defaults);
        let toml = toml::to_string_pretty(&config).context("failed to serialize starter config")?;
        fs::write(&cfg_path, toml)
            .with_context(|| format!("failed to write {}", cfg_path.display()))?;
    }

    let index_path = store_dir.join("metadata/index.json");
    if options.force || !index_path.exists() {
        Index::default().write(&index_path)?;
    }

    ensure_gitignore(&repo_root)?;

    Ok(InitReport {
        repo_root,
        created_config,
        initialized_git,
    })
}

fn absolute_target(target: &Path) -> Result<PathBuf> {
    if target.is_absolute() {
        Ok(target.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(target))
    }
}

fn ensure_gitignore(repo_root: &Path) -> Result<()> {
    let path = repo_root.join(".gitignore");
    let additions = [
        "# dotr runtime files",
        "backup/*.lock",
        "backup/*.tmp",
        "backup/*.log",
    ];

    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut next = existing.clone();
    for line in additions {
        if !existing.lines().any(|existing_line| existing_line == line) {
            if !next.ends_with('\n') && !next.is_empty() {
                next.push('\n');
            }
            next.push_str(line);
            next.push('\n');
        }
    }

    if next != existing {
        fs::write(&path, next).with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(())
}

#[allow(dead_code)]
fn config_file_name() -> &'static str {
    CONFIG_FILE_NAME
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_creates_dotr_layout_without_git() {
        let dir = tempdir().unwrap();
        let options = InitOptions {
            target: dir.path().join("repo"),
            with_defaults: true,
            no_git: true,
            force: false,
        };

        let report = run(&options).unwrap();

        assert!(!report.initialized_git);
        assert!(report.repo_root.join("backup/dotr.toml").exists());
        assert!(report.repo_root.join("backup/files/home").is_dir());
        assert!(report.repo_root.join("backup/files/absolute").is_dir());
        assert!(report.repo_root.join("backup/metadata/index.json").exists());

        let config = Config::load(&report.repo_root).unwrap();
        assert_eq!(config.paths.len(), 4);
    }
}

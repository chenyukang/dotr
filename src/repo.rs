use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{config::config_path, environment::Environment};

pub const DOTR_REPO_ENV: &str = "DOTR_REPO";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoResolution {
    pub root: PathBuf,
    pub source: RepoSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoSource {
    Explicit,
    Environment,
    Ancestor,
    UserConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_repo: Option<String>,
}

pub fn resolve_repo(
    explicit: Option<&Path>,
    cwd: &Path,
    env: &Environment,
) -> Result<RepoResolution> {
    let env_repo = std::env::var_os(DOTR_REPO_ENV).map(PathBuf::from);
    resolve_repo_with_env(explicit, cwd, env, env_repo.as_deref())
}

pub fn resolve_repo_with_env(
    explicit: Option<&Path>,
    cwd: &Path,
    env: &Environment,
    env_repo: Option<&Path>,
) -> Result<RepoResolution> {
    if let Some(path) = explicit {
        return Ok(RepoResolution {
            root: resolve_input_path(cwd, env, path),
            source: RepoSource::Explicit,
        });
    }

    if let Some(path) = env_repo {
        return Ok(RepoResolution {
            root: resolve_input_path(cwd, env, path),
            source: RepoSource::Environment,
        });
    }

    if let Some(root) = find_ancestor_repo(cwd) {
        return Ok(RepoResolution {
            root,
            source: RepoSource::Ancestor,
        });
    }

    let user_config = load_user_config(env)?;
    if let Some(default_repo) = user_config.default_repo.as_deref() {
        return Ok(RepoResolution {
            root: resolve_input_str(cwd, env, default_repo),
            source: RepoSource::UserConfig,
        });
    }

    bail!(
        "could not find a dotr repository; pass --repo, set DOTR_REPO, run inside a repo, or run `dotr init <path> --set-default`"
    )
}

pub fn set_default_repo(env: &Environment, cwd: &Path, raw_repo: &str) -> Result<PathBuf> {
    let repo_root = resolve_input_str(cwd, env, raw_repo);
    let mut config = load_user_config(env)?;
    config.default_repo = Some(repo_root.to_string_lossy().into_owned());
    write_user_config(env, &config)?;
    Ok(repo_root)
}

pub fn load_user_config(env: &Environment) -> Result<UserConfig> {
    let path = user_config_path(env);
    if !path.exists() {
        return Ok(UserConfig::default());
    }

    let raw =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn write_user_config(env: &Environment, config: &UserConfig) -> Result<()> {
    let path = user_config_path(env);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let toml = toml::to_string_pretty(config).context("failed to serialize dotr user config")?;
    fs::write(&path, toml).with_context(|| format!("failed to write {}", path.display()))
}

pub fn user_config_path(env: &Environment) -> PathBuf {
    env.home().join(".config/dotr/config.toml")
}

fn find_ancestor_repo(cwd: &Path) -> Option<PathBuf> {
    let mut cursor = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(cwd)
    };

    loop {
        if config_path(&cursor).is_file() {
            return Some(cursor);
        }

        if !cursor.pop() {
            return None;
        }
    }
}

fn resolve_input_str(cwd: &Path, env: &Environment, raw: &str) -> PathBuf {
    resolve_input_path(cwd, env, Path::new(raw))
}

fn resolve_input_path(cwd: &Path, env: &Environment, raw: &Path) -> PathBuf {
    let expanded = env.expand_tilde(&raw.to_string_lossy());
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn env_for(home: &Path) -> Environment {
        Environment::new(home.to_path_buf()).unwrap()
    }

    #[test]
    fn explicit_repo_wins_over_other_sources() {
        let home = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        fs::write(cwd.path().join("dotr.toml"), "").unwrap();
        let env = env_for(home.path());

        let resolved = resolve_repo_with_env(
            Some(Path::new("~/explicit")),
            cwd.path(),
            &env,
            Some(Path::new("/tmp/from-env")),
        )
        .unwrap();

        assert_eq!(resolved.source, RepoSource::Explicit);
        assert_eq!(resolved.root, home.path().join("explicit"));
    }

    #[test]
    fn environment_repo_wins_over_ancestor() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        fs::write(repo.path().join("dotr.toml"), "").unwrap();
        let env = env_for(home.path());

        let resolved =
            resolve_repo_with_env(None, repo.path(), &env, Some(Path::new("~/from-env"))).unwrap();

        assert_eq!(resolved.source, RepoSource::Environment);
        assert_eq!(resolved.root, home.path().join("from-env"));
    }

    #[test]
    fn finds_repo_by_walking_up_from_current_directory() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let nested = repo.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(repo.path().join("dotr.toml"), "").unwrap();
        let env = env_for(home.path());

        let resolved = resolve_repo_with_env(None, &nested, &env, None).unwrap();

        assert_eq!(resolved.source, RepoSource::Ancestor);
        assert_eq!(resolved.root, repo.path());
    }

    #[test]
    fn falls_back_to_user_default_repo() {
        let home = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        let env = env_for(home.path());
        write_user_config(
            &env,
            &UserConfig {
                default_repo: Some("~/dotbackup".to_string()),
            },
        )
        .unwrap();

        let resolved = resolve_repo_with_env(None, cwd.path(), &env, None).unwrap();

        assert_eq!(resolved.source, RepoSource::UserConfig);
        assert_eq!(resolved.root, home.path().join("dotbackup"));
    }

    #[test]
    fn set_default_repo_writes_user_config() {
        let home = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        let env = env_for(home.path());

        let repo = set_default_repo(&env, cwd.path(), "relative-repo").unwrap();
        let config = load_user_config(&env).unwrap();

        assert_eq!(repo, cwd.path().join("relative-repo"));
        assert_eq!(
            config.default_repo.as_deref(),
            Some(cwd.path().join("relative-repo").to_string_lossy().as_ref())
        );
    }
}

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, bail};

use crate::{
    config::{Config, config_path},
    environment::Environment,
};

pub const DEFAULT_IDENTITY: &str = "~/.config/dotr/identity";
pub const DEFAULT_RECIPIENTS_FILE: &str = "recipients";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeygenOptions {
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeygenReport {
    pub identity_path: PathBuf,
    pub recipients_path: PathBuf,
    pub config_path: PathBuf,
    pub config_updated: bool,
    pub overwritten: Vec<PathBuf>,
}

pub fn run(repo_root: &Path, env: &Environment, options: &KeygenOptions) -> Result<KeygenReport> {
    run_with_confirmation(repo_root, env, options, confirm_overwrite)
}

pub fn run_with_confirmation(
    repo_root: &Path,
    env: &Environment,
    options: &KeygenOptions,
    mut confirm: impl FnMut(&Path) -> Result<bool>,
) -> Result<KeygenReport> {
    let identity_path = env.expand_tilde(DEFAULT_IDENTITY);
    let recipients_path = repo_root.join(DEFAULT_RECIPIENTS_FILE);
    let config_file = config_path(repo_root);

    let overwritten = [identity_path.clone(), recipients_path.clone()]
        .into_iter()
        .filter(|path| path_exists(path))
        .collect::<Vec<_>>();

    if !options.force {
        for path in &overwritten {
            if !confirm(path)? {
                bail!(
                    "refusing to overwrite {}; rerun with --force to overwrite without prompting",
                    path.display()
                );
            }
        }
    }

    let mut config = Config::load(repo_root)?;
    let previous_encryption = config.encryption.clone();
    config.encryption.backend = "age".to_string();
    config.encryption.recipients_file = Some(DEFAULT_RECIPIENTS_FILE.to_string());
    config.encryption.identity = Some(DEFAULT_IDENTITY.to_string());
    let config_updated = config.encryption != previous_encryption;

    if let Some(parent) = identity_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let identity = age::x25519::Identity::generate();
    let recipient = identity.to_public();
    fs::write(
        &identity_path,
        format!("{}\n", identity.to_string().expose_secret()),
    )
    .with_context(|| format!("failed to write {}", identity_path.display()))?;
    lock_down_identity_permissions(&identity_path)?;

    fs::write(&recipients_path, format!("{recipient}\n"))
        .with_context(|| format!("failed to write {}", recipients_path.display()))?;

    if config_updated {
        config.write(repo_root)?;
    }

    Ok(KeygenReport {
        identity_path,
        recipients_path,
        config_path: config_file,
        config_updated,
        overwritten,
    })
}

fn confirm_overwrite(path: &Path) -> Result<bool> {
    eprint!("overwrite {}? [y/N] ", path.display());
    io::stderr().flush().context("failed to flush prompt")?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("failed to read confirmation")?;
    Ok(answer.trim() == "y")
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn lock_down_identity_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to chmod 600 {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;
    use crate::{encryption, init};

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
    fn keygen_creates_files_and_updates_config() {
        let home_dir = tempdir().unwrap();
        let repo = prepare_repo();
        let env = env_for(home_dir.path());

        let report =
            run_with_confirmation(repo.path(), &env, &KeygenOptions { force: false }, |_| {
                panic!("should not prompt when files are absent")
            })
            .unwrap();

        assert_eq!(
            report.identity_path,
            home_dir.path().join(".config/dotr/identity")
        );
        assert_eq!(report.recipients_path, repo.path().join("recipients"));
        assert!(report.config_updated);
        assert!(report.overwritten.is_empty());
        assert_eq!(
            encryption::load_identities(&report.identity_path)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            encryption::load_recipients(&report.recipients_path)
                .unwrap()
                .len(),
            1
        );

        let config = Config::load(repo.path()).unwrap();
        assert_eq!(config.encryption.backend, "age");
        assert_eq!(
            config.encryption.recipients_file.as_deref(),
            Some("recipients")
        );
        assert_eq!(
            config.encryption.identity.as_deref(),
            Some("~/.config/dotr/identity")
        );

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&report.identity_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn keygen_refuses_to_overwrite_without_confirmation() {
        let home_dir = tempdir().unwrap();
        let repo = prepare_repo();
        let env = env_for(home_dir.path());
        let identity_path = home_dir.path().join(".config/dotr/identity");
        fs::create_dir_all(identity_path.parent().unwrap()).unwrap();
        fs::write(&identity_path, "old identity").unwrap();
        fs::write(repo.path().join("recipients"), "old recipients").unwrap();

        let err = run_with_confirmation(repo.path(), &env, &KeygenOptions { force: false }, |_| {
            Ok(false)
        })
        .unwrap_err();

        assert!(err.to_string().contains("refusing to overwrite"));
        assert_eq!(fs::read_to_string(&identity_path).unwrap(), "old identity");
        assert_eq!(
            fs::read_to_string(repo.path().join("recipients")).unwrap(),
            "old recipients"
        );
        assert!(
            Config::load(repo.path())
                .unwrap()
                .encryption
                .identity
                .is_none()
        );
    }

    #[test]
    fn keygen_overwrites_after_confirmation() {
        let home_dir = tempdir().unwrap();
        let repo = prepare_repo();
        let env = env_for(home_dir.path());
        let identity_path = home_dir.path().join(".config/dotr/identity");
        fs::create_dir_all(identity_path.parent().unwrap()).unwrap();
        fs::write(&identity_path, "old identity").unwrap();
        fs::write(repo.path().join("recipients"), "old recipients").unwrap();
        let prompts = Cell::new(0);

        let report =
            run_with_confirmation(repo.path(), &env, &KeygenOptions { force: false }, |_| {
                prompts.set(prompts.get() + 1);
                Ok(true)
            })
            .unwrap();

        assert_eq!(prompts.get(), 2);
        assert_eq!(report.overwritten.len(), 2);
        assert_ne!(fs::read_to_string(&identity_path).unwrap(), "old identity");
        assert_ne!(
            fs::read_to_string(repo.path().join("recipients")).unwrap(),
            "old recipients"
        );
    }

    #[test]
    fn keygen_force_overwrites_without_prompting() {
        let home_dir = tempdir().unwrap();
        let repo = prepare_repo();
        let env = env_for(home_dir.path());
        let identity_path = home_dir.path().join(".config/dotr/identity");
        fs::create_dir_all(identity_path.parent().unwrap()).unwrap();
        fs::write(&identity_path, "old identity").unwrap();

        let report =
            run_with_confirmation(repo.path(), &env, &KeygenOptions { force: true }, |_| {
                panic!("--force should not prompt")
            })
            .unwrap();

        assert_eq!(report.overwritten, vec![identity_path.clone()]);
        assert_ne!(fs::read_to_string(&identity_path).unwrap(), "old identity");
        assert!(repo.path().join("recipients").is_file());
    }
}

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::{
    config::{Config, default_exclude_set, globset_from_patterns},
    encryption,
    environment::Environment,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorReport {
    pub warnings: Vec<String>,
}

pub fn run(repo_root: &Path, env: &Environment) -> Result<DoctorReport> {
    let config = Config::load(repo_root)?;
    let _ = default_exclude_set()?;
    let mut report = DoctorReport::default();

    let mut top_level_paths = config.paths.clone();
    top_level_paths.extend(config.path_sets.iter().flat_map(|set| set.expand()));
    for path in &top_level_paths {
        check_path(path, env, true, &mut report)?;
    }

    for custom in &config.custom_backups {
        let warn_missing = custom.backup_command.is_none();
        for path in &custom.path_configs() {
            check_path(path, env, warn_missing, &mut report)?;
        }
    }

    let store_dir = config.store_dir(repo_root);
    if !store_dir.is_dir() {
        bail!("backup store does not exist: {}", store_dir.display());
    }

    if config.has_encrypted_paths() {
        let recipients_file = config
            .encryption
            .recipients_file
            .as_deref()
            .context("encrypted paths require encryption.recipients_file")?;
        let recipients_path = encryption::resolve_recipients_file(repo_root, recipients_file);
        if !recipients_path.exists() {
            bail!(
                "age recipients file does not exist: {}",
                recipients_path.display()
            );
        }
    }

    if let Some(secret_path) = encryption::find_age_secret(repo_root)? {
        bail!(
            "age identity material must not be committed under repository: {}",
            secret_path.display()
        );
    }

    Ok(report)
}

fn check_path(
    path: &crate::config::PathConfig,
    env: &Environment,
    warn_missing: bool,
    report: &mut DoctorReport,
) -> Result<()> {
    let _ = globset_from_patterns(path.exclude.iter().map(String::as_str))?;
    let source = env.expand_tilde(&path.src);
    if warn_missing && !source.exists() {
        report
            .warnings
            .push(format!("source does not exist: {}", source.display()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::init;

    #[test]
    fn doctor_fails_when_repo_contains_age_secret_key() {
        let repo = tempdir().unwrap();
        init::run(&init::InitOptions {
            target: repo.path().to_path_buf(),
            with_defaults: false,
            no_git: true,
            force: false,
        })
        .unwrap();
        fs::write(repo.path().join("identity.txt"), "AGE-SECRET-KEY-1BAD").unwrap();

        let env = Environment::new(tempdir().unwrap().path().to_path_buf()).unwrap();
        let err = run(repo.path(), &env).unwrap_err();

        assert!(err.to_string().contains("age identity material"));
    }
}

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::{
    config::{Config, CustomBackupConfig},
    environment::Environment,
    paths::absolutize,
    progress::BackupProgress,
};

pub fn run_backup_commands(
    config: &Config,
    repo_root: &Path,
    env: &Environment,
    dry_run: bool,
    scopes: &[PathBuf],
    actions: &mut Vec<String>,
    progress: &mut impl BackupProgress,
) -> Result<()> {
    for custom in &config.custom_backups {
        let Some(command) = custom.backup_command.as_deref() else {
            continue;
        };
        if !custom_matches_scopes(custom, repo_root, env, scopes) {
            continue;
        }
        let action = format!("custom backup {}: {command}", custom.name);
        if dry_run {
            actions.push(format!("would run {action}"));
            continue;
        }

        progress.phase(&format!("running custom backup {}", custom.name));
        run_shell_command(repo_root, env, "backup", &custom.name, command)?;
        actions.push(format!("run {action}"));
    }

    Ok(())
}

pub fn run_restore_commands(
    config: &Config,
    repo_root: &Path,
    env: &Environment,
    dry_run: bool,
    targets: &[String],
    actions: &mut Vec<String>,
) -> Result<()> {
    for custom in &config.custom_backups {
        let Some(command) = custom.restore_command.as_deref() else {
            continue;
        };
        if !custom_matches_targets(custom, repo_root, env, targets) {
            continue;
        }

        let action = format!("custom restore {}: {command}", custom.name);
        if dry_run {
            actions.push(format!("would run {action}"));
            continue;
        }

        run_shell_command(repo_root, env, "restore", &custom.name, command)?;
        actions.push(format!("run {action}"));
    }

    Ok(())
}

fn custom_matches_targets(
    custom: &CustomBackupConfig,
    repo_root: &Path,
    env: &Environment,
    targets: &[String],
) -> bool {
    if targets.is_empty() {
        return true;
    }

    let filters = targets
        .iter()
        .map(|target| absolute_filter(repo_root, env, target))
        .collect::<Vec<_>>();

    custom.path_configs().iter().any(|path| {
        let source = absolutize(&env.expand_tilde(&path.src), repo_root);
        filters.iter().any(|filter| paths_related(&source, filter))
    })
}

fn custom_matches_scopes(
    custom: &CustomBackupConfig,
    repo_root: &Path,
    env: &Environment,
    scopes: &[PathBuf],
) -> bool {
    if scopes.is_empty() {
        return true;
    }

    custom.path_configs().iter().any(|path| {
        let source = absolutize(&env.expand_tilde(&path.src), repo_root);
        scopes.iter().any(|scope| paths_related(&source, scope))
    })
}

fn paths_related(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn absolute_filter(repo_root: &Path, env: &Environment, raw: &str) -> PathBuf {
    let expanded = env.expand_tilde(raw);
    if expanded.is_absolute() {
        expanded
    } else {
        repo_root.join(expanded)
    }
}

fn run_shell_command(
    repo_root: &Path,
    env: &Environment,
    stage: &str,
    name: &str,
    command: &str,
) -> Result<()> {
    let status = shell_command(command)
        .current_dir(repo_root)
        .env("HOME", env.home())
        .env("DOTR_REPO", repo_root)
        .status()
        .with_context(|| format!("failed to run custom {stage} {name} command"))?;

    if !status.success() {
        bail!("custom {stage} {name} command failed with status {status}: {command}");
    }

    Ok(())
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("/bin/sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

use std::{
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

pub trait GitBackend {
    fn init(&self, repo_root: &Path) -> Result<()>;
    fn commit_backup(&self, repo_root: &Path, message: &str, include_unrelated: bool)
    -> Result<()>;
    fn push(&self, repo_root: &Path) -> Result<()>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CommandGit;

impl GitBackend for CommandGit {
    fn init(&self, repo_root: &Path) -> Result<()> {
        run_git(repo_root, ["init"])
    }

    fn commit_backup(
        &self,
        repo_root: &Path,
        message: &str,
        include_unrelated: bool,
    ) -> Result<()> {
        if !include_unrelated && has_unrelated_changes(repo_root)? {
            bail!(
                "repository has changes outside backup/; refusing to auto-commit unrelated files"
            );
        }

        run_git(repo_root, ["add", "backup"])?;

        if !has_backup_changes(repo_root)? {
            return Ok(());
        }

        run_git(repo_root, ["commit", "-m", message])
    }

    fn push(&self, repo_root: &Path) -> Result<()> {
        run_git(repo_root, ["push"])
    }
}

pub fn is_git_repo(repo_root: &Path) -> bool {
    repo_root.join(".git").exists()
}

pub fn has_backup_changes(repo_root: &Path) -> Result<bool> {
    let output = git_output(repo_root, ["status", "--porcelain", "--", "backup"])?;
    Ok(!output.trim().is_empty())
}

pub fn has_unrelated_changes(repo_root: &Path) -> Result<bool> {
    let output = git_output(repo_root, ["status", "--porcelain"])?;
    Ok(output
        .lines()
        .any(|line| !line.get(3..).unwrap_or("").starts_with("backup/")))
}

fn run_git<const N: usize>(repo_root: &Path, args: [&str; N]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("failed to run git in {}", repo_root.display()))?;

    if !status.success() {
        bail!("git command failed with status {status}");
    }

    Ok(())
}

fn git_output<const N: usize>(repo_root: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run git in {}", repo_root.display()))?;

    if !output.status.success() {
        bail!("git command failed with status {}", output.status);
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

use std::{
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

const MANAGED_GIT_PATHS: &[&str] = &[
    "dotr.toml",
    "files",
    "metadata",
    "recipients",
    "recipients.txt",
    ".gitignore",
];

pub trait GitBackend {
    fn init(&self, repo_root: &Path) -> Result<()>;
    fn commit_backup(&self, repo_root: &Path, message: &str) -> Result<()>;
    fn push(&self, repo_root: &Path) -> Result<()>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CommandGit;

impl GitBackend for CommandGit {
    fn init(&self, repo_root: &Path) -> Result<()> {
        run_git(repo_root, ["init"])
    }

    fn commit_backup(&self, repo_root: &Path, message: &str) -> Result<()> {
        add_managed_paths(repo_root)?;

        if !has_managed_changes(repo_root)? {
            return Ok(());
        }

        let mut args = vec!["commit", "-m", message, "--"];
        args.extend(managed_paths_for_git(repo_root)?);
        run_git_slice(repo_root, &args)
    }

    fn push(&self, repo_root: &Path) -> Result<()> {
        run_git(repo_root, ["push"])
    }
}

pub fn is_git_repo(repo_root: &Path) -> bool {
    repo_root.join(".git").exists()
}

pub fn has_managed_changes(repo_root: &Path) -> Result<bool> {
    let mut args = vec!["status", "--porcelain", "--"];
    args.extend(MANAGED_GIT_PATHS);
    let output = git_output_slice(repo_root, &args)?;
    Ok(!output.trim().is_empty())
}

fn add_managed_paths(repo_root: &Path) -> Result<()> {
    for path in managed_paths_for_git(repo_root)? {
        run_git(repo_root, ["add", "-A", path])?;
    }

    Ok(())
}

fn managed_paths_for_git(repo_root: &Path) -> Result<Vec<&'static str>> {
    let mut paths = Vec::new();
    for path in MANAGED_GIT_PATHS {
        if repo_root.join(path).exists() || has_path_status(repo_root, path)? {
            paths.push(*path);
        }
    }
    Ok(paths)
}

fn has_path_status(repo_root: &Path, path: &str) -> Result<bool> {
    let output = git_output(repo_root, ["status", "--porcelain", "--", path])?;
    Ok(!output.trim().is_empty())
}

fn run_git<const N: usize>(repo_root: &Path, args: [&str; N]) -> Result<()> {
    run_git_slice(repo_root, &args)
}

fn run_git_slice(repo_root: &Path, args: &[&str]) -> Result<()> {
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
    git_output_slice(repo_root, &args)
}

fn git_output_slice(repo_root: &Path, args: &[&str]) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::tempdir;

    #[test]
    fn commit_backup_uses_pathspecs_to_leave_staged_unmanaged_files_alone() -> Result<()> {
        let dir = tempdir()?;
        let repo = dir.path();
        let git = CommandGit;
        git.init(repo)?;
        run_git(repo, ["config", "user.email", "dotr@example.test"])?;
        run_git(repo, ["config", "user.name", "dotr test"])?;
        run_git(repo, ["commit", "--allow-empty", "-m", "init"])?;

        fs::create_dir_all(repo.join("files/home"))?;
        fs::write(repo.join("files/home/.zshrc"), "managed")?;
        fs::write(repo.join("README.md"), "unmanaged")?;
        run_git(repo, ["add", "README.md"])?;

        git.commit_backup(repo, "backup")?;

        let status = git_output(repo, ["status", "--porcelain"])?;
        assert!(status.contains("A  README.md"));
        assert!(!status.contains("files/home/.zshrc"));

        let committed = git_output(repo, ["show", "--name-only", "--format=", "HEAD"])?;
        assert!(committed.contains("files/home/.zshrc"));
        assert!(!committed.contains("README.md"));

        Ok(())
    }
}

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
                "repository has changes outside dotr managed paths; refusing to auto-commit unrelated files"
            );
        }

        add_managed_paths(repo_root)?;

        if !has_managed_changes(repo_root)? {
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

pub fn has_managed_changes(repo_root: &Path) -> Result<bool> {
    let output = git_output(
        repo_root,
        [
            "status",
            "--porcelain",
            "--",
            "dotr.toml",
            "files",
            "metadata",
            "recipients.txt",
            ".gitignore",
        ],
    )?;
    Ok(!output.trim().is_empty())
}

pub fn has_unrelated_changes(repo_root: &Path) -> Result<bool> {
    let output = git_output(repo_root, ["status", "--porcelain", "-z"])?;
    Ok(output
        .split('\0')
        .filter_map(status_path)
        .any(|path| !is_managed_status_path(path)))
}

fn add_managed_paths(repo_root: &Path) -> Result<()> {
    for path in [
        "dotr.toml",
        "files",
        "metadata",
        "recipients.txt",
        ".gitignore",
    ] {
        if repo_root.join(path).exists() {
            run_git(repo_root, ["add", path])?;
        }
    }

    Ok(())
}

fn is_managed_status_path(path: &str) -> bool {
    path == "dotr.toml"
        || path == "recipients.txt"
        || path == ".gitignore"
        || path.starts_with("files/")
        || path.starts_with("metadata/")
}

fn status_path(record: &str) -> Option<&str> {
    if record.len() < 4 || record.as_bytes().get(2) != Some(&b' ') {
        return None;
    }

    record.get(3..)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_dotr_managed_status_paths() {
        assert!(is_managed_status_path("dotr.toml"));
        assert!(is_managed_status_path(".gitignore"));
        assert!(is_managed_status_path("files/home/.config/nvim/init.lua"));
        assert!(is_managed_status_path("metadata/index.json"));
        assert!(is_managed_status_path("recipients.txt"));

        assert!(!is_managed_status_path("README.md"));
        assert!(!is_managed_status_path("src/main.rs"));
        assert!(!is_managed_status_path("backup/dotr.toml"));
    }

    #[test]
    fn parses_porcelain_z_paths_with_spaces_without_quotes() {
        let output =
            "A  files/home/Library/Application Support/Code/User/settings.json\0 M README.md\0";
        let paths = output
            .split('\0')
            .filter_map(status_path)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            vec![
                "files/home/Library/Application Support/Code/User/settings.json",
                "README.md"
            ]
        );
        assert!(is_managed_status_path(paths[0]));
        assert!(!is_managed_status_path(paths[1]));
    }
}

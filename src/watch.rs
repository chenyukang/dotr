use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};

use crate::{
    backup::{self, BackupOptions},
    config::Config,
    environment::Environment,
    paths::absolutize,
};

pub fn run(repo_root: &Path, env: &Environment) -> Result<()> {
    let config = Config::load(repo_root)?;
    let debounce = Duration::from_secs(config.watch.debounce_secs);
    let source_roots = config
        .paths
        .iter()
        .map(|path| absolutize(&env.expand_tilde(&path.src), repo_root))
        .collect::<Vec<_>>();

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = tx.send(result);
    })
    .context("failed to create filesystem watcher")?;

    for source in &source_roots {
        if source.exists() {
            watcher
                .watch(source, RecursiveMode::Recursive)
                .with_context(|| format!("failed to watch {}", source.display()))?;
        }
    }

    let running = Arc::new(AtomicBool::new(false));
    loop {
        let event = rx.recv().context("watch channel closed")??;
        if event
            .paths
            .iter()
            .all(|path| should_ignore_event_path(path, repo_root, &source_roots))
        {
            continue;
        }

        let mut deadline = debounce_deadline(Instant::now(), debounce);
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Ok(next)) => {
                    if next
                        .paths
                        .iter()
                        .any(|path| !should_ignore_event_path(path, repo_root, &source_roots))
                    {
                        deadline = debounce_deadline(Instant::now(), debounce);
                        continue;
                    }
                }
                Ok(Err(err)) => return Err(err.into()),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }

        if running.swap(true, Ordering::SeqCst) {
            continue;
        }
        let result = backup::run(
            repo_root,
            env,
            &BackupOptions {
                no_git: false,
                ..BackupOptions::default()
            },
        );
        running.store(false, Ordering::SeqCst);
        result?;
    }
}

pub fn debounce_deadline(now: Instant, debounce: Duration) -> Instant {
    now + debounce
}

pub fn should_ignore_event_path(path: &Path, repo_root: &Path, source_roots: &[PathBuf]) -> bool {
    if !path.starts_with(repo_root) {
        return false;
    }

    !source_roots.iter().any(|source| {
        source == repo_root || (source.starts_with(repo_root) && path.starts_with(source))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_repo_events_unless_repo_is_a_source() {
        let repo = PathBuf::from("/tmp/repo");
        let sources = vec![PathBuf::from("/tmp/home/.codex")];

        assert!(should_ignore_event_path(
            Path::new("/tmp/repo/backup/metadata/index.json"),
            &repo,
            &sources
        ));
        assert!(!should_ignore_event_path(
            Path::new("/tmp/home/.codex/AGENTS.md"),
            &repo,
            &sources
        ));
        assert!(!should_ignore_event_path(
            Path::new("/tmp/repo/backup/dotr.toml"),
            &repo,
            std::slice::from_ref(&repo)
        ));
        assert!(should_ignore_event_path(
            Path::new("/tmp/repo/backup/metadata/index.json"),
            &repo,
            &[PathBuf::from("/tmp")]
        ));
        assert!(!should_ignore_event_path(
            Path::new("/tmp/repo/sources/file"),
            &repo,
            &[PathBuf::from("/tmp/repo/sources")]
        ));
    }

    #[test]
    fn debounce_deadline_moves_forward() {
        let now = Instant::now();
        let deadline = debounce_deadline(now, Duration::from_secs(2));

        assert!(deadline > now);
    }
}

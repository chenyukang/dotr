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
use globset::GlobSet;
use notify::{RecursiveMode, Watcher};
use serde_json::json;

use crate::{
    backup::{self, BackupOptions},
    config::{Config, default_exclude_set, globset_from_patterns},
    environment::Environment,
    paths::absolutize,
    structured_log,
};

pub fn run(repo_root: &Path, env: &Environment) -> Result<()> {
    let config = Config::load(repo_root)?;
    let debounce = Duration::from_secs(config.watch.debounce_secs);
    let event_rules = WatchRules::from_config(&config, repo_root, env)?;
    let source_roots = event_rules.source_roots();
    let watch_specs = watch_specs_for_sources(&source_roots);

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |result| {
        let _ = tx.send(result);
    })
    .context("failed to create filesystem watcher")?;

    structured_log::info(
        "watch_started",
        &[
            ("repo", json!(repo_root.display().to_string())),
            ("debounce_secs", json!(debounce.as_secs())),
        ],
    );
    for spec in &watch_specs {
        watcher
            .watch(&spec.path, spec.recursive_mode())
            .with_context(|| format!("failed to watch {}", spec.path.display()))?;
        structured_log::info(
            "watch_path_registered",
            &[
                ("path", json!(spec.path.display().to_string())),
                ("recursive", json!(spec.recursive)),
            ],
        );
    }
    if watch_specs.is_empty() {
        structured_log::warn("watch_no_sources", &[]);
    }

    let running = Arc::new(AtomicBool::new(false));
    loop {
        let event = rx.recv().context("watch channel closed")??;
        if event
            .paths
            .iter()
            .all(|path| should_ignore_event_path(path, repo_root, &event_rules))
        {
            continue;
        }
        structured_log::info(
            "watch_change_detected",
            &[("paths", json!(display_event_paths(&event.paths)))],
        );

        let mut deadline = debounce_deadline(Instant::now(), debounce);
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Ok(next)) => {
                    if next
                        .paths
                        .iter()
                        .any(|path| !should_ignore_event_path(path, repo_root, &event_rules))
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
        structured_log::info("backup_started", &[]);
        let result = backup::run(
            repo_root,
            env,
            &BackupOptions {
                no_git: false,
                ..BackupOptions::default()
            },
        );
        running.store(false, Ordering::SeqCst);
        let report = match result {
            Ok(report) => report,
            Err(err) => {
                structured_log::error("backup_failed", &[("error", json!(err.to_string()))]);
                return Err(err);
            }
        };
        for action in &report.actions {
            structured_log::info("backup_action", &[("action", json!(action))]);
        }
        structured_log::info(
            "backup_completed",
            &[
                ("added", json!(report.added)),
                ("updated", json!(report.updated)),
                ("deleted", json!(report.deleted)),
                ("unchanged", json!(report.unchanged)),
                ("skipped", json!(report.skipped)),
            ],
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSpec {
    pub path: PathBuf,
    pub recursive: bool,
}

impl WatchSpec {
    fn new(path: PathBuf, recursive: bool) -> Self {
        Self { path, recursive }
    }

    fn recursive_mode(&self) -> RecursiveMode {
        if self.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        }
    }
}

pub fn watch_specs_for_sources(source_roots: &[PathBuf]) -> Vec<WatchSpec> {
    let mut specs = Vec::new();
    for source in source_roots {
        let spec = if source.is_dir() {
            Some(WatchSpec::new(source.clone(), true))
        } else {
            source
                .parent()
                .filter(|parent| parent.exists())
                .map(|parent| WatchSpec::new(parent.to_path_buf(), false))
        };

        if let Some(spec) = spec {
            push_unique_watch_spec(&mut specs, spec);
        }
    }

    specs
}

pub fn debounce_deadline(now: Instant, debounce: Duration) -> Instant {
    now + debounce
}

pub fn should_ignore_event_path(path: &Path, repo_root: &Path, rules: &WatchRules) -> bool {
    !is_relevant_event_path(path, repo_root, rules)
}

#[derive(Debug)]
pub struct WatchRules {
    default_excludes: GlobSet,
    rules: Vec<WatchRule>,
}

impl WatchRules {
    pub fn from_config(config: &Config, repo_root: &Path, env: &Environment) -> Result<Self> {
        let default_excludes = default_exclude_set()?;
        let mut rules = Vec::new();
        for path_config in config.path_configs() {
            let source = absolutize(&env.expand_tilde(&path_config.src), repo_root);
            let include = if path_config.include.is_empty() {
                None
            } else {
                Some(globset_from_patterns(
                    path_config.include.iter().map(String::as_str),
                )?)
            };
            let local_excludes =
                globset_from_patterns(path_config.exclude.iter().map(String::as_str))?;
            rules.push(WatchRule {
                source,
                include,
                local_excludes,
            });
        }

        Ok(Self {
            default_excludes,
            rules,
        })
    }

    fn source_roots(&self) -> Vec<PathBuf> {
        self.rules.iter().map(|rule| rule.source.clone()).collect()
    }
}

#[derive(Debug)]
struct WatchRule {
    source: PathBuf,
    include: Option<GlobSet>,
    local_excludes: GlobSet,
}

impl WatchRule {
    fn matches_event(&self, path: &Path, default_excludes: &GlobSet) -> bool {
        if !is_related_to_source(path, &self.source) {
            return false;
        }

        !is_excluded_event(path, &self.source, default_excludes, &self.local_excludes)
            && is_included_event(path, &self.source, self.include.as_ref())
    }
}

fn is_relevant_event_path(path: &Path, repo_root: &Path, rules: &WatchRules) -> bool {
    if path.starts_with(repo_root) {
        return rules.rules.iter().any(|rule| {
            (rule.source == repo_root
                || (rule.source.starts_with(repo_root) && path.starts_with(&rule.source)))
                && rule.matches_event(path, &rules.default_excludes)
        });
    }

    rules
        .rules
        .iter()
        .any(|rule| rule.matches_event(path, &rules.default_excludes))
}

fn is_related_to_source(path: &Path, source: &Path) -> bool {
    path.starts_with(source) || source == path
}

fn is_excluded_event(path: &Path, source_root: &Path, default: &GlobSet, local: &GlobSet) -> bool {
    let rel = path.strip_prefix(source_root).unwrap_or(path);
    default.is_match(rel)
        || local.is_match(path)
        || local.is_match(rel)
        || path
            .file_name()
            .is_some_and(|file_name| default.is_match(file_name) || local.is_match(file_name))
}

fn is_included_event(path: &Path, source_root: &Path, include: Option<&GlobSet>) -> bool {
    let Some(include) = include else {
        return true;
    };

    if path == source_root {
        return true;
    }

    let rel = path.strip_prefix(source_root).unwrap_or(path);
    include.is_match(path)
        || include.is_match(rel)
        || path
            .file_name()
            .is_some_and(|file_name| include.is_match(file_name))
}

fn push_unique_watch_spec(specs: &mut Vec<WatchSpec>, spec: WatchSpec) {
    if !specs.iter().any(|existing| existing == &spec) {
        specs.push(spec);
    }
}

fn display_event_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "<unknown>".to_string();
    }

    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
fn watch_rules_for_sources(source_roots: &[PathBuf]) -> WatchRules {
    WatchRules {
        default_excludes: default_exclude_set().unwrap(),
        rules: source_roots
            .iter()
            .map(|source| WatchRule {
                source: source.clone(),
                include: None,
                local_excludes: globset_from_patterns(std::iter::empty::<&str>()).unwrap(),
            })
            .collect(),
    }
}

#[cfg(test)]
fn watch_rules_from_toml(raw: &str, repo_root: &Path, env: &Environment) -> WatchRules {
    let config = toml::from_str(raw).unwrap();
    WatchRules::from_config(&config, repo_root, env).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn watches_directories_recursively_and_file_parents_non_recursively() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let file = root.join(".zshrc");
        let dir = root.join(".config/nvim");
        let missing = root.join(".missing");
        fs::write(&file, "").unwrap();
        fs::create_dir_all(&dir).unwrap();

        let specs = watch_specs_for_sources(&[file.clone(), dir.clone(), missing.clone()]);

        assert_eq!(
            specs,
            vec![
                WatchSpec::new(root.to_path_buf(), false),
                WatchSpec::new(dir, true)
            ]
        );
    }

    #[test]
    fn ignores_repo_events_unless_repo_is_a_source() {
        let repo = PathBuf::from("/repo");
        let rules = watch_rules_for_sources(&[PathBuf::from("/home/me/.config/nvim")]);

        assert!(should_ignore_event_path(
            Path::new("/repo/metadata/index.json"),
            &repo,
            &rules
        ));
        assert!(!should_ignore_event_path(
            Path::new("/home/me/.config/nvim/init.lua"),
            &repo,
            &rules
        ));
        assert!(!should_ignore_event_path(
            Path::new("/repo/dotr.toml"),
            &repo,
            &watch_rules_for_sources(std::slice::from_ref(&repo))
        ));
        assert!(should_ignore_event_path(
            Path::new("/repo/metadata/index.json"),
            &repo,
            &watch_rules_for_sources(&[PathBuf::from("/")])
        ));
        assert!(!should_ignore_event_path(
            Path::new("/repo/sources/file"),
            &repo,
            &watch_rules_for_sources(&[PathBuf::from("/repo/sources")])
        ));
    }

    #[test]
    fn filters_parent_watch_events_to_configured_sources() {
        let repo = PathBuf::from("/repo");
        let rules = watch_rules_for_sources(&[
            PathBuf::from("/home/me/.zshrc"),
            PathBuf::from("/home/me/.config/nvim"),
        ]);

        assert!(!should_ignore_event_path(
            Path::new("/home/me/.zshrc"),
            &repo,
            &rules
        ));
        assert!(!should_ignore_event_path(
            Path::new("/home/me/.config/nvim/init.lua"),
            &repo,
            &rules
        ));
        assert!(should_ignore_event_path(
            Path::new("/home/me/.vimrc"),
            &repo,
            &rules
        ));
        assert!(should_ignore_event_path(
            Path::new("/home/me"),
            &repo,
            &rules
        ));
    }

    #[test]
    fn ignores_events_excluded_or_not_included_by_config() {
        let env = Environment::new(PathBuf::from("/home/me")).unwrap();
        let repo = PathBuf::from("/repo");
        let rules = watch_rules_from_toml(
            r#"
            [[path]]
            src = "~/.codex"
            include = ["AGENTS.md", "config.toml", "skills/**"]
            exclude = ["skills/.system/**"]
            "#,
            &repo,
            &env,
        );

        assert!(!should_ignore_event_path(
            Path::new("/home/me/.codex/config.toml"),
            &repo,
            &rules
        ));
        assert!(!should_ignore_event_path(
            Path::new("/home/me/.codex/skills/my-skill/SKILL.md"),
            &repo,
            &rules
        ));
        assert!(should_ignore_event_path(
            Path::new("/home/me/.codex/logs_2.sqlite"),
            &repo,
            &rules
        ));
        assert!(should_ignore_event_path(
            Path::new("/home/me/.codex/sessions/abc.jsonl"),
            &repo,
            &rules
        ));
        assert!(should_ignore_event_path(
            Path::new("/home/me/.codex/skills/.system/openai/SKILL.md"),
            &repo,
            &rules
        ));
    }

    #[test]
    fn custom_backup_paths_are_watch_sources() {
        let env = Environment::new(PathBuf::from("/home/me")).unwrap();
        let repo = PathBuf::from("/repo");
        let rules = watch_rules_from_toml(
            r#"
            [[custom_backup]]
            name = "vscode"
            backup_command = "code --list-extensions > ~/.config/vscode/extensions.txt"

            [[custom_backup.path]]
            src = "~/.config/vscode/extensions.txt"
            "#,
            &repo,
            &env,
        );

        assert!(!should_ignore_event_path(
            Path::new("/home/me/.config/vscode/extensions.txt"),
            &repo,
            &rules
        ));
    }

    #[test]
    fn debounce_deadline_moves_forward() {
        let now = Instant::now();
        let deadline = debounce_deadline(now, Duration::from_secs(2));

        assert!(deadline > now);
    }
}

use std::{
    fs,
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
        let mut changed_paths = relevant_event_paths(&event.paths, repo_root, &event_rules);
        if changed_paths.is_empty() {
            continue;
        }
        structured_log::info(
            "watch_change_detected",
            &[("paths", json!(display_event_paths(&changed_paths)))],
        );

        let mut deadline = debounce_deadline(Instant::now(), debounce);
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Ok(next)) => {
                    let next_paths = relevant_event_paths(&next.paths, repo_root, &event_rules);
                    if !next_paths.is_empty() {
                        changed_paths.extend(next_paths);
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
        let scopes = dedup_paths(changed_paths);
        structured_log::info(
            "backup_started",
            &[("scopes", json!(display_event_paths(&scopes)))],
        );
        let backup_started = Instant::now();
        let result = backup::run(
            repo_root,
            env,
            &BackupOptions {
                no_git: false,
                scopes,
                ..BackupOptions::default()
            },
        );
        let backup_cost = backup_started.elapsed();
        running.store(false, Ordering::SeqCst);
        let report = match result {
            Ok(report) => report,
            Err(err) => {
                structured_log::error(
                    "backup_failed",
                    &[
                        ("error", json!(err.to_string())),
                        ("cost", json!(format!("{} ms", backup_cost.as_millis()))),
                    ],
                );
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
                ("visited", json!(report.visited)),
                ("cost", json!(format!("{} ms", backup_cost.as_millis()))),
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
    scope_for_event_path(path, repo_root, rules).is_none()
}

fn relevant_event_paths(paths: &[PathBuf], repo_root: &Path, rules: &WatchRules) -> Vec<PathBuf> {
    paths
        .iter()
        .filter_map(|path| scope_for_event_path(path, repo_root, rules))
        .collect()
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
            let event_sources = event_sources_for_path(&source, path_config.follow_symlink);
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
                event_sources,
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
        dedup_paths(
            self.rules
                .iter()
                .flat_map(|rule| rule.event_sources.iter().cloned())
                .collect(),
        )
    }
}

#[derive(Debug)]
struct WatchRule {
    source: PathBuf,
    event_sources: Vec<PathBuf>,
    include: Option<GlobSet>,
    local_excludes: GlobSet,
}

impl WatchRule {
    fn scope_for_event(&self, path: &Path, default_excludes: &GlobSet) -> Option<PathBuf> {
        self.event_sources.iter().find_map(|event_source| {
            let parent_event = is_parent_event_for_file(path, event_source);
            if !parent_event && !is_related_to_source(path, event_source) {
                return None;
            }
            if path == event_source && self.include.is_some() {
                return None;
            }
            let filter_path = if parent_event { event_source } else { path };
            if is_excluded_event(
                filter_path,
                event_source,
                default_excludes,
                &self.local_excludes,
            ) {
                return None;
            }
            if !is_included_event(filter_path, event_source, self.include.as_ref()) {
                return None;
            }

            Some(map_event_to_source_scope(
                filter_path,
                event_source,
                &self.source,
            ))
        })
    }
}

fn scope_for_event_path(path: &Path, repo_root: &Path, rules: &WatchRules) -> Option<PathBuf> {
    if path.starts_with(repo_root) {
        return rules.rules.iter().find_map(|rule| {
            let repo_related = rule.event_sources.iter().any(|event_source| {
                event_source == repo_root
                    || (event_source.starts_with(repo_root) && path.starts_with(event_source))
            });
            repo_related
                .then(|| rule.scope_for_event(path, &rules.default_excludes))
                .flatten()
        });
    }

    rules
        .rules
        .iter()
        .find_map(|rule| rule.scope_for_event(path, &rules.default_excludes))
}

fn is_related_to_source(path: &Path, source: &Path) -> bool {
    path.starts_with(source) || source == path
}

fn is_parent_event_for_file(path: &Path, source: &Path) -> bool {
    source.parent() == Some(path) && !source.is_dir()
}

fn event_sources_for_path(source: &Path, follow_symlink: bool) -> Vec<PathBuf> {
    let mut event_sources = vec![source.to_path_buf()];
    if follow_symlink {
        if let Ok(target) = fs::read_link(source) {
            let target = if target.is_absolute() {
                target
            } else {
                source
                    .parent()
                    .map(|parent| parent.join(&target))
                    .unwrap_or(target)
            };
            if target != source {
                event_sources.push(target);
            }
        }
        if let Ok(resolved) = fs::canonicalize(source)
            && resolved != source
        {
            event_sources.push(resolved);
        }
    }
    dedup_paths(event_sources)
}

fn map_event_to_source_scope(path: &Path, event_source: &Path, source: &Path) -> PathBuf {
    if event_source == source {
        return path.to_path_buf();
    }

    path.strip_prefix(event_source)
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                source.to_path_buf()
            } else {
                source.join(relative)
            }
        })
        .unwrap_or_else(|_| source.to_path_buf())
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
    include.is_match(path) || include.is_match(rel)
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

fn dedup_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
fn watch_rules_for_sources(source_roots: &[PathBuf]) -> WatchRules {
    WatchRules {
        default_excludes: default_exclude_set().unwrap(),
        rules: source_roots
            .iter()
            .map(|source| WatchRule {
                source: source.clone(),
                event_sources: vec![source.clone()],
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

    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;
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
        assert_eq!(
            relevant_event_paths(&[PathBuf::from("/home/me")], &repo, &rules),
            vec![PathBuf::from("/home/me/.zshrc")]
        );
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
        assert!(should_ignore_event_path(
            Path::new("/home/me/.codex"),
            &repo,
            &rules
        ));
    }

    #[test]
    fn directory_root_events_still_match_without_include_filter() {
        let env = Environment::new(PathBuf::from("/home/me")).unwrap();
        let repo = PathBuf::from("/repo");
        let rules = watch_rules_from_toml(
            r#"
            [[path]]
            src = "~/.config/nvim"
            "#,
            &repo,
            &env,
        );

        assert!(!should_ignore_event_path(
            Path::new("/home/me/.config/nvim"),
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

    #[cfg(unix)]
    #[test]
    fn followed_symlink_target_events_map_back_to_source_scope() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let target_dir = home.path().join("dotfiles/home");
        fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("dot_zshrc");
        let source = home.path().join(".zshrc");
        fs::write(&target, "zsh").unwrap();
        unix_fs::symlink(&target, &source).unwrap();

        let env = Environment::new(home.path().to_path_buf()).unwrap();
        let rules = watch_rules_from_toml(
            r#"
            [[path]]
            src = "~/.zshrc"
            "#,
            repo.path(),
            &env,
        );

        let source_roots = rules.source_roots();
        assert!(source_roots.contains(&source));
        assert!(source_roots.contains(&target));
        assert_eq!(
            relevant_event_paths(std::slice::from_ref(&target), repo.path(), &rules),
            vec![source.clone()]
        );
        let parent_scopes =
            relevant_event_paths(std::slice::from_ref(&target_dir), repo.path(), &rules);
        assert_eq!(parent_scopes, vec![source.clone()]);
        assert_eq!(
            parent_scopes[0].display().to_string(),
            source.display().to_string()
        );
    }

    #[test]
    fn relevant_event_paths_returns_only_matching_scopes() {
        let repo = PathBuf::from("/repo");
        let rules = watch_rules_for_sources(&[
            PathBuf::from("/home/me/.zshrc"),
            PathBuf::from("/home/me/.config/nvim"),
        ]);

        let paths = relevant_event_paths(
            &[
                PathBuf::from("/home/me/.zshrc"),
                PathBuf::from("/home/me/.vimrc"),
                PathBuf::from("/home/me/.config/nvim/init.lua"),
            ],
            &repo,
            &rules,
        );

        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/me/.zshrc"),
                PathBuf::from("/home/me/.config/nvim/init.lua")
            ]
        );
    }

    #[test]
    fn include_events_are_relative_not_recursive_basenames() {
        let env = Environment::new(PathBuf::from("/home/me")).unwrap();
        let repo = PathBuf::from("/repo");
        let rules = watch_rules_from_toml(
            r#"
            [[path]]
            src = "~/.config/jj"
            include = ["config.toml"]
            "#,
            &repo,
            &env,
        );

        assert!(!should_ignore_event_path(
            Path::new("/home/me/.config/jj/config.toml"),
            &repo,
            &rules
        ));
        assert!(should_ignore_event_path(
            Path::new("/home/me/.config/jj/repos/repo-id/config.toml"),
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

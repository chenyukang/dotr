use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use globset::GlobSet;
use walkdir::WalkDir;

use crate::{
    config::{Config, PathConfig, default_exclude_set, globset_from_patterns, index_path},
    custom_backup, encryption,
    environment::Environment,
    git::{CommandGit, GitBackend},
    hash::{sha256_bytes, sha256_file},
    index::{EntryKind, Index, IndexEntry},
    paths::{absolutize, ensure_safe_relative, source_to_stored},
    progress::{BackupProgress, NoopProgress},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupOptions {
    pub dry_run: bool,
    pub no_delete: bool,
    pub no_git: bool,
    pub commit: bool,
    pub push: bool,
    pub scopes: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReport {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub encrypted: usize,
    pub visited: usize,
    pub actions: Vec<String>,
}

impl BackupReport {
    pub fn changed(&self) -> bool {
        self.added + self.updated + self.deleted > 0
    }
}

pub fn run(repo_root: &Path, env: &Environment, options: &BackupOptions) -> Result<BackupReport> {
    let mut progress = NoopProgress;
    run_with_progress(repo_root, env, options, &mut progress)
}

pub fn run_with_progress(
    repo_root: &Path,
    env: &Environment,
    options: &BackupOptions,
    progress: &mut impl BackupProgress,
) -> Result<BackupReport> {
    progress.phase("loading config");
    let config = Config::load(repo_root)?;
    run_with_config_and_progress(repo_root, env, &config, options, &CommandGit, progress)
}

pub fn run_with_config(
    repo_root: &Path,
    env: &Environment,
    config: &Config,
    options: &BackupOptions,
    git: &impl GitBackend,
) -> Result<BackupReport> {
    let mut progress = NoopProgress;
    run_with_config_and_progress(repo_root, env, config, options, git, &mut progress)
}

pub(crate) fn run_with_config_and_progress(
    repo_root: &Path,
    env: &Environment,
    config: &Config,
    options: &BackupOptions,
    git: &impl GitBackend,
    progress: &mut impl BackupProgress,
) -> Result<BackupReport> {
    progress.start(repo_root);
    progress.phase("reading metadata");
    let store_dir = config.store_dir(repo_root);
    let index_file = index_path(&store_dir);
    let previous = Index::read(&index_file)?;
    let scope = BackupScope::new(repo_root, &options.scopes);

    progress.phase("preparing rules");
    let default_excludes = default_exclude_set()?;
    let max_file_size = config.max_file_size_bytes()?;
    let has_encrypted_paths = config.path_configs().iter().any(|path_config| {
        let source = absolutize(&env.expand_tilde(&path_config.src), repo_root);
        scope.path_config_matches(&source) && path_config.encrypt
    });
    let recipients = if has_encrypted_paths && !options.dry_run {
        let recipients_file = config
            .encryption
            .recipients_file
            .as_deref()
            .context("encrypted paths require encryption.recipients_file")?;
        let recipients_path = encryption::resolve_recipients_file(repo_root, recipients_file);
        Some(encryption::load_recipients(&recipients_path)?)
    } else {
        None
    };

    let mut report = BackupReport::default();
    let mut current = scoped_current_from_previous(&previous, env, &scope);

    custom_backup::run_backup_commands(
        config,
        repo_root,
        env,
        options.dry_run,
        scope.paths(),
        &mut report.actions,
        progress,
    )?;

    for path_config in config.path_configs() {
        let source = absolutize(&env.expand_tilde(&path_config.src), repo_root);
        let candidates = scope.candidates_for_source(&source);
        if candidates.is_empty() {
            continue;
        }
        progress.source(&source);
        let local_includes = if path_config.include.is_empty() {
            None
        } else {
            Some(globset_from_patterns(
                path_config.include.iter().map(String::as_str),
            )?)
        };
        let local_excludes = globset_from_patterns(path_config.exclude.iter().map(String::as_str))?;
        for candidate in candidates {
            process_path_candidate(
                env,
                &store_dir,
                &previous,
                &mut current,
                &mut report,
                &source,
                &candidate,
                &default_excludes,
                &local_excludes,
                local_includes.as_ref(),
                &path_config,
                max_file_size,
                recipients.as_deref(),
                options.dry_run,
                progress,
            )?;
        }
    }

    progress.phase("checking deletions");
    handle_deletions(
        &store_dir,
        &previous,
        &mut current,
        &mut report,
        options.no_delete,
        options.dry_run,
    )?;

    if !options.no_delete && scope.is_full() {
        progress.phase("pruning orphan files");
        prune_orphan_stored_files(&store_dir, &current, &mut report, options.dry_run)?;
    }

    if !options.dry_run {
        progress.phase("writing metadata/index.json");
        let index = Index {
            version: 1,
            entries: current.into_values().collect(),
        };
        index.write(&index_file)?;

        let wants_commit =
            options.commit || config.git.auto_commit || options.push || config.git.auto_push;
        if !options.no_git && wants_commit {
            let message = format!(
                "{} ({})",
                config.git.commit_message,
                change_summary(&report)
            );
            progress.phase("committing git changes");
            git.commit_backup(repo_root, &message)?;
        }

        let wants_push = options.push || config.git.auto_push;
        if !options.no_git && wants_push {
            progress.phase("pushing git changes");
            git.push(repo_root)?;
        }
    }

    Ok(report)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BackupScope {
    paths: Vec<PathBuf>,
}

impl BackupScope {
    fn new(repo_root: &Path, raw_paths: &[PathBuf]) -> Self {
        let mut paths = raw_paths
            .iter()
            .map(|path| absolutize(path, repo_root))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();

        Self { paths }
    }

    fn is_full(&self) -> bool {
        self.paths.is_empty()
    }

    fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    fn path_config_matches(&self, source: &Path) -> bool {
        self.is_full() || self.paths.iter().any(|scope| paths_related(source, scope))
    }

    fn entry_matches(&self, env: &Environment, entry: &IndexEntry) -> bool {
        if self.is_full() {
            return true;
        }

        let source = env.expand_tilde(&entry.source);
        self.paths
            .iter()
            .any(|scope| source == *scope || source.starts_with(scope))
    }

    fn candidates_for_source(&self, source: &Path) -> Vec<PathBuf> {
        if self.is_full() {
            return vec![source.to_path_buf()];
        }

        let candidates = self
            .paths
            .iter()
            .filter_map(|scope| {
                if scope == source || scope.starts_with(source) {
                    Some(scope.clone())
                } else if source.starts_with(scope) {
                    Some(source.to_path_buf())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        dedup_covered_paths(candidates)
    }
}

fn scoped_current_from_previous(
    previous: &Index,
    env: &Environment,
    scope: &BackupScope,
) -> BTreeMap<String, IndexEntry> {
    if scope.is_full() {
        return BTreeMap::new();
    }

    previous
        .entries
        .iter()
        .filter(|entry| !scope.entry_matches(env, entry))
        .map(|entry| (entry.stored.clone(), entry.clone()))
        .collect()
}

fn dedup_covered_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();

    let mut deduped = Vec::<PathBuf>::new();
    for path in paths {
        if deduped.iter().any(|existing| path.starts_with(existing)) {
            continue;
        }
        deduped.push(path);
    }

    deduped
}

fn paths_related(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[allow(clippy::too_many_arguments)]
fn process_path_candidate(
    env: &Environment,
    store_dir: &Path,
    previous: &Index,
    current: &mut BTreeMap<String, IndexEntry>,
    report: &mut BackupReport,
    source_root: &Path,
    candidate: &Path,
    default_excludes: &GlobSet,
    local_excludes: &GlobSet,
    local_includes: Option<&GlobSet>,
    path_config: &PathConfig,
    max_file_size: u64,
    recipients: Option<&[age::x25519::Recipient]>,
    dry_run: bool,
    progress: &mut impl BackupProgress,
) -> Result<()> {
    if !source_exists(candidate, path_config.follow_symlink) {
        report.skipped += 1;
        report
            .actions
            .push(format!("skip missing source {}", candidate.display()));
        return Ok(());
    }

    if should_walk_source(candidate, path_config.follow_symlink)? {
        let mut scanned = 0;
        for entry in WalkDir::new(candidate)
            .follow_links(path_config.follow_symlink)
            .into_iter()
            .filter_entry(|entry| {
                !is_excluded(
                    entry.path(),
                    source_root,
                    default_excludes,
                    local_excludes,
                    path_config.force,
                )
            })
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    report.skipped += 1;
                    if let Some(path) = err.path() {
                        report
                            .actions
                            .push(format!("skip unreadable {}", path.display()));
                    } else {
                        report
                            .actions
                            .push(format!("skip walk error under {}", candidate.display()));
                    }
                    continue;
                }
            };
            report.visited += 1;
            scanned += 1;
            progress.scanned(scanned, entry.path());
            if !is_included(
                entry.path(),
                source_root,
                local_includes,
                path_config.follow_symlink,
            ) {
                continue;
            }
            process_entry(
                env,
                store_dir,
                previous,
                current,
                report,
                entry.path(),
                path_config.encrypt,
                path_config.follow_symlink,
                path_config.include_binary_file,
                path_config.force,
                max_file_size,
                recipients,
                dry_run,
            )?;
        }
    } else if is_excluded(
        candidate,
        source_root,
        default_excludes,
        local_excludes,
        path_config.force,
    ) {
        report.visited += 1;
        report.skipped += 1;
        report
            .actions
            .push(format!("skip excluded {}", candidate.display()));
    } else if !is_included(
        candidate,
        source_root,
        local_includes,
        path_config.follow_symlink,
    ) {
        report.visited += 1;
        report.skipped += 1;
        report
            .actions
            .push(format!("skip not included {}", candidate.display()));
    } else {
        report.visited += 1;
        process_entry(
            env,
            store_dir,
            previous,
            current,
            report,
            candidate,
            path_config.encrypt,
            path_config.follow_symlink,
            path_config.include_binary_file,
            path_config.force,
            max_file_size,
            recipients,
            dry_run,
        )?;
    }

    Ok(())
}

fn is_included(
    path: &Path,
    source_root: &Path,
    include: Option<&GlobSet>,
    follow_symlink: bool,
) -> bool {
    let Some(include) = include else {
        return true;
    };

    let Ok(metadata) = metadata_for_backup(path, follow_symlink) else {
        return false;
    };

    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return false;
    }

    let rel = path.strip_prefix(source_root).unwrap_or(path);
    include.is_match(path) || include.is_match(rel)
}

#[allow(clippy::too_many_arguments)]
fn process_entry(
    env: &Environment,
    store_dir: &Path,
    previous: &Index,
    current: &mut BTreeMap<String, IndexEntry>,
    report: &mut BackupReport,
    source: &Path,
    encrypted: bool,
    follow_symlink: bool,
    include_binary_file: bool,
    force: bool,
    max_file_size: u64,
    recipients: Option<&[age::x25519::Recipient]>,
    dry_run: bool,
) -> Result<()> {
    let metadata = metadata_for_backup(source, follow_symlink)
        .with_context(|| format!("failed to read metadata for {}", source.display()))?;
    let file_type = metadata.file_type();
    let mode = unix_mode(&metadata);

    if file_type.is_file() && !force && !include_binary_file && is_binary_file(source)? {
        report.skipped += 1;
        report
            .actions
            .push(format!("skip binary {}", source.display()));
        return Ok(());
    }

    if file_type.is_file() && !force && metadata.len() > max_file_size {
        report.skipped += 1;
        report.actions.push(format!(
            "skip oversized {} ({} bytes)",
            source.display(),
            metadata.len()
        ));
        return Ok(());
    }

    let stored = source_to_stored(source, env, encrypted && file_type.is_file())?;
    ensure_safe_relative(&stored.relative)?;
    let stored_key = stored.as_index_path();
    let previous_entry = previous.by_stored(&stored_key);
    let mut index_entry = IndexEntry {
        source: env.display_source(source),
        stored: stored_key.clone(),
        kind: kind_for_metadata(&metadata),
        sha256: None,
        mode,
        executable: mode.is_some_and(|mode| mode & 0o111 != 0),
        encrypted: encrypted && file_type.is_file(),
        symlink_target: None,
        size: None,
        modified_unix_nanos: None,
    };

    if file_type.is_dir() {
        if !dry_run {
            fs::create_dir_all(store_dir.join(&stored.relative))?;
        }
        record_change(previous_entry, &index_entry, report, "directory");
        current.insert(stored_key, index_entry);
        return Ok(());
    }

    if file_type.is_symlink() {
        let target = fs::read_link(source)
            .with_context(|| format!("failed to read symlink {}", source.display()))?;
        index_entry.symlink_target = Some(target.to_string_lossy().into_owned());
        if !dry_run {
            remove_if_exists(&store_dir.join(&stored.relative))?;
        }
        record_change(previous_entry, &index_entry, report, "symlink");
        current.insert(stored_key, index_entry);
        return Ok(());
    }

    if !file_type.is_file() {
        report.skipped += 1;
        report
            .actions
            .push(format!("skip unsupported file type {}", source.display()));
        return Ok(());
    }

    index_entry.size = Some(metadata.len());
    index_entry.modified_unix_nanos = modified_unix_nanos(&metadata);

    if encrypted {
        let unchanged = previous_entry.is_some_and(|prev| {
            prev.kind == index_entry.kind
                && prev.encrypted
                && prev.mode == index_entry.mode
                && prev.executable == index_entry.executable
                && prev.size == index_entry.size
                && prev.modified_unix_nanos == index_entry.modified_unix_nanos
                && store_dir.join(&prev.stored).exists()
        });

        if unchanged {
            index_entry.sha256 = previous_entry.and_then(|prev| prev.sha256.clone());
            report.unchanged += 1;
            current.insert(stored_key, index_entry);
            return Ok(());
        }

        report.encrypted += 1;
        warn_if_previously_plaintext(previous, &index_entry, report);
        if !dry_run {
            let recipients = recipients.context("encrypted paths require age recipients")?;
            let plaintext =
                fs::read(source).with_context(|| format!("failed to read {}", source.display()))?;
            let ciphertext = encryption::encrypt_bytes(&plaintext, recipients)?;
            let target = store_dir.join(&stored.relative);
            write_bytes(&target, &ciphertext)?;
            index_entry.sha256 = Some(sha256_bytes(&ciphertext));
        } else {
            index_entry.sha256 = previous_entry.and_then(|prev| prev.sha256.clone());
        }

        record_change(previous_entry, &index_entry, report, "encrypted file");
        current.insert(stored_key, index_entry);
        return Ok(());
    }

    index_entry.sha256 =
        Some(sha256_file(source).with_context(|| format!("failed to hash {}", source.display()))?);
    let unchanged = previous_entry.is_some_and(|prev| {
        file_entry_unchanged_ignoring_mtime(prev, &index_entry)
            && store_dir.join(&stored.relative).exists()
    });

    if unchanged {
        report.unchanged += 1;
        current.insert(stored_key, previous_entry.cloned().expect("checked above"));
        return Ok(());
    } else {
        if !dry_run {
            copy_file(source, &store_dir.join(&stored.relative))?;
        }
        record_change(previous_entry, &index_entry, report, "file");
    }

    current.insert(stored_key, index_entry);
    Ok(())
}

fn handle_deletions(
    store_dir: &Path,
    previous: &Index,
    current: &mut BTreeMap<String, IndexEntry>,
    report: &mut BackupReport,
    no_delete: bool,
    dry_run: bool,
) -> Result<()> {
    let current_keys = current.keys().cloned().collect::<BTreeSet<_>>();
    let mut stale = previous
        .entries
        .iter()
        .filter(|entry| !current_keys.contains(&entry.stored))
        .cloned()
        .collect::<Vec<_>>();
    stale.sort_by_key(|entry| std::cmp::Reverse(entry.stored.len()));

    for entry in stale {
        if no_delete {
            current.insert(entry.stored.clone(), entry);
            continue;
        }

        ensure_safe_relative(Path::new(&entry.stored))?;
        report.deleted += 1;
        report.actions.push(format!("delete {}", entry.stored));
        if !dry_run {
            remove_if_exists(&store_dir.join(&entry.stored))?;
        }
    }

    Ok(())
}

fn prune_orphan_stored_files(
    store_dir: &Path,
    current: &BTreeMap<String, IndexEntry>,
    report: &mut BackupReport,
    dry_run: bool,
) -> Result<()> {
    let files_dir = store_dir.join("files");
    if !files_dir.exists() {
        return Ok(());
    }

    let mut orphan_files = Vec::new();
    for entry in WalkDir::new(&files_dir).follow_links(false) {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            continue;
        }

        let stored = entry
            .path()
            .strip_prefix(store_dir)
            .with_context(|| format!("stored path escaped {}", store_dir.display()))?;
        ensure_safe_relative(stored)?;
        let key = stored.to_string_lossy().replace('\\', "/");
        if !current.contains_key(&key) {
            orphan_files.push((entry.path().to_path_buf(), key));
        }
    }

    orphan_files.sort_by(|a, b| a.1.cmp(&b.1));
    for (path, key) in orphan_files {
        report.deleted += 1;
        report.actions.push(format!("delete orphan {key}"));
        if !dry_run {
            remove_if_exists(&path)?;
        }
    }

    if !dry_run {
        remove_empty_orphan_dirs(&files_dir, store_dir, current)?;
    }

    Ok(())
}

fn remove_empty_orphan_dirs(
    files_dir: &Path,
    store_dir: &Path,
    current: &BTreeMap<String, IndexEntry>,
) -> Result<()> {
    let mut dirs = Vec::new();
    for entry in WalkDir::new(files_dir)
        .follow_links(false)
        .contents_first(true)
    {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            dirs.push(entry.path().to_path_buf());
        }
    }

    for dir in dirs {
        if dir == files_dir || dir == files_dir.join("home") || dir == files_dir.join("root") {
            continue;
        }

        let stored = dir
            .strip_prefix(store_dir)
            .with_context(|| format!("stored path escaped {}", store_dir.display()))?;
        ensure_safe_relative(stored)?;
        let key = stored.to_string_lossy().replace('\\', "/");
        if current.contains_key(&key) {
            continue;
        }

        if fs::read_dir(&dir)?.next().is_none() {
            fs::remove_dir(&dir).with_context(|| format!("failed to remove {}", dir.display()))?;
        }
    }

    Ok(())
}

fn is_excluded(
    path: &Path,
    source_root: &Path,
    default: &GlobSet,
    local: &GlobSet,
    force: bool,
) -> bool {
    let rel = path.strip_prefix(source_root).unwrap_or(path);
    (!force && default.is_match(rel))
        || local.is_match(path)
        || local.is_match(rel)
        || path.file_name().is_some_and(|file_name| {
            (!force && default.is_match(file_name)) || local.is_match(file_name)
        })
}

fn should_walk_source(source: &Path, follow_symlink: bool) -> Result<bool> {
    let metadata = metadata_for_backup(source, follow_symlink)
        .with_context(|| format!("failed to read metadata for {}", source.display()))?;
    Ok(metadata.is_dir() && (follow_symlink || !metadata.file_type().is_symlink()))
}

fn metadata_for_backup(path: &Path, follow_symlink: bool) -> std::io::Result<fs::Metadata> {
    if follow_symlink {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    }
}

fn source_exists(path: &Path, follow_symlink: bool) -> bool {
    if follow_symlink {
        path.exists()
    } else {
        fs::symlink_metadata(path).is_ok()
    }
}

fn record_change(
    previous_entry: Option<&IndexEntry>,
    next_entry: &IndexEntry,
    report: &mut BackupReport,
    noun: &str,
) {
    match previous_entry {
        None => {
            report.added += 1;
            report
                .actions
                .push(format!("add {noun} {}", next_entry.source));
        }
        Some(prev) if prev != next_entry => {
            report.updated += 1;
            report
                .actions
                .push(format!("update {noun} {}", next_entry.source));
        }
        Some(_) => report.unchanged += 1,
    }
}

fn warn_if_previously_plaintext(
    previous: &Index,
    next_entry: &IndexEntry,
    report: &mut BackupReport,
) {
    if !next_entry.encrypted
        || !previous.entries.iter().any(|entry| {
            entry.source == next_entry.source && entry.kind == EntryKind::File && !entry.encrypted
        })
    {
        return;
    }

    report.actions.push(format!(
        "warning: {} was previously backed up as plaintext; Git history may still contain plaintext. Rotate exposed secrets or rewrite history before publishing.",
        next_entry.source
    ));
}

fn file_entry_unchanged_ignoring_mtime(prev: &IndexEntry, next: &IndexEntry) -> bool {
    prev.source == next.source
        && prev.stored == next.stored
        && prev.kind == next.kind
        && prev.sha256 == next.sha256
        && prev.mode == next.mode
        && prev.executable == next.executable
        && prev.encrypted == next.encrypted
        && prev.symlink_target == next.symlink_target
        && prev.size == next.size
}

fn kind_for_metadata(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Directory
    } else {
        EntryKind::File
    }
}

fn unix_mode(metadata: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn modified_unix_nanos(metadata: &fs::Metadata) -> Option<u128> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_if_exists(path)?;
    fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

fn copy_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_if_exists(target)?;
    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            target.display()
        )
    })?;
    Ok(())
}

fn is_binary_file(path: &Path) -> Result<bool> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut buffer = [0_u8; 8192];
    let bytes_read = file
        .read(&mut buffer)
        .with_context(|| format!("failed to read {}", path.display()))?;

    Ok(buffer[..bytes_read].contains(&0))
}

fn remove_if_exists(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
    }
}

pub(crate) fn change_summary(report: &BackupReport) -> String {
    if !report.changed() {
        return "pending managed changes".to_string();
    }

    format!(
        "{} add, {} update, {} delete",
        report.added, report.updated, report.deleted
    )
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, thread, time::Duration};

    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;

    use age::secrecy::ExposeSecret;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        config::{CustomBackupConfig, PathConfig},
        init,
    };

    #[derive(Default)]
    struct RecordingProgress {
        events: Vec<String>,
    }

    #[derive(Default)]
    struct RecordingGit {
        commits: Cell<usize>,
        pushes: Cell<usize>,
    }

    impl GitBackend for RecordingGit {
        fn init(&self, _repo_root: &Path) -> Result<()> {
            Ok(())
        }

        fn commit_backup(&self, _repo_root: &Path, _message: &str) -> Result<()> {
            self.commits.set(self.commits.get() + 1);
            Ok(())
        }

        fn push(&self, _repo_root: &Path) -> Result<()> {
            self.pushes.set(self.pushes.get() + 1);
            Ok(())
        }
    }

    impl crate::progress::BackupProgress for RecordingProgress {
        fn start(&mut self, repo_root: &Path) {
            self.events.push(format!("start {}", repo_root.display()));
        }

        fn phase(&mut self, message: &str) {
            self.events.push(format!("phase {message}"));
        }

        fn source(&mut self, source: &Path) {
            self.events.push(format!("source {}", source.display()));
        }

        fn scanned(&mut self, scanned: usize, current: &Path) {
            self.events
                .push(format!("scanned {scanned} {}", current.display()));
        }
    }

    fn env_for(home: &Path) -> Environment {
        Environment::new(home.to_path_buf()).unwrap()
    }

    fn repo_with_config(home: &Path, config: &Config) -> tempfile::TempDir {
        let repo = tempdir().unwrap();
        init::run(&init::InitOptions {
            target: repo.path().to_path_buf(),
            with_defaults: false,
            no_git: true,
            force: false,
        })
        .unwrap();
        let toml = toml::to_string_pretty(config).unwrap();
        fs::write(repo.path().join("dotr.toml"), toml).unwrap();
        fs::create_dir_all(home).unwrap();
        repo
    }

    #[cfg(unix)]
    fn write_home_file_command(relative: &str, contents: &str) -> String {
        let parent = Path::new(relative).parent().unwrap();
        format!(
            "mkdir -p \"$HOME/{}\" && printf %s {} > \"$HOME/{}\"",
            parent.to_string_lossy(),
            contents,
            relative
        )
    }

    #[test]
    fn backs_up_home_file_and_noops_on_second_run() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/nvim")).unwrap();
        fs::write(home.join(".config/nvim/init.lua"), "rules").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/nvim".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let first = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();
        assert!(first.added >= 1);
        assert_eq!(first.visited, 2);
        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/.config/nvim/init.lua")).unwrap(),
            "rules"
        );

        let second = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();
        assert_eq!(second.added + second.updated + second.deleted, 0);
    }

    #[cfg(unix)]
    #[test]
    fn root_store_directory_is_created_on_demand() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let absolute_root = tempdir().unwrap();
        let source = absolute_root.path().join("Library/example/hello/world");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "absolute").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: source.to_string_lossy().into_owned(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);
        assert!(!repo.path().join("files/root").exists());

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        let stored = source_to_stored(&source, &env, false).unwrap();
        assert!(repo.path().join(stored.relative).is_file());
        assert!(repo.path().join("files/root").is_dir());
    }

    #[test]
    fn scoped_backup_updates_changed_file_without_scanning_unrelated_sources() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::create_dir_all(home.join(".config/other")).unwrap();
        let app_config = home.join(".config/app/config.toml");
        let other_config = home.join(".config/other/config.toml");
        fs::write(&app_config, "one").unwrap();
        fs::write(&other_config, "other").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/app".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        config.paths.push(PathConfig {
            src: "~/.config/other".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();
        fs::write(&app_config, "two").unwrap();

        let mut progress = RecordingProgress::default();
        let report = run_with_config_and_progress(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                scopes: vec![app_config.clone()],
                ..BackupOptions::default()
            },
            &CommandGit,
            &mut progress,
        )
        .unwrap();

        assert_eq!(report.added + report.deleted, 0);
        assert_eq!(report.updated, 1);
        assert_eq!(report.visited, 1);
        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/.config/app/config.toml")).unwrap(),
            "two"
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/.config/other/config.toml")).unwrap(),
            "other"
        );
        assert!(
            progress
                .events
                .iter()
                .any(|event| event.contains(".config/app"))
        );
        assert!(
            !progress
                .events
                .iter()
                .any(|event| event.contains(".config/other"))
        );
    }

    #[test]
    fn scoped_backup_deletes_only_entries_under_scope() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::create_dir_all(home.join(".config/other")).unwrap();
        let app_config = home.join(".config/app/config.toml");
        let other_config = home.join(".config/other/config.toml");
        fs::write(&app_config, "one").unwrap();
        fs::write(&other_config, "other").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/app".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        config.paths.push(PathConfig {
            src: "~/.config/other".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();
        fs::remove_file(&app_config).unwrap();

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                scopes: vec![app_config],
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert_eq!(report.deleted, 1);
        assert_eq!(report.visited, 0);
        assert!(
            !repo
                .path()
                .join("files/home/.config/app/config.toml")
                .exists()
        );
        assert!(
            repo.path()
                .join("files/home/.config/other/config.toml")
                .exists()
        );

        let index = Index::read(&repo.path().join("metadata/index.json")).unwrap();
        assert!(
            !index
                .entries
                .iter()
                .any(|entry| entry.source == "~/.config/app/config.toml")
        );
        assert!(
            index
                .entries
                .iter()
                .any(|entry| entry.source == "~/.config/other/config.toml")
        );
    }

    #[cfg(unix)]
    #[test]
    fn scoped_backup_runs_only_matching_custom_backup_commands() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let one_state = home.join(".config/one/state.txt");
        let two_state = home.join(".config/two/state.txt");

        let mut config = Config::default();
        config.custom_backups.push(CustomBackupConfig {
            name: "one".to_string(),
            backup_command: Some(write_home_file_command(".config/one/state.txt", "one")),
            restore_command: None,
            paths: vec![PathConfig {
                src: "~/.config/one/state.txt".to_string(),
                include: Vec::new(),
                exclude: Vec::new(),
                follow_symlink: true,
                include_binary_file: false,
                force: false,
                encrypt: false,
            }],
            ..CustomBackupConfig::default()
        });
        config.custom_backups.push(CustomBackupConfig {
            name: "two".to_string(),
            backup_command: Some(write_home_file_command(".config/two/state.txt", "two")),
            restore_command: None,
            paths: vec![PathConfig {
                src: "~/.config/two/state.txt".to_string(),
                include: Vec::new(),
                exclude: Vec::new(),
                follow_symlink: true,
                include_binary_file: false,
                force: false,
                encrypt: false,
            }],
            ..CustomBackupConfig::default()
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                scopes: vec![one_state],
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(
            report
                .actions
                .iter()
                .any(|action| action.starts_with("run custom backup one:"))
        );
        assert!(
            !report
                .actions
                .iter()
                .any(|action| action.starts_with("run custom backup two:"))
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/.config/one/state.txt")).unwrap(),
            "one"
        );
        assert!(!two_state.exists());
        assert!(
            !repo
                .path()
                .join("files/home/.config/two/state.txt")
                .exists()
        );
    }

    #[test]
    fn plaintext_same_content_rewrite_preserves_index() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        let source = home.join(".config/app/config.toml");
        fs::write(&source, "theme = 'light'\n").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/app/config.toml".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);
        let options = BackupOptions {
            no_git: true,
            ..BackupOptions::default()
        };

        let first = run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();
        assert_eq!(first.added, 1);
        let before = fs::read_to_string(repo.path().join("metadata/index.json")).unwrap();

        thread::sleep(Duration::from_millis(10));
        fs::write(&source, "theme = 'light'\n").unwrap();

        let second = run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();
        let after = fs::read_to_string(repo.path().join("metadata/index.json")).unwrap();

        assert_eq!(second.added + second.updated + second.deleted, 0);
        assert_eq!(before, after);
    }

    #[test]
    fn plaintext_file_comparison_ignores_mtime_only() {
        let previous = IndexEntry {
            source: "~/.config/app/config.toml".to_string(),
            stored: "files/home/.config/app/config.toml".to_string(),
            kind: EntryKind::File,
            sha256: Some("abc".to_string()),
            mode: Some(0o644),
            executable: false,
            encrypted: false,
            symlink_target: None,
            size: Some(10),
            modified_unix_nanos: Some(1),
        };
        let mut next = previous.clone();
        next.modified_unix_nanos = Some(2);

        assert!(file_entry_unchanged_ignoring_mtime(&previous, &next));

        next.mode = Some(0o755);
        next.executable = true;
        assert!(!file_entry_unchanged_ignoring_mtime(&previous, &next));
    }

    #[test]
    fn explicit_commit_and_push_run_even_without_new_file_changes() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let config = Config::default();
        let repo = repo_with_config(home, &config);
        let env = env_for(home);
        let git = RecordingGit::default();

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                commit: true,
                push: true,
                ..BackupOptions::default()
            },
            &git,
        )
        .unwrap();

        assert_eq!(report.added + report.updated + report.deleted, 0);
        assert_eq!(git.commits.get(), 1);
        assert_eq!(git.pushes.get(), 1);
    }

    #[test]
    fn reports_backup_progress_events() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/nvim")).unwrap();
        fs::write(home.join(".config/nvim/init.lua"), "rules").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/nvim".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);
        let mut progress = RecordingProgress::default();

        run_with_config_and_progress(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
            &mut progress,
        )
        .unwrap();

        assert!(
            progress
                .events
                .iter()
                .any(|event| event.starts_with("start "))
        );
        assert!(
            progress
                .events
                .iter()
                .any(|event| event == "phase reading metadata")
        );
        assert!(
            progress
                .events
                .iter()
                .any(|event| event.contains(".config/nvim"))
        );
        assert!(
            progress
                .events
                .iter()
                .any(|event| event.starts_with("scanned "))
        );
        assert!(
            progress
                .events
                .iter()
                .any(|event| event == "phase writing metadata/index.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn custom_backup_command_runs_before_copying_configured_outputs() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let mut config = Config::default();
        config.custom_backups.push(CustomBackupConfig {
            name: "generated".to_string(),
            backup_command: Some(write_home_file_command(
                ".config/generated/state.txt",
                "generated",
            )),
            restore_command: None,
            paths: vec![PathConfig {
                src: "~/.config/generated/state.txt".to_string(),
                include: Vec::new(),
                exclude: Vec::new(),
                follow_symlink: true,
                include_binary_file: false,
                force: false,
                encrypt: false,
            }],
            ..CustomBackupConfig::default()
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(
            report
                .actions
                .iter()
                .any(|action| action.starts_with("run custom backup generated:"))
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/.config/generated/state.txt")).unwrap(),
            "generated"
        );
    }

    #[test]
    fn dry_run_custom_backup_does_not_run_command() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let mut config = Config::default();
        config.custom_backups.push(CustomBackupConfig {
            name: "generated".to_string(),
            backup_command: Some(
                "mkdir -p ~/.config/generated && touch ~/.config/generated/state.txt".to_string(),
            ),
            restore_command: None,
            paths: vec![PathConfig {
                src: "~/.config/generated/state.txt".to_string(),
                include: Vec::new(),
                exclude: Vec::new(),
                follow_symlink: true,
                include_binary_file: false,
                force: false,
                encrypt: false,
            }],
            ..CustomBackupConfig::default()
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                dry_run: true,
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(!home.join(".config/generated/state.txt").exists());
        assert!(
            report
                .actions
                .iter()
                .any(|action| action.starts_with("would run custom backup generated:"))
        );
    }

    #[test]
    fn excludes_default_secret_like_files() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("app")).unwrap();
        fs::write(home.join("app/config.toml"), "ok").unwrap();
        fs::write(home.join("app/.env"), "token=secret").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/app".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(repo.path().join("files/home/app/config.toml").exists());
        assert!(!repo.path().join("files/home/app/.env").exists());
    }

    #[test]
    fn include_rules_allow_only_selected_files_under_large_app_dirs() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".codex/rules")).unwrap();
        fs::create_dir_all(home.join(".codex/sessions/2026")).unwrap();
        fs::create_dir_all(home.join(".codex/cache")).unwrap();
        fs::write(home.join(".codex/AGENTS.md"), "agents").unwrap();
        fs::write(home.join(".codex/rules/default.rules"), "rules").unwrap();
        fs::write(home.join(".codex/sessions/2026/session.jsonl"), "session").unwrap();
        fs::write(home.join(".codex/cache/item.json"), "cache").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.codex".to_string(),
            include: vec!["AGENTS.md".to_string(), "rules/**".to_string()],
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(repo.path().join("files/home/.codex/AGENTS.md").exists());
        assert!(
            repo.path()
                .join("files/home/.codex/rules/default.rules")
                .exists()
        );
        assert!(
            !repo
                .path()
                .join("files/home/.codex/sessions/2026/session.jsonl")
                .exists()
        );
        assert!(
            !repo
                .path()
                .join("files/home/.codex/cache/item.json")
                .exists()
        );
    }

    #[test]
    fn include_rules_are_relative_not_recursive_basenames() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/jj/repos/repo-id")).unwrap();
        fs::write(home.join(".config/jj/config.toml"), "root").unwrap();
        fs::write(home.join(".config/jj/repos/repo-id/config.toml"), "nested").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/jj".to_string(),
            include: vec!["config.toml".to_string()],
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(
            repo.path()
                .join("files/home/.config/jj/config.toml")
                .exists()
        );
        assert!(
            !repo
                .path()
                .join("files/home/.config/jj/repos/repo-id/config.toml")
                .exists()
        );
    }

    #[test]
    fn skips_binary_files_by_default() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("assets")).unwrap();
        fs::write(home.join("assets/icon.bin"), [0_u8, 159, 146, 150]).unwrap();
        fs::write(home.join("assets/config.toml"), "theme = 'light'").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/assets".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(
            report
                .actions
                .iter()
                .any(|action| action.contains("skip binary"))
        );
        assert!(repo.path().join("files/home/assets/config.toml").exists());
        assert!(!repo.path().join("files/home/assets/icon.bin").exists());
    }

    #[test]
    fn skips_binary_before_oversized() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("assets")).unwrap();
        fs::write(home.join("assets/big.bin"), [0_u8, 1, 2, 3]).unwrap();

        let mut config = Config::default();
        config.policy.max_file_size = "3B".to_string();
        config.paths.push(PathConfig {
            src: "~/assets".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(
            report
                .actions
                .iter()
                .any(|action| action.contains("skip binary"))
        );
        assert!(
            !report
                .actions
                .iter()
                .any(|action| action.contains("skip oversized"))
        );
    }

    #[test]
    fn force_path_bypasses_default_exclude_binary_and_size_checks() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("logs")).unwrap();
        fs::write(home.join("logs/app.log"), [0_u8, 1, 2, 3]).unwrap();

        let mut config = Config::default();
        config.policy.max_file_size = "3B".to_string();
        config.paths.push(PathConfig {
            src: "~/logs/app.log".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: true,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert_eq!(report.added, 1);
        assert!(
            !report
                .actions
                .iter()
                .any(|action| action.starts_with("skip "))
        );
        assert!(repo.path().join("files/home/logs/app.log").is_file());
    }

    #[test]
    fn include_binary_file_allows_included_binary_files() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        let binary = [0_u8, 159, 146, 150];
        fs::create_dir_all(home.join("app/assets")).unwrap();
        fs::write(home.join("app/assets/icon.png"), binary).unwrap();
        fs::write(home.join("app/state.db"), [0_u8, 1, 2, 3]).unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/app".to_string(),
            include: vec!["assets/**".to_string()],
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: true,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert_eq!(
            fs::read(repo.path().join("files/home/app/assets/icon.png")).unwrap(),
            binary
        );
        assert!(!repo.path().join("files/home/app/state.db").exists());
    }

    #[test]
    fn prunes_orphan_files_under_managed_store() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/nvim")).unwrap();
        fs::write(home.join(".config/nvim/init.lua"), "set number").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.config/nvim".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        fs::create_dir_all(repo.path().join("files/home/.codex/cache")).unwrap();
        fs::write(
            repo.path().join("files/home/.codex/cache/stale.json"),
            "stale",
        )
        .unwrap();
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(
            report
                .actions
                .iter()
                .any(|action| action == "delete orphan files/home/.codex/cache/stale.json")
        );
        assert!(
            !repo
                .path()
                .join("files/home/.codex/cache/stale.json")
                .exists()
        );
        assert!(
            repo.path()
                .join("files/home/.config/nvim/init.lua")
                .exists()
        );
    }

    #[test]
    fn deletes_stale_backup_files_by_default() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/tool"), "one").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/bin".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let options = BackupOptions {
            no_git: true,
            ..BackupOptions::default()
        };
        run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();
        fs::remove_file(home.join("bin/tool")).unwrap();
        let report = run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();

        assert!(report.deleted >= 1);
        assert!(!repo.path().join("files/home/bin/tool").exists());
    }

    #[test]
    fn encrypted_backup_writes_age_file_without_plaintext() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".config/app")).unwrap();
        fs::write(home.join(".config/app/token.json"), "secret-token").unwrap();

        let repo = tempdir().unwrap();
        init::run(&init::InitOptions {
            target: repo.path().to_path_buf(),
            with_defaults: false,
            no_git: true,
            force: false,
        })
        .unwrap();

        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        fs::write(repo.path().join("recipients.txt"), recipient.to_string()).unwrap();

        let mut config = Config::default();
        config.encryption.recipients_file = Some("recipients.txt".to_string());
        config.paths.push(PathConfig {
            src: "~/.config/app/token.json".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: true,
        });
        fs::write(
            repo.path().join("dotr.toml"),
            toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let env = env_for(home);
        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        let encrypted =
            fs::read(repo.path().join("files/home/.config/app/token.json.age")).unwrap();
        assert!(
            !encrypted
                .windows(b"secret-token".len())
                .any(|window| window == b"secret-token")
        );

        let index = Index::read(&repo.path().join("metadata/index.json")).unwrap();
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.source.ends_with("token.json"))
            .unwrap();
        assert!(entry.encrypted);
        assert!(entry.sha256.is_some());
        assert_ne!(entry.sha256.as_deref(), Some("secret-token"));

        fs::write(
            repo.path().join("identity.txt"),
            identity.to_string().expose_secret(),
        )
        .unwrap();
    }

    #[test]
    fn encrypting_previous_plaintext_backup_warns_about_git_history() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::write(home.join(".ssh/config"), "Host internal").unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/.ssh/config".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);
        let options = BackupOptions {
            no_git: true,
            ..BackupOptions::default()
        };

        run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();
        assert!(repo.path().join("files/home/.ssh/config").is_file());

        let identity = age::x25519::Identity::generate();
        fs::write(
            repo.path().join("recipients"),
            identity.to_public().to_string(),
        )
        .unwrap();
        config.encryption.recipients_file = Some("recipients".to_string());
        config.paths[0].encrypt = true;
        fs::write(
            repo.path().join("dotr.toml"),
            toml::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let report = run_with_config(repo.path(), &env, &config, &options, &CommandGit).unwrap();

        assert!(report.actions.iter().any(|action| {
            action.contains("warning:")
                && action.contains("previously backed up as plaintext")
                && action.contains("Git history")
        }));
        assert!(repo.path().join("files/home/.ssh/config.age").is_file());
        assert!(!repo.path().join("files/home/.ssh/config").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_followed_by_default() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("links")).unwrap();
        fs::write(home.join("outside"), "outside").unwrap();
        unix_fs::symlink(home.join("outside"), home.join("links/current")).unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/links".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(repo.path().join("files/home/links/current")).unwrap(),
            "outside"
        );
        let index = Index::read(&repo.path().join("metadata/index.json")).unwrap();
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.source.ends_with("links/current"))
            .unwrap();
        assert_eq!(entry.kind, EntryKind::File);
        assert!(entry.symlink_target.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlinks_are_skipped_when_following_symlinks() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("links")).unwrap();
        unix_fs::symlink(home.join("missing"), home.join("links/current")).unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/links".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        let report = run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(report.skipped >= 1);
        assert!(
            report
                .actions
                .iter()
                .any(|action| action.contains("skip unreadable"))
        );
        assert!(!repo.path().join("files/home/links/current").exists());
    }

    #[cfg(unix)]
    #[test]
    fn follow_symlink_false_records_symlinks_not_followed() {
        let home_dir = tempdir().unwrap();
        let home = home_dir.path();
        fs::create_dir_all(home.join("links")).unwrap();
        fs::write(home.join("outside"), "outside").unwrap();
        unix_fs::symlink(home.join("outside"), home.join("links/current")).unwrap();

        let mut config = Config::default();
        config.paths.push(PathConfig {
            src: "~/links".to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: false,
            include_binary_file: false,
            force: false,
            encrypt: false,
        });
        let repo = repo_with_config(home, &config);
        let env = env_for(home);

        run_with_config(
            repo.path(),
            &env,
            &config,
            &BackupOptions {
                no_git: true,
                ..BackupOptions::default()
            },
            &CommandGit,
        )
        .unwrap();

        assert!(!repo.path().join("files/home/links/current").exists());
        let index = Index::read(&repo.path().join("metadata/index.json")).unwrap();
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.source.ends_with("links/current"))
            .unwrap();
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert!(entry.symlink_target.is_some());
    }
}

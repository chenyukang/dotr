use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

pub const DEFAULT_STORE_DIR: &str = ".";
pub const CONFIG_FILE_NAME: &str = "dotr.toml";
pub const INDEX_FILE: &str = "metadata/index.json";

pub const STARTER_PATHS: &[&str] = &[
    "~/.bash_profile",
    "~/.bashrc",
    "~/.profile",
    "~/.zprofile",
    "~/.zshenv",
    "~/.zshrc",
    "~/.inputrc",
    "~/.editorconfig",
    "~/.gitconfig",
    "~/.gitignore_global",
    "~/.ssh/config",
    "~/.gnupg/gpg.conf",
    "~/.gnupg/gpg-agent.conf",
    "~/.tmux.conf",
    "~/.vimrc",
    "~/.ideavimrc",
    "~/.config/git",
    "~/.config/fish",
    "~/.config/nvim",
    "~/.config/helix",
    "~/.config/starship.toml",
    "~/.config/alacritty",
    "~/.config/ghostty",
    "~/.config/kitty",
    "~/.config/wezterm",
    "~/.config/bat",
    "~/.config/direnv",
    "~/.cargo/config.toml",
];

pub const DEFAULT_EXCLUDES: &[&str] = &[
    "**/.git/**",
    "**/.DS_Store",
    "**/__pycache__/**",
    "**/.cache/**",
    "**/cache",
    "**/cache/**",
    "**/caches",
    "**/caches/**",
    "**/.tmp/**",
    "**/tmp",
    "**/tmp/**",
    "**/temp",
    "**/temp/**",
    "**/log",
    "**/log/**",
    "**/logs",
    "**/logs/**",
    "**/sessions",
    "**/sessions/**",
    "**/archived_sessions",
    "**/archived_sessions/**",
    "**/browser/sessions",
    "**/browser/sessions/**",
    "**/shell_snapshots",
    "**/shell_snapshots/**",
    "**/worktrees",
    "**/worktrees/**",
    "**/targets",
    "**/targets/**",
    "**/target",
    "**/target/**",
    "**/generated_images",
    "**/generated_images/**",
    "**/ambient-suggestions",
    "**/ambient-suggestions/**",
    "**/node_repl",
    "**/node_repl/**",
    "**/vendor_imports",
    "**/vendor_imports/**",
    "**/plugins/cache",
    "**/plugins/cache/**",
    "**/node_modules/**",
    "**/.venv/**",
    "**/venv/**",
    "**/.env",
    "**/.env.*",
    "**/auth.json",
    "**/credentials.json",
    "**/*.pem",
    "**/*.key",
    "**/*.db",
    "**/*.sqlite",
    "**/*.sqlite-*",
    "**/*.sqlite3",
    "**/*.sqlite3-*",
    "**/*.log",
    "**/*.pyc",
    "**/*.tmp",
    "**/*.tmp-*",
    "**/.*.tmp-*",
    "**/*.bak",
    "**/*.bak.*",
    "**/.*.bak",
    "**/.*.bak*",
    "**/references/llms*.md",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub repository: RepositoryConfig,
    #[serde(default, rename = "path", skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathConfig>,
    #[serde(default)]
    pub watch: WatchConfig,
    #[serde(default, skip_serializing_if = "is_default_daemon_config")]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub encryption: EncryptionConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    #[serde(default = "default_store")]
    pub store: PathBuf,
}

impl Default for RepositoryConfig {
    fn default() -> Self {
        Self {
            root: None,
            store: PathBuf::from(DEFAULT_STORE_DIR),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathConfig {
    pub src: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub follow_symlink: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_binary_file: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub encrypt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WatchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_debounce_secs")]
    pub debounce_secs: u64,
    #[serde(default = "default_min_backup_interval_secs")]
    pub min_backup_interval_secs: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            debounce_secs: default_debounce_secs(),
            min_backup_interval_secs: default_min_backup_interval_secs(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_log: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_log: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitConfig {
    #[serde(default)]
    pub auto_commit: bool,
    #[serde(default)]
    pub auto_push: bool,
    #[serde(default = "default_commit_message")]
    pub commit_message: String,
    #[serde(default)]
    pub include_unrelated: bool,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            auto_commit: false,
            auto_push: false,
            commit_message: default_commit_message(),
            include_unrelated: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptionConfig {
    #[serde(default = "default_encryption_backend")]
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipients_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            backend: default_encryption_backend(),
            recipients_file: None,
            identity: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyConfig {
    #[serde(default = "default_max_file_size")]
    pub max_file_size: String,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            max_file_size: default_max_file_size(),
        }
    }
}

impl Config {
    pub fn load(repo_root: &Path) -> Result<Self> {
        let path = config_path(repo_root);
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub fn write(&self, repo_root: &Path) -> Result<()> {
        let path = config_path(repo_root);
        let toml = toml::to_string_pretty(self).context("failed to serialize config")?;
        fs::write(&path, toml).with_context(|| format!("failed to write {}", path.display()))
    }

    pub fn store_dir(&self, repo_root: &Path) -> PathBuf {
        if self.repository.store.is_absolute() {
            self.repository.store.clone()
        } else {
            repo_root.join(&self.repository.store)
        }
    }

    pub fn max_file_size_bytes(&self) -> Result<u64> {
        parse_size(&self.policy.max_file_size)
    }

    pub fn has_encrypted_paths(&self) -> bool {
        self.paths.iter().any(|path| path.encrypt)
    }

    pub fn starter(with_defaults: bool) -> Self {
        let mut config = Self::default();

        if with_defaults {
            config.paths = starter_paths();
        }

        config
    }
}

pub fn starter_paths() -> Vec<PathConfig> {
    STARTER_PATHS
        .iter()
        .map(|src| PathConfig {
            src: (*src).to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
            follow_symlink: true,
            include_binary_file: false,
            encrypt: false,
        })
        .collect()
}

pub fn config_path(repo_root: &Path) -> PathBuf {
    repo_root.join(CONFIG_FILE_NAME)
}

pub fn index_path(store_dir: &Path) -> PathBuf {
    store_dir.join(INDEX_FILE)
}

pub fn default_exclude_set() -> Result<GlobSet> {
    globset_from_patterns(DEFAULT_EXCLUDES.iter().copied())
}

pub fn globset_from_patterns<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).with_context(|| format!("invalid glob: {pattern}"))?);
    }
    builder.build().context("failed to build glob set")
}

pub fn parse_size(raw: &str) -> Result<u64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("size must not be empty");
    }

    let split_at = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split_at);
    let value: u64 = number
        .parse()
        .with_context(|| format!("invalid size value: {raw}"))?;

    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        other => bail!("unsupported size unit in {raw}: {other}"),
    };

    value
        .checked_mul(multiplier)
        .with_context(|| format!("size overflows u64: {raw}"))
}

fn default_store() -> PathBuf {
    PathBuf::from(DEFAULT_STORE_DIR)
}

fn default_debounce_secs() -> u64 {
    30
}

fn default_min_backup_interval_secs() -> u64 {
    900
}

fn default_commit_message() -> String {
    "chore(dotr): automated backup".to_string()
}

fn default_encryption_backend() -> String {
    "age".to_string()
}

fn default_max_file_size() -> String {
    "20MiB".to_string()
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_default_daemon_config(value: &DaemonConfig) -> bool {
    value == &DaemonConfig::default()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_sizes() {
        assert_eq!(parse_size("20MiB").unwrap(), 20 * 1024 * 1024);
        assert_eq!(parse_size("7kb").unwrap(), 7 * 1024);
        assert_eq!(parse_size("42").unwrap(), 42);
        assert!(parse_size("1TB").is_err());
    }

    #[test]
    fn default_excludes_match_secret_like_files() {
        let set = default_exclude_set().unwrap();

        assert!(set.is_match("app/.env"));
        assert!(set.is_match("app/private.key"));
        assert!(set.is_match("app/references/llms-full.md"));
        assert!(set.is_match(".codex/cache/item.json"));
        assert!(set.is_match(".codex/sessions/2026/05/session.jsonl"));
        assert!(set.is_match(".codex/plugins/cache/openai-bundled/tool"));
        assert!(set.is_match(".codex/logs_2.sqlite-wal"));
        assert!(set.is_match(".codex/auth.json"));
        assert!(set.is_match(".codex/generated_images/abc/image.png"));
        assert!(set.is_match(".codex/worktrees/abcd/repo/Cargo.toml"));
        assert!(!set.is_match("app/config.toml"));
        assert!(!set.is_match(".codex/AGENTS.md"));
        assert!(!set.is_match(".codex/config.toml"));
        assert!(!set.is_match(".codex/rules/default.rules"));
    }

    #[test]
    fn loads_array_of_paths() {
        let config: Config = toml::from_str(
            r#"
            [[path]]
            src = "~/.config/nvim"

            [[path]]
            src = "/Library/example"
            encrypt = true
            follow_symlink = false
            include_binary_file = true
            include = ["config/**"]
            exclude = ["**/tmp/**"]

            [daemon]
            log_path = "~/logs/dotr-watch.jsonl"
            log_level = "debug"
            "#,
        )
        .unwrap();

        assert_eq!(config.paths.len(), 2);
        assert!(config.paths[1].encrypt);
        assert!(!config.paths[1].follow_symlink);
        assert!(config.paths[1].include_binary_file);
        assert_eq!(config.paths[1].include, vec!["config/**"]);
        assert_eq!(config.watch.debounce_secs, 30);
        assert_eq!(
            config.daemon.log_path.as_deref(),
            Some("~/logs/dotr-watch.jsonl")
        );
        assert_eq!(config.daemon.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn starter_paths_are_generic_and_not_personal() {
        let config = Config::starter(true);
        let sources = config
            .paths
            .iter()
            .map(|path| path.src.as_str())
            .collect::<Vec<_>>();

        assert!(sources.contains(&"~/.zshrc"));
        assert!(sources.contains(&"~/.gitconfig"));
        assert!(sources.contains(&"~/.ssh/config"));
        assert!(sources.contains(&"~/.config/nvim"));
        assert!(!sources.contains(&"~/projects/bin"));
        assert!(!sources.contains(&"~/.custom-personal-tool"));
        assert!(config.paths.iter().all(|path| path.follow_symlink));
        assert!(config.paths.iter().all(|path| !path.include_binary_file));
    }
}

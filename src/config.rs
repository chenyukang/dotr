use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Deserializer, Serialize};

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
    "~/.zpreztorc",
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
    #[serde(default, rename = "path_set", skip_serializing_if = "Vec::is_empty")]
    pub path_sets: Vec<PathSetConfig>,
    #[serde(
        default,
        rename = "custom_backup",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub custom_backups: Vec<CustomBackupConfig>,
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
    pub force: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub encrypt: bool,
    #[serde(
        default,
        deserialize_with = "deserialize_normalize_rules",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub normalize: Vec<NormalizeRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NormalizeRule {
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<NormalizeFormat>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drop_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NormalizeFormat {
    Toml,
    Json,
    Text,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathSetConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<PathSetItem>,
}

impl PathSetConfig {
    pub fn expand(&self) -> Vec<PathConfig> {
        expand_path_set_items(self.base.as_deref(), &self.items)
    }

    pub fn remove_matching(
        &mut self,
        repo_root: &Path,
        env: &crate::environment::Environment,
        source: &Path,
    ) -> bool {
        let before = self.items.len();
        let base = self.base.as_deref();
        self.items.retain(|item| {
            let path = item.to_path_config(base);
            let configured = crate::paths::absolutize(&env.expand_tilde(&path.src), repo_root);
            normalize_path_for_config(&configured) != normalize_path_for_config(source)
        });
        before != self.items.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PathSetItem {
    Src(String),
    Config(PathConfig),
}

impl PathSetItem {
    fn to_path_config(&self, base: Option<&str>) -> PathConfig {
        match self {
            Self::Src(src) => path_config(&join_path_set_base(base, src)),
            Self::Config(path) => PathConfig {
                src: join_path_set_base(base, &path.src),
                ..path.clone()
            },
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomBackupConfig {
    pub name: String,
    #[serde(
        default,
        rename = "backup",
        alias = "backup_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub backup_command: Option<String>,
    #[serde(
        default,
        rename = "restore",
        alias = "restore_command",
        skip_serializing_if = "Option::is_none"
    )]
    pub restore_command: Option<String>,
    #[serde(default, rename = "path", skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_sets: Vec<PathSetConfig>,
    #[serde(default, rename = "paths", skip_serializing_if = "Vec::is_empty")]
    pub simple_paths: Vec<PathSetItem>,
}

impl CustomBackupConfig {
    pub fn path_configs(&self) -> Vec<PathConfig> {
        let mut paths = self.paths.clone();
        paths.extend(expand_path_set_items(None, &self.simple_paths));
        paths.extend(self.path_sets.iter().flat_map(PathSetConfig::expand));
        paths
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WatchConfig {
    #[serde(default = "default_debounce_secs")]
    pub debounce_secs: u64,
    #[serde(
        default = "default_backup_interval_secs",
        alias = "min_backup_interval_secs"
    )]
    pub backup_interval_secs: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_secs: default_debounce_secs(),
            backup_interval_secs: default_backup_interval_secs(),
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
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    #[serde(default)]
    pub auto_commit: bool,
    #[serde(default)]
    pub auto_push: bool,
    #[serde(default = "default_commit_message")]
    pub commit_message: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            auto_commit: false,
            auto_push: false,
            commit_message: default_commit_message(),
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
        self.path_configs().iter().any(|path| path.encrypt)
    }

    pub fn path_configs(&self) -> Vec<PathConfig> {
        let mut paths = self.paths.clone();
        paths.extend(self.path_sets.iter().flat_map(PathSetConfig::expand));
        paths.extend(
            self.custom_backups
                .iter()
                .flat_map(CustomBackupConfig::path_configs),
        );
        paths
    }

    pub fn starter(with_defaults: bool) -> Self {
        let mut config = Self::default();
        config.git.auto_commit = true;

        if with_defaults {
            config.path_sets = starter_path_sets();
            config.custom_backups = starter_custom_backups();
        }

        config
    }
}

pub fn starter_paths() -> Vec<PathConfig> {
    starter_path_sets()
        .iter()
        .flat_map(PathSetConfig::expand)
        .collect()
}

pub fn starter_path_sets() -> Vec<PathSetConfig> {
    let mut items = STARTER_PATHS
        .iter()
        .map(|src| PathSetItem::Src(relative_to_home(src).to_string()))
        .collect::<Vec<_>>();

    items.extend([
        home_item_with_includes("~/.config/alacritty", &["alacritty.toml"]),
        home_item_with_includes("~/.config/atuin", &["config.toml"]),
        home_item_with_includes("~/.config/fastfetch", &["config.jsonc"]),
        home_item_with_includes("~/.config/fresh", &["config.json"]),
        home_item_with_includes("~/.config/gh", &["config.yml"]),
        home_item_with_includes("~/.config/gh-dash", &["config.yml"]),
        home_item_with_includes("~/.config/jj", &["config.toml"]),
        PathSetItem::Src(".jjconfig.toml".to_string()),
        home_item_with_includes("~/.config/karabiner", &["karabiner.json"]),
        home_item_with_includes("~/.config/lvim", &["config.lua"]),
        home_item_with_includes("~/.config/mise", &["config.toml"]),
        home_item_with_includes("~/.config/openspeak", &["config.toml"]),
        home_item_with_includes("~/.config/ripasso", &["settings.toml"]),
        home_item_with_includes(
            "~/.config/yazi",
            &["yazi.toml", "keymap.toml", "theme.toml"],
        ),
        home_item_with_includes(
            "~/.config/zed",
            &["settings.json", "keymap.json", "tasks.json", "snippets/**"],
        ),
        home_item_with_includes("~/.config/zellij", &["config.kdl"]),
        home_item_with_includes("~/.warp", &["keybindings.yaml"]),
        home_item_with_includes("~/.hammerspoon", &["init.lua"]),
    ]);

    vec![PathSetConfig {
        base: Some("~".to_string()),
        items,
    }]
}

pub fn starter_custom_backups() -> Vec<CustomBackupConfig> {
    vec![
        CustomBackupConfig {
            name: "homebrew".to_string(),
            backup_command: Some(
                "if command -v brew >/dev/null 2>&1; then mkdir -p ~/.config/homebrew && brew bundle dump --file ~/.config/homebrew/Brewfile --force; else echo 'dotr: skipping homebrew backup; brew not found' >&2; fi"
                    .to_string(),
            ),
            restore_command: Some(
                "if command -v brew >/dev/null 2>&1 && [ -f ~/.config/homebrew/Brewfile ]; then brew bundle --file ~/.config/homebrew/Brewfile; else echo 'dotr: skipping homebrew restore; brew or Brewfile not found' >&2; fi"
                    .to_string(),
            ),
            paths: Vec::new(),
            path_sets: Vec::new(),
            simple_paths: vec![PathSetItem::Src("~/.config/homebrew/Brewfile".to_string())],
        },
        CustomBackupConfig {
            name: "vscode".to_string(),
            backup_command: Some(
                "if command -v code >/dev/null 2>&1; then mkdir -p ~/.config/vscode && code --list-extensions > ~/.config/vscode/extensions.txt; else echo 'dotr: skipping VS Code extension backup; code not found' >&2; fi"
                    .to_string(),
            ),
            restore_command: Some(
                "if command -v code >/dev/null 2>&1 && [ -f ~/.config/vscode/extensions.txt ]; then xargs -n 1 code --install-extension < ~/.config/vscode/extensions.txt; else echo 'dotr: skipping VS Code extension restore; code or extensions.txt not found' >&2; fi"
                    .to_string(),
            ),
            paths: Vec::new(),
            path_sets: vec![
                PathSetConfig {
                    base: Some("~/Library/Application Support/Code/User".to_string()),
                    items: vscode_user_items(),
                },
                PathSetConfig {
                    base: Some("~/.config/Code/User".to_string()),
                    items: vscode_user_items(),
                },
            ],
            simple_paths: vec![PathSetItem::Src("~/.config/vscode/extensions.txt".to_string())],
        },
    ]
}

pub fn path_config(src: &str) -> PathConfig {
    PathConfig {
        src: src.to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
        follow_symlink: true,
        include_binary_file: false,
        force: false,
        encrypt: false,
        normalize: Vec::new(),
    }
}

pub fn path_config_with_includes(src: &str, include: &[&str]) -> PathConfig {
    PathConfig {
        include: include
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect(),
        ..path_config(src)
    }
}

fn relative_to_home(src: &str) -> &str {
    src.strip_prefix("~/").unwrap_or(src)
}

fn home_item_with_includes(src: &str, include: &[&str]) -> PathSetItem {
    PathSetItem::Config(PathConfig {
        src: relative_to_home(src).to_string(),
        include: include
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect(),
        ..path_config("")
    })
}

fn vscode_user_items() -> Vec<PathSetItem> {
    [
        "settings.json",
        "keybindings.json",
        "tasks.json",
        "snippets",
    ]
    .iter()
    .map(|src| PathSetItem::Src((*src).to_string()))
    .collect()
}

fn expand_path_set_items(base: Option<&str>, items: &[PathSetItem]) -> Vec<PathConfig> {
    items.iter().map(|item| item.to_path_config(base)).collect()
}

fn join_path_set_base(base: Option<&str>, src: &str) -> String {
    if src.starts_with('/') || src == "~" || src.starts_with("~/") {
        return src.to_string();
    }

    let Some(base) = base.filter(|base| !base.is_empty()) else {
        return src.to_string();
    };
    let src = src.trim_start_matches('/');
    if base == "/" {
        format!("/{src}")
    } else {
        format!("{}/{}", base.trim_end_matches('/'), src)
    }
}

fn normalize_path_for_config(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(Path::new("/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
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

fn default_backup_interval_secs() -> u64 {
    300
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

fn deserialize_normalize_rules<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<NormalizeRule>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(NormalizeRule),
        Many(Vec<NormalizeRule>),
    }

    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(rule) => vec![rule],
        OneOrMany::Many(rules) => rules,
    })
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
            force = true
            include = ["config/**"]
            exclude = ["**/tmp/**"]

            [daemon]
            log_path = "~/logs/dotr-watch.log"
            log_level = "debug"

            [[custom_backup]]
            name = "packages"
            backup_command = "pkg dump > ~/.config/packages/list.txt"
            restore_command = "pkg restore < ~/.config/packages/list.txt"

            [[custom_backup.path]]
            src = "~/.config/packages/list.txt"
            "#,
        )
        .unwrap();

        assert_eq!(config.paths.len(), 2);
        assert_eq!(config.custom_backups.len(), 1);
        assert_eq!(config.custom_backups[0].name, "packages");
        assert_eq!(
            config.custom_backups[0].paths[0].src,
            "~/.config/packages/list.txt"
        );
        assert!(config.paths[1].encrypt);
        assert!(!config.paths[1].follow_symlink);
        assert!(config.paths[1].include_binary_file);
        assert!(config.paths[1].force);
        assert_eq!(config.paths[1].include, vec!["config/**"]);
        assert_eq!(config.watch.debounce_secs, 30);
        assert_eq!(config.watch.backup_interval_secs, 300);
        assert_eq!(
            config.daemon.log_path.as_deref(),
            Some("~/logs/dotr-watch.log")
        );
        assert_eq!(config.daemon.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn path_normalize_accepts_inline_table() {
        let config: Config = toml::from_str(
            r#"
            [[path]]
            src = "~/.codex"
            include = ["config.toml"]
            normalize = { match = "config.toml", drop_paths = ["marketplaces.*.last_updated"] }
            "#,
        )
        .unwrap();

        assert_eq!(config.paths[0].normalize.len(), 1);
        let rule = &config.paths[0].normalize[0];
        assert_eq!(rule.match_path.as_deref(), Some("config.toml"));
        assert_eq!(rule.format, None);
        assert_eq!(rule.drop_paths, vec!["marketplaces.*.last_updated"]);
    }

    #[test]
    fn path_set_item_normalize_expands_with_base() {
        let config: Config = toml::from_str(
            r#"
            [[path_set]]
            base = "~"
            items = [
              { src = ".codex", include = ["config.toml"], normalize = { match = "config.toml", drop_paths = ["marketplaces.*.last_updated"] } },
            ]
            "#,
        )
        .unwrap();

        let paths = config.path_configs();
        assert_eq!(paths[0].src, "~/.codex");
        assert_eq!(paths[0].include, vec!["config.toml"]);
        assert_eq!(paths[0].normalize.len(), 1);
        assert_eq!(
            paths[0].normalize[0].drop_paths,
            vec!["marketplaces.*.last_updated"]
        );
    }

    #[test]
    fn watch_accepts_legacy_min_backup_interval_name() {
        let config: Config = toml::from_str(
            r#"
            [watch]
            min_backup_interval_secs = 42
            "#,
        )
        .unwrap();

        assert_eq!(config.watch.backup_interval_secs, 42);

        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("backup_interval_secs = 42"));
        assert!(!serialized.contains("min_backup_interval_secs"));
    }

    #[test]
    fn watch_enabled_is_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [watch]
            enabled = true
            "#,
        )
        .unwrap_err();

        assert!(err.to_string().contains("unknown field `enabled`"));
    }

    #[test]
    fn path_sets_expand_relative_items_against_base() {
        let config: Config = toml::from_str(
            r#"
            [[path_set]]
            base = "~"
            items = [
              ".zshrc",
              { src = ".config/yazi", include = ["yazi.toml", "keymap.toml"] },
              { src = "/Library/example", follow_symlink = false },
            ]
            "#,
        )
        .unwrap();

        let paths = config.path_configs();
        assert_eq!(paths[0].src, "~/.zshrc");
        assert_eq!(paths[1].src, "~/.config/yazi");
        assert_eq!(paths[1].include, vec!["yazi.toml", "keymap.toml"]);
        assert_eq!(paths[2].src, "/Library/example");
        assert!(!paths[2].follow_symlink);
    }

    #[test]
    fn compact_custom_backup_paths_expand() {
        let config: Config = toml::from_str(
            r#"
            [[custom_backup]]
            name = "vscode"
            backup = "code --list-extensions > ~/.config/vscode/extensions.txt"
            restore = "xargs -n 1 code --install-extension < ~/.config/vscode/extensions.txt"
            paths = ["~/.config/vscode/extensions.txt"]
            path_sets = [
              { base = "~/.config/Code/User", items = [
                "settings.json",
                { src = "snippets", include_binary_file = true },
              ] },
            ]
            "#,
        )
        .unwrap();

        let custom = &config.custom_backups[0];
        assert_eq!(
            custom.backup_command.as_deref(),
            Some("code --list-extensions > ~/.config/vscode/extensions.txt")
        );
        assert_eq!(
            custom.restore_command.as_deref(),
            Some("xargs -n 1 code --install-extension < ~/.config/vscode/extensions.txt")
        );

        let paths = custom.path_configs();
        assert_eq!(paths[0].src, "~/.config/vscode/extensions.txt");
        assert_eq!(paths[1].src, "~/.config/Code/User/settings.json");
        assert_eq!(paths[2].src, "~/.config/Code/User/snippets");
        assert!(paths[2].include_binary_file);
    }

    #[test]
    fn starter_paths_are_generic_and_not_personal() {
        let config = Config::starter(true);
        let sources = config
            .path_configs()
            .iter()
            .map(|path| path.src.clone())
            .collect::<Vec<_>>();

        assert!(sources.contains(&"~/.zshrc".to_string()));
        assert!(sources.contains(&"~/.gitconfig".to_string()));
        assert!(sources.contains(&"~/.ssh/config".to_string()));
        assert!(sources.contains(&"~/.config/nvim".to_string()));
        assert!(sources.contains(&"~/.zpreztorc".to_string()));
        assert!(sources.contains(&"~/.config/atuin".to_string()));
        assert!(sources.contains(&"~/.config/alacritty".to_string()));
        assert!(sources.contains(&"~/.config/gh".to_string()));
        assert!(sources.contains(&"~/.config/yazi".to_string()));
        assert!(sources.contains(&"~/.config/zed".to_string()));
        assert!(!sources.contains(&"~/projects/bin".to_string()));
        assert!(!sources.contains(&"~/.custom-personal-tool".to_string()));
        let paths = config.path_configs();
        assert!(paths.iter().all(|path| path.follow_symlink));
        assert!(paths.iter().all(|path| !path.include_binary_file));
        assert!(paths.iter().all(|path| !path.force));
        assert_eq!(
            paths
                .iter()
                .find(|path| path.src == "~/.config/alacritty")
                .unwrap()
                .include,
            vec!["alacritty.toml"]
        );
        assert_eq!(
            paths
                .iter()
                .find(|path| path.src == "~/.config/gh")
                .unwrap()
                .include,
            vec!["config.yml"]
        );
        assert_eq!(
            paths
                .iter()
                .find(|path| path.src == "~/.config/zed")
                .unwrap()
                .include,
            vec!["settings.json", "keymap.json", "tasks.json", "snippets/**"]
        );
    }

    #[test]
    fn starter_custom_backups_cover_homebrew_and_vscode() {
        let config = Config::starter(true);
        let names = config
            .custom_backups
            .iter()
            .map(|custom| custom.name.as_str())
            .collect::<Vec<_>>();

        assert!(names.contains(&"homebrew"));
        assert!(names.contains(&"vscode"));

        let homebrew = config
            .custom_backups
            .iter()
            .find(|custom| custom.name == "homebrew")
            .unwrap();
        assert_eq!(
            homebrew.path_configs()[0].src,
            "~/.config/homebrew/Brewfile"
        );
        assert!(
            homebrew
                .backup_command
                .as_deref()
                .unwrap()
                .contains("brew bundle dump")
        );

        let vscode = config
            .custom_backups
            .iter()
            .find(|custom| custom.name == "vscode")
            .unwrap();
        assert!(
            vscode
                .path_configs()
                .iter()
                .any(|path| path.src == "~/Library/Application Support/Code/User/settings.json")
        );
        assert!(
            vscode
                .path_configs()
                .iter()
                .any(|path| path.src == "~/.config/vscode/extensions.txt")
        );
        assert!(
            vscode
                .restore_command
                .as_deref()
                .unwrap()
                .contains("--install-extension")
        );
    }
}

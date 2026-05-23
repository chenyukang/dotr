use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Index {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<IndexEntry>,
}

impl Default for Index {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexEntry {
    pub source: String,
    pub stored: String,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub encrypted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_nanos: Option<u128>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

impl Index {
    pub fn read(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read index {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse index {}", path.display()))
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut sorted = self.clone();
        sorted.entries.sort_by(|a, b| a.stored.cmp(&b.stored));
        let json = serde_json::to_string_pretty(&sorted).context("failed to serialize index")?;
        fs::write(path, format!("{json}\n"))
            .with_context(|| format!("failed to write index {}", path.display()))
    }

    pub fn by_stored(&self, stored: &str) -> Option<&IndexEntry> {
        self.entries.iter().find(|entry| entry.stored == stored)
    }

    pub fn stored_paths(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.stored.as_str())
    }
}

pub fn index_file(store_dir: &Path) -> PathBuf {
    store_dir.join("metadata").join("index.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_and_reads_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("backup/metadata/index.json");
        let index = Index {
            version: 1,
            entries: vec![IndexEntry {
                source: "~/.codex/AGENTS.md".to_string(),
                stored: "files/home/.codex/AGENTS.md".to_string(),
                kind: EntryKind::File,
                sha256: Some("abc".to_string()),
                mode: Some(0o644),
                executable: false,
                encrypted: false,
                symlink_target: None,
                size: Some(3),
                modified_unix_nanos: Some(1),
            }],
        };

        index.write(&path).unwrap();
        let loaded = Index::read(&path).unwrap();

        assert_eq!(loaded, index);
    }
}

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    home: PathBuf,
}

impl Environment {
    pub fn from_current() -> Result<Self> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Self::new(PathBuf::from(home))
    }

    pub fn new(home: PathBuf) -> Result<Self> {
        if !home.is_absolute() {
            bail!("home directory must be absolute: {}", home.display());
        }
        Ok(Self { home })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn expand_tilde(&self, raw: &str) -> PathBuf {
        if raw == "~" {
            return self.home.clone();
        }

        if let Some(rest) = raw.strip_prefix("~/") {
            return self.home.join(rest);
        }

        PathBuf::from(raw)
    }

    pub fn display_source(&self, path: &Path) -> String {
        if let Ok(rel) = path.strip_prefix(&self.home) {
            if rel.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rel.to_string_lossy().replace('\\', "/"));
        }

        path.to_string_lossy().replace('\\', "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn expands_home_paths() {
        let home = tempdir().unwrap();
        let env = Environment::new(home.path().to_path_buf()).unwrap();

        assert_eq!(env.expand_tilde("~"), home.path());
        assert_eq!(
            env.expand_tilde("~/projects/bin"),
            home.path().join("projects/bin")
        );
        assert_eq!(
            env.display_source(&home.path().join(".config/nvim/init.lua")),
            "~/.config/nvim/init.lua"
        );
    }
}

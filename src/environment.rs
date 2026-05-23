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
            return format!("~/{}", rel.to_string_lossy());
        }

        path.to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_paths() {
        let env = Environment::new(PathBuf::from("/tmp/home")).unwrap();

        assert_eq!(env.expand_tilde("~"), PathBuf::from("/tmp/home"));
        assert_eq!(
            env.expand_tilde("~/projects/bin"),
            PathBuf::from("/tmp/home/projects/bin")
        );
        assert_eq!(
            env.display_source(Path::new("/tmp/home/.config/nvim/init.lua")),
            "~/.config/nvim/init.lua"
        );
    }
}

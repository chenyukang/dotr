use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::environment::Environment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredRoot {
    Home,
    Absolute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPath {
    pub root: StoredRoot,
    pub relative: PathBuf,
}

impl StoredPath {
    pub fn as_index_path(&self) -> String {
        self.relative.to_string_lossy().replace('\\', "/")
    }

    pub fn is_absolute_restore(&self) -> bool {
        self.root == StoredRoot::Absolute
    }
}

pub fn absolutize(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

pub fn source_to_stored(path: &Path, env: &Environment, encrypted: bool) -> Result<StoredPath> {
    if !path.is_absolute() {
        bail!(
            "source path must be absolute after expansion: {}",
            path.display()
        );
    }

    let (root, rel_under_root) = if let Ok(rel) = path.strip_prefix(env.home()) {
        (StoredRoot::Home, rel.to_path_buf())
    } else {
        (StoredRoot::Absolute, absolute_without_root(path)?)
    };

    ensure_safe_relative(&rel_under_root)?;

    let mut relative = match root {
        StoredRoot::Home => PathBuf::from("files").join("home").join(rel_under_root),
        StoredRoot::Absolute => PathBuf::from("files").join("absolute").join(rel_under_root),
    };

    if encrypted {
        append_age_extension(&mut relative);
    }

    Ok(StoredPath { root, relative })
}

pub fn stored_index_to_target(stored: &str, env: &Environment) -> Result<(StoredRoot, PathBuf)> {
    let stored_path = Path::new(stored);
    ensure_safe_relative(stored_path)?;

    let rel = stored_path
        .strip_prefix("files/home")
        .map(|home_rel| {
            (
                StoredRoot::Home,
                env.home().join(remove_age_suffix(home_rel)),
            )
        })
        .or_else(|_| {
            stored_path.strip_prefix("files/absolute").map(|abs_rel| {
                (
                    StoredRoot::Absolute,
                    PathBuf::from("/").join(remove_age_suffix(abs_rel)),
                )
            })
        })
        .with_context(|| format!("stored path is outside dotr files roots: {stored}"))?;

    Ok(rel)
}

pub fn ensure_safe_relative(path: &Path) -> Result<()> {
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir => {}
            Component::ParentDir => bail!("path traversal is not allowed: {}", path.display()),
            Component::RootDir | Component::Prefix(_) => {
                bail!("stored path must be relative: {}", path.display())
            }
        }
    }

    Ok(())
}

pub fn is_stored_absolute(stored: &str) -> bool {
    Path::new(stored).starts_with("files/absolute")
}

fn absolute_without_root(path: &Path) -> Result<PathBuf> {
    let mut rel = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => rel.push(part),
            Component::CurDir => {}
            Component::ParentDir => bail!("absolute path contains traversal: {}", path.display()),
            Component::Prefix(_) => bail!("windows prefixes are not supported in v0"),
        }
    }

    if rel.as_os_str().is_empty() {
        bail!("cannot map filesystem root itself");
    }

    Ok(rel)
}

fn append_age_extension(path: &mut PathBuf) {
    let next_ext = match path.extension() {
        Some(ext) => format!("{}.age", ext.to_string_lossy()),
        None => "age".to_string(),
    };
    path.set_extension(next_ext);
}

fn remove_age_suffix(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.to_path_buf();
    };

    let Some(stripped) = file_name.strip_suffix(".age") else {
        return path.to_path_buf();
    };

    let mut restored = path.to_path_buf();
    restored.set_file_name(stripped);
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment {
        Environment::new(PathBuf::from("/Users/me")).unwrap()
    }

    #[test]
    fn maps_home_paths_under_files_home() {
        let mapped = source_to_stored(Path::new("/Users/me/code/bin/tool"), &env(), false).unwrap();

        assert_eq!(mapped.root, StoredRoot::Home);
        assert_eq!(mapped.as_index_path(), "files/home/code/bin/tool");
    }

    #[test]
    fn maps_absolute_paths_under_files_absolute() {
        let mapped =
            source_to_stored(Path::new("/Library/example/hello/world"), &env(), false).unwrap();

        assert_eq!(mapped.root, StoredRoot::Absolute);
        assert_eq!(
            mapped.as_index_path(),
            "files/absolute/Library/example/hello/world"
        );
    }

    #[test]
    fn encrypted_paths_get_age_suffix() {
        let mapped = source_to_stored(Path::new("/Users/me/.ssh/config"), &env(), true).unwrap();

        assert_eq!(mapped.as_index_path(), "files/home/.ssh/config.age");
    }

    #[test]
    fn rejects_path_traversal_in_stored_paths() {
        assert!(ensure_safe_relative(Path::new("files/home/../secret")).is_err());
    }

    #[test]
    fn maps_stored_paths_back_to_targets() {
        let (_, target) = stored_index_to_target("files/home/.codex/AGENTS.md", &env()).unwrap();
        assert_eq!(target, PathBuf::from("/Users/me/.codex/AGENTS.md"));

        let (_, target) = stored_index_to_target("files/absolute/Library/example", &env()).unwrap();
        assert_eq!(target, PathBuf::from("/Library/example"));
    }
}

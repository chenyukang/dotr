use std::path::Path;

use crate::{
    Result,
    backup::{self, BackupOptions, BackupReport},
    environment::Environment,
};

pub fn run(repo_root: &Path, env: &Environment) -> Result<BackupReport> {
    backup::run(
        repo_root,
        env,
        &BackupOptions {
            dry_run: true,
            no_git: true,
            ..BackupOptions::default()
        },
    )
}

pub mod backup;
pub mod cli;
pub mod config;
pub mod doctor;
pub mod encryption;
pub mod environment;
pub mod git;
pub mod hash;
pub mod index;
pub mod init;
pub mod paths;
pub mod restore;
pub mod status;
pub mod watch;

pub type Result<T> = anyhow::Result<T>;

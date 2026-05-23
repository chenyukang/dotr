use std::path::PathBuf;

use anyhow::bail;
use clap::{Parser, Subcommand};

use crate::{
    Result,
    backup::{self, BackupOptions},
    daemon, doctor,
    environment::Environment,
    init,
    manage::{self, AddOptions, RemoveOptions},
    progress::StderrProgress,
    repo,
    restore::{self, RestoreOptions},
    status, watch,
};

#[derive(Debug, Parser)]
#[command(
    name = "dotr",
    version,
    about = "Rust-native config backup into Git",
    long_about = "dotr backs up selected personal configuration files into a Git repository and can restore them later."
)]
pub struct Cli {
    #[arg(
        long,
        short = 'C',
        value_name = "REPO",
        help = "Use a specific dotr repository instead of auto-discovery"
    )]
    repo: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Create the dotr layout and optionally initialize Git")]
    Init {
        #[arg(
            default_value = ".",
            value_name = "TARGET",
            help = "Repository directory to create or prepare"
        )]
        target: PathBuf,
        #[arg(
            long,
            help = "Write a generic starter config for common shell, Git, SSH, editor, and terminal paths"
        )]
        with_defaults: bool,
        #[arg(long, help = "Create the dotr layout without running git init")]
        no_git: bool,
        #[arg(long, help = "Overwrite existing dotr starter files")]
        force: bool,
        #[arg(long, help = "Store this repository as the user default repo")]
        set_default: bool,
    },
    #[command(about = "Run one backup pass")]
    Backup {
        #[arg(long, help = "Show planned changes without writing files")]
        dry_run: bool,
        #[arg(long, help = "Keep backup files whose source files disappeared")]
        no_delete: bool,
        #[arg(long, help = "Skip git commit and push integration")]
        no_git: bool,
        #[arg(long, help = "Commit backup changes after a successful backup")]
        commit: bool,
        #[arg(long, help = "Push after committing backup changes")]
        push: bool,
    },
    #[command(about = "Add a source path to dotr.toml and back it up")]
    Add {
        #[arg(value_name = "PATH", help = "File or directory to add")]
        path: PathBuf,
        #[arg(long, help = "Skip git commit and push integration")]
        no_git: bool,
        #[arg(long, help = "Commit changes after the backup")]
        commit: bool,
        #[arg(long, help = "Push after committing changes")]
        push: bool,
    },
    #[command(about = "Remove a source path from dotr.toml and delete its backup")]
    Remove {
        #[arg(value_name = "PATH", help = "Configured file or directory to remove")]
        path: PathBuf,
        #[arg(long, help = "Skip git commit and push integration")]
        no_git: bool,
        #[arg(long, help = "Commit changes after the backup")]
        commit: bool,
        #[arg(long, help = "Push after committing changes")]
        push: bool,
    },
    #[command(about = "Show pending backup changes without writing")]
    Status,
    #[command(about = "Restore files from metadata/index.json")]
    Restore {
        #[arg(long, help = "Preview restore actions without writing files")]
        dry_run: bool,
        #[arg(
            long,
            help = "Apply restore actions; restore is dry-run without this flag"
        )]
        apply: bool,
        #[arg(long, help = "Overwrite differing destination files or symlinks")]
        force: bool,
        #[arg(long, help = "Allow restoring paths stored under files/absolute")]
        allow_absolute: bool,
        #[arg(help = "Optional source path scopes to restore, such as ~/.config/nvim")]
        targets: Vec<String>,
    },
    #[command(about = "Watch configured source paths and run debounced backups")]
    Watch,
    #[command(about = "Control the cross-platform watch daemon")]
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    #[command(about = "Check config, repository layout, and secret guardrails")]
    Doctor,
    #[command(about = "Print the repository that dotr would use")]
    Repo,
    #[command(about = "Manage user-level dotr configuration")]
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommands {
    #[command(about = "Set a user-level configuration value")]
    Set {
        #[arg(help = "Config key to set; currently only default_repo is supported")]
        key: String,
        #[arg(help = "Config value")]
        value: String,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommands {
    #[command(about = "Start the background watcher for the resolved repo")]
    Start,
    #[command(about = "Stop the background watcher")]
    Stop,
    #[command(about = "Show whether the background watcher is configured and running")]
    Status,
}

pub fn run() -> Result<()> {
    run_from(Cli::parse())
}

pub fn run_from(cli: Cli) -> Result<()> {
    let env = Environment::from_current()?;
    let cwd = std::env::current_dir()?;
    let Cli { repo, command } = cli;

    match command {
        Commands::Init {
            target,
            with_defaults,
            no_git,
            force,
            set_default,
        } => {
            let report = init::run(&init::InitOptions {
                target,
                with_defaults,
                no_git,
                force,
            })?;
            println!("initialized dotr at {}", report.repo_root.display());
            if report.created_config {
                println!("created dotr.toml");
            }
            if report.initialized_git {
                println!("initialized git repository");
            }
            if set_default {
                repo::set_default_repo(&env, &cwd, &report.repo_root.to_string_lossy())?;
                println!("set default_repo = {}", report.repo_root.display());
            }
        }
        Commands::Backup {
            dry_run,
            no_delete,
            no_git,
            commit,
            push,
        } => {
            let repo_root = repo::resolve_repo(repo.as_deref(), &cwd, &env)?.root;
            let mut progress = StderrProgress::new();
            let report = backup::run_with_progress(
                &repo_root,
                &env,
                &BackupOptions {
                    dry_run,
                    no_delete,
                    no_git,
                    commit,
                    push,
                },
                &mut progress,
            )?;
            print_actions(&report.actions);
            println!(
                "backup: {} added, {} updated, {} deleted, {} unchanged, {} skipped",
                report.added, report.updated, report.deleted, report.unchanged, report.skipped
            );
        }
        Commands::Add {
            path,
            no_git,
            commit,
            push,
        } => {
            let repo_root = repo::resolve_repo(repo.as_deref(), &cwd, &env)?.root;
            let mut progress = StderrProgress::new();
            let report = manage::add_with_progress(
                &repo_root,
                &cwd,
                &env,
                &AddOptions {
                    path,
                    no_git,
                    commit,
                    push,
                },
                &mut progress,
            )?;
            if report.config_changed {
                println!("added path {}", report.source);
            } else {
                println!("path already configured {}", report.source);
            }
            print_actions(&report.backup.actions);
            println!(
                "backup: {} added, {} updated, {} deleted, {} unchanged, {} skipped",
                report.backup.added,
                report.backup.updated,
                report.backup.deleted,
                report.backup.unchanged,
                report.backup.skipped
            );
        }
        Commands::Remove {
            path,
            no_git,
            commit,
            push,
        } => {
            let repo_root = repo::resolve_repo(repo.as_deref(), &cwd, &env)?.root;
            let mut progress = StderrProgress::new();
            let report = manage::remove_with_progress(
                &repo_root,
                &cwd,
                &env,
                &RemoveOptions {
                    path,
                    no_git,
                    commit,
                    push,
                },
                &mut progress,
            )?;
            println!("removed path {}", report.source);
            print_actions(&report.backup.actions);
            println!(
                "backup: {} added, {} updated, {} deleted, {} unchanged, {} skipped",
                report.backup.added,
                report.backup.updated,
                report.backup.deleted,
                report.backup.unchanged,
                report.backup.skipped
            );
        }
        Commands::Status => {
            let repo_root = repo::resolve_repo(repo.as_deref(), &cwd, &env)?.root;
            let report = status::run(&repo_root, &env)?;
            print_actions(&report.actions);
            println!(
                "status: {} additions, {} updates, {} deletions, {} unchanged, {} skipped",
                report.added, report.updated, report.deleted, report.unchanged, report.skipped
            );
        }
        Commands::Restore {
            dry_run,
            apply,
            force,
            allow_absolute,
            targets,
        } => {
            let repo_root = repo::resolve_repo(repo.as_deref(), &cwd, &env)?.root;
            let report = restore::run(
                &repo_root,
                &env,
                &RestoreOptions {
                    dry_run,
                    apply,
                    force,
                    allow_absolute,
                    targets,
                },
            )?;
            print_actions(&report.actions);
            println!(
                "restore: {} restored, {} planned, {} skipped",
                report.restored, report.planned, report.skipped
            );
        }
        Commands::Watch => {
            let repo_root = repo::resolve_repo(repo.as_deref(), &cwd, &env)?.root;
            watch::run(&repo_root, &env)?;
        }
        Commands::Daemon { command } => match command {
            DaemonCommands::Start => {
                let repo_root = resolve_daemon_start_repo(repo.as_deref(), &cwd, &env)?;
                let report = daemon::start(&env, repo_root.as_deref())?;
                if report.already_running {
                    println!(
                        "daemon {} already running with pid {}",
                        report.name, report.pid
                    );
                } else {
                    println!("started daemon {} with pid {}", report.name, report.pid);
                }
                println!("log: {}", report.log_path.display());
            }
            DaemonCommands::Stop => {
                let report = daemon::stop(&env)?;
                if report.stopped {
                    println!(
                        "stopped daemon {} with pid {}",
                        report.name,
                        report.pid.unwrap_or_default()
                    );
                } else if let Some(pid) = report.pid {
                    println!(
                        "daemon {} was not running; removed stale pid {pid}",
                        report.name
                    );
                } else {
                    println!("daemon {} was not running", report.name);
                }
            }
            DaemonCommands::Status => {
                let status = daemon::status(&env)?;
                println!(
                    "daemon {}: {}",
                    status.name,
                    daemon_status_label(status.state)
                );
                println!("config: {}", status.config.display());
                if let Some(pid) = status.pid {
                    println!("pid: {pid}");
                }
                if let Some(repo_root) = status.repo_root {
                    println!("repo: {}", repo_root.display());
                }
                println!("log: {}", status.log_path.display());
                println!("log_level: {}", status.log_level);
            }
        },
        Commands::Doctor => {
            let repo_root = repo::resolve_repo(repo.as_deref(), &cwd, &env)?.root;
            let report = doctor::run(&repo_root, &env)?;
            for warning in report.warnings {
                println!("warning: {warning}");
            }
            println!("doctor: ok");
        }
        Commands::Repo => {
            let resolution = repo::resolve_repo(repo.as_deref(), &cwd, &env)?;
            println!("{}", resolution.root.display());
        }
        Commands::Config { command } => match command {
            ConfigCommands::Set { key, value } => {
                if key != "default_repo" {
                    bail!("unsupported user config key: {key}");
                }
                let repo_root = repo::set_default_repo(&env, &cwd, &value)?;
                println!("set default_repo = {}", repo_root.display());
            }
        },
    }

    Ok(())
}

fn print_actions(actions: &[String]) {
    for action in actions {
        println!("{action}");
    }
}

fn daemon_status_label(state: daemon::DaemonState) -> &'static str {
    match state {
        daemon::DaemonState::NotConfigured => "not configured",
        daemon::DaemonState::Running => "running",
        daemon::DaemonState::Stopped => "configured but stopped",
        daemon::DaemonState::StalePid => "configured with stale pid",
    }
}

fn resolve_daemon_start_repo(
    explicit: Option<&std::path::Path>,
    cwd: &std::path::Path,
    env: &Environment,
) -> Result<Option<PathBuf>> {
    match repo::resolve_repo(explicit, cwd, env) {
        Ok(resolution) => Ok(Some(resolution.root)),
        Err(_) if daemon::is_configured(env) => Ok(None),
        Err(err) => Err(err),
    }
}

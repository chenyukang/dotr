use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{
    Result,
    backup::{self, BackupOptions},
    doctor,
    environment::Environment,
    init,
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
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Create the backup/ layout and optionally initialize Git")]
    Init {
        #[arg(
            default_value = ".",
            value_name = "TARGET",
            help = "Repository directory to create or prepare"
        )]
        target: PathBuf,
        #[arg(
            long,
            help = "Write starter paths for ~/.codex, ~/.agents, ~/.hermes, and ~/code/bin"
        )]
        with_defaults: bool,
        #[arg(long, help = "Create the dotr layout without running git init")]
        no_git: bool,
        #[arg(long, help = "Overwrite existing dotr starter files")]
        force: bool,
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
    #[command(about = "Show pending backup changes without writing")]
    Status,
    #[command(about = "Restore files from backup/metadata/index.json")]
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
        #[arg(
            long,
            help = "Allow restoring paths stored under backup/files/absolute"
        )]
        allow_absolute: bool,
        #[arg(help = "Optional source path scopes to restore, such as ~/.codex")]
        targets: Vec<String>,
    },
    #[command(about = "Watch configured source paths and run debounced backups")]
    Watch,
    #[command(about = "Check config, repository layout, and secret guardrails")]
    Doctor,
}

pub fn run() -> Result<()> {
    run_from(Cli::parse())
}

pub fn run_from(cli: Cli) -> Result<()> {
    let env = Environment::from_current()?;
    let cwd = std::env::current_dir()?;

    match cli.command {
        Commands::Init {
            target,
            with_defaults,
            no_git,
            force,
        } => {
            let report = init::run(&init::InitOptions {
                target,
                with_defaults,
                no_git,
                force,
            })?;
            println!("initialized dotr at {}", report.repo_root.display());
            if report.created_config {
                println!("created backup/dotr.toml");
            }
            if report.initialized_git {
                println!("initialized git repository");
            }
        }
        Commands::Backup {
            dry_run,
            no_delete,
            no_git,
            commit,
            push,
        } => {
            let report = backup::run(
                &cwd,
                &env,
                &BackupOptions {
                    dry_run,
                    no_delete,
                    no_git,
                    commit,
                    push,
                },
            )?;
            print_actions(&report.actions);
            println!(
                "backup: {} added, {} updated, {} deleted, {} unchanged, {} skipped",
                report.added, report.updated, report.deleted, report.unchanged, report.skipped
            );
        }
        Commands::Status => {
            let report = status::run(&cwd, &env)?;
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
            let report = restore::run(
                &cwd,
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
        Commands::Watch => watch::run(&cwd, &env)?,
        Commands::Doctor => {
            let report = doctor::run(&cwd, &env)?;
            for warning in report.warnings {
                println!("warning: {warning}");
            }
            println!("doctor: ok");
        }
    }

    Ok(())
}

fn print_actions(actions: &[String]) {
    for action in actions {
        println!("{action}");
    }
}

use std::env;
use std::io;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use porch_gate::{
    InitOptions, admit_push, git_dir_from_env, init, notify_push, porch_home, run_daemon,
};

#[derive(Parser)]
#[command(name = "porch", version, about = "Local git gate")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Install the porch remote, bare repo, and hooks in this working tree.
    Init,
    /// Daemon process (hooks call these subcommands).
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Long-lived process: flock, socket, sqlite.
    Run,
    /// pre-receive: currently always allow.
    AdmitPush,
    /// post-receive: record a pending run.
    NotifyPush,
}

fn main() -> Result<()> {
    let argv: Vec<String> = env::args().collect();
    // Fast path: do not attach a stderr logger that would steal daemon logs.
    if argv.len() == 3 && argv[1] == "daemon" && argv[2] == "run" {
        init_file_tracing();
        let home = porch_home();
        run_daemon(&home).context("daemon run")?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(io::stderr)
        .init();

    match Cli::parse().command {
        Command::Init => {
            let work = env::current_dir()?;
            let home = porch_home();
            let bin = env::current_exe().context("current_exe")?;
            let result = init(InitOptions {
                work_tree: &work,
                porch_home: &home,
                porch_bin: &bin,
                start_daemon: true,
            })?;
            println!("porch remote -> {}", result.bare_path.display());
        }
        Command::Daemon {
            command: DaemonCommand::Run,
        } => {
            let home = porch_home();
            run_daemon(&home)?;
        }
        Command::Daemon {
            command: DaemonCommand::AdmitPush,
        } => {
            admit_push(io::stdin())?;
        }
        Command::Daemon {
            command: DaemonCommand::NotifyPush,
        } => {
            let home = porch_home();
            let git_dir = git_dir_from_env()?;
            let ids = notify_push(&home, &git_dir, io::stdin())?;
            for id in ids {
                eprintln!("porch: recorded run {id}");
            }
        }
    }
    Ok(())
}

fn init_file_tracing() {
    let home = porch_home();
    let logs = home.join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("daemon.log"));
    match file {
        Ok(f) => {
            tracing_subscriber::fmt()
                .with_writer(std::sync::Mutex::new(f))
                .with_env_filter("info")
                .init();
        }
        Err(_) => {
            tracing_subscriber::fmt().with_writer(io::stderr).init();
        }
    }
}

// Silence unused import if notify module path needs re-export.

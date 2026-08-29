use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use porch_gate::{
    InitOptions, admit_push, git_dir_from_env, init, notify_push, porch_home, run_daemon,
};
use porch_run::{AgentResponse, PipelineExecutor, agent_respond, agent_status};

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
    /// Headless agent interface (JSON on stdout).
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Long-lived process: flock, socket, sqlite, run executor.
    Run,
    /// pre-receive: currently always allow.
    AdmitPush,
    /// post-receive: record a pending run and ask the daemon to start it.
    NotifyPush,
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Print run status JSON (default: latest parked for this repo).
    Status {
        /// Run id (ULID). Defaults to latest parked run for the cwd repo.
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Respond to a parked review: approve | skip | abort | fix.
    Respond {
        /// `approve`, `skip`, `abort`, or `fix`.
        response: String,
        /// Run id (ULID). Defaults to latest parked run for the cwd repo.
        #[arg(long)]
        run_id: Option<String>,
        /// Comma-separated finding ids (only with `fix`).
        #[arg(long)]
        findings: Option<String>,
        /// After one fix round, approve remaining findings (only with `fix`).
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("porch: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn main_inner() -> Result<ExitCode> {
    let argv: Vec<String> = env::args().collect();
    // Fast path: do not attach a stderr logger that would steal daemon logs.
    if argv.len() == 3 && argv[1] == "daemon" && argv[2] == "run" {
        init_file_tracing();
        let home = porch_home();
        let executor: Arc<dyn porch_gate::RunExecutor> = Arc::new(PipelineExecutor);
        run_daemon(&home, &executor).context("daemon run")?;
        return Ok(ExitCode::SUCCESS);
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
            Ok(ExitCode::SUCCESS)
        }
        Command::Daemon {
            command: DaemonCommand::Run,
        } => {
            let home = porch_home();
            let executor: Arc<dyn porch_gate::RunExecutor> = Arc::new(PipelineExecutor);
            run_daemon(&home, &executor)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Daemon {
            command: DaemonCommand::AdmitPush,
        } => {
            admit_push(io::stdin())?;
            Ok(ExitCode::SUCCESS)
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
            Ok(ExitCode::SUCCESS)
        }
        Command::Agent {
            command: AgentCommand::Status { run_id },
        } => {
            let home = porch_home();
            let work = env::current_dir()?;
            let result = agent_status(&home, run_id.as_deref(), &work);
            Ok(emit_agent(&result))
        }
        Command::Agent {
            command:
                AgentCommand::Respond {
                    response,
                    run_id,
                    findings,
                    yes,
                },
        } => Ok(run_agent_respond(
            &response,
            run_id.as_deref(),
            findings.as_deref(),
            yes,
        )?),
    }
}

fn run_agent_respond(
    response: &str,
    run_id: Option<&str>,
    findings: Option<&str>,
    yes: bool,
) -> Result<ExitCode> {
    if (findings.is_some() || yes) && response != "fix" {
        let _ = writeln!(
            io::stdout(),
            "{}",
            serde_json::json!({
                "error": "--findings and --yes are only valid with fix",
                "code": "usage"
            })
        );
        return Ok(ExitCode::from(2));
    }
    let parsed = match parse_agent_response(response, findings, yes) {
        Ok(r) => r,
        Err(msg) => {
            let _ = writeln!(
                io::stdout(),
                "{}",
                serde_json::json!({"error": msg, "code": "usage"})
            );
            return Ok(ExitCode::from(2));
        }
    };
    let home = porch_home();
    let work = env::current_dir()?;
    Ok(emit_agent(&agent_respond(&home, run_id, &work, parsed)))
}

fn parse_agent_response(
    response: &str,
    findings: Option<&str>,
    yes: bool,
) -> std::result::Result<AgentResponse, String> {
    match response {
        "approve" => Ok(AgentResponse::Approve),
        "skip" => Ok(AgentResponse::Skip),
        "abort" => Ok(AgentResponse::Abort),
        "fix" => {
            let finding_ids = findings.map(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });
            Ok(AgentResponse::Fix { finding_ids, yes })
        }
        other => Err(format!(
            "unknown response {other:?}; expected approve|skip|abort|fix"
        )),
    }
}

fn emit_agent(result: &porch_run::AgentCliResult) -> ExitCode {
    println!("{}", result.json);
    ExitCode::from(u8::try_from(result.exit_code).unwrap_or(1))
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

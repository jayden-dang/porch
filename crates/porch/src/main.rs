use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use porch_gate::{
    InitOptions, admit_push, ensure_daemon, get_run, git_dir_from_env, health_check, init,
    install_service, list_runs, notify_push, porch_home, repo_id_for, run_daemon, service_status,
    start_service, stop_daemon, uninstall_service,
};
use porch_run::{AgentResponse, PipelineExecutor, agent_respond, agent_status};

mod doctor;
mod setup;
mod tui;

#[derive(Parser)]
#[command(name = "porch", version, about = "Local git gate")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Install the porch remote, bare repo, and hooks in this working tree.
    Init {
        /// Run setup non-interactively when review is missing, then init.
        #[arg(long)]
        yes: bool,
        /// Skip first-run review setup entirely.
        #[arg(long)]
        skip_setup: bool,
    },
    /// Detect review engine, write `$PORCH_HOME/config.yaml` + wrapper.
    Setup {
        /// Detect, write, verify; print JSON (non-interactive).
        #[arg(long)]
        yes: bool,
        /// Re-check wrapper/config without rewriting.
        #[arg(long)]
        verify: bool,
        /// Force engine (`ocr` or `generic`).
        #[arg(long)]
        engine: Option<String>,
        /// Rewrite wrapper from current config.yaml.
        #[arg(long)]
        apply: bool,
    },
    /// Check PATH / home / daemon prerequisites for a push.
    Doctor,
    /// List recent runs for this repo (JSON array).
    Runs {
        /// Max rows (default 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Daemon health + latest run summary for this repo.
    Status {
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Attach the park TUI (or print a snapshot when not a TTY).
    Attach {
        #[arg(long)]
        run_id: Option<String>,
    },
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
    /// Write the OS service definition (launchd/systemd).
    Install,
    /// Stop if possible and remove the service definition.
    Uninstall,
    /// Start via OS manager, or detached `ensure_daemon` fallback.
    Start,
    /// Stop the daemon process (definition stays unless uninstall).
    Stop {
        /// Allow stop even with pending/running/parked runs.
        #[arg(long)]
        force: bool,
    },
    /// Print daemon / service status.
    Status {
        #[arg(long)]
        json: bool,
    },
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
        None => run_bare(),
        Some(Command::Init { yes, skip_setup }) => run_init(yes, skip_setup),
        Some(Command::Setup {
            yes,
            verify,
            engine,
            apply,
        }) => {
            let home = porch_home();
            setup::run(
                &home,
                &setup::SetupArgs {
                    yes,
                    verify,
                    apply,
                    engine,
                },
            )
        }
        Some(Command::Doctor) => Ok(doctor::run()?),
        Some(Command::Runs { limit }) => run_runs(limit),
        Some(Command::Status { json }) => run_status(json),
        Some(Command::Attach { run_id }) => run_attach_cmd(run_id.as_deref()),
        Some(Command::Daemon { command }) => run_daemon_command(&command),
        Some(Command::Agent {
            command: AgentCommand::Status { run_id },
        }) => {
            let home = porch_home();
            let work = env::current_dir()?;
            let result = agent_status(&home, run_id.as_deref(), &work);
            Ok(emit_agent(&result))
        }
        Some(Command::Agent {
            command:
                AgentCommand::Respond {
                    response,
                    run_id,
                    findings,
                    yes,
                },
        }) => Ok(run_agent_respond(
            &response,
            run_id.as_deref(),
            findings.as_deref(),
            yes,
        )?),
    }
}

fn run_daemon_command(command: &DaemonCommand) -> Result<ExitCode> {
    match command {
        DaemonCommand::Run => {
            let home = porch_home();
            let executor: Arc<dyn porch_gate::RunExecutor> = Arc::new(PipelineExecutor);
            run_daemon(&home, &executor)?;
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::AdmitPush => {
            admit_push(io::stdin())?;
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::NotifyPush => {
            let home = porch_home();
            let git_dir = git_dir_from_env()?;
            let ids = notify_push(&home, &git_dir, io::stdin())?;
            for id in ids {
                eprintln!("porch: recorded run {id}");
            }
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Install => {
            let home = porch_home();
            let user_home = user_home_dir()?;
            let bin = env::current_exe().context("current_exe")?;
            let paths = install_service(&bin, &home, &user_home)?;
            println!("wrote {}", paths.definition_path.display());
            println!("label: {}", paths.label);
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Uninstall => {
            let home = porch_home();
            let user_home = user_home_dir()?;
            let paths = uninstall_service(&home, &user_home)?;
            println!("removed {}", paths.definition_path.display());
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Start => {
            let home = porch_home();
            let user_home = user_home_dir()?;
            let bin = env::current_exe().context("current_exe")?;
            let msg = start_service(&bin, &home, &user_home)?;
            println!("{msg}");
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Stop { force } => {
            let home = porch_home();
            stop_daemon(&home, *force)?;
            println!("daemon stopped");
            Ok(ExitCode::SUCCESS)
        }
        DaemonCommand::Status { json } => {
            let home = porch_home();
            let user_home = user_home_dir()?;
            let st = service_status(&home, &user_home)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!(
                    "running={} pid={:?} socket_healthy={} service={} exists={}",
                    st.running,
                    st.pid,
                    st.socket_healthy,
                    st.service_file.display(),
                    st.service_file_exists
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn user_home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is unset")
}

fn is_git_work_tree(work: &Path) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(work)
        .output()
        .ok()
        .is_some_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

fn ensure_daemon_for_cwd(home: &Path) -> Result<()> {
    let bin = env::current_exe().context("current_exe")?;
    ensure_daemon(&bin, home).context("ensure daemon (try: porch daemon start)")
}

fn run_init(yes: bool, skip_setup: bool) -> Result<ExitCode> {
    let work = env::current_dir()?;
    let home = porch_home();
    if !skip_setup {
        let review_missing = setup::setup_incomplete(&home);
        if review_missing {
            if yes {
                let result = porch_review::setup_yes(&home, None)?;
                if !result.ok {
                    eprintln!(
                        "porch: setup failed: {}",
                        result.error.as_deref().unwrap_or("unknown")
                    );
                    println!("{}", serde_json::to_string_pretty(&result)?);
                    return Ok(ExitCode::from(1));
                }
            } else if io::stdin().is_terminal() {
                eprintln!(
                    "porch: review setup incomplete — run `porch setup` (or `porch init --yes`)"
                );
            } else {
                eprintln!(
                    "porch: review setup incomplete — run `porch setup --yes` or `porch init --skip-setup`"
                );
            }
        }
    }
    let bin = env::current_exe().context("current_exe")?;
    let result = init(InitOptions {
        work_tree: &work,
        porch_home: &home,
        porch_bin: &bin,
        start_daemon: true,
    })?;
    print_init_next_steps(&result, &work, &home);
    Ok(ExitCode::SUCCESS)
}

fn run_bare() -> Result<ExitCode> {
    let work = env::current_dir()?;
    if !is_git_work_tree(&work) {
        eprintln!("porch: not a git work tree; run from a git repo after `porch init`");
        return Ok(ExitCode::from(1));
    }
    let home = porch_home();
    ensure_daemon_for_cwd(&home)?;

    let repo_id = repo_id_for(&work);
    let branch = doctor::current_branch(&work);
    let runs = list_runs(&home, Some(&repo_id), Some(20)).unwrap_or_default();
    let active = runs.iter().find(|r| {
        let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let b = r.get("branch").and_then(|v| v.as_str()).unwrap_or("");
        b == branch && matches!(status, "pending" | "running" | "parked")
    });

    if io::stdin().is_terminal() {
        if let Some(run) = active {
            let run_id = run.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if !run_id.is_empty() {
                tui::run_attach(&home, &work, run_id)?;
                return Ok(ExitCode::SUCCESS);
            }
        }
        if setup::setup_incomplete(&home) {
            return setup::run(
                &home,
                &setup::SetupArgs {
                    yes: false,
                    verify: false,
                    apply: false,
                    engine: None,
                },
            );
        }
    }

    print_runs_summary(&runs);
    if setup::setup_incomplete(&home) {
        println!("hint: review setup incomplete — run `porch setup --yes`");
    }
    Ok(ExitCode::SUCCESS)
}

fn print_runs_summary(runs: &[serde_json::Value]) {
    if runs.is_empty() {
        println!("no runs yet");
    } else {
        for r in runs.iter().take(10) {
            let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let branch = r.get("branch").and_then(|v| v.as_str()).unwrap_or("?");
            let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?");
            let sha = r.get("sha").and_then(|v| v.as_str()).unwrap_or("");
            let sha_pfx: String = sha.chars().take(8).collect();
            println!("{id}  {branch}  {status}  {sha_pfx}");
        }
    }
    println!("hint: git push porch   or   porch attach --run-id <id>");
}

fn run_runs(limit: usize) -> Result<ExitCode> {
    let work = env::current_dir()?;
    if !is_git_work_tree(&work) {
        bail!("not a git work tree");
    }
    let home = porch_home();
    ensure_daemon_for_cwd(&home)?;
    let repo_id = repo_id_for(&work);
    let runs = list_runs(&home, Some(&repo_id), Some(limit))?;
    println!("{}", serde_json::to_string_pretty(&runs)?);
    Ok(ExitCode::SUCCESS)
}

fn run_status(json: bool) -> Result<ExitCode> {
    let work = env::current_dir()?;
    let home = porch_home();
    let healthy = health_check(&home).unwrap_or(false);
    let mut latest: Option<serde_json::Value> = None;
    if is_git_work_tree(&work) {
        let _ = ensure_daemon_for_cwd(&home);
        let repo_id = repo_id_for(&work);
        if let Ok(runs) = list_runs(&home, Some(&repo_id), Some(1)) {
            latest = runs.into_iter().next();
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "daemon_healthy": healthy,
                "porch_home": home,
                "latest_run": latest,
            })
        );
    } else {
        println!("daemon_healthy={healthy}");
        println!("PORCH_HOME={}", home.display());
        match latest {
            Some(r) => {
                println!(
                    "latest: {} {} {}",
                    r.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
                    r.get("branch").and_then(|v| v.as_str()).unwrap_or("?"),
                    r.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
                );
            }
            None => println!("latest: (none)"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_attach_cmd(run_id: Option<&str>) -> Result<ExitCode> {
    let work = env::current_dir()?;
    if !is_git_work_tree(&work) {
        bail!("not a git work tree");
    }
    let home = porch_home();
    ensure_daemon_for_cwd(&home)?;
    let repo_id = repo_id_for(&work);
    let id = if let Some(id) = run_id {
        id.to_string()
    } else {
        let runs = list_runs(&home, Some(&repo_id), Some(20))?;
        let branch = doctor::current_branch(&work);
        runs.iter()
            .find(|r| {
                let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let b = r.get("branch").and_then(|v| v.as_str()).unwrap_or("");
                b == branch && matches!(status, "pending" | "running" | "parked")
            })
            .or_else(|| runs.first())
            .and_then(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
            .ok_or_else(|| anyhow::anyhow!("no active run"))?
    };

    if !io::stdin().is_terminal() {
        if let Ok(snap) = get_run(&home, &id) {
            println!(
                "run {}  branch {}  status {}",
                snap.run_id, snap.branch, snap.status
            );
            println!("{}", serde_json::to_string_pretty(&snap)?);
        } else {
            println!("no active run");
            let runs = list_runs(&home, Some(&repo_id), Some(5)).unwrap_or_default();
            print_runs_summary(&runs);
        }
        return Ok(ExitCode::SUCCESS);
    }

    tui::run_attach(&home, &work, &id)?;
    Ok(ExitCode::SUCCESS)
}

fn print_init_next_steps(
    result: &porch_gate::InitResult,
    work: &std::path::Path,
    home: &std::path::Path,
) {
    let branch = doctor::current_branch(work);
    println!("porch remote -> {}", result.bare_path.display());
    println!("repo id: {}", result.repo_id);
    println!("default branch: {}", result.default_branch);
    println!("PORCH_HOME: {}", home.display());
    println!("next: git push porch HEAD:refs/heads/{branch}");

    let review_bin = porch_review::review_bin();
    let gh_bin = env::var("PORCH_GH_BIN").unwrap_or_else(|_| "gh".into());
    let missing_review = !doctor::bin_on_path(&review_bin);
    let missing_gh = !doctor::bin_on_path(&gh_bin);
    if missing_review || missing_gh {
        println!(
            "tip: run `porch setup` / `porch doctor` — review and/or gh look missing for a complete run"
        );
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

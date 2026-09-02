//! Gate slice: named remote, bare repo, hooks, daemon, run rows.
//! Pipeline execution lives in `porch-run` (injected via [`RunExecutor`]).

mod admit;
mod daemon;
mod db;
mod eject;
mod events;
mod executor;
mod home;
mod id;
mod init;
mod notes;
mod notify;
mod proc;
pub mod rounds;
mod rpc;
mod service;
mod skill;

pub use admit::admit_push;
pub use daemon::{ensure_daemon, run_daemon, wait_for_health};
pub use db::{Db, RepoRow, RunRow, StepResultRow, UncertifiedPipelineRange};
pub use eject::{EjectOptions, EjectResult, eject};
pub use events::{Event, EventHub, Subscriber, clear_event_hub, event_hub, install_event_hub};
pub use executor::RunExecutor;
pub use home::{
    db_path, lock_path, logs_dir, pid_path, porch_home, repos_dir, run_artifact_dir,
    run_deliver_repair_dir, run_fixer_dir, run_review_dir, run_worktree_dir, socket_path,
    worktrees_dir,
};
pub use id::repo_id_for;
pub use init::{InitOptions, InitResult, init};
pub use notes::{finding_notes_path, load_finding_notes, set_finding_note};
pub use notify::{git_dir_from_env, notify_push};
pub use proc::{
    collect_porch_env, collect_porch_env_from, kill_group, spawn_detached, spawn_detached_with_env,
};
pub use rpc::start_run as rpc_start_run;
pub use rpc::{
    AssuranceRecord, AuditIdentity, FINDING_HUNK_MAX_BYTES, LegacyFindingDto, RunSnapshot,
    StatusFindingDto, StepSnapshot, UnavailableAudit, clear_rounds_for_run, compact_run_row,
    get_finding_hunk, get_run, health_check, list_runs, resolve_run_assurance, round_for_decision,
    subscribe_events,
};
pub use service::{
    ServicePaths, ServiceStatus, daemon_service_suffix, install_service, render_launchd_plist,
    render_systemd_unit, render_windows_task_command, service_paths, service_status,
    set_skip_service_load_for_tests, start_service, stop_daemon, uninstall_service,
};
pub use skill::{
    SKILL_NAME, SkillInstallReport, install_agent_skills, install_agent_skills_for, skill_markdown,
    user_home_from_env,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Git(#[from] porch_git::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

//! Gate slice: named remote, bare repo, hooks, daemon, run rows.
//! Pipeline execution lives in `porch-run` (injected via [`RunExecutor`]).

mod admit;
mod daemon;
mod db;
mod executor;
mod home;
mod id;
mod init;
mod notify;
mod proc;
mod rpc;

pub use admit::admit_push;
pub use daemon::{ensure_daemon, run_daemon, wait_for_health};
pub use db::{Db, RepoRow, RunRow, StepResultRow};
pub use executor::RunExecutor;
pub use home::{
    db_path, lock_path, logs_dir, pid_path, porch_home, repos_dir, run_worktree_dir, socket_path,
    worktrees_dir,
};
pub use id::repo_id_for;
pub use init::{InitOptions, InitResult, init};
pub use notify::{git_dir_from_env, notify_push};
pub use proc::{kill_group, spawn_detached, spawn_detached_with_env};
pub use rpc::start_run as rpc_start_run;

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

//! M1 vertical slice: named remote, bare repo, hooks, daemon, run rows.
//! No pipeline.

mod admit;
mod daemon;
mod db;
mod home;
mod id;
mod init;
mod notify;
mod proc;
mod rpc;

pub use admit::admit_push;
pub use daemon::{ensure_daemon, run_daemon, wait_for_health};
pub use db::Db;
pub use home::porch_home;
pub use id::repo_id_for;
pub use init::{InitOptions, InitResult, init};
pub use notify::{git_dir_from_env, notify_push};
pub use proc::{kill_group, spawn_detached};

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

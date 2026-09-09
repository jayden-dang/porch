//! What condition the daemon is in, and why.
//!
//! `GOAL-4` promises an operator can inspect state without a healthy daemon. That
//! requires porch to have a word for each way the daemon can fail to be healthy, and
//! for the word to be reachable without the daemon and without the state database
//! (`DFAULT-3.6`, `DFAULT-3.7`).
//!
//! Three of the four conditions are indistinguishable from each other by socket
//! presence alone:
//!
//! - a dead daemon leaves no socket;
//! - a daemon that **refused to start** also leaves no socket, because `run_daemon`
//!   returns before `UnixListener::bind` when a startup barrier fails — which is why
//!   the refusal is recorded durably here rather than left in `logs/daemon.log`, a file
//!   every subsequent spawn truncates (`DFAULT-2.2`);
//! - a **wedged** daemon leaves a socket that accepts and never answers, which is only
//!   observable because the RPC client now has a deadline (`DFAULT-1.1`).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::home::{pid_path, refusal_path, socket_path};
use crate::rpc::health_check;

/// Which startup barrier refused (`DFAULT-2.5`).
pub const BARRIER_RECONCILE_STALE: &str = "reconcile-stale-rounds";
/// Which startup barrier refused (`DFAULT-2.5`).
pub const BARRIER_RECOVER_STALE: &str = "recover-stale-runs";

/// A durable record that the daemon refused to start, and why.
///
/// Written before `run_daemon` returns its error, cleared once a later start binds the
/// socket. Plain JSON in `$PORCH_HOME` so it is readable with no daemon and no database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalRecord {
    /// Which startup barrier refused.
    pub barrier: String,
    /// The barrier's own error text.
    pub cause: String,
    /// Unix seconds, matching the convention used for row timestamps.
    pub at: String,
}

fn now_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".into(), |d| d.as_secs().to_string())
}

/// Record that the daemon refused to start (`DFAULT-2.1`).
///
/// Best-effort: a daemon that cannot write this still reports its error by its exit,
/// and refusing to refuse would be worse than an unrecorded refusal.
pub fn record_refusal(home: &Path, barrier: &str, cause: &str) {
    let rec = RefusalRecord {
        barrier: barrier.to_string(),
        cause: cause.to_string(),
        at: now_secs(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&rec) {
        let _ = std::fs::write(refusal_path(home), json);
    }
}

/// Read the durable refusal record, if one is present.
#[must_use]
pub fn read_refusal(home: &Path) -> Option<RefusalRecord> {
    let raw = std::fs::read_to_string(refusal_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Clear the refusal record once a start succeeds (`DFAULT-2.3`).
///
/// A stale refusal must not be reported as a current one.
pub fn clear_refusal(home: &Path) {
    let _ = std::fs::remove_file(refusal_path(home));
}

/// The condition porch's daemon is in.
///
/// Four variants, each induced by a distinct fault and each carrying what the operator
/// needs to act on it (`DFAULT-3.1`). A fifth — live but not ready — is deliberately
/// absent: nothing reachable in the daemon holds the database mutex across an unbounded
/// operation, so no fault can induce it and no test could assert it (`DFAULT-7.2`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "condition", rename_all = "kebab-case")]
pub enum DaemonCondition {
    /// `health` answered within its deadline.
    Ready { pid: Option<u32> },
    /// The socket could not be connected: dead, or never started.
    Unreachable { reason: String },
    /// A durable refusal record exists and the socket is not answering.
    Refusing {
        barrier: String,
        cause: String,
        at: String,
    },
    /// The socket accepted a connection and `health` exceeded its deadline. The wedged
    /// daemon. The pid is what the operator needs in order to act on it.
    NotAnswering { pid: Option<u32>, waited_ms: u64 },
}

impl DaemonCondition {
    /// Stable one-word label, for output and for tests to assert on.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready { .. } => "ready",
            Self::Unreachable { .. } => "unreachable",
            Self::Refusing { .. } => "refusing",
            Self::NotAnswering { .. } => "not-answering",
        }
    }

    /// Whether the daemon is answering RPCs.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// A command the operator can run (`DFAULT-3.8`).
    #[must_use]
    pub fn remedy(&self) -> &'static str {
        match self {
            Self::Ready { .. } => "nothing to do",
            Self::Unreachable { .. } => "try: porch daemon start",
            Self::Refusing { .. } => {
                "the daemon refuses to serve until this clears; \
                 try: porch doctor, then porch daemon start"
            }
            Self::NotAnswering { .. } => {
                "try: porch daemon stop --force, then porch daemon start; \
                 porch eject still detaches without the daemon"
            }
        }
    }

    /// One operator-facing line describing the condition and its cause.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Ready { pid } => match pid {
                Some(pid) => format!("ready (pid {pid})"),
                None => "ready".to_string(),
            },
            Self::Unreachable { reason } => format!("unreachable ({reason})"),
            Self::Refusing { barrier, cause, at } => {
                format!("refusing to start at {barrier} (since {at}): {cause}")
            }
            Self::NotAnswering { pid, waited_ms } => match pid {
                Some(pid) => format!(
                    "not answering — accepted the connection, no reply in {waited_ms}ms (pid {pid})"
                ),
                None => {
                    format!("not answering — accepted the connection, no reply in {waited_ms}ms")
                }
            },
        }
    }
}

fn pid_from_file(home: &Path) -> Option<u32> {
    std::fs::read_to_string(pid_path(home))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Resolve the daemon's condition.
///
/// Starts nothing and opens no database, so it is answerable in every state it
/// describes (`DFAULT-3.6`, `DFAULT-3.7`).
#[must_use]
pub fn daemon_condition(home: &Path) -> DaemonCondition {
    match health_check(home) {
        Ok(true) => DaemonCondition::Ready {
            pid: pid_from_file(home),
        },
        // Bounded by `method_timeout("health")`: the socket accepted and went quiet.
        Err(crate::Error::RpcTimeout { ms, .. }) => DaemonCondition::NotAnswering {
            pid: pid_from_file(home),
            waited_ms: ms,
        },
        // A refusal record outlives the process that wrote it, so it is only current
        // while the socket is also not answering — which is the branch we are in.
        other => {
            if let Some(rec) = read_refusal(home) {
                return DaemonCondition::Refusing {
                    barrier: rec.barrier,
                    cause: rec.cause,
                    at: rec.at,
                };
            }
            let reason = match other {
                Err(e) => format!("socket {}: {e}", socket_path(home).display()),
                Ok(_) => format!(
                    "socket {} answered health without ok",
                    socket_path(home).display()
                ),
            };
            DaemonCondition::Unreachable { reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn no_socket_and_no_record_is_unreachable() {
        let tmp = TempDir::new().unwrap();
        let c = daemon_condition(tmp.path());
        assert_eq!(c.label(), "unreachable");
        assert!(!c.is_ready());
        assert!(c.remedy().contains("porch daemon start"));
    }

    #[test]
    fn a_refusal_record_makes_the_same_absence_refusing() {
        let tmp = TempDir::new().unwrap();
        record_refusal(tmp.path(), BARRIER_RECOVER_STALE, "boom");
        let c = daemon_condition(tmp.path());
        assert_eq!(c.label(), "refusing");
        assert!(c.summary().contains("recover-stale-runs"), "{c:?}");
        assert!(c.summary().contains("boom"), "{c:?}");
    }

    #[test]
    fn a_refusal_record_survives_rereading_and_clears_on_demand() {
        let tmp = TempDir::new().unwrap();
        record_refusal(tmp.path(), BARRIER_RECONCILE_STALE, "cause one");
        assert_eq!(
            read_refusal(tmp.path()).unwrap().barrier,
            BARRIER_RECONCILE_STALE
        );
        // Re-reading must not consume it: the operator retries, then asks why.
        assert_eq!(read_refusal(tmp.path()).unwrap().cause, "cause one");
        clear_refusal(tmp.path());
        assert!(read_refusal(tmp.path()).is_none());
        assert_eq!(daemon_condition(tmp.path()).label(), "unreachable");
    }

    #[test]
    fn every_condition_carries_a_remedy_naming_a_command() {
        let conditions = [
            DaemonCondition::Unreachable { reason: "x".into() },
            DaemonCondition::Refusing {
                barrier: "b".into(),
                cause: "c".into(),
                at: "1".into(),
            },
            DaemonCondition::NotAnswering {
                pid: Some(7),
                waited_ms: 2000,
            },
        ];
        for c in conditions {
            assert!(c.remedy().contains("porch"), "{c:?} has no command");
            assert!(!c.summary().is_empty());
        }
    }
}

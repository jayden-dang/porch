use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::home::socket_path;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub method: String,
    pub id: u64,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub result: serde_json::Value,
    pub id: u64,
}

/// Probe the daemon health RPC over the Unix socket.
///
/// # Errors
///
/// Returns an error when the socket cannot be connected, written, read, or
/// the response is not valid JSON-RPC health payload.
pub fn health_check(home: &Path) -> Result<bool> {
    let mut stream = UnixStream::connect(socket_path(home))?;
    let req = Request {
        jsonrpc: "2.0".into(),
        method: "health".into(),
        id: 1,
        params: None,
    };
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&req).map_err(|e| crate::Error::Other(e.to_string()))?
    )?;
    let mut buf = String::new();
    std::io::BufReader::new(&mut stream).read_line(&mut buf)?;
    let resp: Response =
        serde_json::from_str(buf.trim()).map_err(|e| crate::Error::Other(e.to_string()))?;
    Ok(resp
        .result
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

/// Ask the daemon to start (or queue) a run.
///
/// # Errors
///
/// Returns an error if the socket cannot be reached or the response is invalid.
pub fn start_run(home: &Path, run_id: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path(home))?;
    let req = Request {
        jsonrpc: "2.0".into(),
        method: "start_run".into(),
        id: 1,
        params: Some(serde_json::json!({"run_id": run_id})),
    };
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&req).map_err(|e| crate::Error::Other(e.to_string()))?
    )?;
    let mut buf = String::new();
    std::io::BufReader::new(&mut stream).read_line(&mut buf)?;
    let resp: Response =
        serde_json::from_str(buf.trim()).map_err(|e| crate::Error::Other(e.to_string()))?;
    if resp.result.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        let err = resp
            .result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("start_run failed");
        Err(crate::Error::Other(err.into()))
    }
}

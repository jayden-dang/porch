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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub result: serde_json::Value,
    pub id: u64,
}

pub fn health_check(home: &Path) -> Result<bool> {
    let mut stream = UnixStream::connect(socket_path(home))?;
    let req = Request {
        jsonrpc: "2.0".into(),
        method: "health".into(),
        id: 1,
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

pub fn handle_line(line: &str) -> Result<String> {
    let req: Request =
        serde_json::from_str(line.trim()).map_err(|e| crate::Error::Other(e.to_string()))?;
    let result = match req.method.as_str() {
        "health" => serde_json::json!({"ok": true, "pid": std::process::id()}),
        other => serde_json::json!({"error": format!("unknown method {other}")}),
    };
    let resp = Response {
        jsonrpc: "2.0".into(),
        result,
        id: req.id,
    };
    serde_json::to_string(&resp).map_err(|e| crate::Error::Other(e.to_string()))
}

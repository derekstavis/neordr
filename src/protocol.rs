use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize)]
pub struct Request {
    pub id: String,
    pub method: String,
    #[serde(default = "empty_params")]
    pub params: Value,
}

fn empty_params() -> Value {
    json!({})
}

pub fn success(id: &str, result: Value) -> Value {
    json!({"id": id, "result": result})
}

pub fn error(id: &str, code: &str, message: &str) -> Value {
    json!({"id": id, "error": {"code": code, "message": message}})
}

/// Keep a backend API error intact through the default-socket router.
#[derive(Debug)]
pub struct RemoteError(pub crate::contracts::api::ErrorBody);

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server: {}: {}", self.0.code, self.0.message)
    }
}
impl std::error::Error for RemoteError {}

pub fn failure(id: &str, fallback: &str, cause: &anyhow::Error) -> Value {
    if let Some(remote) = cause.downcast_ref::<RemoteError>() {
        json!({"id": id, "error": remote.0})
    } else {
        error(id, fallback, &cause.to_string())
    }
}

pub fn read_frame(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut frame = Vec::new();
    reader
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut frame)?;
    ensure!(
        frame.len() <= MAX_FRAME,
        "request exceeds {MAX_FRAME} bytes"
    );
    ensure!(
        frame.last() == Some(&b'\n'),
        "expected newline-terminated JSON"
    );
    Ok(frame)
}

pub fn call(socket: &Path, request: &Request) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_millis(1000)))?;
    stream.set_write_timeout(Some(Duration::from_millis(1000)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let reply: Value = serde_json::from_slice(&read_frame(&mut BufReader::new(stream))?)?;
    ensure!(reply["id"] == request.id, "response ID mismatch");
    if let Some(error) = reply.get("error") {
        return Err(RemoteError(serde_json::from_value(error.clone())?).into());
    }
    reply.get("result").cloned().context("missing result")
}

/// Typed application RPC; raw requests above exist only for CLI/compatibility edges.
pub fn call_typed(socket: &Path, request: &crate::api::Request) -> Result<crate::api::Reply> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_millis(1000)))?;
    stream.set_write_timeout(Some(Duration::from_millis(1000)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let reply: crate::api::Response =
        serde_json::from_slice(&read_frame(&mut BufReader::new(stream))?)?;
    ensure!(reply.id == request.id, "response ID mismatch");
    reply.into_result()
}

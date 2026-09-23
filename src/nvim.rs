use std::{
    collections::BTreeSet,
    env, fs,
    io::Write,
    os::unix::{fs::FileTypeExt, net::UnixStream},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use rmpv::Value;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub socket: PathBuf,
    pub pid: u64,
    pub cwd: String,
    pub servername: String,
    pub generation: Option<String>,
}

/// Each call owns a short-lived channel; notifications may precede the reply.
pub fn exec_lua(socket: &Path, code: &str, args: Vec<Value>) -> Result<Value> {
    lua_rpc(socket, "nvim_exec_lua", code, args)
}

/// Neovim 0.12's immediate RPC entry point. Use only for bounded reads: normal
/// editor operations must remain sequenced with input through exec_lua.
pub fn read_lua(socket: &Path, code: &str, args: Vec<Value>) -> Result<Value> {
    lua_rpc(socket, "nvim__exec_lua_fast", code, args)
}

fn lua_rpc(socket: &Path, method: &str, code: &str, args: Vec<Value>) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to Neovim at {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    rmpv::encode::write_value(
        &mut stream,
        &Value::Array(vec![
            0.into(),
            1.into(),
            method.into(),
            Value::Array(vec![code.into(), Value::Array(args)]),
        ]),
    )?;
    stream.flush()?;
    // Bound unexpected notifications rather than letting an unresponsive peer
    // keep this synchronous discovery operation alive indefinitely.
    for _ in 0..32 {
        let message = rmpv::decode::read_value(&mut stream)?;
        let fields = message.as_array().context("invalid Neovim RPC message")?;
        if fields.first().and_then(Value::as_u64) == Some(2) {
            continue;
        }
        ensure!(
            fields.len() == 4 && fields[0].as_u64() == Some(1) && fields[1].as_u64() == Some(1),
            "unexpected Neovim RPC response"
        );
        ensure!(fields[2].is_nil(), "Neovim: {}", fields[2]);
        return Ok(fields[3].clone());
    }
    bail!("too many Neovim RPC notifications without a response")
}

pub fn inspect(socket: &Path) -> Result<Instance> {
    let response = read_lua(
        socket,
        "local socket=...; return vim.json.encode({socket=socket,pid=vim.fn.getpid(), cwd=vim.fn.getcwd(), \
         servername=vim.v.servername, generation=vim.g.neordr_generation})",
        vec![socket.to_str().context("non-UTF8 socket path")?.into()],
    )?;
    Ok(serde_json::from_str(
        response.as_str().context("expected JSON from Neovim")?,
    )?)
}

pub fn register_generation(socket: &Path) -> Result<Instance> {
    exec_lua(
        socket,
        "if not vim.g.neordr_generation then \
           vim.g.neordr_generation = tostring(vim.fn.getpid()) .. '-' .. string.format('%x', vim.uv.hrtime()) \
         end; return true",
        vec![],
    )?;
    inspect(socket)
}

fn add_candidates(root: &Path, depth: usize, found: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_socket() && name.starts_with("nvim.") && name.ends_with(".0") {
            found.insert(entry.path());
        } else if kind.is_dir() && depth > 0 {
            add_candidates(&entry.path(), depth - 1, found);
        }
    }
}

/// Mirrors the user's OMF plugin roots; explicit paths support custom --listen.
pub fn discover(explicit: &[PathBuf]) -> Vec<Instance> {
    let mut paths: BTreeSet<PathBuf> = explicit.iter().cloned().collect();
    if explicit.is_empty() {
        let user = env::var("USER").unwrap_or_default();
        if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR") {
            let runtime = PathBuf::from(runtime);
            add_candidates(&runtime, 0, &mut paths);
            add_candidates(&runtime.join(format!("nvim.{user}")), 1, &mut paths);
        }
        add_candidates(&env::temp_dir().join(format!("nvim.{user}")), 1, &mut paths);
        if let Some(socket) = env::var_os("NVIM") {
            paths.insert(socket.into());
        }
    }
    // A server may expose several listening sockets. Return one row per process.
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter_map(|path| inspect(&path).ok())
        .filter(|instance| seen.insert(instance.pid))
        .collect()
}

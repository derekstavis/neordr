use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
    api,
    contracts::api::{Method, PingParams, ResponseResult},
    nvim::Instance,
    protocol,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub default: bool,
    pub running: bool,
    pub socket_path: PathBuf,
    pub session_dir: PathBuf,
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name != "default"
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "use a named session of 1–64 ASCII letters, digits, hyphens or underscores; 'default' is reserved"
    );
    Ok(())
}

pub fn config_root() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        ensure!(path.is_absolute(), "XDG_CONFIG_HOME must be absolute");
        return Ok(path);
    }
    Ok(PathBuf::from(env::var_os("HOME").context("HOME is unset")?).join(".config"))
}

pub fn socket_path(config: &Path, name: &str) -> Result<PathBuf> {
    if name == "default" {
        return Ok(config.join("herdr/herdr.sock"));
    }
    validate_name(name)?;
    Ok(config.join("herdr/sessions").join(name).join("herdr.sock"))
}

pub fn inspect_bridge(socket: &Path) -> Result<Instance> {
    let reply = protocol::call_typed(
        socket,
        &api::request("neordr:discovery", Method::Ping(PingParams::default())),
    )?;
    ensure!(
        matches!(reply.result, ResponseResult::Pong { .. }),
        "unexpected ping response"
    );
    reply
        .identity
        .neordr
        .context("this session is not served by Neordr")
}

pub fn bridges(config: &Path) -> Result<Vec<SessionInfo>> {
    Ok(live_bridges(config)?
        .into_iter()
        .map(|(session, _)| session)
        .collect())
}

/// Retain the freshly verified editor identity for request-scoped reads.
pub fn live_bridges(config: &Path) -> Result<Vec<(SessionInfo, Instance)>> {
    let root = config.join("herdr/sessions");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut sessions = vec![];
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if validate_name(&name).is_err() {
            continue;
        }
        let socket = socket_path(config, &name)?;
        sessions.push(SessionInfo {
            name,
            default: false,
            running: true,
            socket_path: socket,
            session_dir: entry.path(),
        });
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let live = crate::parallel::map(&sessions, |s| inspect_bridge(&s.socket_path).ok());
    Ok(sessions
        .into_iter()
        .zip(live)
        .filter_map(|(s, instance)| instance.map(|instance| (s, instance)))
        .collect())
}

/// The default session groups all registered editor workspaces. Per-editor
/// endpoints remain addressable for existing clients, but are not sessions.
pub fn list(config: &Path) -> Result<Vec<SessionInfo>> {
    let socket = config.join("herdr/herdr.sock");
    if protocol::call_typed(
        &socket,
        &api::request("session:list", Method::Ping(PingParams {})),
    )
    .is_err()
    {
        return Ok(vec![]);
    }
    Ok(vec![SessionInfo {
        name: "default".into(),
        default: true,
        running: true,
        socket_path: socket,
        session_dir: config.join("herdr"),
    }])
}

//! Read-only diagnostics. No service startup, terminal input, or config changes.
use crate::{
    api,
    contracts::api::{Method, PingParams, ResponseResult},
    nvim, protocol, session,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Serialize)]
pub struct Check {
    pub subject: String,
    pub status: &'static str,
    pub detail: String,
}
#[derive(Serialize)]
pub struct Report {
    pub version: &'static str,
    pub checks: Vec<Check>,
    pub editors: Vec<Editor>,
}
#[derive(Serialize)]
pub struct Editor {
    pub session: String,
    pub pid: u64,
    pub uis: Vec<Ui>,
    pub panes: Vec<Pane>,
}
#[derive(Serialize, Deserialize)]
pub struct Ui {
    pub channel: u64,
    pub width: u64,
    pub height: u64,
    pub client: String,
}
#[derive(Serialize)]
pub struct Pane {
    pub id: String,
    pub agent: Option<String>,
    pub conversation: Option<String>,
    pub hook_conversations: Vec<String>,
}
#[derive(Default, Deserialize)]
struct Tree {
    #[serde(default)]
    groups: Vec<Group>,
}
#[derive(Deserialize)]
struct Group {
    #[serde(default)]
    children: Vec<Tab>,
}
#[derive(Deserialize)]
struct Tab {
    #[serde(default)]
    panes: Vec<MoshiPane>,
}
#[derive(Deserialize)]
struct MoshiPane {
    id: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}
#[derive(Deserialize)]
struct Version {
    version: String,
}
#[derive(Deserialize)]
struct Hook {
    #[serde(rename = "herdrPane")]
    pane: Option<String>,
    #[serde(rename = "sessionEndedAt")]
    ended: Option<serde_json::Value>,
}
impl Report {
    fn check(
        &mut self,
        subject: impl Into<String>,
        status: &'static str,
        detail: impl Into<String>,
    ) {
        self.checks.push(Check {
            subject: subject.into(),
            status,
            detail: detail.into(),
        });
    }
    pub fn text(&self) -> String {
        let mut text = format!("Neordr {} diagnostics (read-only)\n", self.version);
        for c in &self.checks {
            text += &format!("[{}] {}: {}\n", c.status, c.subject, c.detail);
        }
        for e in &self.editors {
            text += &format!(
                "{}: Neovim PID {}, {} attached UIs\n",
                e.session,
                e.pid,
                e.uis.len()
            );
            for ui in &e.uis {
                text += &format!(
                    "  UI {}: {}x{} ({})\n",
                    ui.channel, ui.width, ui.height, ui.client
                );
            }
        }
        text
    }
}

// Loopback only; bounded reads, including chunked Go HTTP responses. No proxy,
// redirects, credentials or transcript contents enter the diagnostics report.
fn get(port: u16, path: &str) -> Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut bytes = vec![];
    let mut chunk = [0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        ensure!(Instant::now() < deadline, "gateway response timed out");
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        ensure!(
            bytes.len() + n <= 4 * 1024 * 1024,
            "gateway response exceeds limit"
        );
        bytes.extend_from_slice(&chunk[..n]);
    }
    let header = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid HTTP response")?;
    let headers = std::str::from_utf8(&bytes[..header])?;
    ensure!(
        headers
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            == Some("200"),
        "gateway returned {}",
        headers.lines().next().unwrap_or("invalid response")
    );
    let body = &bytes[header + 4..];
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return dechunk(body);
    }
    Ok(body.to_vec())
}
fn dechunk(mut body: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = vec![];
    loop {
        let end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("invalid HTTP chunk")?;
        let size = usize::from_str_radix(
            std::str::from_utf8(&body[..end])?
                .split(';')
                .next()
                .unwrap(),
            16,
        )?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        ensure!(
            size <= body.len().saturating_sub(2) && &body[size..size + 2] == b"\r\n",
            "truncated HTTP chunk"
        );
        decoded.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}
fn state_dir() -> Option<PathBuf> {
    if let Some(p) =
        std::env::var_os("MOSHI_CONFIG_DIR").or_else(|| std::env::var_os("MOSHI_STATE_DIR"))
    {
        return Some(p.into());
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Moshi")
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"))
            .join("moshi")
    })
}
fn hooks(report: &mut Report) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::<String, Vec<String>>::new();
    let Some(root) = state_dir() else {
        return result;
    };
    for agent in ["codex", "claude"] {
        let Ok(entries) = fs::read_dir(root.join(format!("{agent}-sessions"))) else {
            continue;
        };
        for entry in entries.flatten().take(10000) {
            let p = entry.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let parsed = (|| -> Result<Hook> {
                ensure!(
                    entry.metadata()?.len() <= 64 * 1024,
                    "hook record exceeds limit"
                );
                Ok(serde_json::from_slice(&fs::read(&p)?)?)
            })();
            match parsed {
                Ok(h) if h.ended.is_none() => {
                    if let (Some(pane), Some(id)) = (h.pane, p.file_stem().and_then(|x| x.to_str()))
                    {
                        result.entry(pane).or_default().push(id.into());
                    }
                }
                Ok(_) => {}
                Err(e) => report.check(
                    format!("{agent} hook record"),
                    "warn",
                    format!("{}: {e}", p.display()),
                ),
            }
        }
    }
    result
}

pub fn inspect(config: &Path, port: u16, selected: Option<&str>) -> Result<Report> {
    let mut report = Report {
        version: env!("CARGO_PKG_VERSION"),
        checks: vec![],
        editors: vec![],
    };
    let ping = api::request("doctor:ping", Method::Ping(PingParams {}));
    match protocol::call_typed(&session::socket_path(config, "default")?, &ping) {
        Ok(r) if r.identity.neordr_router == Some(true) => {
            report.check("router", "ok", "Neordr router responds")
        }
        Ok(_) => report.check("router", "warn", "Default socket is not a Neordr router"),
        Err(e) => report.check("router", "warn", e.to_string()),
    }
    match get(port, "/v1/version").and_then(|b| Ok(serde_json::from_slice::<Version>(&b)?)) {
        Ok(v) => report.check(
            "Moshi gateway",
            "ok",
            format!("moshi-hook {} on 127.0.0.1:{port}", v.version),
        ),
        Err(e) => report.check(
            "Moshi gateway",
            "warn",
            format!("Unavailable: {e}; check the Moshi service"),
        ),
    }
    let discovered =
        get(port, "/v1/workspaces?mux=herdr").and_then(|b| Ok(serde_json::from_slice::<Tree>(&b)?));
    let tree = match discovered {
        Ok(t) => Some(
            t.groups
                .into_iter()
                .flat_map(|g| g.children)
                .flat_map(|t| t.panes)
                .map(|p| (p.id, p.session_id))
                .collect::<BTreeMap<_, _>>(),
        ),
        Err(e) => {
            report.check("Moshi discovery", "warn", e.to_string());
            None
        }
    };
    let hooks = hooks(&mut report);
    let registry = config.join("herdr/sessions");
    let entries = match fs::read_dir(&registry) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.check("editors", "warn", "No registered editors");
            return Ok(report);
        }
        Err(e) => return Err(e.into()),
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if session::validate_name(&name).is_err() {
            continue;
        }
        if selected.is_some_and(|s| s != "default" && s != name) {
            continue;
        }
        let socket = session::socket_path(config, &name)?;
        let instance = match session::inspect_bridge(&socket) {
            Ok(v) => v,
            Err(e) => {
                report.check(name, "warn", format!("Bridge unavailable: {e}"));
                continue;
            }
        };
        report.check(&name, "ok", "Bridge responds");
        let uis = (|| -> Result<Vec<Ui>> {
            let v = nvim::read_lua(
                &instance.socket,
                r#"local r = {}; for _,u in ipairs(vim.api.nvim_list_uis()) do
local c=vim.api.nvim_get_chan_info(u.chan).client or {}
r[#r+1]={channel=u.chan,width=u.width,height=u.height,client=c.name or 'unreported'} end
return vim.json.encode(r)"#,
                vec![],
            )?;
            Ok(serde_json::from_str(
                v.as_str().context("invalid UI report")?,
            )?)
        })();
        let uis = match uis {
            Ok(v) => v,
            Err(e) => {
                report.check(format!("{name} UIs"), "warn", e.to_string());
                vec![]
            }
        };
        if uis.len() > 1 {
            report.check(format!("{name} sizing"),"info","Multiple UIs share Neovim dimensions; a small UI is not by itself proof of a stale client");
        }
        let snapshot = protocol::call_typed(
            &socket,
            &api::request(
                "doctor:snapshot",
                Method::SessionSnapshot(crate::contracts::api::EmptyParams {}),
            ),
        );
        let mut panes = vec![];
        match snapshot {
            Ok(reply) => {
                if let ResponseResult::SessionSnapshot { snapshot } = reply.result {
                    for p in snapshot.panes {
                        let conversation = tree
                            .as_ref()
                            .and_then(|t| t.get(&p.pane_id))
                            .cloned()
                            .flatten();
                        let saved = hooks.get(&p.pane_id).cloned().unwrap_or_default();
                        if p.agent.is_some()
                            && let Some(tree) = &tree
                        {
                            if !tree.contains_key(&p.pane_id) {
                                report.check(
                                    &p.pane_id,
                                    "warn",
                                    "Agent pane is absent from Moshi workspace discovery",
                                );
                            } else if conversation.is_none() {
                                report.check(&p.pane_id,"warn",format!("Agent detected but Moshi has no conversation ID ({} active hook records). Compare a fresh moshi-hook context with the gateway; this can be stale discovery, not missing history",saved.len()));
                            } else if !saved.is_empty()
                                && !saved.contains(conversation.as_ref().unwrap())
                            {
                                report.check(&p.pane_id,"warn","Moshi conversation differs from saved hook records; verify session lifecycle before routing input");
                            } else {
                                report.check(&p.pane_id,"ok","Moshi advertises a conversation; transcript readability is not checked");
                            }
                        }
                        panes.push(Pane {
                            id: p.pane_id,
                            agent: p.agent,
                            conversation,
                            hook_conversations: saved,
                        });
                    }
                }
            }
            Err(e) => report.check(format!("{name} snapshot"), "warn", e.to_string()),
        }
        report.editors.push(Editor {
            session: name,
            pid: instance.pid,
            uis,
            panes,
        });
    }
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunked_http_is_bounded_and_rejects_truncation() {
        assert_eq!(
            dechunk(b"3\r\nabc\r\n2;ext=yes\r\nde\r\n0\r\n\r\n").unwrap(),
            b"abcde"
        );
        assert!(dechunk(b"ffffffffffffffff\r\na").is_err());
        assert!(dechunk(b"4\r\nabc\r\n").is_err());
    }
}

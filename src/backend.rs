//! Typed Neovim application operations; JSON is decoded once at the Lua boundary.
use crate::{
    api::{Command, LaunchParams},
    contracts::api::*,
    nvim::{self, Instance},
    process,
    protocol::RemoteError,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Debug, Clone, Deserialize)]
pub struct Pane {
    #[serde(flatten)]
    pub info: PaneInfo,
    pub neordr: PaneMetadata,
}
#[derive(Debug, Clone, Deserialize)]
pub struct PaneMetadata {
    pub tabpage: i64,
    #[serde(default)]
    pub buf: Option<i64>,
    #[serde(default)]
    pub channel: Option<i64>,
    #[serde(default)]
    pub shell_pid: Option<u32>,
    #[serde(default)]
    pub tty: Option<String>,
}
#[derive(Debug, Deserialize)]
pub struct Snapshot {
    pub panes: Vec<Pane>,
    pub tabs: Vec<TabInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum LuaReply<T> {
    Success { result: T },
    Error { error: ErrorBody },
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum Internal {
    #[serde(rename = "neordr.snapshot")]
    Snapshot(EmptyParams),
    #[serde(rename = "neordr.send")]
    Send(PaneSendTextParams),
    #[serde(rename = "neordr.launch")]
    Launch(LaunchParams),
    #[serde(rename = "neordr.gesture_prepare")]
    GesturePrepare(GestureParams),
    #[serde(rename = "neordr.gesture_result")]
    GestureResult(GestureOwner),
    #[serde(rename = "neordr.gesture")]
    Gesture(GestureParams),
}
#[derive(Debug, Clone, Serialize)]
pub struct GestureParams {
    pub action: String,
    pub owner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspaces: Option<Vec<WorkspaceInfo>>,
}
#[derive(Debug, Clone, Serialize)]
pub struct GestureOwner {
    pub owner: String,
}
#[derive(Deserialize)]
pub struct Marker {
    pub marker: String,
}
#[derive(Deserialize)]
pub struct GestureResult {
    pub workspace_id: Option<String>,
    pub done: bool,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
}

pub fn initialize(instance: &Instance, session: &str) -> Result<()> {
    // A plugin-configured backend survives its bridge process. Reuse it even
    // while the editor is waiting at a prompt; configuring it again is a
    // deferred mutation and can time out before the bridge opens its endpoint.
    if nvim::read_lua(
        &instance.socket,
        "local p=package.loaded['neordr.backend']; return p ~= nil and p.session == ...",
        vec![session.into()],
    )?
    .as_bool()
        == Some(true)
    {
        return Ok(());
    }
    nvim::exec_lua(
        &instance.socket,
        "local code,session=...; local key='neordr.backend'; if not package.loaded[key] then package.loaded[key]=assert(loadstring(code))() end; return package.loaded[key].configure(session)",
        vec![
            include_str!("../lua/neordr/backend.lua").into(),
            session.into(),
        ],
    )?;
    Ok(())
}
fn lua<T: DeserializeOwned>(
    instance: &Instance,
    method: &impl Serialize,
    read_only: bool,
) -> Result<T> {
    let code = "local request,generation=...; request=vim.json.decode(request); return vim.json.encode(require('neordr.backend').dispatch(request.method,request.params,generation))";
    let args = vec![
        serde_json::to_string(method)?.into(),
        instance
            .generation
            .clone()
            .context("missing instance generation")?
            .into(),
    ];
    let response = if read_only {
        let fast = nvim::read_lua(
            &instance.socket,
            &format!("if not vim.api.nvim_get_mode().blocking then return false end; {code}"),
            args.clone(),
        )?;
        if fast.as_str().is_some() {
            fast
        } else {
            nvim::exec_lua(&instance.socket, code, args)?
        }
    } else {
        nvim::exec_lua(&instance.socket, code, args)?
    };
    match serde_json::from_str::<LuaReply<T>>(
        response.as_str().context("backend returned non-JSON")?,
    )? {
        LuaReply::Success { result } => Ok(result),
        LuaReply::Error { error } => Err(RemoteError(error).into()),
    }
}
pub fn internal<T: DeserializeOwned>(instance: &Instance, method: &Internal) -> Result<T> {
    lua(instance, method, matches!(method, Internal::Snapshot(_)))
}
pub fn snapshot(instance: &Instance) -> Result<Snapshot> {
    internal(instance, &Internal::Snapshot(EmptyParams {}))
}
pub fn empty_session_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        version: concat!("neordr/", env!("CARGO_PKG_VERSION")).into(),
        protocol: crate::contracts::wire::PROTOCOL_VERSION,
        focused_workspace_id: None,
        focused_tab_id: None,
        focused_pane_id: None,
        workspaces: vec![],
        tabs: vec![],
        panes: vec![],
        layouts: vec![],
        agents: vec![],
    }
}

/// Enrich a fresh editor read with a process table owned by this request.
/// The router shares this table across editors; it never caches mutation targets.
pub fn enrich_agents(snapshot: &mut Snapshot, rows: &[process::Process]) {
    for pane in &mut snapshot.panes {
        pane.info.agent = process::foreground(pane, rows)
            .iter()
            .find_map(process::agent)
            .map(str::to_owned);
    }
}

pub fn project_session(snapshot: Snapshot) -> SessionSnapshot {
    let mut s = empty_session_snapshot();
    s.workspaces = snapshot.workspaces;
    s.tabs = snapshot.tabs;
    s.layouts = snapshot.layouts;
    s.panes = snapshot.panes.into_iter().map(|p| p.info).collect();
    s.agents = s
        .panes
        .iter()
        .filter(|p| p.agent.is_some())
        .map(agent_info)
        .collect();
    s.focused_workspace_id = s
        .workspaces
        .iter()
        .find(|w| w.focused)
        .map(|w| w.workspace_id.clone());
    s.focused_tab_id = s.tabs.iter().find(|t| t.focused).map(|t| t.tab_id.clone());
    s.focused_pane_id = s
        .panes
        .iter()
        .find(|p| p.focused)
        .map(|p| p.pane_id.clone());
    s
}
fn keys(keys: &[String]) -> Result<String> {
    let mut bytes = String::new();
    for key in keys {
        let lower = key.to_ascii_lowercase();
        let literal = match lower.as_str() {
            "enter" | "return" => "\r",
            "escape" | "esc" => "\x1b",
            "tab" => "\t",
            "btab" | "shift+tab" => "\x1b[Z",
            "backspace" | "bspace" => "\x7f",
            "up" => "\x1b[A",
            "down" => "\x1b[B",
            "right" => "\x1b[C",
            "left" => "\x1b[D",
            "home" => "\x1b[H",
            "end" => "\x1b[F",
            "delete" => "\x1b[3~",
            "pageup" => "\x1b[5~",
            "pagedown" | "pagedn" => "\x1b[6~",
            "space" => " ",
            _ => {
                if let Some(letter) = lower
                    .strip_prefix("ctrl+")
                    .or_else(|| lower.strip_prefix("ctrl-"))
                    .or_else(|| lower.strip_prefix("c-"))
                    && letter.len() == 1
                    && letter.as_bytes()[0].is_ascii_lowercase()
                {
                    bytes.push((letter.as_bytes()[0] - b'a' + 1) as char);
                    continue;
                }
                if key.chars().count() == 1 {
                    bytes.push_str(key);
                    continue;
                }
                bail!("unsupported terminal key: {key}")
            }
        };
        bytes.push_str(literal);
    }
    Ok(bytes)
}
fn agent_info(p: &PaneInfo) -> AgentInfo {
    AgentInfo {
        terminal_id: p.terminal_id.clone(),
        name: p.label.clone(),
        agent: p.agent.clone(),
        title: p.title.clone(),
        terminal_title: p.terminal_title.clone(),
        terminal_title_stripped: p.terminal_title_stripped.clone(),
        display_agent: p.display_agent.clone(),
        agent_status: p.agent_status,
        screen_detection_skipped: false,
        state_labels: p.state_labels.clone(),
        tokens: p.tokens.clone(),
        agent_session: p.agent_session.clone(),
        workspace_id: p.workspace_id.clone(),
        tab_id: p.tab_id.clone(),
        pane_id: p.pane_id.clone(),
        focused: p.focused,
        launch_pending: false,
        interactive_ready: false,
        state_change_seq: 0,
        completion_seq: None,
        cwd: p.cwd.clone(),
        foreground_cwd: p.foreground_cwd.clone(),
        revision: p.revision,
    }
}
fn find<'a>(panes: &'a [Pane], target: Option<&str>) -> Result<&'a Pane> {
    panes
        .iter()
        .find(|p| target.map_or(p.info.focused, |t| p.info.pane_id == t))
        .context("unknown pane")
}
pub fn dispatch(instance: &Instance, command: &Command) -> Result<ResponseResult> {
    match command {
        Command::PaneList(params) => {
            let mut result = dispatch(
                instance,
                &Command::Herdr(Method::PaneList(params.upstream.clone())),
            )?;
            if let ResponseResult::PaneList { panes } = &mut result {
                panes.retain(|p| params.tab_id.as_ref().is_none_or(|t| &p.tab_id == t));
            }
            return Ok(result);
        }
        Command::Split(_) => return lua(instance, command, false),
        Command::Launch(params) => {
            let snapshot = snapshot(instance)?;
            let pane = find(&snapshot.panes, Some(&params.pane_id))?;
            let rows = process::snapshot()?;
            let foreground = process::foreground(pane, &rows);
            anyhow::ensure!(
                !foreground.is_empty()
                    && foreground.iter().all(|p| {
                        let name = p.command.split_whitespace().next().unwrap_or("");
                        matches!(
                            std::path::Path::new(name)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| n.trim_start_matches('-')),
                            Some("sh" | "bash" | "zsh" | "fish" | "dash")
                        )
                    }),
                "agent launch requires an idle shell"
            );
            return internal(instance, &Internal::Launch(params.clone()));
        }
        Command::Herdr(method) => match method {
            Method::WorkspaceFocus(_)
            | Method::TabFocus(_)
            | Method::PaneFocus(_)
            | Method::AgentFocus(_)
            | Method::WorkspaceCreate(_)
            | Method::TabCreate(_)
            | Method::PaneSplit(_)
            | Method::WorkspaceRename(_)
            | Method::TabRename(_)
            | Method::PaneRename(_)
            | Method::WorkspaceClose(_)
            | Method::TabClose(_)
            | Method::PaneClose(_)
            | Method::WorkspaceMove(_)
            | Method::TabMove(_) => return lua(instance, method, false),
            Method::PaneLayout(_) => return lua(instance, method, true),
            Method::PaneSendText(p) => return internal(instance, &Internal::Send(p.clone())),
            Method::PaneSendKeys(p) => {
                return internal(
                    instance,
                    &Internal::Send(PaneSendTextParams {
                        pane_id: p.pane_id.clone(),
                        text: keys(&p.keys)?,
                    }),
                );
            }
            Method::SessionSnapshot(_)
            | Method::WorkspaceList(_)
            | Method::TabList(_)
            | Method::PaneList(_)
            | Method::AgentList(_)
            | Method::PaneGet(_)
            | Method::PaneProcessInfo(_)
            | Method::PaneRead(_) => {}
            _ => {
                return Err(RemoteError(ErrorBody {
                    code: "invalid_request".into(),
                    message: format!("{} is not supported", command.name()),
                })
                .into());
            }
        },
    }
    let Command::Herdr(method) = command else {
        unreachable!()
    };
    let mut snapshot = snapshot(instance)?;
    if let Method::PaneProcessInfo(p) = method {
        let pane = find(&snapshot.panes, p.pane_id.as_deref())?;
        let rows = process::snapshot_for(pane)?;
        return Ok(ResponseResult::PaneProcessInfo {
            process_info: process::info(pane, &rows),
        });
    }
    if matches!(
        method,
        Method::SessionSnapshot(_)
            | Method::AgentList(_)
            | Method::PaneList(_)
            | Method::PaneGet(_)
    ) {
        let rows = process::snapshot()?;
        enrich_agents(&mut snapshot, &rows);
    }
    Ok(match method {
        Method::SessionSnapshot(_) => ResponseResult::SessionSnapshot {
            snapshot: Box::new(project_session(snapshot)),
        },
        Method::WorkspaceList(_) => ResponseResult::WorkspaceList {
            workspaces: snapshot.workspaces,
        },
        Method::TabList(p) => ResponseResult::TabList {
            tabs: snapshot
                .tabs
                .into_iter()
                .filter(|t| p.workspace_id.as_ref().is_none_or(|w| &t.workspace_id == w))
                .collect(),
        },
        Method::PaneList(p) => ResponseResult::PaneList {
            panes: snapshot
                .panes
                .into_iter()
                .filter(|r| {
                    p.workspace_id
                        .as_ref()
                        .is_none_or(|w| &r.info.workspace_id == w)
                })
                .map(|p| p.info)
                .collect(),
        },
        Method::AgentList(_) => ResponseResult::AgentList {
            agents: snapshot
                .panes
                .iter()
                .filter(|p| p.info.agent.is_some())
                .map(|p| agent_info(&p.info))
                .collect(),
        },
        Method::PaneGet(p) => ResponseResult::PaneInfo {
            pane: find(&snapshot.panes, Some(&p.pane_id))?.info.clone(),
        },
        Method::PaneRead(p) => {
            #[derive(Serialize)]
            struct Read<'a> {
                #[serde(flatten)]
                upstream: &'a PaneReadParams,
                #[serde(skip_serializing_if = "Option::is_none")]
                neordr_rows: Option<u16>,
            }
            #[derive(Serialize)]
            struct ReadRequest<'a> {
                method: &'static str,
                params: Read<'a>,
            }
            let pane = find(&snapshot.panes, Some(&p.pane_id))?;
            let rows = pane
                .neordr
                .tty
                .as_deref()
                .map(process::tty_info)
                .transpose()?
                .filter(|n| *n > 0);
            return lua(
                instance,
                &ReadRequest {
                    method: "pane.read",
                    params: Read {
                        upstream: p,
                        neordr_rows: rows,
                    },
                },
                true,
            );
        }
        _ => unreachable!("unsupported method handled before snapshot"),
    })
}

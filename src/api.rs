//! Typed application boundary around the complete, upstream-derived Herdr API.
//! Raw JSON exists only at the transport/CLI edge; dispatch uses these contracts.
use crate::{
    contracts::{self, api::*},
    nvim::Instance,
    protocol,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchParams {
    pub pane_id: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub enter: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitParams {
    #[serde(flatten)]
    pub upstream: PaneSplitParams,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneListParams {
    #[serde(flatten)]
    pub upstream: crate::contracts::api::PaneListParams,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // Keep the authoritative Method directly matchable.
pub enum Command {
    Herdr(Method),
    PaneList(PaneListParams),
    Split(SplitParams),
    Launch(LaunchParams),
}
impl Serialize for Command {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Herdr(method) => method.serialize(serializer),
            Self::PaneList(params) => serialize_method(serializer, "pane.list", params),
            Self::Split(params) => serialize_method(serializer, "pane.split", params),
            Self::Launch(params) => serialize_method(serializer, "neordr.launch", params),
        }
    }
}
fn serialize_method<S: serde::Serializer, P: Serialize>(
    serializer: S,
    method: &str,
    params: &P,
) -> std::result::Result<S::Ok, S::Error> {
    let mut s = serializer.serialize_struct("Method", 2)?;
    s.serialize_field("method", method)?;
    s.serialize_field("params", params)?;
    s.end()
}
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub id: String,
    #[serde(flatten)]
    pub command: Command,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Identity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neordr: Option<Instance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neordr_router: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    #[serde(flatten)]
    pub result: ResponseResult,
    #[serde(flatten)]
    pub identity: Identity,
}
impl From<ResponseResult> for Reply {
    fn from(result: ResponseResult) -> Self {
        Self {
            result,
            identity: Identity::default(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Outcome {
    Success { result: Box<Reply> },
    Error { error: ErrorBody },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}
impl Response {
    pub fn success(id: &str, result: impl Into<Reply>) -> Self {
        Self {
            id: id.into(),
            outcome: Outcome::Success {
                result: Box::new(result.into()),
            },
        }
    }
    pub fn error(id: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            outcome: Outcome::Error {
                error: ErrorBody {
                    code: code.into(),
                    message: message.into(),
                },
            },
        }
    }
    pub fn failure(id: &str, code: &str, error: &anyhow::Error) -> Self {
        match error.downcast_ref::<protocol::RemoteError>() {
            Some(remote) => Self {
                id: id.into(),
                outcome: Outcome::Error {
                    error: remote.0.clone(),
                },
            },
            None => Self::error(id, code, error.to_string()),
        }
    }
    pub fn into_result(self) -> Result<Reply> {
        match self.outcome {
            Outcome::Success { result } => Ok(*result),
            Outcome::Error { error } => Err(protocol::RemoteError(error).into()),
        }
    }
}

impl Command {
    pub fn name(&self) -> &str {
        match self {
            Self::Herdr(m) => contracts::method_name(m).unwrap_or("(internal)"),
            Self::Split(_) => "pane.split",
            Self::PaneList(_) => "pane.list",
            Self::Launch(_) => "neordr.launch",
        }
    }
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::Split(p) => p
                .upstream
                .target_pane_id
                .as_deref()
                .or(p.upstream.workspace_id.as_deref()),
            Self::Launch(p) => Some(&p.pane_id),
            Self::PaneList(p) => p.tab_id.as_deref().or(p.upstream.workspace_id.as_deref()),
            Self::Herdr(m) => match m {
                Method::WorkspaceFocus(p) | Method::WorkspaceGet(p) => Some(&p.workspace_id),
                Method::WorkspaceClose(p) => Some(&p.workspace_id),
                Method::WorkspaceRename(p) => Some(&p.workspace_id),
                Method::WorkspaceMove(p) => Some(&p.workspace_id),
                Method::WorkspaceCreate(p) => p.source_workspace_id.as_deref(),
                Method::TabCreate(p) => p.workspace_id.as_deref(),
                Method::TabList(p) => p.workspace_id.as_deref(),
                Method::TabFocus(p) | Method::TabGet(p) | Method::TabClose(p) => Some(&p.tab_id),
                Method::TabRename(p) => Some(&p.tab_id),
                Method::TabMove(p) => Some(&p.tab_id),
                Method::PaneFocus(p) | Method::PaneGet(p) | Method::PaneClose(p) => {
                    Some(&p.pane_id)
                }
                Method::PaneLayout(p) => p.pane_id.as_deref(),
                Method::PaneProcessInfo(p) => p.pane_id.as_deref(),
                Method::PaneRename(p) => Some(&p.pane_id),
                Method::PaneRead(p) => Some(&p.pane_id),
                Method::PaneSendText(p) => Some(&p.pane_id),
                Method::PaneSendKeys(p) => Some(&p.pane_id),
                Method::PaneList(p) => p.workspace_id.as_deref(),
                Method::AgentFocus(p) => Some(&p.target),
                _ => None,
            },
        }
    }
    pub fn remembers_selection(&self) -> bool {
        match self {
            Self::Split(p) => p.upstream.focus,
            Self::Herdr(
                Method::WorkspaceFocus(_)
                | Method::TabFocus(_)
                | Method::PaneFocus(_)
                | Method::AgentFocus(_),
            ) => true,
            Self::Herdr(Method::WorkspaceCreate(p)) => p.focus,
            Self::Herdr(Method::TabCreate(p)) => p.focus,
            _ => false,
        }
    }
}

/// Legacy aliases/defaults stay at this edge, then undergo full upstream typing.
/// This preserves Moshi compatibility without weakening any dispatch parameter.
pub fn decode(raw: &protocol::Request) -> Result<Request> {
    let mut params = raw.params.clone();
    anyhow::ensure!(params.is_object(), "params must be an object");
    let command = match raw.method.as_str() {
        "neordr.launch" => Command::Launch(serde_json::from_value(params)?),
        "pane.list" if params.get("tab_id").is_some() => {
            Command::PaneList(serde_json::from_value(params)?)
        }
        "pane.split" => Command::Split(serde_json::from_value(params)?),
        method => {
            if method == "pane.read" {
                params
                    .as_object_mut()
                    .unwrap()
                    .entry("source")
                    .or_insert(Value::String("visible".into()));
                if !matches!(
                    params["source"].as_str(),
                    Some("visible" | "recent" | "recent_unwrapped" | "detection")
                ) {
                    return Err(protocol::RemoteError(ErrorBody {
                        code: "unsupported_source".into(),
                        message: "Use visible, detection, recent or recent_unwrapped".into(),
                    })
                    .into());
                }
            }
            if method == "agent.focus"
                && params.get("target").is_none()
                && let Some(pane) = params.get("pane_id").cloned()
            {
                params["target"] = pane;
            }
            if method == "tab.focus"
                && params.get("tab_id").is_none()
                && let Some(pane) = params.get("pane_id").and_then(Value::as_str)
            {
                params["tab_id"] = format!("{pane}:tab").into();
            }
            Command::Herdr(
                serde_json::from_value(serde_json::json!({"method":method,"params":params}))
                    .with_context(|| format!("invalid {method} request"))?,
            )
        }
    };
    Ok(Request {
        id: raw.id.clone(),
        command,
    })
}

pub fn request(id: impl Into<String>, method: Method) -> Request {
    Request {
        id: id.into(),
        command: Command::Herdr(method),
    }
}

//! Pure upstream detection; mutable observations live in the owning editor.
#[allow(dead_code, unused_imports, clippy::all)]
mod upstream {
    include!(concat!(env!("OUT_DIR"), "/detection.rs"));
}

pub fn detect(agent: &str, screen: &str, title: &str) -> Option<&'static str> {
    let agent = upstream::parse_agent_label(agent)?;
    let found = upstream::manifest::detect_with_osc(
        agent,
        upstream::manifest::DetectionInput {
            screen,
            osc_title: title,
            osc_progress: "",
        },
    );
    if found.skip_state_update {
        return None;
    }
    // An unmatched screen is not evidence of completion. Keep the last known
    // state until live chrome supplies a positive signal.
    if found.state == upstream::AgentState::Idle && !found.visible_idle {
        return None;
    }
    Some(upstream::manifest::agent_state_label(found.state))
}

use crate::{
    backend::Snapshot,
    contracts::api::AgentStatus,
    nvim::{self, Instance},
    process,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct State {
    pub status: AgentStatus,
    pub seq: u64,
    #[serde(default)]
    pub completion: Option<u64>,
    #[serde(default)]
    pub identity: String,
    pub observation: u64,
}
#[derive(Serialize)]
struct Vote<'a> {
    pane_id: &'a str,
    revision: u64,
    title: &'a str,
    observation: u64,
    identity: String,
    status: &'static str,
}

pub fn enrich(
    instance: &Instance,
    snapshot: &mut Snapshot,
    rows: &[process::Process],
) -> Result<()> {
    let mut votes = Vec::new();
    for pane in &snapshot.panes {
        let previous = pane.neordr.activity.as_ref();
        let identity = process::foreground(pane, rows)
            .iter()
            .find_map(|p| process::agent(p).map(|agent| format!("{agent}:{}", p.pid)))
            .unwrap_or_default();
        let status = pane.info.agent.as_deref().and_then(|agent| {
            detect(
                agent,
                &pane.neordr.screen,
                pane.info.terminal_title.as_deref().unwrap_or_default(),
            )
        });
        let status = if identity.is_empty() {
            Some("unknown")
        } else {
            status
        };
        let current = previous.map_or(AgentStatus::Unknown, |p| p.status);
        let changed = previous.map_or(!identity.is_empty(), |p| p.identity != identity)
            || status.is_some_and(|s| {
                s != label(current)
                    && !(s == "idle" && current == AgentStatus::Done && !pane.info.focused)
            })
            || (current == AgentStatus::Done && pane.info.focused);
        if changed {
            votes.push(Vote {
                pane_id: &pane.info.pane_id,
                revision: pane.info.revision,
                title: pane.info.terminal_title.as_deref().unwrap_or_default(),
                observation: previous.map_or(0, |p| p.observation),
                identity,
                status: status.unwrap_or(""),
            });
        }
    }
    if !votes.is_empty() {
        let result = nvim::read_lua(
            &instance.socket,
            "local votes,generation=...; return vim.json.encode(require('neordr.backend').update_activity(vim.json.decode(votes),generation))",
            vec![
                serde_json::to_string(&votes)?.into(),
                instance
                    .generation
                    .clone()
                    .context("missing generation")?
                    .into(),
            ],
        )?;
        let states: HashMap<String, State> =
            serde_json::from_str(result.as_str().context("invalid activity reply")?)?;
        for pane in &mut snapshot.panes {
            if let Some(state) = states.get(&pane.info.pane_id) {
                pane.neordr.activity = Some(state.clone());
            }
        }
    }
    for pane in &mut snapshot.panes {
        pane.info.agent_status = pane
            .neordr
            .activity
            .as_ref()
            .map_or(AgentStatus::Unknown, |s| s.status);
    }
    for tab in &mut snapshot.tabs {
        tab.agent_status = aggregate(
            snapshot
                .panes
                .iter()
                .filter(|p| p.info.tab_id == tab.tab_id)
                .map(|p| p.info.agent_status),
        );
    }
    for workspace in &mut snapshot.workspaces {
        workspace.agent_status = aggregate(
            snapshot
                .panes
                .iter()
                .filter(|p| p.info.workspace_id == workspace.workspace_id)
                .map(|p| p.info.agent_status),
        );
    }
    Ok(())
}
fn aggregate(states: impl Iterator<Item = AgentStatus>) -> AgentStatus {
    states
        .max_by_key(|s| match s {
            AgentStatus::Blocked => 4,
            AgentStatus::Done => 3,
            AgentStatus::Working => 2,
            AgentStatus::Idle => 1,
            AgentStatus::Unknown => 0,
        })
        .unwrap_or(AgentStatus::Unknown)
}
fn label(state: AgentStatus) -> &'static str {
    match state {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upstream_title_and_live_screen_signals() {
        assert_eq!(detect("codex", "", "⠋ Building"), Some("working"));
        assert_eq!(detect("codex", "", "Action Required"), Some("blocked"));
        assert_eq!(detect("codex", "", "Codex"), Some("idle"));
        assert_eq!(detect("claude", "", "◐ Building"), Some("working"));
        assert_eq!(detect("claude", "", "✳ Claude Code"), Some("idle"));
        assert_eq!(
            detect("codex", "• Working (3s • esc to interrupt)\n› ", "Codex"),
            Some("working")
        );
        assert_eq!(
            detect("claude", "✻ Thinking… (3s · esc to interrupt)", ""),
            Some("working")
        );
    }
    #[test]
    fn history_and_unmatched_text_do_not_complete_a_turn() {
        assert_eq!(detect("claude", "Ordinary response text", ""), None);
        assert_eq!(detect("shell", "Working", ""), None);
        assert_eq!(
            detect(
                "codex",
                "↑/↓ to scroll  pgup/pgdn to  home/end to jump  q to quit  esc to edit prev",
                "Codex"
            ),
            None
        );
    }
}

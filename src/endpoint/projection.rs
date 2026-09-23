//! Pure projection from the public API snapshot to Herdr's stable shell model.
use crate::contracts::{api, wire};
use std::collections::HashMap;

fn sorted_pairs(map: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut pairs: Vec<_> = map
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    pairs.sort();
    pairs
}

pub fn snapshot(
    source: &api::SessionSnapshot,
    boot: &str,
    revision: u64,
    cwd: &str,
) -> wire::ClientShellSnapshot {
    wire::ClientShellSnapshot {
        boot_id: boot.to_owned(),
        revision,
        config_diagnostic: None,
        product_announcement: None,
        update_available: None,
        update_install_command: String::new(),
        server_keybindings_toml: None,
        latest_release_notes_available: false,
        integration_updates_available: false,
        worktree_directory: String::new(),
        release_notes: None,
        focused_workspace_id: source.focused_workspace_id.clone(),
        focused_tab_id: source.focused_tab_id.clone(),
        focused_pane_id: source.focused_pane_id.clone(),
        tab_bar_right: Vec::new(),
        tab_bar_right_separator: String::new(),
        agent_view_label: None,
        agent_order: Vec::new(),
        commands: Vec::new(),
        workspaces: source
            .workspaces
            .iter()
            .map(|workspace| wire::ClientShellWorkspace {
                workspace_id: workspace.workspace_id.clone(),
                active_tab_id: workspace.active_tab_id.clone(),
                new_workspace_cwd: cwd.to_owned(),
                number: workspace.number,
                label: workspace.label.clone(),
                custom_label: true,
                branch: None,
                git_ahead_behind: None,
                tokens: sorted_pairs(&workspace.tokens),
                worktree: workspace
                    .worktree
                    .as_ref()
                    .map(|worktree| wire::ClientShellWorktree {
                        key: worktree.repo_key.clone(),
                        label: worktree.repo_name.clone(),
                        is_linked_worktree: worktree.is_linked_worktree,
                    }),
                focused: workspace.focused,
                agent_status: workspace.agent_status,
            })
            .collect(),
        tabs: source
            .tabs
            .iter()
            .map(|tab| wire::ClientShellTab {
                tab_id: tab.tab_id.clone(),
                workspace_id: tab.workspace_id.clone(),
                number: tab.number,
                label: tab.label.clone(),
                custom_label: true,
                zoomed: false,
                focused: tab.focused,
                agent_status: tab.agent_status,
            })
            .collect(),
        panes: source
            .panes
            .iter()
            .map(|pane| wire::ClientShellPane {
                pane_id: pane.pane_id.clone(),
                workspace_id: pane.workspace_id.clone(),
                tab_id: pane.tab_id.clone(),
                label: pane.label.clone(),
                cwd: pane.cwd.clone(),
                foreground_cwd: pane.foreground_cwd.clone(),
                focused: pane.focused,
                right_click_passthrough: false,
            })
            .collect(),
        agents: source
            .agents
            .iter()
            .map(|agent| wire::ClientShellAgent {
                pane_id: agent.pane_id.clone(),
                workspace_id: agent.workspace_id.clone(),
                tab_id: agent.tab_id.clone(),
                name: agent.name.clone(),
                display_agent: agent.display_agent.clone().or_else(|| agent.agent.clone()),
                agent: agent.agent.clone(),
                title: agent.title.clone(),
                terminal_title: agent.terminal_title.clone(),
                terminal_title_stripped: agent
                    .terminal_title_stripped
                    .clone()
                    .or_else(|| agent.terminal_title.clone()),
                agent_status: agent.agent_status,
                state_change_seq: agent.state_change_seq,
                state_labels: sorted_pairs(&agent.state_labels),
                tokens: sorted_pairs(&agent.tokens),
                focused: agent.focused,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_excludes_screen_revisions_and_preserves_identity_and_metadata() {
        let mut source: api::SessionSnapshot = serde_json::from_value(serde_json::json!({
            "version":"test", "protocol":22, "focused_workspace_id":"w", "focused_tab_id":"t", "focused_pane_id":"p",
            "workspaces":[{"workspace_id":"w", "number":1, "label":"editor", "focused":true,"pane_count":1,"tab_count":1,"active_tab_id":"t","agent_status":"working","tokens":{"z":"last","a":"first"}}],
            "tabs":[{"tab_id":"t","workspace_id":"w","number":1,"label":"shell","focused":true,"pane_count":1,"agent_status":"working"}],
            "panes":[{"pane_id":"p","terminal_id":"term","workspace_id":"w","tab_id":"t","focused":true,"agent_status":"working","revision":1,"cwd":"/project"}],
            "layouts":[],
            "agents":[{"terminal_id":"term","pane_id":"p","workspace_id":"w","tab_id":"t","focused":true,"agent_status":"working","revision":1,"name":"assistant","agent":"codex","terminal_title":"Codex","state_change_seq":7}]
        })).unwrap();
        let before = snapshot(&source, "boot", 4, "/project");
        source.panes[0].revision += 1;
        source.agents[0].revision += 1;
        assert_eq!(before, snapshot(&source, "boot", 4, "/project"));
        assert_eq!(
            before.workspaces[0].tokens,
            vec![("a".into(), "first".into()), ("z".into(), "last".into())]
        );
        assert_eq!(before.agents[0].state_change_seq, 7);
        assert_eq!(before.agents[0].display_agent.as_deref(), Some("codex"));
        assert_eq!(before.panes[0].cwd.as_deref(), Some("/project"));
        source.panes[0].label = Some("new label".into());
        assert_ne!(before, snapshot(&source, "boot", 4, "/project"));
    }
}

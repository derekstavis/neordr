//! Read-only Moshi hook associations projected through Herdr's typed contract.
//! A session-name migration must not require restarting an existing agent. Pane
//! identity and its live foreground agent PID are the authority, not cwd/title.
use crate::{
    contracts::{agent_resume::AgentSessionRefKind, api::AgentSessionInfo},
    process,
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hook {
    herdr_pane: String,
    agent_pid: u32,
    #[serde(default)]
    session_ended_at: Option<serde_json::Value>,
}
struct Entry {
    pid: u32,
    session: AgentSessionInfo,
}
#[derive(Default)]
pub(crate) struct Index {
    panes: HashMap<String, Vec<Entry>>,
}
impl Index {
    fn read(root: &Path) -> Self {
        let mut index = Self::default();
        for agent in ["claude", "codex"] {
            let Ok(files) = fs::read_dir(root.join(format!("{agent}-sessions"))) else {
                continue;
            };
            for file in files.flatten().take(10000) {
                let path = file.path();
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if path.extension().and_then(|s| s.to_str()) != Some("json") || id.is_empty() {
                    continue;
                }
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() || meta.len() > 64 * 1024 {
                    continue;
                }
                let Ok(bytes) = fs::read(&path) else { continue };
                let Ok(hook) = serde_json::from_slice::<Hook>(&bytes) else {
                    continue;
                };
                if hook.session_ended_at.is_some() || hook.agent_pid == 0 {
                    continue;
                }
                index.panes.entry(hook.herdr_pane).or_default().push(Entry {
                    pid: hook.agent_pid,
                    session: AgentSessionInfo {
                        source: "moshi-hook".into(),
                        agent: agent.into(),
                        kind: AgentSessionRefKind::Id,
                        value: id.into(),
                    },
                });
            }
        }
        index
    }
    pub(crate) fn resolve(
        &self,
        pane: &str,
        foreground: &[process::Process],
    ) -> Option<AgentSessionInfo> {
        let mut matches = self.panes.get(pane)?.iter().filter(|entry| {
            foreground.iter().any(|p| {
                p.pid == entry.pid && process::agent(p) == Some(entry.session.agent.as_str())
            })
        });
        let first = matches.next()?;
        // A reused process with conflicting unended conversations is ambiguous.
        if matches.any(|other| other.session != first.session) {
            return None;
        }
        Some(first.session.clone())
    }
}
struct Cache {
    root: Option<PathBuf>,
    refreshed: Instant,
    index: Arc<Index>,
}
pub(crate) fn current() -> Arc<Index> {
    static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();
    let root = crate::doctor::state_dir();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cache.as_ref()
        && c.root == root
        && c.refreshed.elapsed() < Duration::from_secs(1)
    {
        return Arc::clone(&c.index);
    }
    let index = Arc::new(root.as_deref().map(Index::read).unwrap_or_default());
    *cache = Some(Cache {
        root,
        refreshed: Instant::now(),
        index: Arc::clone(&index),
    });
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn process(pid: u32, agent: &str) -> process::Process {
        process::Process {
            pid,
            parent: 1,
            group: pid,
            foreground_group: pid as i32,
            command: agent.into(),
        }
    }
    #[test]
    fn legacy_session_requires_exact_live_pane_pid_and_agent() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("claude-sessions");
        fs::create_dir(&dir).unwrap();
        let record = |name: &str, value: serde_json::Value| {
            fs::write(dir.join(format!("{name}.json")), value.to_string()).unwrap()
        };
        let live =
            json!({"herdrPane":"editor:g1:w1:p1","herdrSession":"legacy-editor","agentPid":42});
        record("current", live.clone());
        record(
            "ended",
            json!({"herdrPane":"editor:g1:w1:p1","agentPid":42,"sessionEndedAt":1}),
        );
        record(
            "old-process",
            json!({"herdrPane":"editor:g1:w1:p1","agentPid":41}),
        );
        record("missing-pid", json!({"herdrPane":"editor:g1:w1:p1"}));
        fs::write(dir.join("broken.json"), "{").unwrap();
        let index = Index::read(root.path());
        assert_eq!(
            index
                .resolve("editor:g1:w1:p1", &[process(42, "claude")])
                .unwrap()
                .value,
            "current"
        );
        assert!(
            index
                .resolve("editor:g2:w1:p1", &[process(42, "claude")])
                .is_none()
        );
        assert!(
            index
                .resolve("editor:g1:w1:p1", &[process(43, "claude")])
                .is_none()
        );
        assert!(
            index
                .resolve("editor:g1:w1:p1", &[process(42, "codex")])
                .is_none()
        );
        assert!(index.resolve("editor:g1:w1:p1", &[]).is_none());
        record("ambiguous", live);
        assert!(
            Index::read(root.path())
                .resolve("editor:g1:w1:p1", &[process(42, "claude")])
                .is_none()
        );
        fs::remove_file(dir.join("current.json")).unwrap();
        assert_eq!(
            Index::read(root.path())
                .resolve("editor:g1:w1:p1", &[process(42, "claude")])
                .unwrap()
                .value,
            "ambiguous"
        );
    }
}

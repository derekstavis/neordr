//! Foreground process and PTY metadata; never infer an agent from screen text.
use crate::{
    backend::Pane,
    contracts::api::{PaneProcessInfo, PaneProcessInfoProcess},
};
use anyhow::{Context, Result};
use std::{
    fs::OpenOptions,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    pub foreground_group: i32,
    pub command: String,
}

const FIELDS: &str = "pid=,ppid=,pgid=,tpgid=,command=";

pub fn snapshot() -> Result<Vec<Process>> {
    query(&["-axo", FIELDS])
}

fn query(args: &[&str]) -> Result<Vec<Process>> {
    let output = Command::new("ps").args(args).output()?;
    anyhow::ensure!(output.status.success(), "ps failed");
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some(Process {
                pid: words.next()?.parse().ok()?,
                parent: words.next()?.parse().ok()?,
                group: words.next()?.parse().ok()?,
                foreground_group: words.next()?.parse().ok()?,
                command: words.collect::<Vec<_>>().join(" "),
            })
        })
        .collect())
}

/// Fresh metadata for one terminal, without reading every host command line.
/// Selection is by process group/session, not TTY: a foreground-group member
/// can detach its controlling terminal and must still be included.
pub fn snapshot_for(pane: &Pane) -> Result<Vec<Process>> {
    let Some(root) = pane.neordr.shell_pid else {
        return Ok(vec![]);
    };
    if let Ok(Some(rows)) = scoped_snapshot(root, pane.neordr.tty.as_deref())
        && complete_foreground(root, &rows)
    {
        return Ok(rows);
    }
    // Unsupported selectors, changing groups, missing roots or ancestry gaps
    // use the existing full-host path. No cached identity or guess is used.
    snapshot()
}

#[cfg(target_os = "macos")]
fn scoped_snapshot(root: u32, tty: Option<&str>) -> Result<Option<Vec<Process>>> {
    let pid = root.to_string();
    let initial = query(&["-p", &pid, "-o", FIELDS])?;
    let Some(process) = initial.iter().find(|p| p.pid == root) else {
        return Ok(None);
    };
    let group = process.foreground_group;
    if group <= 0 {
        return Ok(Some(initial));
    }
    // Darwin combines selectors with OR. Include the entire foreground group
    // (even TTY-detached members), the root, and the TTY's background wrappers
    // that often connect a foreground agent back to its shell. Missing ancestry
    // still falls back to a full scan. Re-read the root to detect group changes.
    let group_arg = group.to_string();
    let mut args = vec!["-p", &pid, "-g", &group_arg, "-o", FIELDS];
    if let Some(tty) = tty.filter(|t| !t.is_empty()) {
        args.extend(["-t", tty]);
    }
    let rows = query(&args)?;
    Ok(rows
        .iter()
        .any(|p| p.pid == root && p.foreground_group == group)
        .then_some(rows))
}

#[cfg(target_os = "linux")]
fn scoped_snapshot(root: u32, _tty: Option<&str>) -> Result<Option<Vec<Process>>> {
    let pid = libc::pid_t::try_from(root)?;
    anyhow::ensure!(pid > 0, "invalid terminal root PID");
    // getsid only reads process identity. Linux ps -s includes all members of
    // the session, including group members that have detached their TTY.
    let session = unsafe { libc::getsid(pid) };
    anyhow::ensure!(session > 0, "cannot query terminal process session");
    Ok(Some(query(&["-s", &session.to_string(), "-o", FIELDS])?))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn scoped_snapshot(_root: u32, _tty: Option<&str>) -> Result<Option<Vec<Process>>> {
    Ok(None)
}

fn complete_foreground(root: u32, rows: &[Process]) -> bool {
    let Some(process) = rows.iter().find(|p| p.pid == root) else {
        return false;
    };
    let Ok(group) = u32::try_from(process.foreground_group) else {
        return true;
    };
    rows.iter()
        .filter(|p| p.group == group)
        .all(|p| ancestry(p, root, rows).is_some())
}

pub fn tty_info(path: &str) -> Result<u16> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)
        .context("open terminal for metadata")?;
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = file.as_raw_fd();
    // The fd is an owned live terminal descriptor; ioctl writes one winsize.
    anyhow::ensure!(
        unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0,
        "cannot query terminal dimensions"
    );
    Ok(size.ws_row)
}

/// None means the selected rows cannot establish the ancestry, not "unrelated".
fn ancestry(process: &Process, root: u32, rows: &[Process]) -> Option<bool> {
    let mut pid = process.pid;
    for _ in 0..128 {
        if pid == root {
            return Some(true);
        }
        let parent = rows.iter().find(|p| p.pid == pid)?;
        if parent.parent == pid || parent.parent == 0 {
            return Some(false);
        }
        pid = parent.parent;
    }
    None
}

pub fn foreground(pane: &Pane, rows: &[Process]) -> Vec<Process> {
    let Some(root) = pane.neordr.shell_pid else {
        return vec![];
    };
    let Some(group) = foreground_group(pane, rows) else {
        return vec![];
    };
    rows.iter()
        .filter(|p| p.group == group && ancestry(p, root, rows) == Some(true))
        .cloned()
        .collect()
}

fn foreground_group(pane: &Pane, rows: &[Process]) -> Option<u32> {
    let root = pane.neordr.shell_pid?;
    let group = rows.iter().find(|p| p.pid == root)?.foreground_group;
    u32::try_from(group).ok().filter(|g| *g > 0)
}

pub fn agent(process: &Process) -> Option<&'static str> {
    let words: Vec<_> = process.command.split_whitespace().collect();
    let first = std::path::Path::new(words.first()?).file_name()?.to_str()?;
    let name = match first {
        "node" | "nodejs" | "python" | "python3" | "Python" | "bash" | "sh" => {
            std::path::Path::new(words.get(1)?).file_name()?.to_str()?
        }
        name => name,
    };
    match name {
        "claude" | "claude-code" | "claude.js" => Some("claude"),
        "codex" | "codex.js" => Some("codex"),
        _ => None,
    }
}

pub fn info(pane: &Pane, rows: &[Process]) -> PaneProcessInfo {
    let foreground_processes = foreground(pane, rows)
        .iter()
        .map(|p| {
            let argv: Vec<_> = p.command.split_whitespace().collect();
            PaneProcessInfoProcess {
                pid: p.pid,
                name: agent(p)
                    .unwrap_or_else(|| argv.first().copied().unwrap_or(""))
                    .into(),
                argv0: argv.first().map(|s| (*s).into()),
                cmdline: Some(p.command.clone()),
                argv: None,
                cwd: None,
            }
        })
        .collect();
    PaneProcessInfo {
        pane_id: pane.info.pane_id.clone(),
        shell_pid: pane.neordr.shell_pid,
        foreground_process_group_id: foreground_group(pane, rows),
        tty: pane.neordr.tty.clone(),
        foreground_processes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, parent: u32, group: u32) -> Process {
        Process {
            pid,
            parent,
            group,
            foreground_group: 30,
            command: String::new(),
        }
    }

    #[test]
    fn scoped_rows_must_establish_every_foreground_members_ancestry() {
        let root = row(10, 1, 10);
        let child = row(30, 20, 30);
        assert!(!complete_foreground(10, &[root.clone(), child.clone()]));
        assert!(complete_foreground(
            10,
            &[root.clone(), row(20, 10, 20), child.clone()]
        ));
        assert!(!complete_foreground(10, std::slice::from_ref(&child)));
        assert!(!complete_foreground(10, &[root, row(20, 30, 20), child]));
    }

    #[test]
    fn unrelated_and_background_processes_do_not_create_ancestry_gaps() {
        let rows = [row(10, 1, 10), row(20, 0, 30), row(40, 99, 40)];
        assert!(complete_foreground(10, &rows));
        assert_eq!(ancestry(&rows[1], 10, &rows), Some(false));
        assert_eq!(ancestry(&rows[0], 10, &rows), Some(true));
    }
}

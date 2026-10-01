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

pub fn snapshot() -> Result<Vec<Process>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,tpgid=,command="])
        .output()?;
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

fn descendant(process: &Process, root: u32, rows: &[Process]) -> bool {
    let mut pid = process.pid;
    for _ in 0..128 {
        if pid == root {
            return true;
        }
        let Some(parent) = rows.iter().find(|p| p.pid == pid) else {
            return false;
        };
        if parent.parent == pid || parent.parent == 0 {
            return false;
        }
        pid = parent.parent;
    }
    false
}

pub fn foreground(pane: &Pane, rows: &[Process]) -> Vec<Process> {
    let Some(root) = pane.neordr.shell_pid else {
        return vec![];
    };
    let Some(group) = foreground_group(pane, rows) else {
        return vec![];
    };
    rows.iter()
        .filter(|p| p.group == group && descendant(p, root, rows))
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

//! Herdr JSON-lines transport over the existing, lifecycle-safe full-editor attach.
use crate::{
    api,
    contracts::{
        api::{Method, PaneTarget},
        terminal_control::terminal_control_command_from_json,
        wire::{AttachScrollDirection, AttachScrollSource, ClientMessage},
    },
    protocol, session,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use std::{
    collections::VecDeque,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 4 * 1024 * 1024;
#[derive(Serialize)]
#[serde(tag = "type")]
enum Record {
    #[serde(rename = "terminal.frame")]
    Frame {
        seq: u64,
        encoding: &'static str,
        width: u16,
        height: u16,
        full: bool,
        bytes: String,
    },
    #[serde(rename = "terminal.closed")]
    Closed { reason: &'static str },
}

struct Attachment {
    child: Child,
    master: File,
}
impl Drop for Attachment {
    fn drop(&mut self) {
        // The child is our own attachment, never the editor. Its signal handler
        // tears down its remote UI, including gesture state and terminal modes.
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let until = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
struct OutputFlags(i32);
impl Drop for OutputFlags {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(1, libc::F_SETFL, self.0);
        }
    }
}

fn dimensions(cols: u16, rows: u16) -> Result<libc::winsize> {
    ensure!(
        (1..=1000).contains(&cols) && (1..=1000).contains(&rows),
        "terminal dimensions must be 1–1000 cells"
    );
    Ok(libc::winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    })
}
fn start(name: &str, size: &mut libc::winsize) -> Result<Attachment> {
    let (mut master, mut slave) = (-1, -1);
    ensure!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                size,
            )
        } == 0,
        "allocate control PTY: {}",
        io::Error::last_os_error()
    );
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
            "set PTY close-on-exec"
        );
    }
    // Disable echo before any queued input can reach the child.
    let mut term = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut term) } == 0,
        "read PTY mode"
    );
    unsafe {
        libc::cfmakeraw(&mut term);
    }
    ensure!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &term) } == 0,
        "set PTY mode"
    );
    ensure!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == 0,
        "set PTY nonblocking"
    );
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["--session", name])
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(Attachment {
        child: cmd.spawn().context("start control attachment")?,
        master,
    })
}
fn enqueue(queue: &mut VecDeque<u8>, bytes: &[u8]) -> Result<()> {
    ensure!(
        queue.len() + bytes.len() <= MAX_PENDING,
        "terminal control backlog exceeds limit"
    );
    queue.extend(bytes);
    Ok(())
}
fn record(queue: &mut VecDeque<u8>, value: Record) -> Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    enqueue(queue, &bytes)
}
fn flush(fd: i32, queue: &mut VecDeque<u8>) -> Result<()> {
    if queue.is_empty() {
        return Ok(());
    }
    let bytes = queue.as_slices().0;
    let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if n > 0 {
        queue.drain(..n as usize);
    } else if n < 0 {
        let e = io::Error::last_os_error();
        if !matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            return Err(e.into());
        }
    }
    Ok(())
}
fn scroll(
    source: AttachScrollSource,
    direction: AttachScrollDirection,
    lines: u16,
    column: Option<u16>,
    row: Option<u16>,
    modifiers: u8,
) -> Vec<u8> {
    if let AttachScrollSource::PageKey { input } = source {
        return input;
    }
    let code = match direction {
        AttachScrollDirection::Up => 64,
        AttachScrollDirection::Down => 65,
    } + if modifiers & 1 != 0 { 4 } else { 0 }
        + if modifiers & 2 != 0 { 16 } else { 0 }
        + if modifiers & 4 != 0 { 8 } else { 0 };
    format!(
        "\x1b[<{code};{};{}M",
        u32::from(column.unwrap_or(0)) + 1,
        u32::from(row.unwrap_or(0)) + 1
    )
    .repeat(lines.min(100) as usize)
    .into_bytes()
}

pub fn run(
    config: &Path,
    requested_session: Option<&str>,
    target: &str,
    cols: u16,
    rows: u16,
) -> Result<i32> {
    let mut size = dimensions(cols, rows)?;
    let (name, _) = target
        .split_once(':')
        .context("target must be a Neordr pane ID")?;
    ensure!(
        requested_session.is_none_or(|s| s == "default" || s == name),
        "pane belongs to another session"
    );
    let socket = session::socket_path(config, name)?;
    let instance = session::inspect_bridge(&socket)?;
    protocol::call_typed(
        &socket,
        &api::request(
            "control:focus",
            Method::PaneFocus(PaneTarget {
                pane_id: target.into(),
            }),
        ),
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let flags = unsafe { libc::fcntl(1, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(1, libc::F_SETFL, flags | libc::O_NONBLOCK) } == 0,
        "set control output nonblocking"
    );
    let _flags = OutputFlags(flags);
    let mut ui = start(name, &mut size)?;
    let parent = unsafe { libc::getppid() };
    let (mut incoming, mut input, mut output) = (Vec::new(), VecDeque::new(), VecDeque::new());
    let mut seq = 0;
    loop {
        if stop.load(Ordering::Relaxed) || unsafe { libc::getppid() } != parent {
            break;
        }
        let mut fds = [
            libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: ui.master.as_raw_fd(),
                events: libc::POLLIN | if input.is_empty() { 0 } else { libc::POLLOUT },
                revents: 0,
            },
            libc::pollfd {
                fd: 1,
                events: if output.is_empty() { 0 } else { libc::POLLOUT },
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 3, 30) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::last_os_error().into());
        }
        if fds[2].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            break;
        }
        let mut bytes = [0u8; 16384];
        if fds[0].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(0, bytes.as_mut_ptr().cast(), bytes.len()) };
            if n <= 0 {
                break;
            }
            incoming.extend_from_slice(&bytes[..n as usize]);
            while let Some(end) = incoming.iter().position(|b| *b == b'\n') {
                ensure!(
                    end <= protocol::MAX_FRAME,
                    "terminal control command exceeds limit"
                );
                let line: Vec<_> = incoming.drain(..=end).collect();
                let line = std::str::from_utf8(&line)?;
                if line.trim().is_empty() {
                    continue;
                }
                let command =
                    terminal_control_command_from_json(line).map_err(anyhow::Error::msg)?;
                match command {
                    ClientMessage::Detach => {
                        record(&mut output, Record::Closed { reason: "released" })?;
                        flush(1, &mut output)?;
                        return Ok(0);
                    }
                    ClientMessage::Input { data } => enqueue(&mut input, &data)?,
                    ClientMessage::Resize { cols, rows, .. } => {
                        size = dimensions(cols, rows)?;
                        ensure!(
                            unsafe { libc::ioctl(ui.master.as_raw_fd(), libc::TIOCSWINSZ, &size) }
                                == 0,
                            "resize control PTY"
                        );
                    }
                    ClientMessage::AttachScroll {
                        source,
                        direction,
                        lines,
                        column,
                        row,
                        modifiers,
                    } => {
                        let (column, row) = if matches!(source, AttachScrollSource::Wheel)
                            && (column.is_none() || row.is_none())
                        {
                            // Moshi's touch gestures omit a position. In a full
                            // editor, (0,0) may be a tab bar or another split.
                            let position = crate::nvim::read_lua(
                                &instance.socket,
                                "local w=vim.api.nvim_get_current_win(); local p=vim.api.nvim_win_get_position(w); return {p[2]+math.floor(vim.api.nvim_win_get_width(w)/2),p[1]+math.floor(vim.api.nvim_win_get_height(w)/2)}",
                                vec![],
                            )?;
                            let position =
                                position.as_array().context("invalid scroll position")?;
                            let coordinate = |i: usize| -> Result<u16> {
                                Ok(u16::try_from(
                                    position
                                        .get(i)
                                        .and_then(|v| v.as_u64())
                                        .context("invalid scroll coordinate")?,
                                )?)
                            };
                            (
                                Some(column.unwrap_or(coordinate(0)?)),
                                Some(row.unwrap_or(coordinate(1)?)),
                            )
                        } else {
                            (column, row)
                        };
                        enqueue(
                            &mut input,
                            &scroll(source, direction, lines, column, row, modifiers),
                        )?;
                    }
                    _ => anyhow::bail!("unsupported control command"),
                }
            }
            ensure!(
                incoming.len() <= protocol::MAX_FRAME,
                "terminal control command exceeds limit"
            );
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            break;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let n = unsafe {
                libc::read(
                    ui.master.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if n > 0 {
                seq += 1;
                record(
                    &mut output,
                    Record::Frame {
                        seq,
                        encoding: "ansi",
                        width: size.ws_col,
                        height: size.ws_row,
                        full: false,
                        bytes: STANDARD.encode(&bytes[..n as usize]),
                    },
                )?;
            } else if n == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EIO) {
                break;
            }
        }
        if fds[1].revents & libc::POLLOUT != 0 {
            flush(ui.master.as_raw_fd(), &mut input)?;
        }
        if fds[2].revents & libc::POLLOUT != 0 {
            flush(1, &mut output)?;
        }
        if ui.child.try_wait()?.is_some()
            || fds[1].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
        {
            break;
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upstream_input_contract_rejects_ambiguous_or_corrupt_bytes() {
        assert!(
            terminal_control_command_from_json(
                r#"{"type":"terminal.input","text":"x","bytes":"eA=="}"#
            )
            .is_err()
        );
        assert!(
            terminal_control_command_from_json(r#"{"type":"terminal.input","bytes":"!"}"#).is_err()
        );
        assert!(
            terminal_control_command_from_json(r#"{"type":"terminal.resize","cols":0,"rows":24}"#)
                .is_err()
        );
        assert!(
            terminal_control_command_from_json(
                r#"{"type":"terminal.scroll","direction":"up","lines":0}"#
            )
            .is_err()
        );
    }
    #[test]
    fn wheel_positions_modifiers_and_page_keys_preserve_terminal_semantics() {
        assert_eq!(
            scroll(
                AttachScrollSource::Wheel,
                AttachScrollDirection::Down,
                2,
                Some(4),
                Some(8),
                7
            ),
            b"\x1b[<93;5;9M\x1b[<93;5;9M"
        );
        assert_eq!(
            scroll(
                AttachScrollSource::PageKey {
                    input: b"\x1b[5~".to_vec()
                },
                AttachScrollDirection::Up,
                8,
                None,
                None,
                0
            ),
            b"\x1b[5~"
        );
    }
}

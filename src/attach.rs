//! A PTY in front of nvim --remote-ui lets us consume remote-only Herdr chords.
use crate::contracts::api::ResponseResult;
use crate::{
    backend,
    gestures::{Decoder, Event},
    nvim::Instance,
    selection, session,
};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Terminal(libc::termios);
impl Terminal {
    fn raw() -> Result<Self> {
        let mut old = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::tcgetattr(0, &mut old) } == 0,
            "attachment needs a terminal"
        );
        let mut raw = old;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        ensure!(
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } == 0,
            "set raw terminal mode"
        );
        Ok(Self(old))
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

// A full PTY is backpressure, not permission to stop servicing the other
// direction. Blocking write_all used to deadlock against nvim's output flush
// and hide SIGHUP/SIGTERM indefinitely (the signal handler uses SA_RESTART).
struct Connection {
    stopped: Arc<AtomicBool>,
    parent: libc::pid_t,
    stdout_flags: libc::c_int,
}
impl Connection {
    fn new(stopped: Arc<AtomicBool>) -> Result<Self> {
        let stdout_flags = unsafe { libc::fcntl(1, libc::F_GETFL) };
        ensure!(stdout_flags >= 0, "read output flags");
        ensure!(
            unsafe { libc::fcntl(1, libc::F_SETFL, stdout_flags | libc::O_NONBLOCK) } == 0,
            "make attachment output nonblocking"
        );
        Ok(Self {
            stopped,
            parent: unsafe { libc::getppid() },
            stdout_flags,
        })
    }

    fn closed(&self) -> bool {
        let mut fd = libc::pollfd {
            fd: 0,
            events: 0,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut fd, 1, 0);
        }
        self.stopped.load(Ordering::Relaxed)
            || unsafe { libc::getppid() } != self.parent
            || fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    }

    fn write(&self, fd: RawFd, mut bytes: &[u8], drain: Option<RawFd>) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.closed() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "attachment closed",
                ));
            }
            let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if written > 0 {
                bytes = &bytes[written as usize..];
                continue;
            }
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {}
                _ => return Err(error),
            }
            let mut fds = [
                libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                },
                libc::pollfd {
                    fd: 0,
                    events: 0,
                    revents: 0,
                },
                libc::pollfd {
                    fd: drain.unwrap_or(-1),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 30) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if fds
                .iter()
                .any(|f| f.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0)
            {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            if fds[2].revents & libc::POLLIN != 0 {
                let mut output = [0u8; 16384];
                let n = unsafe { libc::read(fds[2].fd, output.as_mut_ptr().cast(), output.len()) };
                if n > 0 {
                    self.write(1, &output[..n as usize], None)?;
                } else if n == 0 {
                    return Err(io::ErrorKind::BrokenPipe.into());
                } else {
                    let error = io::Error::last_os_error();
                    if !matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            libc::fcntl(1, libc::F_SETFL, self.stdout_flags);
        }
    }
}

struct Ui {
    child: Child,
    master: File,
    instance: Instance,
    owner: String,
    session_config: Option<PathBuf>,
}
impl Ui {
    fn start(config: &Path, name: &str, follow_selection: bool) -> Result<Self> {
        let instance = session::inspect_bridge(&session::socket_path(config, name)?)?;
        crate::nvim::prepare_attach(&instance)?;
        let (mut master, mut slave) = (-1, -1);
        let mut size = dimensions()?;
        ensure!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    // Darwin declares this pointer mutable; Linux declares it
                    // const. A raw pointer matches both FFI signatures.
                    &raw mut size,
                )
            } == 0,
            "allocate remote UI PTY: {}",
            io::Error::last_os_error()
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        ensure!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == 0,
            "make remote UI PTY nonblocking"
        );
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            ensure!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
                "set PTY close-on-exec"
            );
        }
        let mut command = Command::new("nvim");
        command
            .arg("--server")
            .arg(&instance.socket)
            .arg("--remote-ui")
            .env("HERDR_ENV", "1")
            .env("HERDR_SESSION", name)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().context("start Neovim remote UI")?;
        let owner = format!("{}-{}", std::process::id(), child.id());
        Ok(Self {
            child,
            master,
            instance,
            owner,
            session_config: follow_selection.then(|| config.to_owned()),
        })
    }

    fn action(
        &mut self,
        connection: &Connection,
        action: &str,
        index: Option<u8>,
    ) -> Result<Option<String>> {
        let workspaces = if action == "workspace_open" {
            self.session_config
                .as_ref()
                .map(|config| -> Result<_> {
                    let request = crate::api::request(
                        "gesture:workspaces",
                        crate::contracts::api::Method::WorkspaceList(
                            crate::contracts::api::EmptyParams {},
                        ),
                    );
                    let ResponseResult::WorkspaceList { workspaces } =
                        crate::router::execute(config, &request)?.result
                    else {
                        anyhow::bail!("invalid workspace list")
                    };
                    Ok(workspaces)
                })
                .transpose()?
        } else {
            None
        };
        let reply: backend::Marker = backend::internal(
            &self.instance,
            &backend::Internal::GesturePrepare(backend::GestureParams {
                action: action.into(),
                index,
                workspaces,
                owner: self.owner.clone(),
            }),
        )?;
        let marker = &reply.marker;
        connection.write(
            self.master.as_raw_fd(),
            marker.as_bytes(),
            Some(self.master.as_raw_fd()),
        )?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            ensure!(!connection.closed(), "attachment closed during gesture");
            // Drain output while waiting: the TUI must not block on a full PTY
            // before it can read the marker and acknowledge the gesture.
            let mut fd = libc::pollfd {
                fd: self.master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut fd, 1, 5);
            }
            if fd.revents & libc::POLLIN != 0 {
                let mut bytes = [0; 16384];
                let n = self.master.read(&mut bytes)?;
                ensure!(n > 0, "remote UI closed during gesture");
                connection.write(1, &bytes[..n], None)?;
            }
            let reply: backend::GestureResult = backend::internal(
                &self.instance,
                &backend::Internal::GestureResult(backend::GestureOwner {
                    owner: self.owner.clone(),
                }),
            )?;
            if reply.done {
                ensure!(
                    reply.ok,
                    "gesture failed: {}",
                    reply.message.as_deref().unwrap_or("unknown failure")
                );
                return Ok(reply.workspace_id);
            }
            ensure!(
                Instant::now() < deadline,
                "remote UI did not acknowledge gesture"
            );
        }
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        let _: Result<ResponseResult> = backend::internal(
            &self.instance,
            &backend::Internal::Gesture(backend::GestureParams {
                action: "cleanup".into(),
                owner: self.owner.clone(),
                index: None,
                workspaces: None,
            }),
        );
        if matches!(self.child.try_wait(), Ok(None)) {
            // Only this UI process exits. The editor and terminal jobs stay alive.
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
        // Restore terminal modes even when the child could not emit its TUI teardown.
        let reset = b"\x1b[?1049l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l";
        // Best effort only: a disconnected or full outer PTY must not block Drop.
        unsafe {
            libc::write(1, reset.as_ptr().cast(), reset.len());
        }
    }
}

fn dimensions() -> Result<libc::winsize> {
    let mut size = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut size) } == 0,
        "read terminal dimensions"
    );
    Ok(size)
}

fn remaining_editor(config: &Path, current: &str) -> Result<Option<String>> {
    // A UI can exit because the editor closed or because the user detached.
    // Reattach only when that editor is gone, never undo an ordinary detach.
    if session::inspect_bridge(&session::socket_path(config, current)?).is_ok() {
        return Ok(None);
    }
    Ok(selection::current(config)?.or_else(|| {
        session::bridges(config)
            .ok()?
            .first()
            .map(|s| s.name.clone())
    }))
}

pub fn run(config: &Path, name: String, follow_selection: bool) -> Result<i32> {
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = stopped.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let _terminal = Terminal::raw()?;
    let connection = Connection::new(stopped)?;
    let result = run_connected(config, name, follow_selection, &connection);
    if connection.closed() { Ok(0) } else { result }
}

fn run_connected(
    config: &Path,
    mut name: String,
    follow_selection: bool,
    connection: &Connection,
) -> Result<i32> {
    loop {
        let mut ui = Ui::start(config, &name, follow_selection)?;
        let mut decoder = Decoder::default();
        let mut next_check = Instant::now();
        let mut size = dimensions()?;
        loop {
            if connection.closed() {
                return Ok(0);
            }
            if let Some(status) = ui.child.try_wait()? {
                if follow_selection && let Some(next) = remaining_editor(config, &name)? {
                    name = next;
                    break;
                }
                return Ok(status.code().unwrap_or(1));
            }
            if Instant::now() >= next_check {
                next_check = Instant::now() + Duration::from_millis(200);
                let new_size = dimensions()?;
                if (size.ws_row, size.ws_col) != (new_size.ws_row, new_size.ws_col) {
                    ensure!(
                        unsafe { libc::ioctl(ui.master.as_raw_fd(), libc::TIOCSWINSZ, &new_size) }
                            == 0,
                        "resize remote UI"
                    );
                    size = new_size;
                }
                if follow_selection
                    && let Some(selected) = selection::current(config)?
                    && selected != name
                {
                    name = selected;
                    break;
                }
            }
            let mut fds = [
                libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: ui.master.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, 30) } < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            let mut buffer = [0u8; 16384];
            if fds[1].revents & libc::POLLIN != 0 {
                match ui.master.read(&mut buffer) {
                    Ok(0) => {
                        if follow_selection && let Some(next) = remaining_editor(config, &name)? {
                            name = next;
                            break;
                        }
                        return Ok(0);
                    }
                    Ok(n) => {
                        connection.write(1, &buffer[..n], None)?;
                    }
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                        if follow_selection && let Some(next) = remaining_editor(config, &name)? {
                            name = next;
                            break;
                        }
                        return Ok(0);
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            let events = if fds[0].revents & libc::POLLIN != 0 {
                let n = match io::stdin().read(&mut buffer) {
                    Ok(n) => n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                if n == 0 {
                    return Ok(0);
                }
                decoder.feed(&buffer[..n])
            } else {
                decoder.expire()
            };
            for event in events {
                let result = match event {
                    Event::Bytes(bytes) => {
                        connection.write(
                            ui.master.as_raw_fd(),
                            &bytes,
                            Some(ui.master.as_raw_fd()),
                        )?;
                        continue;
                    }
                    Event::Action("detach") => {
                        ui.action(connection, "barrier", None)?;
                        return Ok(0);
                    }
                    Event::Action("ignore") => continue,
                    Event::Action(action) => ui.action(connection, action, None),
                    Event::Tab(index) => ui.action(connection, "tab", Some(index)),
                };
                // If ordering cannot be established, detach rather than send
                // subsequent text to a possibly different terminal.
                if let Some(workspace) = result.context("remote gesture")?
                    && follow_selection
                {
                    crate::router::execute(
                        config,
                        &crate::api::request(
                            "gesture:focus",
                            crate::contracts::api::Method::WorkspaceFocus(
                                crate::contracts::api::WorkspaceTarget {
                                    workspace_id: workspace.clone(),
                                },
                            ),
                        ),
                    )?;
                    let selected = workspace.split_once(':').context("invalid workspace ID")?.0;
                    if selected != name {
                        name = selected.to_owned();
                        drop(ui);
                        ui = Ui::start(config, &name, follow_selection)?;
                    }
                }
            }
            let closed = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
            if fds[0].revents & closed != 0 {
                return Ok(0);
            }
            if fds[1].revents & closed != 0 {
                // Linux can report PTY HUP without POLLIN or a reaped child.
                if follow_selection && let Some(next) = remaining_editor(config, &name)? {
                    name = next;
                    break;
                }
                return Ok(0);
            }
        }
    }
}

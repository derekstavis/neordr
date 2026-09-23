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
    io::{self, Read, Write},
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

struct Ui {
    child: Child,
    master: File,
    instance: Instance,
    owner: String,
}
impl Ui {
    fn start(config: &Path, name: &str) -> Result<Self> {
        let instance = session::inspect_bridge(&session::socket_path(config, name)?)?;
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
        })
    }

    fn action(&mut self, action: &str, index: Option<u8>) -> Result<()> {
        let reply: backend::Marker = backend::internal(
            &self.instance,
            &backend::Internal::GesturePrepare(backend::GestureParams {
                action: action.into(),
                index,
                owner: self.owner.clone(),
            }),
        )?;
        let marker = &reply.marker;
        self.master.write_all(marker.as_bytes())?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
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
                io::stdout().write_all(&bytes[..n])?;
                io::stdout().flush()?;
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
                return Ok(());
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
        let _ = io::stdout().write_all(
            b"\x1b[?1049l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l",
        );
        let _ = io::stdout().flush();
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

pub fn run(config: &Path, mut name: String, follow_selection: bool) -> Result<i32> {
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = stopped.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let _terminal = Terminal::raw()?;
    loop {
        let mut ui = Ui::start(config, &name)?;
        let mut decoder = Decoder::default();
        let mut next_check = Instant::now();
        let mut size = dimensions()?;
        loop {
            if stopped.load(Ordering::Relaxed) {
                return Ok(0);
            }
            if let Some(status) = ui.child.try_wait()? {
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
                    Ok(0) => return Ok(0),
                    Ok(n) => {
                        io::stdout().write_all(&buffer[..n])?;
                        io::stdout().flush()?;
                    }
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(0),
                    Err(e) => return Err(e.into()),
                }
            }
            let events = if fds[0].revents & libc::POLLIN != 0 {
                let n = io::stdin().read(&mut buffer)?;
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
                        ui.master.write_all(&bytes)?;
                        continue;
                    }
                    Event::Action("detach") => {
                        ui.action("barrier", None)?;
                        return Ok(0);
                    }
                    Event::Action("ignore") => continue,
                    Event::Action(action) => ui.action(action, None),
                    Event::Tab(index) => ui.action("tab", Some(index)),
                };
                // If ordering cannot be established, detach rather than send
                // subsequent text to a possibly different terminal.
                result.context("remote gesture")?;
            }
            if fds
                .iter()
                .any(|fd| fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0)
            {
                return Ok(0);
            }
        }
    }
}

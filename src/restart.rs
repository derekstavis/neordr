//! Restart Neordr processes without ending the editors they serve.
use std::{
    io::{BufReader, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use crate::{
    api,
    contracts::api::{Method, PingParams},
    nvim::Instance,
    protocol, router, service, session,
};
use anyhow::{Context, Result, bail, ensure};

// Never signal a PID merely because it was supplied by a socket response. Ask
// the kernel which process owns the connected endpoint, and require agreement.
#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    };
    ensure!(
        result == 0,
        "read socket peer PID: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        pid > 0 && len as usize == std::mem::size_of_val(&pid),
        "invalid socket peer PID"
    );
    Ok(pid as u32)
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    ensure!(
        result == 0,
        "read socket peer credentials: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        cred.pid > 0 && len as usize == std::mem::size_of_val(&cred),
        "invalid socket peer PID"
    );
    Ok(cred.pid as u32)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_pid(_stream: &UnixStream) -> Result<u32> {
    bail!("restart requires socket peer PID support")
}

fn ping(path: &Path) -> Result<(u32, api::Reply)> {
    let mut stream =
        UnixStream::connect(path).with_context(|| format!("connect {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let peer = peer_pid(&stream)?;
    let request = api::request("neordr:restart", Method::Ping(PingParams {}));
    serde_json::to_writer(&mut stream, &request)?;
    stream.write_all(b"\n")?;
    let response: api::Response =
        serde_json::from_slice(&protocol::read_frame(&mut BufReader::new(stream))?)?;
    ensure!(
        response.id == request.id,
        "restart ping response ID mismatch"
    );
    let reply = response.into_result()?;
    ensure!(
        reply.identity.pid == Some(peer),
        "restart ping PID does not match socket peer"
    );
    ensure!(
        peer != std::process::id(),
        "refusing to signal the restart process"
    );
    Ok((peer, reply))
}

fn terminate(pid: u32) -> Result<()> {
    let pid = i32::try_from(pid).context("invalid process ID")?;
    ensure!(
        unsafe { libc::kill(pid, libc::SIGTERM) } == 0,
        "signal process {pid}: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> Result<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let count = unsafe {
        libc::proc_pidpath(
            i32::try_from(pid)?,
            bytes.as_mut_ptr().cast(),
            bytes.len() as u32,
        )
    };
    ensure!(
        count > 0,
        "cannot inspect executable for process {pid}: {}",
        std::io::Error::last_os_error()
    );
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(count as usize);
    Ok(std::ffi::OsStr::from_bytes(&bytes[..end]).into())
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> Result<std::path::PathBuf> {
    Ok(std::fs::read_link(format!("/proc/{pid}/exe"))?)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_executable(_pid: u32) -> Result<std::path::PathBuf> {
    bail!("restart requires process executable inspection")
}

fn verify_executable(pid: u32) -> Result<()> {
    let expected = std::env::current_exe()?.canonicalize()?;
    let actual = process_executable(pid)?
        .canonicalize()
        .with_context(|| format!("resolve executable for process {pid}"))?;
    ensure!(
        actual == expected,
        "replacement process {pid} uses {}, not {}; check the service and editor executable paths",
        actual.display(),
        expected.display()
    );
    Ok(())
}

fn same_editor(a: &Instance, b: &Instance) -> bool {
    a.pid == b.pid && a.generation.is_some() && a.generation == b.generation && a.socket == b.socket
}

fn wait_router(config: &Path, previous: Option<u32>) -> Result<u32> {
    let path = session::socket_path(config, "default")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match ping(&path) {
            Ok((pid, reply)) => {
                ensure!(
                    reply.identity.neordr_router == Some(true),
                    "default socket is not a Neordr router"
                );
                if Some(pid) != previous {
                    return Ok(pid);
                }
            }
            Err(_) => {
                let _ = router::start_background(config);
            }
        }
        ensure!(
            Instant::now() < deadline,
            "replacement router did not become ready"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

/// Restart the shared router and each currently reachable, verified bridge.
pub fn run(config: &Path) -> Result<()> {
    let mut bridges = Vec::new();
    for (session, editor) in session::live_bridges(config)? {
        let (pid, reply) = ping(&session.socket_path)?;
        let actual = reply
            .identity
            .neordr
            .context("endpoint is not a Neordr editor bridge")?;
        ensure!(
            same_editor(&editor, &actual),
            "editor identity changed before restart: {}",
            session.name
        );
        bridges.push((session, editor, pid));
    }
    let router_path = session::socket_path(config, "default")?;
    let old_router = match ping(&router_path) {
        Ok((pid, reply)) => {
            ensure!(
                reply.identity.neordr_router == Some(true),
                "default socket is not a Neordr router"
            );
            Some(pid)
        }
        Err(error) => {
            // A reachable but malformed/foreign endpoint is never ours to replace.
            if UnixStream::connect(&router_path).is_ok() {
                return Err(error);
            }
            None
        }
    };
    if let Some(pid) = old_router {
        if service::installed_for(config) && service::manager_pid() == Some(pid) {
            service::restart(config)?;
        } else {
            terminate(pid).context("stop existing router")?;
        }
    }
    let new_router = wait_router(config, old_router)?;
    verify_executable(new_router).context("verify replacement router")?;
    println!("Router restarted (pid {new_router}).");
    let mut failures = Vec::new();
    for (session, editor, old_pid) in bridges {
        let result = (|| -> Result<()> {
            match ping(&session.socket_path) {
                Ok((pid, reply)) => {
                    let actual = reply
                        .identity
                        .neordr
                        .context("endpoint is not a Neordr bridge")?;
                    ensure!(same_editor(&editor, &actual), "editor identity changed");
                    if pid == old_pid {
                        terminate(pid)?;
                    } else {
                        verify_executable(pid)?;
                        return Ok(());
                    }
                }
                Err(error) => {
                    if UnixStream::connect(&session.socket_path).is_ok() {
                        return Err(error);
                    }
                }
            }
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if let Ok((pid, reply)) = ping(&session.socket_path)
                    && pid != old_pid
                    && let Some(actual) = reply.identity.neordr
                {
                    ensure!(
                        same_editor(&editor, &actual),
                        "editor identity changed during recovery"
                    );
                    verify_executable(pid)?;
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }
            bail!("replacement bridge did not become ready")
        })();
        match result {
            Ok(()) => println!("Editor bridge {} restarted.", session.name),
            Err(error) => failures.push(format!("{}: {error:#}", session.name)),
        }
    }
    ensure!(
        failures.is_empty(),
        "some editor bridges could not be restarted: {}",
        failures.join("; ")
    );
    println!("Neovim and terminal jobs remain running; reconnect remote clients if needed.");
    Ok(())
}

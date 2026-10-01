use std::{
    fs::{self, OpenOptions},
    io::{BufReader, Write},
    os::fd::AsRawFd,
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;

use crate::{
    backend, endpoint,
    nvim::{self, Instance},
    protocol::{self, Request},
};

struct SocketGuard(PathBuf);
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn serve(socket: &Path, nvim_socket: &Path) -> Result<()> {
    // The attach command may run from a different working directory.
    let nvim_socket = fs::canonicalize(nvim_socket).context("resolve Neovim socket path")?;
    let initial = nvim::register_generation(&nvim_socket)?;
    let parent = socket.parent().context("socket has no parent directory")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(parent.join(".neordr-bridge.lock"))?;
    ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "editor bridge is already running"
    );
    let marker = parent.join(".neordr-bridge-owned");
    // A lock and previous ownership are both required for crash recovery.
    // Never replace a foreign endpoint, a regular file, or an accepting socket.
    for endpoint in [socket.to_owned(), parent.join("herdr-client.sock")] {
        if let Ok(metadata) = endpoint.symlink_metadata() {
            ensure!(
                marker.exists()
                    && metadata.file_type().is_socket()
                    && UnixStream::connect(&endpoint).is_err(),
                "refusing to replace existing endpoint {}",
                endpoint.display()
            );
            fs::remove_file(endpoint)?;
        }
    }
    let listener = UnixListener::bind(socket)?;
    fs::write(marker, "Neordr editor bridge endpoints\n")?;
    let _guard = SocketGuard(socket.to_owned());
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    let session = parent
        .file_name()
        .and_then(|s| s.to_str())
        .context("invalid session name")?;
    backend::initialize(&initial, session)?;
    crate::editors::record(&crate::session::config_root()?, session, &initial)?;
    listener.set_nonblocking(true)?;
    let client_socket = parent.join("herdr-client.sock");
    let clients = UnixListener::bind(&client_socket).with_context(|| {
        format!(
            "bind {}; existing sockets are never replaced",
            client_socket.display()
        )
    })?;
    let _client_guard = SocketGuard(client_socket.clone());
    fs::set_permissions(&client_socket, fs::Permissions::from_mode(0o600))?;
    clients.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = stop.clone();
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Relaxed))?;
    let active = Arc::new(AtomicUsize::new(0));
    let active_clients = Arc::new(AtomicUsize::new(0));
    eprintln!(
        "neordr listening on {} → {}",
        socket.display(),
        nvim_socket.display()
    );
    while !stop.load(Ordering::Relaxed) {
        if !nvim_socket.exists() {
            break;
        }
        protocol::wait_for_client(&[listener.as_raw_fd(), clients.as_raw_fd()])?;
        match clients.accept() {
            Ok((stream, _)) if active_clients.load(Ordering::Relaxed) < 8 => {
                active_clients.fetch_add(1, Ordering::Relaxed);
                let active_clients = active_clients.clone();
                let initial = initial.clone();
                let stop = stop.clone();
                thread::spawn(move || {
                    if let Err(error) =
                        endpoint::handle(stream, endpoint::Target::Editor(initial), stop)
                    {
                        // Herdr probes the socket by connecting and immediately closing it.
                        if error.downcast_ref::<std::io::Error>().is_none() {
                            eprintln!("Herdr endpoint: {error:#}");
                        }
                    }
                    active_clients.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if active.load(Ordering::Relaxed) >= 16 {
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                let active = active.clone();
                let initial = initial.clone();
                thread::spawn(move || {
                    let _ = handle(stream, &initial);
                    active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
    // Explicit workspace close runs in a request worker. Let it acknowledge
    // editor exit before the main thread tears down the process and sockets.
    stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(3);
    while (active.load(Ordering::Relaxed) > 0 || active_clients.load(Ordering::Relaxed) > 0)
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn handle(mut stream: UnixStream, initial: &Instance) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let frame = protocol::read_frame(&mut BufReader::new(stream.try_clone()?));
    let response = match frame.and_then(|frame| Ok(serde_json::from_slice::<Request>(&frame)?)) {
        Ok(request) => dispatch(&request, initial),
        Err(e) => protocol::error("", "invalid_request", &e.to_string()),
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")?;
    Ok(())
}

pub fn dispatch(request: &Request, initial: &Instance) -> Value {
    let response = match crate::api::decode(request) {
        Ok(typed) => execute(&typed, initial),
        Err(error) => crate::api::Response::failure(
            &request.id,
            if crate::contracts::method_names().contains(&request.method.as_str()) {
                "invalid_params"
            } else {
                "invalid_request"
            },
            &error,
        ),
    };
    serde_json::to_value(response).expect("API response is serializable")
}

pub fn execute(request: &crate::api::Request, initial: &Instance) -> crate::api::Response {
    use crate::{
        api::{Command, Identity, Reply, Response},
        contracts::api::{Method, ResponseResult, ServerCapabilities},
    };
    let result = (|| -> Result<Reply> {
        if matches!(request.command, Command::Herdr(Method::Ping(_))) {
            let current = nvim::inspect(&initial.socket)?;
            ensure!(
                current.pid == initial.pid && current.generation == initial.generation,
                "Neovim instance changed; restart this bridge"
            );
            return Ok(Reply {
                result: ResponseResult::Pong {
                    version: concat!("neordr/", env!("CARGO_PKG_VERSION")).into(),
                    // Herdr also gates JSON API commands on this schema version.
                    // Interactive attach negotiates endpoint codecs separately.
                    protocol: crate::contracts::wire::PROTOCOL_VERSION,
                    capabilities: Some(ServerCapabilities {
                        live_handoff: false,
                        detached_server_daemon: false,
                        endpoint_protocol_generation: Some(1),
                        surface_interest: false,
                        health_check: false,
                        ssh_agent_registration: false,
                    }),
                },
                identity: Identity {
                    neordr: Some(current),
                    pid: Some(std::process::id()),
                    ..Identity::default()
                },
            });
        }
        if let Command::Herdr(Method::WorkspaceCreate(params)) = &request.command {
            return crate::editors::create(&crate::session::config_root()?, params, Some(initial));
        }
        if let Command::Herdr(Method::WorkspaceClose(params)) = &request.command {
            ensure!(!params.close_group, "Workspace groups are not supported");
            return crate::editors::close(initial, &params.workspace_id);
        }
        Ok(backend::dispatch(initial, &request.command)?.into())
    })();
    match result {
        Ok(reply) => Response::success(&request.id, reply),
        Err(error) => Response::failure(&request.id, "request_failed", &error),
    }
}

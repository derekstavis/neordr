//! Compatibility with moshi-hook 0.3.21's session-less TUI input requests.
//! IDs start with a validated session name, so an input request has exactly one
//! possible destination. Never search for a similarly named pane in another UI.
use crate::{
    api::{self, Command as ApiCommand},
    contracts::api::{Method, PaneTarget, PingParams, ResponseResult},
    protocol::{self, Request},
    session,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::UnixListener,
            process::CommandExt,
        },
    },
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Legacy transport boundary. All routing and response handling below is typed.
pub fn dispatch(config: &Path, request: &Request) -> Value {
    let response = match api::decode(request) {
        Ok(typed) => match execute(config, &typed) {
            Ok(result) => api::Response::success(&request.id, result),
            Err(error) => api::Response::failure(&request.id, "routing_failed", &error),
        },
        Err(error) => api::Response::failure(
            &request.id,
            if crate::contracts::method_names().contains(&request.method.as_str()) {
                "invalid_params"
            } else {
                "invalid_request"
            },
            &error,
        ),
    };
    serde_json::to_value(response).expect("typed API response is serializable")
}

pub fn execute(config: &Path, request: &api::Request) -> Result<api::Reply> {
    if let ApiCommand::Herdr(Method::Ping(_)) = &request.command {
        return Ok(api::Reply {
            result: ResponseResult::Pong {
                version: concat!("neordr/", env!("CARGO_PKG_VERSION")).into(),
                protocol: crate::contracts::wire::PROTOCOL_VERSION,
                capabilities: None,
            },
            identity: api::Identity {
                neordr_router: Some(true),
                pid: Some(std::process::id()),
                ..Default::default()
            },
        });
    }
    if let ApiCommand::Herdr(Method::WorkspaceMove(params)) = &request.command {
        return crate::ordering::move_workspace(config, &request.id, params);
    }
    if let Some(target) = request.command.target() {
        let (name, _) = target
            .split_once(':')
            .context("target must include its Neordr session prefix")?;
        let socket = session::socket_path(config, name)?;
        session::inspect_bridge(&socket)?;
        if matches!(
            &request.command,
            ApiCommand::Herdr(Method::PaneSendText(_) | Method::PaneSendKeys(_))
        ) {
            let reply = protocol::call_typed(
                &socket,
                &api::request(
                    request.id.clone(),
                    Method::PaneGet(PaneTarget {
                        pane_id: target.into(),
                    }),
                ),
            )?;
            let ResponseResult::PaneInfo { pane } = reply.result else {
                anyhow::bail!("invalid pane response");
            };
            if pane.agent.is_none() {
                let (command, enter) = match &request.command {
                    ApiCommand::Herdr(Method::PaneSendText(params)) => {
                        (Some(params.text.clone()), false)
                    }
                    ApiCommand::Herdr(Method::PaneSendKeys(params)) => (
                        None,
                        params.keys.len() == 1 && params.keys[0].eq_ignore_ascii_case("enter"),
                    ),
                    _ => unreachable!(),
                };
                ensure!(
                    matches!(command.as_deref(), Some("claude" | "codex")) || enter,
                    "refusing Chat View input: this pane no longer has a supported foreground agent"
                );
                return protocol::call_typed(
                    &socket,
                    &api::Request {
                        id: request.id.clone(),
                        command: ApiCommand::Launch(api::LaunchParams {
                            pane_id: target.into(),
                            command,
                            enter,
                        }),
                    },
                );
            }
        }
        let reply = protocol::call_typed(&socket, request)?;
        if request.command.remembers_selection() {
            crate::selection::remember(config, name)?;
        }
        return Ok(reply);
    }
    if matches!(
        &request.command,
        ApiCommand::Herdr(Method::WorkspaceCreate(_) | Method::TabCreate(_))
    ) {
        let name = if let Some(name) = crate::selection::current(config)? {
            name
        } else {
            let sessions = session::list(config)?;
            ensure!(
                sessions.len() == 1,
                "select a Neovim workspace in Moshi before creating a terminal"
            );
            sessions[0].name.clone()
        };
        let reply = protocol::call_typed(&session::socket_path(config, &name)?, request)?;
        if request.command.remembers_selection() {
            crate::selection::remember(config, &name)?;
        }
        return Ok(reply);
    }
    if matches!(
        &request.command,
        ApiCommand::Herdr(Method::SessionSnapshot(_))
    ) {
        let sessions = session::list(config)?;
        let selected = crate::selection::current(config)?;
        let mut snapshot = crate::backend::empty_session_snapshot();
        for item in &sessions {
            let reply = protocol::call_typed(&item.socket_path, request)?;
            let ResponseResult::SessionSnapshot { snapshot: source } = reply.result else {
                anyhow::bail!("invalid session snapshot");
            };
            snapshot.workspaces.extend(source.workspaces);
            snapshot.tabs.extend(source.tabs);
            snapshot.panes.extend(source.panes);
            snapshot.layouts.extend(source.layouts);
            snapshot.agents.extend(source.agents);
            if selected.as_deref() == Some(&item.name) || sessions.len() == 1 {
                snapshot.focused_workspace_id = source.focused_workspace_id;
                snapshot.focused_tab_id = source.focused_tab_id;
                snapshot.focused_pane_id = source.focused_pane_id;
            }
        }
        crate::ordering::sort(config, &mut snapshot.workspaces)?;
        return Ok(ResponseResult::SessionSnapshot {
            snapshot: Box::new(snapshot),
        }
        .into());
    }
    let mut aggregate = match &request.command {
        ApiCommand::Herdr(Method::WorkspaceList(_)) => {
            ResponseResult::WorkspaceList { workspaces: vec![] }
        }
        ApiCommand::Herdr(Method::TabList(_)) => ResponseResult::TabList { tabs: vec![] },
        ApiCommand::Herdr(Method::PaneList(_)) => ResponseResult::PaneList { panes: vec![] },
        ApiCommand::Herdr(Method::AgentList(_)) => ResponseResult::AgentList { agents: vec![] },
        _ => {
            return Err(protocol::RemoteError(crate::contracts::api::ErrorBody {
                code: "invalid_request".into(),
                message: "method needs an explicit target or is unsupported".into(),
            })
            .into());
        }
    };
    for item in session::list(config)? {
        let reply = protocol::call_typed(&item.socket_path, request)?;
        match (&mut aggregate, reply.result) {
            (
                ResponseResult::WorkspaceList { workspaces },
                ResponseResult::WorkspaceList { workspaces: rows },
            ) => workspaces.extend(rows),
            (ResponseResult::TabList { tabs }, ResponseResult::TabList { tabs: rows }) => {
                tabs.extend(rows)
            }
            (ResponseResult::PaneList { panes }, ResponseResult::PaneList { panes: rows }) => {
                panes.extend(rows)
            }
            (ResponseResult::AgentList { agents }, ResponseResult::AgentList { agents: rows }) => {
                agents.extend(rows)
            }
            _ => anyhow::bail!("invalid list response"),
        }
    }
    if let ResponseResult::WorkspaceList { workspaces } = &mut aggregate {
        crate::ordering::sort(config, workspaces)?;
    }
    Ok(aggregate.into())
}

fn router_ready(config: &Path) -> Result<()> {
    let reply = protocol::call_typed(
        &config.join("herdr/herdr.sock"),
        &api::request("neordr:router-health", Method::Ping(PingParams {})),
    )?;
    ensure!(
        reply.identity.neordr_router == Some(true),
        "default socket belongs to another Herdr server"
    );
    Ok(())
}

/// A Neovim job is not a daemon lifetime boundary. Detach the router's session
/// and standard streams so closing the editor cannot tear down Chat View input.
pub fn start_background(config: &Path) -> Result<()> {
    if router_ready(config).is_ok() {
        return Ok(());
    }
    ensure!(
        std::os::unix::net::UnixStream::connect(config.join("herdr/herdr.sock")).is_err(),
        "default Herdr socket is accepting connections but is not a healthy Neordr router"
    );
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("router")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Only the async-signal-safe setsid syscall runs between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("start independent input router")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if router_ready(config).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "input router exited: {status}");
            // A concurrent launcher may have won the lock but not bound yet.
        }
        ensure!(
            Instant::now() < deadline,
            "input router did not become ready"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn serve(config: &Path) -> Result<()> {
    let root = config.join("herdr");
    fs::create_dir_all(&root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".neordr-router.lock"))?;
    // The open file retains the advisory lock until this process exits.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(());
    }
    let path = root.join("herdr.sock");
    let marker = root.join(".neordr-router-owned");
    if path.symlink_metadata().is_ok() {
        if let Ok(result) = protocol::call_typed(
            &path,
            &api::request("neordr:router", Method::Ping(PingParams {})),
        ) {
            ensure!(
                result.identity.neordr_router == Some(true),
                "the default socket belongs to another Herdr server"
            );
            return Ok(());
        }
        ensure!(
            marker.exists() && fs::symlink_metadata(&path)?.file_type().is_socket(),
            "refusing to replace an existing default Herdr endpoint: {}",
            path.display()
        );
        ensure!(
            std::os::unix::net::UnixStream::connect(&path).is_err(),
            "default socket is still accepting connections"
        );
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    fs::write(
        marker,
        "Neordr owns the default API socket; the named session servers own Neovim instances.\n",
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    listener.set_nonblocking(true)?;
    let active = Arc::new(AtomicUsize::new(0));
    eprintln!("neordr router listening on {}", path.display());
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // A partial request or a paused editor must not block router
                // health checks and every other workspace behind it.
                if active.load(Ordering::Relaxed) >= 32 {
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                let active = active.clone();
                let config = config.to_owned();
                thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let frame = protocol::read_frame(&mut BufReader::new(&mut stream));
                    let response =
                        match frame.and_then(|f| Ok(serde_json::from_slice::<Request>(&f)?)) {
                            Ok(request) => dispatch(&config, &request),
                            Err(e) => protocol::error("", "invalid_request", &e.to_string()),
                        };
                    let _ = serde_json::to_writer(&mut stream, &response)
                        .and_then(|()| stream.write_all(b"\n").map_err(serde_json::Error::io));
                    active.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(e) => return Err(e.into()),
        }
    }
    fs::remove_file(path)?;
    Ok(())
}

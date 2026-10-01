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
                capabilities: Some(crate::contracts::api::ServerCapabilities {
                    endpoint_protocol_generation: Some(1),
                    live_handoff: false,
                    detached_server_daemon: false,
                    surface_interest: false,
                    health_check: false,
                    ssh_agent_registration: false,
                }),
            },
            identity: api::Identity {
                neordr_router: Some(true),
                pid: Some(std::process::id()),
                ..Default::default()
            },
        });
    }
    if let ApiCommand::Herdr(Method::WorkspaceCreate(params)) = &request.command {
        let source_key = match params.source_workspace_id.as_deref() {
            Some(id) => Some(
                id.split_once(':')
                    .context("invalid source workspace ID")?
                    .0
                    .to_owned(),
            ),
            None => crate::selection::current(config)?,
        };
        let source = source_key
            .as_deref()
            .map(|key| session::inspect_bridge(&session::socket_path(config, key)?))
            .transpose()?;
        if let (Some(id), Some(instance)) = (&params.source_workspace_id, &source) {
            ensure!(
                crate::backend::snapshot(instance)?
                    .workspaces
                    .iter()
                    .any(|w| w.workspace_id == *id),
                "unknown source workspace"
            );
        }
        return crate::editors::create(config, params, source.as_ref());
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
    if matches!(&request.command, ApiCommand::Herdr(Method::TabCreate(_))) {
        let name = if let Some(name) = crate::selection::current(config)? {
            name
        } else {
            let sessions = session::bridges(config)?;
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
        let sessions = session::live_bridges(config)?;
        let selected = crate::selection::current(config)?
            .or_else(|| sessions.first().map(|(s, _)| s.name.clone()));
        let mut snapshot = crate::backend::empty_session_snapshot();
        // Read through the same generation-checked Lua boundary as each bridge,
        // then enumerate the host's processes once for the whole collection.
        let replies = crate::parallel::map(&sessions, |(_, instance)| {
            crate::backend::snapshot(instance)
        });
        let rows = if sessions.is_empty() {
            vec![]
        } else {
            crate::process::snapshot()?
        };
        for ((item, _), reply) in sessions.iter().zip(replies) {
            let mut source = match reply {
                Ok(reply) => reply,
                Err(_) if session::inspect_bridge(&item.socket_path).is_err() => continue,
                Err(error) => return Err(error),
            };
            crate::backend::enrich_agents(&mut source, &rows);
            let mut source = crate::backend::project_session(source);
            let focused = selected.as_deref() == Some(&item.name);
            for ws in &mut source.workspaces {
                ws.focused &= focused;
            }
            for tab in &mut source.tabs {
                tab.focused &= focused;
            }
            for pane in &mut source.panes {
                pane.focused &= focused;
            }
            for agent in &mut source.agents {
                agent.focused &= focused;
            }
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
    let snapshot_request = api::request(
        &request.id,
        Method::SessionSnapshot(crate::contracts::api::EmptyParams {}),
    );
    let result = match &request.command {
        ApiCommand::Herdr(
            Method::WorkspaceList(_)
            | Method::TabList(_)
            | Method::PaneList(_)
            | Method::AgentList(_),
        ) => {
            let ResponseResult::SessionSnapshot { snapshot } =
                execute(config, &snapshot_request)?.result
            else {
                unreachable!()
            };
            match &request.command {
                ApiCommand::Herdr(Method::WorkspaceList(_)) => ResponseResult::WorkspaceList {
                    workspaces: snapshot.workspaces,
                },
                ApiCommand::Herdr(Method::TabList(_)) => ResponseResult::TabList {
                    tabs: snapshot.tabs,
                },
                ApiCommand::Herdr(Method::PaneList(_)) => ResponseResult::PaneList {
                    panes: snapshot.panes,
                },
                _ => ResponseResult::AgentList {
                    agents: snapshot.agents,
                },
            }
        }
        _ => anyhow::bail!("method needs an explicit target or is unsupported"),
    };
    Ok(result.into())
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
    // A refused/dropped health request is not proof of a foreign server. Under
    // load the bounded request pool can reject a probe, and another launcher
    // can still be between bind and accept. Retry without replacing the socket.
    let retry_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < retry_deadline {
        if std::os::unix::net::UnixStream::connect(config.join("herdr/herdr.sock")).is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
        if router_ready(config).is_ok() {
            return Ok(());
        }
    }
    ensure!(
        std::os::unix::net::UnixStream::connect(config.join("herdr/herdr.sock")).is_err(),
        "default Herdr socket is accepting connections but is not a healthy Neordr router"
    );
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("router")
        .env("XDG_CONFIG_HOME", config)
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
            thread::spawn(move || {
                let _ = child.wait();
            });
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
    let previously_owned = marker.exists();
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
        &marker,
        "Neordr owns the default API socket; the named session servers own Neovim instances.\n",
    )?;
    let client_path = root.join("herdr-client.sock");
    if client_path.exists() {
        ensure!(
            previously_owned
                && fs::symlink_metadata(&client_path)?.file_type().is_socket()
                && std::os::unix::net::UnixStream::connect(&client_path).is_err(),
            "client socket already in use"
        );
        fs::remove_file(&client_path)?;
    }
    let clients = UnixListener::bind(&client_path)?;
    fs::set_permissions(&client_path, fs::Permissions::from_mode(0o600))?;
    clients.set_nonblocking(true)?;
    let active_clients = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    listener.set_nonblocking(true)?;
    let active = Arc::new(AtomicUsize::new(0));
    let discover_config = config.to_owned();
    let discover_stop = stop.clone();
    thread::spawn(move || {
        while !discover_stop.load(Ordering::Relaxed) {
            let _ = crate::editors::discover(&discover_config);
            for _ in 0..50 {
                if discover_stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
    eprintln!("neordr session listening on {}", path.display());
    while !stop.load(Ordering::Relaxed) {
        protocol::wait_for_client(&[listener.as_raw_fd(), clients.as_raw_fd()])?;
        match clients.accept() {
            Ok((stream, _)) if active_clients.load(Ordering::Relaxed) < 8 => {
                active_clients.fetch_add(1, Ordering::Relaxed);
                let count = active_clients.clone();
                let stop = stop.clone();
                let config = config.to_owned();
                thread::spawn(move || {
                    if let Err(error) = crate::endpoint::handle(
                        stream,
                        crate::endpoint::Target::Session(config),
                        stop,
                    ) {
                        eprintln!("Herdr session endpoint: {error:#}");
                    }
                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
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
                    // Darwin inherits O_NONBLOCK from the listener. Worker I/O
                    // uses deadlines, not EAGAIN as an end-of-request marker.
                    let _ = stream.set_nonblocking(false);
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
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
    fs::remove_file(path)?;
    fs::remove_file(client_path)?;
    Ok(())
}

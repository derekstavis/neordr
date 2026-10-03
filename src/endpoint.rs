//! Herdr's stable shell endpoint backed by one ordered Neovim remote UI.
mod grid;
mod input;
mod projection;
mod surface;
mod ui;
mod wire;
use crate::{
    api,
    contracts::{
        api::{EmptyParams, Method, ResponseResult},
        endpoint::*,
    },
    nvim::Instance,
    protocol, server,
};
use anyhow::{Context, Result, ensure};
use rmpv::Value as Msg;
use std::{
    collections::VecDeque,
    io::BufReader,
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use wire::{ClientMessage, ServerMessage};
const METHODS: &[&str] = &[
    "session.snapshot",
    "workspace.list",
    "workspace.focus",
    "workspace.create",
    "workspace.rename",
    "workspace.move",
    "workspace.close",
    "tab.list",
    "tab.focus",
    "tab.create",
    "tab.rename",
    "tab.move",
    "tab.close",
    "pane.list",
    "pane.focus",
    "pane.split",
    "pane.close",
    "pane.rename",
    "pane.layout",
    "agent.list",
    "agent.focus",
];

static PROJECTION_REVISION: AtomicU64 = AtomicU64::new(1);
struct Connection(UnixStream);
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}
enum Event {
    Client(Vec<u8>),
    Nvim(u64, Msg),
    UiClosed(u64),
    Closed(String),
}
fn dimensions(cols: u16, rows: u16) -> Result<(u16, u16)> {
    ensure!(
        cols > 0
            && rows > 0
            && cols <= 1000
            && rows <= 500
            && u32::from(cols) * u32::from(rows) <= 100_000,
        "unsupported endpoint dimensions"
    );
    Ok((cols, rows))
}
pub enum Target {
    Editor(Instance),
    Session(PathBuf),
}
impl Target {
    fn execute(&self, request: &api::Request) -> api::Response {
        match self {
            Self::Editor(instance) => server::execute(request, instance),
            Self::Session(config) => match crate::router::execute(config, request) {
                Ok(reply) => api::Response::success(&request.id, reply),
                Err(error) => api::Response::failure(&request.id, "request_failed", &error),
            },
        }
    }
    fn selected(&self) -> Result<Option<Instance>> {
        match self {
            Self::Editor(instance) => Ok(instance.socket.exists().then(|| instance.clone())),
            Self::Session(config) => {
                let key = crate::selection::current(config)?.or_else(|| {
                    crate::session::bridges(config)
                        .ok()?
                        .first()
                        .map(|s| s.name.clone())
                });
                Ok(key.and_then(|key| {
                    crate::session::socket_path(config, &key)
                        .ok()
                        .and_then(|path| crate::session::inspect_bridge(&path).ok())
                }))
            }
        }
    }
    fn snapshot(&self, boot: &str, revision: u64) -> Result<wire::ClientShellSnapshot> {
        let request = api::request("endpoint:snapshot", Method::SessionSnapshot(EmptyParams {}));
        let ResponseResult::SessionSnapshot { snapshot } =
            self.execute(&request).into_result()?.result
        else {
            anyhow::bail!("invalid session snapshot")
        };
        let label = match self {
            Self::Editor(i) => i.cwd.as_str(),
            Self::Session(_) => "Neordr",
        };
        Ok(projection::snapshot(&snapshot, boot, revision, label))
    }
}
struct ActiveUi {
    instance: Instance,
    connection: ui::UiSession,
    epoch: u64,
}
fn synchronize_ui(
    target: &Target,
    active: &mut Option<ActiveUi>,
    grid: &mut grid::Grid,
    size: (u16, u16),
    tx: &mpsc::SyncSender<Event>,
    epoch: &mut u64,
) -> Result<()> {
    let selected = target.selected()?;
    if active
        .as_ref()
        .map(|a| (&a.instance.socket, &a.instance.generation))
        == selected.as_ref().map(|i| (&i.socket, &i.generation))
    {
        return Ok(());
    }
    *active = None; // dropping only this channel detaches the previous UI
    *grid = grid::Grid::new();
    let Some(instance) = selected else {
        grid.clear(size.0, size.1);
        return Ok(());
    };
    crate::nvim::prepare_attach(&instance)?;
    let mut connection = ui::UiSession::connect(
        &instance.socket,
        instance.generation.clone().context("missing generation")?,
    )?;
    *epoch += 1;
    let current = *epoch;
    let mut reader = BufReader::new(connection.try_clone_reader()?);
    let tx = tx.clone();
    thread::spawn(move || {
        loop {
            match rmpv::decode::read_value(&mut reader) {
                Ok(event) => {
                    if tx.send(Event::Nvim(current, event)).is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = tx.send(Event::UiClosed(current));
                    return;
                }
            }
        }
    });
    connection.attach(size.0, size.1)?;
    *active = Some(ActiveUi {
        instance,
        connection,
        epoch: current,
    });
    Ok(())
}
fn respond(client: &mut UnixStream, boot: &str, response: api::Response) -> Result<()> {
    wire::send(
        client,
        &ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: boot.into(),
            request_id: response.id.clone(),
            final_chunk: true,
            data: serde_json::to_vec(&response)?,
        },
    )
}
pub fn handle(stream: UnixStream, target: Target, stop: Arc<AtomicBool>) -> Result<()> {
    let mut client = Connection(stream);
    client.0.set_nonblocking(false)?;
    client.0.set_read_timeout(Some(Duration::from_secs(5)))?;
    client.0.set_write_timeout(Some(Duration::from_secs(2)))?;
    let ClientMessage::EndpointControl { kind, data } = wire::decode(&wire::read(&mut client.0)?)?
    else {
        anyhow::bail!("expected endpoint hello")
    };
    ensure!(kind == ENDPOINT_HELLO_KIND, "expected endpoint hello");
    let hello: EndpointClientHello = serde_json::from_str(&data)?;
    let compatible =
        hello.generation == ENDPOINT_PROTOCOL_GENERATION && hello.supports_required_codecs();
    let welcome = EndpointServerWelcome {
        generation: ENDPOINT_PROTOCOL_GENERATION,
        server_version: concat!("neordr/", env!("CARGO_PKG_VERSION")).into(),
        snapshot_codec: SNAPSHOT_CODEC_V1.into(),
        surface_codec: SURFACE_CODEC_V1.into(),
        input_codec: INPUT_CODEC_V1.into(),
        blob_codec: BLOB_CODEC_V1.into(),
        methods: METHODS.iter().map(|s| (*s).into()).collect(),
        capabilities: vec![surface::CAPABILITY.into()],
        error: (!compatible).then(|| EndpointHandshakeError {
            code: "incompatible_endpoint".into(),
            message: "Neordr requires Herdr endpoint generation 1".into(),
        }),
    };
    wire::control(&mut client.0, ENDPOINT_WELCOME_KIND, &welcome)?;
    ensure!(compatible, "unsupported Herdr endpoint codecs");
    let mut size = dimensions(hello.surface_size.cols, hello.surface_size.rows)?;
    let mut surfaces = surface::Stream::new(hello.surface_scroll);
    let boot = format!(
        "neordr-{}-{}",
        std::process::id(),
        PROJECTION_REVISION.fetch_add(1, Ordering::Relaxed)
    );
    let mut projection_revision = PROJECTION_REVISION.fetch_add(1, Ordering::Relaxed);
    let mut projection = target.snapshot(&boot, projection_revision)?;
    wire::control(&mut client.0, ENDPOINT_SNAPSHOT_KIND, &projection)?;
    client.0.set_read_timeout(None)?;
    let (tx, rx) = mpsc::sync_channel(128);
    let mut client_reader = BufReader::new(client.0.try_clone()?);
    let client_tx = tx.clone();
    thread::spawn(move || {
        loop {
            match wire::read(&mut client_reader) {
                Ok(bytes) => {
                    if client_tx.send(Event::Client(bytes)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = client_tx.send(Event::Closed(format!("Herdr read: {error:#}")));
                    return;
                }
            }
        }
    });
    let mut active = None;
    let mut epoch = 0;
    let mut grid = grid::Grid::new();
    grid.clear(size.0, size.1);
    synchronize_ui(&target, &mut active, &mut grid, size, &tx, &mut epoch)?;
    let mut surface_revision = 0;
    let mut last_snapshot = Instant::now();
    let mut last_frame = Instant::now();
    let mut pending: Option<(u64, api::Request)> = None;
    let mut deferred = VecDeque::new();
    let mut deferred_bytes = 0;
    while !stop.load(Ordering::Relaxed) {
        // Controls cannot overtake bytes merely accepted into Neovim's input queue.
        // Continue reading RPC replies/redraw while the consumption marker runs.
        if let Some(ui) = &mut active
            && let Err(error) = ui.connection.tick()
        {
            if matches!(target, Target::Editor(_)) {
                return Err(error);
            }
            active = None;
            grid.clear(size.0, size.1);
            if let Some((_, request)) = pending.take() {
                respond(
                    &mut client.0,
                    &boot,
                    api::Response::error(&request.id, "editor_closed", "Editor disconnected"),
                )?;
            }
            last_snapshot = Instant::now() - Duration::from_secs(1);
        }
        if pending.as_ref().is_some_and(|(ticket, _)| {
            active
                .as_ref()
                .is_some_and(|ui| ui.connection.barrier_done(*ticket))
        }) {
            let (ticket, request) = pending.take().unwrap();
            let response = match active
                .as_mut()
                .unwrap()
                .connection
                .take_barrier_result(ticket)
                .expect("completed input barrier")
            {
                Ok(()) => target.execute(&request),
                Err(message) => api::Response::error(&request.id, "busy", message),
            };
            respond(&mut client.0, &boot, response)?;
            synchronize_ui(&target, &mut active, &mut grid, size, &tx, &mut epoch)?;
            last_snapshot = Instant::now() - Duration::from_secs(1);
        }
        let event = if pending.is_none() && !deferred.is_empty() {
            let bytes: Vec<u8> = deferred.pop_front().unwrap();
            deferred_bytes -= bytes.len();
            Ok(Event::Client(bytes))
        } else {
            rx.recv_timeout(Duration::from_millis(10))
        };
        match event {
            Ok(Event::Closed(reason)) => {
                eprintln!("Herdr endpoint: {reason}");
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Event::UiClosed(closed)) => {
                if active.as_ref().is_some_and(|ui| ui.epoch == closed) {
                    active = None;
                    grid.clear(size.0, size.1);
                    if let Some((_, request)) = pending.take() {
                        respond(
                            &mut client.0,
                            &boot,
                            api::Response::error(
                                &request.id,
                                "editor_closed",
                                "Editor disconnected",
                            ),
                        )?;
                    }
                    if matches!(target, Target::Editor(_)) {
                        break;
                    }
                    last_snapshot = Instant::now() - Duration::from_secs(1);
                }
            }
            Ok(Event::Nvim(current, event)) => {
                let Some(ui) = &mut active else { continue };
                if ui.epoch != current {
                    continue;
                }
                if !ui.connection.handle_response(&event)?
                    && let Some(a) = event.as_array()
                    && a.len() == 3
                    && a[1].as_str() == Some("redraw")
                {
                    grid.redraw(a[2].as_array().context("invalid redraw")?)?;
                }
            }
            Ok(Event::Client(bytes)) => {
                let message: ClientMessage = wire::decode(&bytes)?;
                if matches!(message, ClientMessage::Detach) {
                    break;
                }
                if pending.is_some() {
                    deferred_bytes += bytes.len();
                    ensure!(
                        deferred_bytes <= 8 * 1024 * 1024,
                        "endpoint control backlog exceeds limit"
                    );
                    deferred.push_back(bytes);
                    continue;
                }
                match message {
                    ClientMessage::ClientShellResize { surface_size, .. } => {
                        let (c, r) = dimensions(surface_size.cols, surface_size.rows)?;
                        size = (c, r);
                        if let Some(ui) = &mut active {
                            ui.connection.resize(c, r)?;
                        }
                    }
                    ClientMessage::ClientShellPaneInput { pane_id, events } => {
                        if let Some(ui) = &mut active {
                            ui.connection.enqueue_input(pane_id, events)?;
                        }
                    }
                    ClientMessage::ClientShellEndpointRequest { boot_id, request } => {
                        let raw: protocol::Request = serde_json::from_str(&request)?;
                        if boot_id != boot {
                            respond(
                                &mut client.0,
                                &boot,
                                api::Response::error(
                                    &raw.id,
                                    "stale_instance",
                                    "Endpoint restarted; reconnect",
                                ),
                            )?;
                        } else if !METHODS.contains(&raw.method.as_str()) {
                            respond(
                                &mut client.0,
                                &boot,
                                api::Response::error(
                                    &raw.id,
                                    "unsupported_method",
                                    "This operation is not supported by Neordr",
                                ),
                            )?;
                        } else {
                            match api::decode(&raw) {
                                Ok(request) => {
                                    if matches!(
                                        &request.command,
                                        api::Command::Herdr(
                                            Method::SessionSnapshot(_)
                                                | Method::WorkspaceList(_)
                                                | Method::TabList(_)
                                                | Method::PaneList(_)
                                                | Method::AgentList(_)
                                                | Method::PaneLayout(_)
                                        ) | api::Command::PaneList(_)
                                    ) {
                                        respond(&mut client.0, &boot, target.execute(&request))?;
                                    } else if let Some(ui) = &mut active {
                                        let ticket = ui.connection.request_barrier()?;
                                        pending = Some((ticket, request));
                                    } else {
                                        respond(&mut client.0, &boot, target.execute(&request))?;
                                        last_snapshot = Instant::now() - Duration::from_secs(1);
                                    }
                                }
                                Err(error) => respond(
                                    &mut client.0,
                                    &boot,
                                    api::Response::failure(&raw.id, "invalid_params", &error),
                                )?,
                            }
                        }
                    }
                    ClientMessage::EndpointControl { .. }
                    | ClientMessage::ClientShellHostTheme { .. }
                    | ClientMessage::ClientShellFocus { .. }
                    | ClientMessage::ClientShellMouseCapture { .. } => {}
                    _ => wire::send(
                        &mut client.0,
                        &ServerMessage::ClientShellError {
                            message: "This Herdr operation is not supported by Neordr".into(),
                        },
                    )?,
                }
            }
        }
        if pending.is_none() && last_snapshot.elapsed() >= Duration::from_millis(300) {
            synchronize_ui(&target, &mut active, &mut grid, size, &tx, &mut epoch)?;
            if let Ok(mut next) = target.snapshot(&boot, projection_revision)
                && next != projection
            {
                projection_revision = PROJECTION_REVISION.fetch_add(1, Ordering::Relaxed);
                next.revision = projection_revision;
                projection = next;
                wire::control(&mut client.0, ENDPOINT_SNAPSHOT_KIND, &projection)?;
                grid.dirty = true;
            }
            last_snapshot = Instant::now();
        }
        if grid.ready && grid.dirty && last_frame.elapsed() >= Duration::from_millis(16) {
            surface_revision += 1;
            let rect = wire::Rect {
                x: 0,
                y: 0,
                width: grid.width,
                height: grid.height,
            };
            let panes = projection
                .focused_pane_id
                .as_ref()
                .map(|pane| wire::Pane {
                    pane_id: pane.clone(),
                    content_revision: surface_revision,
                    rect,
                    inner_rect: rect,
                    scrollbar_rect: None,
                    scroll: None,
                    focused: true,
                    mouse_reporting: true,
                    sgr_pixel_mouse: false,
                    alternate_screen_active: true,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .into_iter()
                .collect();
            surfaces.send(
                &mut client.0,
                wire::Surface {
                    boot_id: boot.clone(),
                    projection_revision,
                    surface_revision,
                    frame: wire::Frame {
                        cells: grid.cells(),
                        width: grid.width,
                        height: grid.height,
                        cursor: Some(grid.cursor()),
                        hyperlinks: vec![],
                        graphics: vec![],
                    },
                    panes,
                    splits: vec![],
                    popup: None,
                    graphics: wire::Graphics::default(),
                },
                epoch,
            )?;
            grid.dirty = false;
            last_frame = Instant::now();
        }
    }
    Ok(())
}

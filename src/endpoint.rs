//! Herdr's stable shell endpoint backed by one ordered Neovim remote UI.
mod grid;
mod input;
mod projection;
mod ui;
mod wire;
use crate::{
    api, backend,
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
    Nvim(Msg),
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
fn snapshot(instance: &Instance, boot: &str, revision: u64) -> Result<wire::ClientShellSnapshot> {
    let ResponseResult::SessionSnapshot { snapshot } = backend::dispatch(
        instance,
        &api::Command::Herdr(Method::SessionSnapshot(EmptyParams {})),
    )?
    else {
        anyhow::bail!("invalid session snapshot")
    };
    Ok(projection::snapshot(
        &snapshot,
        boot,
        revision,
        &instance.cwd,
    ))
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
pub fn handle(stream: UnixStream, instance: &Instance, stop: Arc<AtomicBool>) -> Result<()> {
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
        capabilities: vec![],
        error: (!compatible).then(|| EndpointHandshakeError {
            code: "incompatible_endpoint".into(),
            message: "Neordr requires Herdr endpoint generation 1".into(),
        }),
    };
    wire::control(&mut client.0, ENDPOINT_WELCOME_KIND, &welcome)?;
    ensure!(compatible, "unsupported Herdr endpoint codecs");
    let (cols, rows) = dimensions(hello.surface_size.cols, hello.surface_size.rows)?;
    crate::nvim::prepare_attach(instance)?;
    let generation = instance
        .generation
        .as_deref()
        .context("missing generation")?;
    let boot = format!("neordr-{generation}-{}", std::process::id());
    let mut projection_revision = PROJECTION_REVISION.fetch_add(1, Ordering::Relaxed);
    let mut projection = snapshot(instance, &boot, projection_revision)?;
    wire::control(&mut client.0, ENDPOINT_SNAPSHOT_KIND, &projection)?;
    client.0.set_read_timeout(None)?;
    let mut nvim = ui::UiSession::connect(&instance.socket, generation.into())?;
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
    let mut nvim_reader = BufReader::new(nvim.try_clone_reader()?);
    thread::spawn(move || {
        loop {
            match rmpv::decode::read_value(&mut nvim_reader) {
                Ok(event) => {
                    if tx.send(Event::Nvim(event)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Event::Closed(format!("Neovim disconnected: {error}")));
                    return;
                }
            }
        }
    });
    nvim.attach(cols, rows)?;
    let mut grid = grid::Grid::new();
    let mut surface_revision = 0;
    let mut last_snapshot = Instant::now();
    let mut last_frame = Instant::now();
    let mut pending: Option<(u64, api::Request)> = None;
    let mut deferred = VecDeque::new();
    let mut deferred_bytes = 0;
    while !stop.load(Ordering::Relaxed) {
        // Controls cannot overtake bytes merely accepted into Neovim's input queue.
        // Continue reading RPC replies/redraw while the consumption marker runs.
        nvim.tick()?;
        if pending
            .as_ref()
            .is_some_and(|(ticket, _)| nvim.barrier_done(*ticket))
        {
            let (ticket, request) = pending.take().unwrap();
            let response = match nvim
                .take_barrier_result(ticket)
                .expect("completed input barrier")
            {
                Ok(()) => server::execute(&request, instance),
                Err(message) => api::Response::error(&request.id, "busy", message),
            };
            respond(&mut client.0, &boot, response)?;
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
            Ok(Event::Nvim(event)) => {
                if !nvim.handle_response(&event)?
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
                        nvim.resize(c, r)?;
                    }
                    ClientMessage::ClientShellPaneInput { pane_id, events } => {
                        nvim.enqueue_input(pane_id, events)?
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
                                        respond(
                                            &mut client.0,
                                            &boot,
                                            server::execute(&request, instance),
                                        )?;
                                    } else {
                                        let ticket = nvim.request_barrier()?;
                                        pending = Some((ticket, request));
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
            if let Ok(mut next) = snapshot(instance, &boot, projection_revision)
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
            wire::send(
                &mut client.0,
                &ServerMessage::PaneSurface(wire::Surface {
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
                }),
            )?;
            grid.dirty = false;
            last_frame = Instant::now();
        }
    }
    Ok(())
}

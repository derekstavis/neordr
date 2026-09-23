//! A single acknowledged, ordered Neovim UI connection.
//!
//! RPC replies are driven by the endpoint event loop, never read synchronously
//! here. In particular, accepting input is not the same as consuming it: a
//! marker barrier is required before a separate connection changes focus.
use super::input::{self, Operation};
use crate::contracts::wire::ClientPaneInputEvent;
use anyhow::{Context, Result, bail, ensure};
use rmpv::Value as Msg;
use std::{
    collections::{BTreeMap, VecDeque},
    io::Write,
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const RETRY: Duration = Duration::from_millis(10);
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
const INPUT_CHUNK_BYTES: usize = 4096;
static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

// Do not recompute the snapshot here: it calls jobwait, which is not fast-safe.
// Live buffer/tab IDs applied to the retained topology give the same virtual
// pane policy (matching terminal, otherwise first pane in the editor tab).
// Never focus the terminal: its surface may currently be editing a file.
const GUARDED_FAST_INPUT: &str = r#"
local generation, pane, method, args = ...
if generation ~= vim.g.neordr_generation then error('Neordr generation changed') end
local backend = require('neordr.backend')
local tab, buf = vim.api.nvim_get_current_tabpage(), vim.api.nvim_get_current_buf()
-- A newly-created terminal may not be in the retained snapshot yet. Its own
-- registered identity must never fall through to an older editor surface.
local registered = vim.b[buf].neordr_pane
if registered and registered ~= pane then return { false } end
local focused, matched_buffer
for _, row in ipairs((backend.last_snapshot or {}).panes or {}) do
  if row.neordr.tabpage == tab then
    if not focused then focused = row.pane_id end
    if row.neordr.buf == buf then focused = row.pane_id; matched_buffer = true; break end
  end
end
if not matched_buffer and vim.api.nvim_get_option_value('buftype', { buf = buf }) == 'terminal' then
  return { false }
end
if focused ~= pane then return { false } end
return { true, vim.api[method](unpack(args)) }
"#;

const GUARDED_INPUT: &str = r#"
local generation, pane, method, args = ...
if generation ~= vim.g.neordr_generation then error('Neordr generation changed') end
for _, row in ipairs(require('neordr.backend').snapshot().panes) do
  if row.focused and row.pane_id == pane then
    return { true, vim.api[method](unpack(args)) }
  end
end
return { false }
"#;

const PREPARE_BARRIER: &str = r#"
local generation, owner = ...
if generation ~= vim.g.neordr_generation then error('Neordr generation changed') end
if vim.api.nvim_get_mode().blocking then
  return { false, 'Neovim is waiting at a prompt; dismiss it before changing panes' }
end
local reply = require('neordr.backend').dispatch('neordr.gesture_prepare',
  { owner = owner, action = 'barrier' }, generation)
if reply.error then error(reply.error.message) end
return { true, reply.result.marker }
"#;

// Reading an already-computed result is safe while Neovim waits at a prompt.
const POLL_BARRIER: &str = r#"
local generation, owner = ...
if generation ~= vim.g.neordr_generation then error('Neordr generation changed') end
local backend = require('neordr.backend')
if vim.api.nvim_get_mode().blocking then
  for _, pending in pairs(backend.gesture_markers) do
    if pending.owner == owner then pending.cancelled = true end
  end
  backend.gesture_results[owner] = nil
  return { false, 'Neovim is waiting at a prompt; dismiss it before changing panes' }
end
local reply = backend.gesture_results[owner]
if not reply or not reply.done then return { true, false } end
if not reply.ok then error(reply.message or 'Neordr input barrier failed') end
backend.gesture_results[owner] = nil
return { true, true }
"#;

#[derive(Debug)]
enum Command {
    Call {
        method: &'static str,
        args: Vec<Msg>,
    },
    Input {
        pane: String,
        operation: Operation,
    },
    PrepareBarrier {
        ticket: u64,
        started: Instant,
    },
    Marker {
        ticket: u64,
        started: Instant,
        bytes: Vec<u8>,
    },
    PollBarrier {
        ticket: u64,
        started: Instant,
    },
}

struct Pending {
    id: u64,
    command: Command,
    sent: Instant,
}

pub(super) struct UiSession {
    stream: UnixStream,
    generation: String,
    owner: String,
    next_id: u64,
    next_ticket: u64,
    barrier_results: BTreeMap<u64, std::result::Result<(), String>>,
    queue: VecDeque<Command>,
    pending: Option<Pending>,
    retry_at: Instant,
    progress_at: Instant,
    queued_bytes: usize,
    accepted_since_barrier: usize,
}

impl UiSession {
    pub(super) fn connect(socket: impl AsRef<Path>, generation: String) -> Result<Self> {
        let stream = UnixStream::connect(socket)?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self::from_stream(stream, generation))
    }

    fn from_stream(stream: UnixStream, generation: String) -> Self {
        Self {
            stream,
            generation,
            owner: format!(
                "endpoint-{}-{}",
                std::process::id(),
                NEXT_OWNER.fetch_add(1, Ordering::Relaxed)
            ),
            next_id: 1,
            next_ticket: 1,
            barrier_results: BTreeMap::new(),
            queue: VecDeque::new(),
            pending: None,
            retry_at: Instant::now(),
            progress_at: Instant::now(),
            queued_bytes: 0,
            accepted_since_barrier: 0,
        }
    }

    pub(super) fn try_clone_reader(&self) -> Result<UnixStream> {
        Ok(self.stream.try_clone()?)
    }

    pub(super) fn attach(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.queue.push_back(Command::Call {
            method: "nvim_ui_attach",
            args: vec![
                cols.into(),
                rows.into(),
                Msg::Map(vec![
                    ("rgb".into(), true.into()),
                    ("ext_linegrid".into(), true.into()),
                ]),
            ],
        });
        self.tick()
    }

    pub(super) fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.queue.push_back(Command::Call {
            method: "nvim_ui_try_resize",
            args: vec![cols.into(), rows.into()],
        });
        self.tick()
    }

    pub(super) fn enqueue_input(
        &mut self,
        pane: String,
        events: Vec<ClientPaneInputEvent>,
    ) -> Result<()> {
        if self.is_idle() {
            self.progress_at = Instant::now();
        }
        for event in events {
            for operation in input::translate(event) {
                let size = operation_size(&operation);
                ensure!(
                    self.queued_bytes.saturating_add(size) <= MAX_QUEUED_BYTES,
                    "Neovim input queue is full"
                );
                self.queued_bytes += size;
                self.queue.push_back(Command::Input {
                    pane: pane.clone(),
                    operation,
                });
            }
        }
        self.tick()
    }

    /// Queue a consumption fence. The caller holds later client events until
    /// `barrier_done(ticket)`, then can safely dispatch a topology mutation.
    pub(super) fn request_barrier(&mut self) -> Result<u64> {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.queue.push_back(Command::PrepareBarrier {
            ticket,
            started: Instant::now(),
        });
        self.tick()?;
        Ok(ticket)
    }

    pub(super) fn barrier_done(&self, ticket: u64) -> bool {
        self.barrier_results.contains_key(&ticket)
    }

    pub(super) fn take_barrier_result(
        &mut self,
        ticket: u64,
    ) -> Option<std::result::Result<(), String>> {
        self.barrier_results.remove(&ticket)
    }

    pub(super) fn is_idle(&self) -> bool {
        self.pending.is_none() && self.queue.is_empty()
    }

    pub(super) fn tick(&mut self) -> Result<()> {
        if let Some(pending) = &self.pending {
            ensure!(
                pending.sent.elapsed() < TIMEOUT,
                "Neovim UI request timed out"
            );
            return Ok(());
        }
        if Instant::now() < self.retry_at {
            return Ok(());
        }
        let Some(mut command) = self.queue.pop_front() else {
            return Ok(());
        };
        if let Command::PrepareBarrier { started, .. } = &mut command {
            *started = Instant::now();
        }
        let (method, args) = match &command {
            Command::Call { method, args } => (*method, args.clone()),
            Command::Input { pane, operation } => {
                let (method, args) = match operation {
                    Operation::Input(bytes) => (
                        "nvim_input",
                        vec![Msg::Binary(
                            bytes[..input_chunk_len(
                                bytes,
                                INPUT_CHUNK_BYTES - self.accepted_since_barrier,
                            )]
                                .to_vec(),
                        )],
                    ),
                    Operation::Call { method, args } => (*method, args.clone()),
                };
                let (entrypoint, guard) = if matches!(operation, Operation::Input(_)) {
                    ("nvim__exec_lua_fast", GUARDED_FAST_INPUT)
                } else {
                    ("nvim_exec_lua", GUARDED_INPUT)
                };
                (
                    entrypoint,
                    vec![
                        guard.into(),
                        Msg::Array(vec![
                            self.generation.clone().into(),
                            pane.clone().into(),
                            method.into(),
                            Msg::Array(args),
                        ]),
                    ],
                )
            }
            Command::PrepareBarrier { .. } => (
                "nvim__exec_lua_fast",
                vec![
                    PREPARE_BARRIER.into(),
                    Msg::Array(vec![
                        self.generation.clone().into(),
                        self.owner.clone().into(),
                    ]),
                ],
            ),
            Command::Marker { bytes, .. } => ("nvim_input", vec![Msg::Binary(bytes.clone())]),
            Command::PollBarrier { started, .. } => {
                ensure!(
                    started.elapsed() < TIMEOUT,
                    "Neovim input consumption barrier timed out"
                );
                (
                    "nvim__exec_lua_fast",
                    vec![
                        POLL_BARRIER.into(),
                        Msg::Array(vec![
                            self.generation.clone().into(),
                            self.owner.clone().into(),
                        ]),
                    ],
                )
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        rmpv::encode::write_value(
            &mut self.stream,
            &Msg::Array(vec![0.into(), id.into(), method.into(), Msg::Array(args)]),
        )?;
        self.stream.flush()?;
        self.pending = Some(Pending {
            id,
            command,
            sent: Instant::now(),
        });
        Ok(())
    }

    /// Returns false for notifications (e.g. redraw), true for consumed replies.
    pub(super) fn handle_response(&mut self, event: &Msg) -> Result<bool> {
        let Some(a) = event.as_array() else {
            return Ok(false);
        };
        if a.first().and_then(Msg::as_u64) != Some(1) {
            return Ok(false);
        }
        ensure!(a.len() == 4, "Malformed Neovim UI response");
        let id = a[1]
            .as_u64()
            .context("Neovim response ID is not an integer")?;
        ensure!(
            self.pending.as_ref().is_some_and(|p| p.id == id),
            "Unexpected Neovim UI response ID {id}"
        );
        let pending = self.pending.take().unwrap();
        ensure!(a[2].is_nil(), "Neovim UI request: {}", a[2]);
        match pending.command {
            Command::Call { .. } => {}
            Command::Input { pane, operation } => {
                let reply = a[3].as_array().context("Invalid guarded input response")?;
                let accepted_target = reply
                    .first()
                    .and_then(Msg::as_bool)
                    .context("Missing input target acknowledgement")?;
                if !accepted_target {
                    self.queued_bytes -= operation_size(&operation);
                } else {
                    match operation {
                        Operation::Input(mut bytes) => {
                            let accepted = reply
                                .get(1)
                                .and_then(Msg::as_u64)
                                .context("Missing input byte acknowledgement")?
                                as usize;
                            ensure!(
                                accepted
                                    <= input_chunk_len(
                                        &bytes,
                                        INPUT_CHUNK_BYTES - self.accepted_since_barrier
                                    ),
                                "Neovim accepted more input bytes than sent"
                            );
                            if accepted > 0 {
                                self.progress_at = Instant::now();
                                self.accepted_since_barrier += accepted;
                            }
                            self.queued_bytes -= accepted;
                            bytes.drain(..accepted);
                            if !bytes.is_empty() {
                                self.retry_input(accepted)?;
                                self.queue.push_front(Command::Input {
                                    pane,
                                    operation: Operation::Input(bytes),
                                });
                            }
                            if self.accepted_since_barrier >= INPUT_CHUNK_BYTES - 32 {
                                // Neovim 0.12.5 cannot reclaim a fully exhausted
                                // input-buffer tail (upstream issue #40577). An
                                // actual consumption fence, not a timed sleep,
                                // keeps cumulative accepted input below capacity.
                                self.queue.push_front(Command::PrepareBarrier {
                                    ticket: 0,
                                    started: Instant::now(),
                                });
                            }
                        }
                        Operation::Call { method, args } => {
                            self.queued_bytes -= operation_size(&Operation::Call { method, args });
                            if method == "nvim_paste" {
                                ensure!(
                                    reply.get(1).and_then(Msg::as_bool) == Some(true),
                                    "Neovim cancelled paste"
                                );
                            }
                        }
                    }
                }
            }
            Command::PrepareBarrier { ticket, started } => {
                let reply = barrier_reply(&a[3])?;
                if reply.0 {
                    let marker = reply
                        .1
                        .as_str()
                        .context("Neovim did not provide an input marker")?;
                    self.queue.push_front(Command::Marker {
                        ticket,
                        started,
                        bytes: marker.as_bytes().to_vec(),
                    });
                } else {
                    self.finish_barrier(
                        ticket,
                        Err(reply.1.as_str().context("Missing barrier error")?.into()),
                    );
                }
            }
            Command::Marker {
                ticket,
                started,
                mut bytes,
            } => {
                let accepted = a[3]
                    .as_u64()
                    .context("Missing marker byte acknowledgement")?
                    as usize;
                ensure!(
                    accepted <= bytes.len(),
                    "Neovim accepted more marker bytes than sent"
                );
                bytes.drain(..accepted);
                if bytes.is_empty() {
                    self.queue
                        .push_front(Command::PollBarrier { ticket, started });
                } else {
                    ensure!(started.elapsed() < TIMEOUT, "Neovim input marker timed out");
                    self.retry_at = Instant::now() + RETRY;
                    self.queue.push_front(Command::Marker {
                        ticket,
                        started,
                        bytes,
                    });
                }
            }
            Command::PollBarrier { ticket, started } => {
                let reply = barrier_reply(&a[3])?;
                if !reply.0 {
                    self.finish_barrier(
                        ticket,
                        Err(reply.1.as_str().context("Missing barrier error")?.into()),
                    );
                } else if reply
                    .1
                    .as_bool()
                    .context("Invalid input barrier acknowledgement")?
                {
                    self.finish_barrier(ticket, Ok(()));
                } else {
                    self.retry_at = Instant::now() + RETRY;
                    self.queue
                        .push_front(Command::PollBarrier { ticket, started });
                }
            }
        }
        self.tick()?;
        Ok(true)
    }

    fn retry_input(&mut self, accepted: usize) -> Result<()> {
        if accepted > 0 {
            self.progress_at = Instant::now();
        } else {
            if self.progress_at.elapsed() >= TIMEOUT {
                bail!("Neovim stopped accepting input");
            }
            self.retry_at = Instant::now() + RETRY;
        }
        Ok(())
    }

    fn finish_barrier(&mut self, ticket: u64, result: std::result::Result<(), String>) {
        self.accepted_since_barrier = 0;
        // An internal fence has no topology mutation to refuse. A blocking
        // prompt already consumed prior input; leave it for subsequent explicit
        // keys to answer rather than disconnecting or dismissing it ourselves.
        if ticket != 0 {
            self.barrier_results.insert(ticket, result);
        }
    }
}

fn barrier_reply(value: &Msg) -> Result<(bool, &Msg)> {
    let fields = value.as_array().context("Invalid input barrier response")?;
    ensure!(fields.len() == 2, "Invalid input barrier response length");
    Ok((
        fields[0]
            .as_bool()
            .context("Invalid input barrier status")?,
        &fields[1],
    ))
}

fn input_chunk_len(bytes: &[u8], budget: usize) -> usize {
    let mut limit = bytes.len().min(budget);
    if limit == bytes.len() {
        return limit;
    }
    // Keep both UTF-8 codepoints and Neovim's <...> key notation intact.
    while limit > 0 && bytes[limit] & 0xc0 == 0x80 {
        limit -= 1;
    }
    if let Some(start) = bytes[..limit].iter().rposition(|b| *b == b'<')
        && !bytes[start..limit].contains(&b'>')
    {
        limit = start;
    }
    limit
}

fn operation_size(operation: &Operation) -> usize {
    match operation {
        Operation::Input(bytes) => bytes.len(),
        Operation::Call { args, .. } => args.iter().map(|a| a.as_str().map_or(32, str::len)).sum(),
    }
}

impl Drop for UiSession {
    fn drop(&mut self) {
        // Best effort only; channel shutdown itself always detaches this UI.
        let cleanup = "local generation, owner = ...; require('neordr.backend').dispatch('neordr.gesture', {owner=owner, action='cleanup'}, generation)";
        let _ = rmpv::encode::write_value(
            &mut self.stream,
            &Msg::Array(vec![
                2.into(),
                "nvim_exec_lua".into(),
                Msg::Array(vec![
                    cleanup.into(),
                    Msg::Array(vec![
                        self.generation.clone().into(),
                        self.owner.clone().into(),
                    ]),
                ]),
            ]),
        );
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(peer: &mut UnixStream) -> (u64, String, Vec<Msg>) {
        let msg = rmpv::decode::read_value(peer).unwrap();
        let a = msg.as_array().unwrap();
        (
            a[1].as_u64().unwrap(),
            a[2].as_str().unwrap().into(),
            a[3].as_array().unwrap().clone(),
        )
    }
    fn reply(ui: &mut UiSession, id: u64, value: Msg) -> Result<bool> {
        ui.handle_response(&Msg::Array(vec![1.into(), id.into(), Msg::Nil, value]))
    }
    fn guarded_bytes(args: &[Msg]) -> &[u8] {
        args[1].as_array().unwrap()[3].as_array().unwrap()[0]
            .as_slice()
            .unwrap()
    }
    fn pair() -> (UiSession, UnixStream) {
        let (stream, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        (UiSession::from_stream(stream, "generation".into()), peer)
    }

    #[test]
    fn retries_exact_bytes_before_paste_and_resize() {
        let (mut ui, mut peer) = pair();
        ui.enqueue_input(
            "pane".into(),
            vec![
                ClientPaneInputEvent::TextCommit("λabc".into()),
                ClientPaneInputEvent::Paste("paste".into()),
            ],
        )
        .unwrap();
        ui.resize(90, 30).unwrap();
        let (id, method, args) = read(&mut peer);
        assert_eq!(method, "nvim__exec_lua_fast");
        assert_eq!(guarded_bytes(&args), "λabc".as_bytes());
        // Even a byte count inside a multibyte character must not panic or lose bytes.
        reply(&mut ui, id, Msg::Array(vec![true.into(), 1.into()])).unwrap();
        let (id, _, args) = read(&mut peer);
        assert_eq!(guarded_bytes(&args), &"λabc".as_bytes()[1..]);
        reply(&mut ui, id, Msg::Array(vec![true.into(), 4.into()])).unwrap();
        let (id, _, args) = read(&mut peer);
        assert_eq!(args[1].as_array().unwrap()[2].as_str(), Some("nvim_paste"));
        reply(&mut ui, id, Msg::Array(vec![true.into(), true.into()])).unwrap();
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim_ui_try_resize");
        reply(&mut ui, id, Msg::Nil).unwrap();
        assert!(ui.is_idle());
        assert_eq!(ui.queued_bytes, 0);
    }

    #[test]
    fn stale_focus_is_discarded_and_zero_acceptance_yields() {
        let (mut ui, mut peer) = pair();
        ui.enqueue_input(
            "pane".into(),
            vec![
                ClientPaneInputEvent::TextCommit("old".into()),
                ClientPaneInputEvent::TextCommit("next".into()),
            ],
        )
        .unwrap();
        let (id, _, _) = read(&mut peer);
        reply(&mut ui, id, Msg::Array(vec![false.into()])).unwrap();
        let (id, _, _) = read(&mut peer);
        reply(&mut ui, id, Msg::Array(vec![true.into(), 0.into()])).unwrap();
        assert!(ui.pending.is_none());
        assert!(!ui.is_idle());
        ui.retry_at = Instant::now();
        ui.tick().unwrap();
        let (id, _, args) = read(&mut peer);
        assert_eq!(guarded_bytes(&args), b"next");
        reply(&mut ui, id, Msg::Array(vec![true.into(), 4.into()])).unwrap();
        assert!(ui.is_idle());
    }

    #[test]
    fn barrier_waits_for_consumption_not_acceptance() {
        let (mut ui, mut peer) = pair();
        let ticket = ui.request_barrier().unwrap();
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim__exec_lua_fast");
        reply(
            &mut ui,
            id,
            Msg::Array(vec![true.into(), "\u{f0000}".into()]),
        )
        .unwrap();
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim_input");
        reply(&mut ui, id, 4.into()).unwrap();
        assert!(!ui.barrier_done(ticket));
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim__exec_lua_fast");
        reply(&mut ui, id, Msg::Array(vec![true.into(), false.into()])).unwrap();
        assert!(!ui.barrier_done(ticket));
        ui.retry_at = Instant::now();
        ui.tick().unwrap();
        let (id, _, _) = read(&mut peer);
        reply(&mut ui, id, Msg::Array(vec![true.into(), true.into()])).unwrap();
        assert!(ui.barrier_done(ticket));
        assert_eq!(ui.take_barrier_result(ticket), Some(Ok(())));
        assert!(ui.is_idle());
    }

    #[test]
    fn blocking_prompt_rejects_control_without_losing_the_input_connection() {
        let (mut ui, mut peer) = pair();
        let ticket = ui.request_barrier().unwrap();
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim__exec_lua_fast");
        reply(&mut ui, id, Msg::Array(vec![false.into(), "prompt".into()])).unwrap();
        assert_eq!(ui.take_barrier_result(ticket), Some(Err("prompt".into())));
        assert!(ui.is_idle());
        ui.enqueue_input(
            "pane".into(),
            vec![ClientPaneInputEvent::TextCommit("\r".into())],
        )
        .unwrap();
        let (id, _, _) = read(&mut peer);
        reply(&mut ui, id, Msg::Array(vec![true.into(), 1.into()])).unwrap();
        assert!(ui.is_idle());
    }

    #[test]
    fn malformed_acknowledgements_are_errors() {
        let (mut ui, mut peer) = pair();
        ui.enqueue_input(
            "pane".into(),
            vec![ClientPaneInputEvent::TextCommit("x".into())],
        )
        .unwrap();
        let (id, _, _) = read(&mut peer);
        assert!(reply(&mut ui, id + 1, Msg::Nil).is_err());
        assert!(reply(&mut ui, id, Msg::Array(vec![true.into(), 2.into()])).is_err());
    }

    #[test]
    fn bounded_writes_preserve_utf8_and_key_notation_boundaries() {
        let unicode = format!("{}界rest", "a".repeat(INPUT_CHUNK_BYTES - 1));
        assert_eq!(
            input_chunk_len(unicode.as_bytes(), INPUT_CHUNK_BYTES),
            INPUT_CHUNK_BYTES - 1
        );
        let notation = format!("{}<LT>rest", "a".repeat(INPUT_CHUNK_BYTES - 2));
        assert_eq!(
            input_chunk_len(notation.as_bytes(), INPUT_CHUNK_BYTES),
            INPUT_CHUNK_BYTES - 2
        );
        let (mut ui, mut peer) = pair();
        ui.enqueue_input(
            "pane".into(),
            vec![ClientPaneInputEvent::TextCommit(unicode.clone())],
        )
        .unwrap();
        let (id, _, args) = read(&mut peer);
        let prefix = guarded_bytes(&args).to_vec();
        assert_eq!(prefix.len(), INPUT_CHUNK_BYTES - 1);
        reply(
            &mut ui,
            id,
            Msg::Array(vec![true.into(), prefix.len().into()]),
        )
        .unwrap();
        // The next chunk waits for actual consumption, not just acceptance.
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim__exec_lua_fast");
        reply(
            &mut ui,
            id,
            Msg::Array(vec![true.into(), "\u{f0000}".into()]),
        )
        .unwrap();
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim_input");
        reply(&mut ui, id, 4.into()).unwrap();
        let (id, method, _) = read(&mut peer);
        assert_eq!(method, "nvim__exec_lua_fast");
        reply(&mut ui, id, Msg::Array(vec![true.into(), true.into()])).unwrap();
        assert!(ui.barrier_results.is_empty());
        let (id, _, args) = read(&mut peer);
        let suffix = guarded_bytes(&args);
        assert_eq!([prefix, suffix.to_vec()].concat(), unicode.as_bytes());
        reply(
            &mut ui,
            id,
            Msg::Array(vec![true.into(), suffix.len().into()]),
        )
        .unwrap();
        assert!(ui.is_idle());
    }

    #[test]
    fn separate_input_events_share_the_same_consumption_budget() {
        let (mut ui, mut peer) = pair();
        ui.enqueue_input(
            "pane".into(),
            vec![
                ClientPaneInputEvent::TextCommit("a".repeat(INPUT_CHUNK_BYTES / 2)),
                ClientPaneInputEvent::TextCommit("b".repeat(INPUT_CHUNK_BYTES / 2)),
                ClientPaneInputEvent::TextCommit("tail".into()),
            ],
        )
        .unwrap();
        for _ in 0..2 {
            let (id, _, args) = read(&mut peer);
            assert_eq!(guarded_bytes(&args).len(), INPUT_CHUNK_BYTES / 2);
            reply(
                &mut ui,
                id,
                Msg::Array(vec![true.into(), (INPUT_CHUNK_BYTES / 2).into()]),
            )
            .unwrap();
        }
        assert!(matches!(
            ui.pending.as_ref().unwrap().command,
            Command::PrepareBarrier { ticket: 0, .. }
        ));
        assert_eq!(ui.accepted_since_barrier, INPUT_CHUNK_BYTES);
    }
}

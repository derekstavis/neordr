# Design and compatibility contract

## Implemented state (development)

The session bridge, terminal model, Lua lifecycle integration, Claude/Codex
foreground discovery and host-side Chat View path are implemented. The installed
moshi-hook resolves native session IDs, streams initial and appended transcript
records, submits multiline prompts, lists workspaces and focuses their terminals
in isolated tests. Actual Neovim remote UI attachment/detachment and the user's
tiny-term configuration are also exercised by smoke tests.

Testing uncovered a load-bearing binary behavior: prompt submission omits the
named session when choosing its Herdr API socket. A shared default-socket router
therefore routes explicit globally unique IDs to the per-instance servers.
IDs include the internal bridge prefix, Neovim generation, and terminal lifetime.
The router checks for a supported foreground agent before session-less input.

The design notes below explain the architecture and original milestones. See the
README for the precise supported subset and remaining limits: unknown agent
status, approximate recent unwrapped capture,
and untested phone/cloud approval flows.

Local question/plan/permission answer delivery is now exercised through the real
Moshi gateway with simulated Claude/Codex screens at desktop and phone widths.
See [approval compatibility](approval-compatibility.md) for the exact coverage,
wrapped-title limitation, and stale Claude question hook-state behavior.

A session groups editor processes; currently one default session is supported.
Each editor is one workspace. Native tabpages are layout within that editor.
Workspace creation launches a detached headless process, loads normal user config,
and creates a shell. The registry records socket, PID, generation, and ownership;
recovery validates identity before reconnecting. Process lifetime is independent
of server or CLI lifetime. Explicit workspace close checks all modified editor
buffers and then quits the editor and its jobs, regardless of ownership.
An empty default session continues serving creation requests.

Terminal close delegates to the configured plugin callback. Terminal order uses
stable buffer IDs across native tabs; adapter callbacks receive only the current
native tab's terminals. Workspace order is stored atomically by the session
server in `herdr/.neordr-order.json`.
The indexes use Herdr's insertion-boundary semantics, including insertion at
the item count to move an item to the end. Backend API errors retain their
structured code through the router, allowing Moshi's `invalid_request` fallback.

`pane.split` deliberately preserves the existing mapping: a terminal buffer is
one virtual Herdr tab with one pane. It opens a neighboring Neovim window while
leaving the complete editor UI visible. It does not introduce Herdr tab groups
or a second layout manager. The target pane's directory is inherited unless
overridden; its stable ID remains unchanged, and the new job gets its own ID and
hook environment. Default-router routing recognizes `target_pane_id` and records
the selected editor when `focus` is true. A plugin split callback owns creation,
and the existing close callback handles rollback and normal destruction.

This machine's tiny-term adapter records each panel's actual window instead of
relying solely on the plugin's single window-per-position registry. Explicit
split panels remember an anchor and direction for restoration. If the anchor
window has closed, restoration uses the current window; hiding and restoring a
nested layout does not promise an exact saved split tree.

Workspace/tab creation and workspace/tab/pane naming are implemented. Moshi
0.3.21 creates a tab with `focus`, `label`, and `workspace_id`, then sends the
bare agent command and Enter in two requests. Default-socket input stages only
`claude` or `codex` for an untouched, newly created shell and consumes that
allowance when Enter arrives. No command bytes are sent during staging. The
allowance expires after 30 seconds and is cancelled by UI typing or ordinary
API input; returning to a shell after agent exit does not renew it.

The terminal adapter's `create_terminal(opts)` callback keeps plugin ownership
of terminal jobs and panels. Explicit labels override dynamic terminal titles
without changing the stable pane or tab IDs. New workspaces start an editor in the requested directory; terminal creation
can override its job cwd without changing other workspaces.

Discovery uses Neovim 0.12's experimental `nvim__exec_lua_fast` entry point for
bounded identity reads, so modal prompts do not hide a running session. Normal
snapshots are retained in Lua. While `nvim_get_mode().blocking` is true, list
requests read that retained snapshot and capture can read terminal buffer text;
they do not run `jobwait` or rebuild terminal/plugin state inside the modal loop.
Unblocked snapshot refreshes and all editor mutations use regular deferred RPC.
The default router handles requests concurrently with a 32-connection bound,
preventing slow requests from causing false health failures and error prompts.

Moshi-hook 0.3.26 uses `session.snapshot` for workspace enumeration. Neordr
returns Herdr's public snapshot shape (workspaces, tabs, panes, layouts, agents,
and focused IDs), preserving globally unique IDs across default-router
aggregation. Earlier separate list methods remain available.

## Remote gesture transport

Attachments supervise `nvim --remote-ui` behind a private PTY. The outer terminal
is put in raw mode and restored on exit; output is relayed unchanged, dimensions
are forwarded to the child PTY, and SIGTERM/SIGHUP or Ctrl-B q detach only that UI.
The supervising Herdr process remains in the ancestry used by Moshi.

A streaming decoder handles the default Ctrl-B chords across read boundaries.
Bracketed paste is passed literally, including control bytes, and ordinary
typing goes to Neovim's TUI. Recognized actions use generation-checked local RPC;
an action is armed over RPC, then ordered behind a one-use Plane-15 private-use
input marker in the TUI stream. `vim.on_key` discards the marker before it reaches
the editor or job, schedules the action, and acknowledges completion. The
transport waits before forwarding subsequent input. This prevents rapid swipes
from moving focus ahead of queued typing. Terminal-mode transitions are queued
without remapping so the first subsequent character is not a Normal-mode command.
If acknowledgement fails, the connection detaches instead of forwarding text to
an uncertain target. Failed actions are never replayed into a shell. Prefix state and
workspace-navigation input belong to each attachment, not global Neovim mappings.

Tabs cycle terminal buffers through the existing focus adapter. Pane gestures
cycle visible Neovim splits (a presentation-level extension; the API model still
has one pane per virtual tab). The default workspace navigator obtains the full
session list and switches the remote UI between editors after acknowledging
prior input. Explicit legacy bridge-name attachments remain pinned. A new attachment is required to pick up a rebuilt transport executable.

Zoom uses a floating window showing the same buffer, leaving the original split
tree and plugin window identities intact. It resizes with the editor and records
its attachment owner. Clean detach restores only that owner's zoom/picker, and
does not close another client's view. Neovim still shares focus and dimensions
between UIs. Killing the supervisor with SIGKILL cannot run its cleanup, and a
disconnected Mosh client still relies on Mosh's server timeout to exit.

## Target

Support the installed moshi-hook 0.3.21 without changing Moshi or moshi-hook.
Two user-visible outcomes drive the project:

1. Discover and attach to running Neovim instances from Moshi's session picker.
2. Show and interact with Claude/Codex conversations in Neovim terminal jobs
   through Moshi Chat View.

The app will identify Neordr as Herdr. Creating a native Neovim category requires
changes to the app and is outside this compatibility approach.

## Separate integration paths

The connect-time picker runs `herdr session list --json` over SSH through `sh -lc`.
It does not use moshi-hook. A real `herdr` executable or symlink must be on that
shell's PATH; a fish function and MOSHI_HERDR_PATH alone are insufficient.

The connected workspace/agent UI uses moshi-hook's gateway. That gateway talks
to Herdr through newline-delimited JSON over Unix sockets. Both interfaces must
work; successful CLI listing alone does not establish Chat View compatibility.

For named sessions the inspected hook resolves
`$XDG_CONFIG_HOME/herdr/sessions/<name>/herdr.sock`, falling back to
`~/.config/herdr/sessions/<name>/herdr.sock`. HERDR_SOCKET_PATH only overrides the
default session in that inspected resolver. Do not replace another server's
socket or overwrite a user's existing herdr executable.

## Ownership and identity

- Session: group of Neovim instances (one default group today).
- Neovim instance: workspace.
- Native tabpage: editor layout within the workspace.
- Terminal buffer/job: virtual tab containing a pane.
- Neovim windows: presentation, not persistent terminal identity.

Hidden terminal buffers continue to exist and must remain controllable. Buffer
numbers, PIDs and socket names can be reused; registration must bind an instance
generation and terminal job lifetime before agent routing is enabled. A native
Claude/Codex conversation ID is distinct from a pane ID.

The existing oh-my-fish Neovim plugin discovers sockets under XDG_RUNTIME_DIR
and TMPDIR/nvim.$USER and attaches with `nvim --remote-ui --server <socket>`.
Keep that behavior, add live RPC checks and an explicit registration path for
custom --listen sockets. A remote UI shares the running editor's state and can
affect its dimensions; it is not an independent layout. Persistent sessions need
a deliberate server/detach lifecycle.

An attach process should supervise the remote UI rather than immediately exec
it: moshi-hook also has Herdr client process-ancestry detection. Whether a symlink
invocation is sufficient for its process-name tests remains an end-to-end check.

## Chat View

Use the existing Moshi Claude/Codex hooks and native transcript readers. Do not
reimplement transcript parsing or use terminal scrollback as conversation data.

A small Neovim Lua integration should supply HERDR_ENV=1, HERDR_SESSION=default
and HERDR_PANE_ID in the environment of each new terminal job. Preserve NVIM.
Handle inherited competing mux variables locally to managed jobs. Existing
shells cannot acquire new environment values retroactively without a wrapper or
explicit process-based registration.

HERDR_SESSION is `default`, not the per-editor name: Moshi's gateway matches a
hook record to a pane only when the record's session equals the session in the
request, and the app requests plain `mux=herdr` (default). Pane IDs already
carry the owning editor, so routing is unaffected. Agents started before this
change keep the old value until restarted.

Expose real shell/job PIDs and agent identity through pane.process_info and
agent.list. Pane discovery must work without hook state, avoiding a circular
dependency when the first hook event attempts to resolve its pane.

Send terminal input with nvim_chan_send(job_channel, bytes). Editor input APIs
depend on focus and mode and are inappropriate. Translate named keys to terminal
bytes; support bracketed paste deliberately. Capture the terminal's active
screen for pane.read, independently of user scroll position, and validate hidden
and alternate-screen behavior before using it for approval prompt checks.

Integrate focus with the user's tiny-term restore/show lifecycle rather than
replacing arbitrary editor buffers. Their panels retain multiple hidden jobs.

## Implementation boundaries

Keep CLI/session management, Herdr wire types, socket transport and the Neovim
backend separate. Unsupported methods must return structured errors, never
successful empty placeholders that imply agent support.

Herdr source inspected at commit
`28360107c1bfab57fb0de99d74bfa48e74423ca8` (package version 0.9.1) contains one
application package, no src/lib.rs and no reusable API crate. A direct Cargo Git
dependency cannot expose its modules. A maintained fork extracting a protocol
crate is possible; simply adding a library target would also inherit the
application dependencies and Ghostty/Zig build script.

Neordr derives the entire upstream API, event, wire and endpoint schema at build
time from an unmodified pinned Git submodule. Its AST extractor retains upstream
serde behavior and rejects unknown dependencies during compilation. Generated
Rust stays in Cargo's output directory. Application dispatch, routing, backend
results and snapshot projection all use these types; raw JSON is confined to
transport decoding, legacy parameter normalization and upstream opaque payloads.
Neordr extensions compose upstream types rather than redefining their fields.
See [contract generation](../build_support/README.md) for the pin and update rules.

The Herdr client bridge implements stable endpoint generation 1 independently,
using bincode 2 framing on the default and per-editor `herdr-client.sock`. It projects
the existing Neordr workspace model into Herdr's shell snapshot and translates
Neovim `ext_linegrid` events into Herdr cells. Semantic input goes to Neovim RPC,
preserving the full editor. Closing the endpoint's RPC connection detaches its UI.
The private direct-terminal protocol remains unsupported (`ping.protocol = 0`);
the separate endpoint capability advertises generation 1. See
[the interface compatibility report](herdr-interface.md) for the tested binary,
scope, and validation commands.

## Milestones and acceptance checks

1. Rust CLI/server foundation: discover live Neovim sockets, register a bridge,
   list sessions in Herdr's JSON shape, attach a remote UI, return framed JSON
   replies, and clean up owned sockets. Test stale sockets and duplicate names.
2. Neovim terminal model: workspace/tab/pane listing and stable identities,
   hidden terminal input, screen capture and focus restoration. Verify that
   sending input leaves the selected editor buffer unchanged.
3. Agent integration: per-job hook environment, accurate agent/session/process
   association, native transcript resolution and prompt submission. Test two
   simultaneous agents and session restart/PID reuse; do not choose the newest
   transcript as an identity shortcut.
4. Actual Moshi validation: connect-time listing and attachment, Jump To, Claude
   and Codex Chat View, reconnect/detach, resize, multiline input and approvals.
5. Additional Herdr commands only when their Neovim semantics are implemented:
   creation, rename, move, close, layout and navigation.

## References

- https://github.com/herdrdev/herdr/tree/28360107c1bfab57fb0de99d74bfa48e74423ca8
- https://herdr.dev/docs/socket-api/
- https://getmoshi.app/docs/debug-multiplexer-chooser
- https://neovim.io/doc/user/api/
- https://neovim.io/doc/user/remote/

Local binary inspection artifacts were created under
`/private/tmp/moshi-inspection/`; these are local evidence, not upstream source.

## Live conversation associations

Older running terminals retain their per-editor `HERDR_SESSION` even after the
plugin starts exporting `default`. Moshi 0.4.27 rejects those saved hook records
when its client queries the default session. Rewriting the records is temporary:
the next completion hook restores the original environment's session name.

Neordr projects an optional typed Herdr `agent_session` from existing Moshi
Claude/Codex hook metadata when the full pane identity, live foreground agent
PID and agent type all match. Ended, missing-PID, malformed and ambiguous records
are ignored. This reads association metadata, not transcripts, and does not edit
agent settings or hook records. No Moshi installation means no enrichment.

The per-process index refreshes at most once per second and is shared across
editor snapshots; foreground processes are still validated on each request.
Record files are limited to 64 KiB and directory scans to 10,000 entries per
agent. The small hook-record adapter is a Moshi file-format dependency; unknown
or changed metadata fails closed, leaving Moshi's own discovery available.
The outbound `AgentSessionInfo` schema comes from the pinned Herdr source.

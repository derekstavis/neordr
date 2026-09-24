# Operating Neordr

These notes describe behavior shared by local installations. For a first setup,
start with [installation](installation.md).

## Session and terminal model

The default session groups all discovered Neovim editors. One editor process is
one workspace; native Neovim tabpages remain editor layout. Each managed terminal
buffer becomes a virtual Herdr tab with one pane. Selecting it focuses the
buffer in input mode, including terminals in other native tabpages. The complete
editor remains visible.

Creating a workspace launches a detached headless Neovim in the requested
absolute directory, loads your normal configuration, and starts an initial
shell through your terminal adapter. Several workspaces may use the same folder.
Editors survive both client detach and Neordr server exits. Explicitly closing
a workspace quits its editor and stops its jobs, whether headless or opened
locally. All modified nonterminal buffers, including hidden ones, must be saved
first. There is currently no forced-discard API. Closing the last workspace
leaves the default session available for creation through Herdr or Moshi.

A new terminal uses the configured adapter, or a shell split without one.
Splitting right or down opens an adjacent terminal window; jobs remain separate
sidebar entries. Closing a terminal preserves the other jobs and file buffers.

Bare `herdr` (the Neordr alias) and `--session default` follow the selected editor,
or the first available workspace. The workspace gesture picker switches across
editors. Focus and layout are shared between clients. Legacy per-editor names
remain usable with `--session NAME` for pinned attachment, but no longer appear
as separate sessions in the picker. Named groups are not implemented yet.

```sh
neordr session list --json
neordr discover --json
neordr session attach default
neordr --session default rpc session.snapshot
neordr --session default rpc workspace.create --params '{"cwd":"/absolute/project","focus":true}'
neordr --session default rpc agent.list
```

The session server periodically scans normal Neovim sockets in its configuration
namespace and recovers recorded editors after restart. Custom sockets need plugin
registration or an explicit `serve --nvim` bridge. Recovery checks both PID and
editor generation. `:NeordrStatus` shows the editor's internal bridge name and
socket. It is not a separately listed session.

## Plugin diagnostics

With the development plugin, run `:checkhealth neordr` to check the supported
Neovim version, executable and version, setup state, bridge job, router, and
session readiness. Checks do not restart running services or alter terminal
jobs. Run `:NeordrStatus` for a compact view of the current session, socket,
executable, bridge process state, and last error; it does not probe connections.
Use `:help neordr` for the complete Lua API. These additions are newer than
`v0.1.0`.

`require('neordr').setup(opts)` returns `true` when initialization succeeds, or
`false, error_message` when it fails. Startup failure is reported without
aborting the rest of your editor configuration. Fix the reported path, version,
or configuration issue, then retry `setup` with the same session options.
Repeated setup with the same session and executable reuses a running bridge.
Changing an initialized session or the executable path of a running bridge
requires an editor restart. Successful setup starts the bridge asynchronously;
check health again if it is still starting. Initialize before launching terminal jobs so they receive the
correct agent identity.

## Remote shortcuts and gestures

Remote attachments interpret Herdr's default prefix, **Ctrl-B**. Local Neovim
mappings are unchanged. For Moshi touch gestures, select the corresponding
multiplexer actions under **Settings → Input → Gestures**; a gesture configured
to change font size or switch connections continues to perform that action.

| Action | Keys | Effect |
| --- | --- | --- |
| Next / previous terminal | Ctrl-B, `n` / `p` | Cycle terminal buffers in this workspace, including hidden ones. |
| Next / previous pane | Ctrl-B, Tab / Shift-Tab | Cycle visible Neovim splits, including file windows. |
| Next / previous workspace | Ctrl-B, `w`, Down / Up, Enter | Attach to another editor workspace. |
| Zoom | Ctrl-B, `z` | Toggle a full-size floating view of the focused buffer. |
| New terminal | Ctrl-B, `c` | Create a terminal through the configured adapter. |
| Numbered terminal | Ctrl-B, `1`–`9` | Select a virtual terminal tab. |
| Directional focus | Ctrl-B, `h` / `j` / `k` / `l`, or arrows | Focus a neighboring split. |
| Detach | Ctrl-B, `q` | Release this UI and keep the editor running. |

The workspace picker also opens with Ctrl-B, `g`; Escape cancels it. It is not
Herdr's complete goto prompt. Send a literal Ctrl-B by pressing it twice.
An unmatched prefix is forwarded after one second. Bracketed paste bypasses
shortcut interpretation. Custom prefixes and other shortcut-panel actions,
including kill and split chords, are not translated; unmatched chords pass
through to Neovim.

Zoom preserves underlying splits and jobs. Toggling it off, switching focus, or
cleanly detaching its owning connection restores the normal view. Herdr's own
interactive client handles its native shortcuts separately, including its
new-terminal naming dialog.

## Shared UI size and connection lifetime

On attachment, Neordr automatically acknowledges a blocking Neovim
"Press ENTER" message, so an error or warning cannot leave the new UI blank.
This applies to Moshi's terminal view and Herdr's interactive interface.
The message remains in `:messages`. Confirmation dialogs, input prompts, and
pager prompts are not answered automatically. Recovery runs only on attachment;
session discovery never dismisses prompts. If another blocking prompt prevents
attachment, answer it in an existing UI and reconnect.

All attached UIs share Neovim's focus, layout, and mode. Neovim sizes the editor
to the smallest attached UI; a phone connection can therefore constrain the
view seen by a larger client.

Cleanly detaching closes that client's Neovim UI and releases its size
constraint. Closing a mobile app or losing the network may leave a remote
transport alive. In particular, a surviving Mosh session can keep the remote
process attached. Use Ctrl-B, q or `:detach` before leaving when you want an
immediate detach. Do not quit Neovim just to disconnect a UI: quitting ends its
terminal jobs. Use Neovim's `nvim_list_uis()` when diagnosing a lingering client.

## Chat View and agent identity

Managed terminals inherit Herdr-compatible session and pane identities before
their jobs start. Existing terminal processes retain their original environment;
create new managed terminals after enabling integration. Moshi resolves Claude
and Codex native session IDs and reads their conversation files. Neordr supplies
process/terminal identity, screen capture, and input routing.

The default router rejects ordinary Chat View input once a supported foreground
agent is gone. Named-session API calls retain direct shell input behavior.
For automatic agent startup, a freshly created untouched terminal has a single
30-second launch allowance. Typing or ordinary API input cancels it; returning
to the shell after an agent exits does not renew it. Choose an agent during
creation, or use Term view to launch one later.

Local question, plan, and permission answers have tests using real Moshi gateway
requests and simulated agent prompts. Exact soft-wrap reconstruction, wrapped
inferred plan titles, stale Claude question-hook state, physical phone gesture
behavior, and cloud approval delivery have limits described in the
[approval compatibility report](approval-compatibility.md).

## Terminal plugin adapters

Without an adapter, `:NeordrTerm [command ...]` opens a shell or command in a
managed split. Plugins must prepare a buffer **before** starting its terminal
job and use the returned environment:

```lua
local neordr = require('neordr')
local buf = vim.api.nvim_get_current_buf()
local env = neordr.prepare(buf, { MY_VARIABLE = 'value' })
vim.fn.jobstart({ vim.o.shell }, { term = true, env = env })
```

Configure `setup` with the callbacks your plugin implements:

| Option | Contract |
| --- | --- |
| `bin` | Neordr executable path; defaults to `exepath('neordr')`. |
| `session` | Optional explicit session name: 1–64 ASCII letters, digits, hyphens, or underscores; `default` is reserved. Defaults to a generated per-editor name. |
| `focus_terminal(buf)` | Restore/focus the plugin's terminal panel; return true when handled. |
| `label_terminal(buf)` | Return the plugin's display label for the buffer. |
| `create_terminal(opts)` | Create and return a new terminal buffer. Apply `cwd`, `env`, and `label`; call `prepare` before starting its job. |
| `split_terminal(opts)` | Create and return a terminal buffer beside `target_win` / `target_buf`, with `direction`, `ratio`, `cwd`, `env`, and `label`. |
| `close_terminal(buf)` | Close through the plugin's lifecycle; return true when handled. |
| `reorder_terminals(buffers)` | Apply the ordered buffer IDs for one workspace to the plugin's panels. |

The split ratio describes the original window's share. Neovim's minimum window
sizes constrain the result. A configured creation adapter without a split
callback returns an error on split, preserving the plugin's ownership of jobs.
Creation failures roll back newly allocated jobs, buffers, and windows.

`neordr.tiny_term` offers `close` and `reorder` helpers for the reference tiny-term
panel integration. It is not a complete adapter for arbitrary tiny-term setups:
focus, creation, splitting, labels, and panel restoration depend on the host's
configuration.

## Compatibility API

Supported operations include:

- `ping`, `session.snapshot`, workspace/tab/pane/agent lists, `pane.get`,
  `pane.process_info`, `pane.layout`, and terminal text capture with `pane.read`.
- Workspace/tab/pane focus and `agent.focus`.
- Workspace and tab creation, workspace/tab/pane renaming, and `pane.split`.
- Workspace/tab/pane close, workspace/tab move, and `pane.send_text` /
  `pane.send_keys`.
- Moshi's `config check` and `server reload-config` preflight commands.

Workspace order across editors is persisted by the router. Jobs never move
between editor processes. IDs include a session prefix and editor generation;
stale IDs do not select a replacement job. Socket permissions are 0600, request
frames are bounded, and unsupported operations produce structured errors.

`pane.read` captures the active terminal screen independently of user scroll
position and supports alternate screens. `recent_unwrapped` returns bounded
recent buffer rows, but Neovim does not expose soft-wrap boundaries for exact
unwrapping. Hard line breaks are not guessed away. ANSI capture is unsupported.

`config check` validates TOML and the sidebar mode; reload validates and
acknowledges. Neovim owns the UI, so Herdr UI settings have no renderer to update.
`HERDR_CONFIG_PATH` overrides `$XDG_CONFIG_HOME/herdr/config.toml`.

## Router and socket lifecycle

Setup configures terminal identity immediately; the editor bridge connects
after `VimEnter` so the rest of `init.lua` can finish before its first RPC.
Setup called after startup schedules the connection on the next event-loop turn.

The shared router runs independently of the editor that started it. Every
integrated editor checks its health every five seconds and restarts it if
needed. `neordr router --background` also ensures it is running and returns when
ready. A per-editor bridge exits when its Neovim socket disappears.

SIGINT/SIGTERM removes owned sockets. The default router uses a process lock and
ownership marker to recover its own stale socket without replacing another
listening server. Named stale sockets from an ungraceful shutdown are not
overwritten; verify ownership before removing one. A real Herdr server cannot
share the same default socket with Neordr's router.

An editor at a modal “Press ENTER” prompt stays discoverable. Lists retain its
last normal snapshot without dismissing the prompt. Focus and other mutations
wait for normal Neovim input processing; input can still dismiss the prompt.

After upgrading the binary or Lua integration, restart each editor when its
files are saved and terminal jobs can be stopped, then reconnect remote views.
Replacing the executable or repeating `setup()` does not replace a running
bridge. Health checks also report an older shared router, which survives editor
restarts; they do not replace it automatically.

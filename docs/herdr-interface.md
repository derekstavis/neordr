# Herdr interface compatibility

Validated with unmodified macOS ARM64 Herdr clients:

- Stable `0.9.3`, SHA-256
  `5173a3e0ae42d5d1ab7ebfa5d5e6329f7c3d23f8e1a3677c7ce3231da2884157`.
- Previous `0.9.1-preview.2026-09-21-0ff0f27e2226`, SHA-256
  `203d7b29599a8bafaf94ba476912e7173cac2fb73d2988492ae9abe4e0fb44da`.
- Contracts and optional scroll codec come from upstream revision
  `5da0a01e1eedda054db0c81dd3a780000c40d9f0` (October 3, 2026).
  Upstream still specifies protocol **22**, endpoint generation **1**. Optional
  capabilities extend that protocol without changing its version number.

## Connecting

Run `neordr session list`, then invoke the downloaded Herdr binary with
`--session default`. Its sidebar groups separate Neovim editors as workspaces;
selection switches the remote UI connection between them. The session remains
available after its last workspace closes. Legacy per-editor bridge names still
support pinned attachment. Keep the `herdr -> neordr` symlink for Moshi.

Herdr may show its own first-run onboarding. That is client-local; Neordr does
not install Herdr agent integrations or replace the existing Moshi hooks. Herdr's
`onboarding = false` top-level configuration setting skips that screen.

## How it works

The session server and each internal bridge expose a mode-0600 Unix socket,
`herdr-client.sock`, next to the JSON `herdr.sock`. Crash recovery replaces only
unresponsive sockets marked as Neordr-owned, while holding an exclusive lock. The Herdr preflight
reads `capabilities.endpoint_protocol_generation = 1`. JSON ping replies and
session snapshots advertise the protocol version from the pinned Herdr schema
(currently 22), because Herdr also checks that version before ordinary CLI API
requests. Advertising zero blocked those commands with `protocol_mismatch` even
while the interactive interface worked. This version does not enable private
direct-terminal attach; interactive connections still negotiate endpoint codecs,
and unsupported operations still return errors.

The interface negotiates `endpoint.hello.v1` / `endpoint.welcome.v1` and the four
generation-1 core codecs. It receives `shell.snapshot.v1` JSON projections and
bincode-framed semantic cell surfaces. Neovim supplies those cells through its
own `nvim_ui_attach` connection using `ext_linegrid`; Neordr handles highlights,
cursor shapes, repeated cells, wide-cell continuations, scrolling, and resizing.
Herdr draws its sidebar, tabs, menus, and dialogs locally.

Input translation, the acknowledged Neovim connection, projection, grid rendering,
and transport framing have separate modules. The input connection owns request
IDs and its queue, verifies the live target before injection, retries unaccepted
bytes, and fences topology changes behind input consumption. Read-only requests
remain available at blocking prompts; pane mutations report `busy` so Enter can
still dismiss the prompt. Bounded chunks also wait for consumption, avoiding the
input-buffer exhaustion reproduced in this machine's Neovim 0.12.5
([upstream issue](https://github.com/neovim/neovim/issues/40577)). This uses no
editor-version guess and retains Neovim's normal key handling.

A displayed Herdr pane contains the entire Neovim editor. Selecting a different
terminal changes Neovim focus rather than creating isolated terminal-only views.
Semantic keys, text, paste and mouse input go to Neovim's input APIs. Herdr's
workspace/terminal operations use the existing Neordr backend. Advertised methods
are limited to supported workspace, tab, pane and agent discovery/focus operations
and the existing create/split/rename/reorder/close operations.

The complete API and wire schema comes from the pinned upstream Git submodule,
including operations Neordr does not implement. Dispatch uses typed requests and
responses; the endpoint framing module contains no independent message layouts
or numeric message-tag table. Unsupported operations still return errors. See
[contract generation](../build_support/README.md) for build and update details.

The socket reader handles fragmented frames with a 2 MiB frame limit and bounds
surface dimensions and concurrent clients. Accepted sockets explicitly return to
blocking mode: macOS inherits the nonblocking listener flag, which otherwise
disconnects an idle client with `EAGAIN`.

Each connection closes its own Neovim RPC channel on detach, connection loss,
or bridge shutdown. Neovim removes that UI and recalculates its minimum attached
size. Existing editors, jobs, and other remote UIs survive.

Before attaching, Neordr uses Neovim's fast Lua RPC to acknowledge a blocking
hit-enter message. This lets the initial UI attach complete after an error or
warning; confirmations and other input prompts are never answered automatically.

## Current scope

Named sessions and default-router aggregation are supported. SSH federation,
surface-interest negotiation, private direct-terminal attach, graphics blobs,
clipboard-image upload, Herdr plugin/integration management, agent resume,
Herdr scrollback overlays, and Herdr-specific pane groups are not implemented.
Unsupported operations return errors; unrecognized optional named controls are
ignored. Neovim manages scrollback, windows, and editor behavior. Agent activity
uses upstream screen/title detection for discovered Claude and Codex processes;
see [detection semantics](../build_support/README.md#agent-activity-detection).

Surface updates run at up to 60 Hz; snapshots are polled approximately every
300 ms. Clients negotiating `surface_scroll` receive changed-cell patches or
upstream `endpoint.surface-scroll.v1` messages, whichever is smaller. Initial
frames, topology/size changes, editor switches, and uneconomical patches use full
frames. Baselines are private to each connection. Older clients retain full-frame
delivery. The separate `surface_delta` and `surface_reuse` codecs are not advertised.

Moshi continues using its existing JSON API and incremental Neovim remote-UI
attachment; the native Herdr surface optimization does not alter that path.
JSON control mode additionally accepts upstream `terminal.mouse` messages for
clicks, releases, drags, and movement in zero-based editor cell coordinates.

## Validation

```sh
git submodule update --init vendor/herdr
cargo build --locked --bin neordr --example herdr_contract_fixture
cargo test --locked -- --test-threads=2
cargo clippy --all-targets -- -D warnings
HERDR_BIN="$HOME/Downloads/herdr-macos-aarch64" python3 tests/herdr_smoke.py
HERDR_BIN="$HOME/Downloads/herdr-macos-aarch64" \
  NEORDR_NVIM_CONFIG="$HOME/.config/nvim" python3 tests/herdr_smoke.py
```

The smoke test checks the actual Herdr CLI against both named-session and router
JSON sockets, then launches its interface in a PTY against a disposable Neovim.
It verifies editor rendering and editing, native new-tab dialogs and
switching, terminal input, resize, clean detach, and killed-client cleanup. An
independent Neovim UI stays connected throughout; both disconnect paths restore
its larger size and preserve the terminal processes. The full-configuration mode
uses a disposable copy of the user's tiny-term configuration.

Both configurations also pass exact raw-input comparisons for a 188,416-byte
Unicode `TextCommit`, a 102,400-byte bracketed paste, 20 adjacent focus/input
sequences, blocking-prompt reads and mutation rejection/recovery, and stale input
after a second UI changes focus. The native fixture encodes and decodes the
complete upstream contract types. The actual Herdr client tags text following a
switch shortcut in the same batch with the old pane; Neordr rejects that stale
input, and subsequent input with the current target works.

Rust tests cover fragmented/truncated/oversized frames, semantic key translation,
colored and wide cells, scrolling, cursor positions, and grid allocation bounds.

The [October 3 upstream audit](audits/2026-10-03-herdr-update.md) records the
source update, negotiated features, compatibility checks, and scaling results.

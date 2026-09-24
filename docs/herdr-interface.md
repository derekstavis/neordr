# Herdr interface compatibility

Validated with the unmodified macOS ARM64 binary supplied on September 22, 2026:

- Version: `0.9.1-preview.2026-09-21-0ff0f27e2226`.
- SHA-256: `203d7b29599a8bafaf94ba476912e7173cac2fb73d2988492ae9abe4e0fb44da`.
- Wire reference: Herdr's `src/protocol/wire.rs` and `endpoint.rs` at
  `28360107c1bfab57fb0de99d74bfa48e74423ca8`. This reference revision differs
  from the binary; compatibility is checked with the actual binary.

## Connecting

Run `neordr session list`, then invoke the downloaded Herdr binary with
`--session NAME`. The named Neordr bridge must have been restarted after installing
the endpoint-capable executable. This restarts only the bridge, not Neovim or its
terminal jobs. Keep the existing `herdr -> neordr` symlink for Moshi.

Herdr may show its own first-run onboarding. That is client-local; Neordr does
not install Herdr agent integrations or replace the existing Moshi hooks. Herdr's
`onboarding = false` top-level configuration setting skips that screen.

## How it works

Each bridge exposes a second mode-0600 Unix socket, `herdr-client.sock`, next to
the JSON `herdr.sock`. It never replaces an existing socket. The Herdr preflight
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

Named local sessions are supported. Default-router aggregation, SSH federation,
surface-interest negotiation, private direct-terminal attach, graphics blobs,
clipboard-image upload, Herdr plugin/integration management, Herdr scrollback
overlays, and Herdr-specific pane groups are not implemented. Unsupported
operations return errors; unrecognized optional named controls are ignored.
Neovim manages scrollback, windows, and editor behavior. Agent status retains
Neordr's existing `unknown` value. Full frames are sent at up to 60 Hz; snapshot
changes are polled at approximately 300 ms. Surface deltas are not advertised.

Moshi continues using its existing JSON API and Neovim remote-UI attachment.

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

# Development

## Build

```sh
git submodule update --init vendor/herdr
cargo build --locked --bin neordr --example herdr_contract_fixture
```

The Rust toolchain is pinned in `rust-toolchain.toml`. Neovim integration tests
use Neovim 0.12.5; Python smoke tests require Python 3 and the standard library.
The build extracts the complete Herdr schema from the pinned Git source without
building Herdr or its terminal engine. After fetching Cargo dependencies and
initializing the submodule, builds can run offline.

## Code map

| Location | Responsibility |
| --- | --- |
| `src/main.rs` | CLI and compatibility entry points. |
| `src/api.rs`, `src/protocol.rs` | Typed application requests and JSON transport boundaries. |
| `src/server.rs`, `src/router.rs` | Per-editor service and default cross-editor routing. |
| `src/backend.rs`, `src/process.rs` | Typed Neovim backend operations and foreground process discovery. |
| `src/attach.rs` | PTY supervision, remote UI attachment, and shortcut translation. |
| `src/endpoint/` | Herdr interactive protocol: framing, projection, input sequencing, UI ownership, and rendering. |
| `lua/neordr/` | Editor integration, terminal identity, lifecycle, and optional adapters. |
| `build_support/`, `vendor/herdr` | Reviewed upstream contract extraction and source pin. |
| `tests/`, `examples/herdr_contract_fixture.rs` | Disposable integration fixtures and typed native-client test driver. |

See the [design notes](design.md) for mapping decisions and
[contract source](../build_support/README.md) before changing upstream versions.

## Core checks

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked -- --test-threads=2
python3 tests/remote_ui_smoke.py
python3 tests/default_attach_smoke.py
python3 tests/gestures_smoke.py
python3 tests/router_lifecycle_smoke.py
NEORDR_SKIP_MOSHI=1 python3 tests/split_smoke.py
```

Tests need process-spawning, PTY, and local Unix socket permissions. A restricted
sandbox may deny these even when the implementation is correct. The Rust
integration suite starts real Neovim instances. Python tests default to
`target/debug/neordr`; set `NEORDR_BIN` to test another build.

Core checks cover typed contracts, stale identities, terminal management,
modal-prompt discovery, remote input, detach, router recovery, and workspace
selection. Inspect `.github/workflows/ci.yml` for the exact CI matrix.

## Optional client checks

Third-party client executables are not included in the repository or release
archives. These tests use disposable synthetic agents and transcripts; they do
not call an LLM or require real conversations.

### Moshi gateway

```sh
export MOSHI_HOOK_BIN=/path/to/moshi-hook
python3 tests/moshi_smoke.py
python3 tests/moshi_create_smoke.py
python3 tests/approval_smoke.py
python3 tests/split_smoke.py
```

These exercise the installed hook and gateway: transcript backlog and streaming,
multiline Chat View input, workspace/terminal creation, split/focus/close, launch
protection, and local question/plan/permission answers. The approval fixture
runs at 120 and 32 columns; `NEORDR_TEST_COLUMNS=32` selects just the narrow case.
Cloud delivery and physical phone interaction are separate manual checks. See
[approval compatibility](approval-compatibility.md) for known upstream limits.

For an isolated browser fixture:

```sh
NEORDR_BROWSER_READY_FILE=/tmp/neordr-browser.json python3 tests/moshi_smoke.py
```

The ready file contains the fixture URL and location. The fixture remains alive
for up to five minutes; delete the ready file to end it early. Its local proxy
routes gateway requests to the disposable daemon.

### Herdr interactive client

```sh
cargo build --locked --bin neordr --example herdr_contract_fixture
HERDR_BIN=/path/to/herdr python3 tests/herdr_smoke.py
```

The test drives the actual Herdr interface and the typed protocol fixture. It
covers editor rendering/editing, terminal creation and selection, large Unicode
input/paste, focus barriers, stale-target rejection, modal prompt dismissal,
resize, detach, and killed-client UI cleanup.

Herdr currently classifies text immediately after a switch shortcut in the same
input batch against its previous pane. Neordr rejects that stale text; fresh
input after the client receives the new state works. Do not replace rejection
with delivery to whichever terminal happens to be focused.

### Personal terminal adapters

`NEORDR_NVIM_CONFIG` modes in the gesture, split, and Herdr tests copy an existing
Neovim configuration into a disposable directory. They expect the project's
reference adapter conventions, including `lua/config/neordr.lua`; they are not
a generic compatibility test for arbitrary configurations. `tiny_term_smoke.py`
also expects the reference `config.terminal` and `config.terminal_tabs` modules.
Do not claim these modes pass for a new adapter until that adapter is exercised.

## Releases and website

Release artifacts are named
`neordr-vVERSION-TARGET.tar.gz` and accompanied by `SHA256SUMS`. The four targets
are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin`, and `aarch64-apple-darwin`.
Archives contain `bin/neordr` and `share/neordr/lua` plus documentation and notices.
The installed Lua runtimepath is `share/neordr`, not `share/neordr/lua`.

Review `.github/workflows/release.yml` for tag and publication rules. Validate
all four platform builds before treating a release as complete; a host build
does not establish another architecture's runtime compatibility.

The landing page lives in `site/` and deploys through GitHub Pages. Keep its
feature claims aligned with the README and link private downloads to GitHub's
authenticated release flow. A public Pages deployment must contain only the
static website, not repository source, configuration, or private artifacts.

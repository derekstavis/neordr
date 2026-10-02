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
| `src/control.rs`, `src/doctor.rs` | Typed control-mode CLI streaming and read-only integration diagnostics. |
| `src/attach.rs` | PTY supervision, remote UI attachment, and shortcut translation. |
| `src/endpoint/` | Herdr interactive protocol: framing, projection, input sequencing, UI ownership, and rendering. |
| `lua/neordr/` | Plugin setup, health checks, terminal identity, lifecycle, and optional adapters. |
| `doc/neordr.txt` | Neovim help for commands, setup options, and adapter contracts. |
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
selection. The attachment lifecycle test forces simultaneous PTY backpressure,
checks lossless forwarding and blocked-output cancellation, and verifies that
signals, terminal hangup, and parent death release a narrow UI while preserving
the editor and terminal jobs. Inspect `.github/workflows/ci.yml` for the CI checks.

## Performance measurements

Run the opt-in fixture with a release build and no competing build/test workload:

```sh
cargo build --release --locked
python3 tests/performance.py --bin target/release/neordr --editors 6 --samples 20 --output /tmp/neordr-performance.json
```

It creates disposable editors and terminals, times bridge/router RPCs, a CLI
command, four simultaneous snapshot requests, and named/default attachment
rendering. It records raw samples, median/p95, platform, and binary hash. The
attachment metric runs from process spawn completion until a known editor line
arrives; it excludes cleanup, network, SSH/Mosh setup, and client painting.
Timings are diagnostic, not machine-dependent CI thresholds. Compare the same
fixture and host conditions; the fixture never attaches to a real workspace.

The [October 1 audit](audits/2026-10-01-performance.md) documents the baseline and optimized results. Neordr does
not yet persist per-request latency metrics in production; Moshi's historical
HTTP timings are not a substitute for Neordr command measurements.

To separate actual Moshi gateway latency from Neordr RPC latency, install
`moshi-hook` and run the isolated fixture:

```sh
NEORDR_BIN=target/release/neordr python3 tests/moshi_performance.py --editors 6 --samples 10 --output /tmp/neordr-moshi-performance.json
```

It measures workspace/pane lookup, per-pane process metadata, focus, synthetic Claude/Codex transcript
backlogs, and a fresh control-mode terminal connection. Method-only traces show
router and bridge calls, including their start/end offsets within each sample;
nested durations overlap and must not be summed as wall time. It never reads
real conversations or attaches to a real workspace. These
are protocol timings, not Term/Chat click-to-paint timings. The
[view-switching audit](audits/2026-10-01-view-switching.md) records the distinction
and the installed desktop's switch behavior.
The [pane-discovery audit](audits/2026-10-01-pane-discovery.md) isolates repeated
process enumeration and Moshi's remaining agent-discovery work.

For terminal-size scaling, compare direct Neovim remote UI, Neordr control,
and both Moshi gateway modes on disposable editors:

```sh
NEORDR_BIN=target/release/neordr python3 tests/terminal_performance.py --output /tmp/neordr-terminal-performance.json
```

This measures input-to-ANSI arrival and output volume at 120×36, 240×72, and
480×144 cells. It excludes desktop painting and remote network latency, and
acknowledges WebSocket output immediately. Use `--modes gateway-whole` to select
only full-editor Moshi attachment. The
[large-terminal audit](audits/2026-10-02-large-terminal.md) explains the separate
native Herdr full-grid path and the measured incremental Moshi path.

## Plugin development

The source checkout is a standard Neovim plugin. Add its root to `runtimepath`
and call `require('neordr').setup({ bin = '/absolute/path/to/target/debug/neordr' })`
in a disposable configuration. Loading the plugin alone requires no Herdr
submodule or Rust toolchain; building its executable does.

Keep `:help neordr`, setup option validation, and `:checkhealth neordr` aligned
when changing the Lua API. Health checks must remain safe to run against an
editor with active jobs: inspection must not restart the bridge, router, editor,
or terminals. Test missing executables and incompatible versions as well as
successful setup, and check startup ordering before terminal creation.

Plugin and executable releases share tags. Update the plugin's supported binary
version when changing their interface, and package `doc/neordr.txt` alongside
`lua/` in `share/neordr`. Generate help tags during packaging or installation;
do not commit machine-generated `doc/tags`.

## Optional client checks

Third-party client executables are not included in the repository or release
archives. These tests use disposable synthetic agents and transcripts; they do
not call an LLM or require real conversations.

### Moshi gateway

```sh
export MOSHI_HOOK_BIN=/path/to/moshi-hook
bash scripts/check-moshi.sh
python3 tests/split_smoke.py
```

These exercise the installed hook and gateway: transcript backlog and streaming,
multiline Chat View input, workspace/terminal creation, split/focus/close, launch
protection, and local question/plan/permission answers. The approval fixture
runs at 120 and 32 columns; `NEORDR_TEST_COLUMNS=32` selects just the narrow case.
The interaction suite additionally checks long Unicode input, multipart pasted
blocks, literal attachment-path delivery, reverse-tab/page-down/interrupt keys,
and incomplete, mismatched and remaining asynchronous question answers. It
asserts exact input and isolation from the other fixture pane. File upload
storage, cloud delivery and physical phone interaction remain separate checks.
The control-mode fixture exercises Moshi's real WebSocket transport through
Neordr's JSON-lines CLI, alongside the existing ordinary PTY path. See
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

Regular pull-request and `main` CI runs on Linux x86-64. Release builds run
tests on Linux and macOS, each on x86-64 and AArch64 native runners.

Release artifacts are named
`neordr-vVERSION-TARGET.tar.gz` and accompanied by `SHA256SUMS`. The four targets
are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin`, and `aarch64-apple-darwin`.
Archives contain `bin/neordr` and `share/neordr/lua` plus documentation and notices.
New releases also include Neovim help under `share/neordr/doc`.
The installed Lua runtimepath is `share/neordr`, not `share/neordr/lua`.

Pushing a version tag automatically builds, tests, and publishes all four
platform archives and `SHA256SUMS` through `.github/workflows/release.yml`.
Update the version in `Cargo.toml`, `Cargo.lock`, and `lua/neordr/version.lua`
together, commit to `main`, and push before tagging that commit:

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

The tag must match the package/plugin version and point to a commit on `main`.
Publication waits for all four builds and checksum verification. Assets are
uploaded to a draft, then the complete release is published automatically.
Tags such as `vX.Y.Z-rc.1` publish as prereleases, without becoming Latest.
Failed builds do not publish a release. Rerun failed jobs or use the workflow's
manual tag input to retry; published assets are never overwritten.

The landing page lives in `site/` and deploys through GitHub Pages. Keep its
feature claims aligned with the README and link private downloads to GitHub's
authenticated release flow. A public Pages deployment must contain only the
static website, not repository source, configuration, or private artifacts.

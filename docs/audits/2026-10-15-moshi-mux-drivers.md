# Moshi 0.4.30 mux-driver analysis

## Conclusion

The driver layer announced in [Moshi 0.4.28](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.28)
provides an internal extension point for terminal detection, capture and input.
The installed 0.4.30 binary registers TUIOS and Tern drivers. It does not expose
an external driver installation mechanism that we could identify.

This makes a native Neordr integration more plausible upstream. It does not
currently let Neordr supply a plugin to the installed hook, nor does implementing
the driver contract alone provide the workspace browser that we use through
Herdr. Keep the Herdr-compatible interface until native integration covers both
Chat and Term, workspace navigation and lifecycle operations.

No live services, editor sessions, agent settings or transcripts were changed.
This audit adds documentation only.

## Scope and evidence

Compared the installed Darwin arm64 binary with the official pre-announcement
0.4.27 archive. Both contain Go 1.26.6 build metadata. Although stripped, their
Go function/line tables and runtime interface tables remain readable. These
reveal names, source locations and compiled behavior, not the original source.

| Item | Identity |
| --- | --- |
| Installed binary | `/opt/homebrew/Cellar/moshi-hook/0.4.30/bin/moshi-hook` |
| Module | `github.com/rjyo/moshi/app-hook` |
| Current commit | `4673bfcbce720415702b913b8cd1a8f523859a8f` |
| Current SHA-256 | `6f0efb17c829bd9aa99c9c0beff126e0055cc9750b442615806e59c201e9caeb` |
| Previous binary SHA-256 | `af8fac4e4dd57d5f46f4595005d886c0bd1be75da616e79ebb99243c1137d7ff` |
| Previous archive SHA-256 | `9ccc55f97ce4c4feb5df0442be064fdec2970bbe4852179f9ca84ccb3da4a20e` |

The previous archive came from
`https://cdn.getmoshi.app/hook/v0.4.27/moshi-hook-app_Darwin_arm64.tar.gz`.
Its checksum matches the formula stored at the public tap's `v0.4.28` tag;
that tagged formula still specifies 0.4.27. Version identity was checked in the
extracted binary rather than inferred from the tap tag.

Recovered 3,708 application functions in 0.4.27 and 3,930 in 0.4.30, including
299 added and 77 removed names. These are symbol differences, not a count of
features or proof that every renamed/inlined function changed behavior.

## The driver contract

The `internal/mux.Driver` interface has seven methods. Their membership is
confirmed by the two concrete driver interface tables, not just string matches.
Descriptions below summarize roles; they are not reconstructed Go signatures.

| Method | Responsibility | Existing Neordr building block |
| --- | --- | --- |
| `Kind` | Identify the multiplexer | Native `neordr` identifier would be new |
| `Detect` | Derive mux/pane location from environment | Terminal environment prepared by the Lua backend |
| `EnvKeys` | Identify environment keys to preserve/propagate | Currently exported `HERDR_*` identity |
| `Exists` | Check that a target pane is still available | Pane listing and stable identities |
| `Capture` | Read terminal text in requested capture mode | `pane.read` and supported capture modes |
| `SendKeys` | Deliver key events | `pane.send_keys` |
| `SendText` | Deliver text | `pane.send_text` |

Additional capability interfaces appear in runtime type metadata:

- `PaneLister`: pane enumeration (`ListPanes` on both drivers).
- `HealthChecker`: health reporting (`Health` on both).
- `PaneComparer`: pane identity comparison (`SamePane` on Tern).
- `Attacher`: terminal executable and attachment arguments (`Binary` and
  `AttachArgs` on TUIOS).
- `ClientMatcher`: associate a client process with its mux location
  (`MatchClient` on TUIOS).

The shared types include `Location`, `PaneRef`, `Pane`, `Health`, `CaptureMode`,
`Key`, `KeyStep` and `KeyNames`. A registry supports registration with priorities,
lookup, enumeration, environment keys, detection ranges and pane comparison.

`drivers.New` makes exactly two registrations: TUIOS at priority 200, then Tern
at 500. A scan of direct ARM64 calls to `Registry.Register` found only those two
sites. No loadable-driver option was found in the CLI/settings or inspected
registration path. This is evidence for compiled-in integration, not a claim
that every possible private extension facility has been disproven.

The old Herdr and tmux paths remain alongside this registry. Herdr is not one
of the two `Driver` implementations registered by this constructor.

## Where the abstraction is used

- Generic terminal and agent context resolution from environment, saved state,
  PIDs and client processes (`gateway/mux_drivers.go`).
- A terminal adapter for ordinary, visible and recent capture, keys, text and
  existence checks (`tui/driver.go`). This connects drivers to agent TUI actions.
- Environment propagation in daemon/install code.
- Host proxy attachment through the optional attacher capability.
- Doctor checks for registered drivers.

Terminal capture is not the chat transcript. The generic agent-context path
still calls `findAgentSessionFromStateWithRecency`; the driver locates and
controls the pane while Moshi's agent/session machinery supplies chat state.
A native driver would therefore still need reliable pane-to-agent-session
association, including old sessions and wrapped agent processes.

## Runtime boundary: context support is not workspace support

An isolated gateway, router and Neovim were started with temporary configuration
and state. They were stopped by the fixture afterward. No model calls were made.

| Probe | Result in 0.4.30 |
| --- | --- |
| `GET /v1/muxes` | 200; disposable Neordr router advertised as `herdr:default` |
| `GET /v1/workspaces?mux=herdr:default` | 200; disposable editor workspace and pane |
| `GET /v1/workspaces?mux=neordr` | 400, `invalid mux target "neordr"` |
| Same request with `tern`, `tuios`, or `zellij` | 400, corresponding invalid mux target |
| `moshi-hook context` with Tern identity environment | `kind: "tern"`, expected window and pane |
| `moshi-hook context` with TUIOS identity environment | `kind: "tuios"`, expected session and pane |

The context probes used deliberately nonexistent sockets. They prove environment
recognition, not successful capture, attachment or a working Tern/TUIOS server.
Tern uses `TERN_PANE`, `TERN_WINDOW_KEY` and `TERN_PANE_SOCKET`. TUIOS uses
`TUIOS_ENV=1`, `TUIOS_PANE_ID` (falling back to `TUIOS_WINDOW_ID`),
`TUIOS_SESSION` and `TUIOS_SOCKET`.

Disassembly independently shows `gateway.parseMuxTarget` still recognizing tmux
and Herdr targets, including named variants. Thus the generic driver contract
is not yet the complete workspace navigation/lifecycle contract. The rejected
workspace query does not imply that all other APIs reject those muxes.

The existing integration test also passed with the installed 0.4.30 hook:

```sh
NEORDR_BIN=target/release/neordr python3 tests/moshi_legacy_sessions_smoke.py
```

It verifies Claude and Codex discovery, subsequent hook events, transcript
backlog and append streaming, prompt delivery and isolation from the other
pane, while retaining legacy `herdrSession` metadata. Agents and transcripts
are synthetic. This does not validate native Mac/mobile UI rendering or every
new agent action.

## Implications for Neordr

1. Keep the current Herdr compatibility layer. No new mandatory server message
   was established by this investigation, and the existing Chat contract passes.
2. A native upstream driver could be a small Go adapter to Neordr's existing
   Rust CLI/socket APIs. Neordr does not need to be rewritten in Go. Explicit
   native environment identity and attachment selection would need agreement
   with Moshi; changing our environment alone would not enable support.
3. Require workspace listing, pane focus, creation/closure and remote attachment
   alongside the core driver methods before replacing Herdr mode. Native-client
   mux selection/capability handling may need changes too; those clients were
   not analyzed in this audit.
4. Preserve the existing session association and lifecycle safeguards. A driver
   abstraction does not itself fix shared-backend agent ownership, ended
   transcript records, or Neovim's minimum attached-UI dimensions.

The concrete upstream integration question is whether Moshi intends to expose
an external executable/RPC driver contract, or accept a compiled-in Neordr
adapter and extend the workspace API. The latter is the extension seam visible
in this binary. No upstream message or submission was sent.

## Other relevant changes

[0.4.30 release notes](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.30)
describe improved Herdr discovery for agents launched through wrappers.
The new `herdrPaneFacts.confirmWrapped` function calls the existing session-state
lookup, consistent with that change. Wrapped-agent behavior deserves a focused
fixture before removing any Neordr discovery logic; this audit did not test it.

[0.4.28 release notes](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.28)
also introduce Claude rewind and suggested-prompt APIs, worktree transcript
continuity, Herdr input-clearing fixes and reduced background polling on macOS.
New rewind functions and `claudePromptSuggestionStamp` are present in the symbol
comparison. Rewind and prompt suggestions are useful next compatibility tests;
their end-to-end behavior was not exercised here. No performance improvement is
claimed from binary inspection alone.

## Static evidence anchors

These virtual addresses apply only to the identified 0.4.30 binary:

| Function/data | Address | Embedded source location |
| --- | --- | --- |
| `mux.(*Registry).Register` | `0x100145470` | `internal/mux/registry.go:39` |
| `mux/drivers.New` | `0x1001881e0` | `internal/mux/drivers/drivers.go:17` |
| TUIOS `Driver` interface table | `0x101467190` | Seven method pointers after 24-byte header |
| Tern `Driver` interface table | `0x1014671e0` | Seven method pointers after 24-byte header |
| `gateway.driverAgentContext` | `0x1003d47c0` | `internal/gateway/mux_drivers.go:92` |
| `gateway.parseMuxTarget` | `0x1003d5be0` | `internal/gateway/muxes.go:39` |
| `gateway.listLoopbackMuxes` | `0x1003d6900` | `internal/gateway/muxes.go:128` |
| `hostproxy.driverAttacher` | `0x10035b3b0` | `internal/hostproxy/pty_drivers.go:14` |
| `gateway.herdrPaneFacts.confirmWrapped` | `0x1004490d0` | `internal/gateway/workspaces.go:1243` |

Reproduction tools: `go version -m`, `shasum -a 256`, Go `debug/macho` plus
`debug/gosym` over `__gopclntab`/`__text`, and `objdump -d` with the recovered
function boundaries. Temporary analysis material is in
`/private/tmp/moshi-0430-audit` on the audit machine; it is not a durable artifact.

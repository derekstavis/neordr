# Moshi 0.4.9: opportunities for Neordr

Analysis only, 2026-09-29. Checked the publisher's releases API: 0.4.9 is the
latest published version and matches the installed binary. Compared releases
0.3.27–0.4.9 with the earlier 0.3.26 audit and current Neordr implementation.
No production code, agent configuration, or service setting was changed.

## Recommended opportunities

| Opportunity | Verified gap | Recommendation |
| --- | --- | --- |
| Herdr control-mode transport | Moshi invokes a CLI subcommand Neordr does not implement; isolated connection closes without a terminal frame | Add compatibility as an explicit transport option, preserving the full-editor product decision |
| Agent discovery and launch registry | Foreground detection and both launch checks allow only Claude/Codex | Add agents selectively, beginning with ones actually used; test discovery, launch, input, and exit together |
| Integrated diagnostics | Neordr health checks stop at bridge/router; Moshi doctor reports no Herdr server despite working Neordr workspaces | Add Neordr-aware CLI/health diagnostics for the complete Moshi-to-editor path |
| Question/approval contract coverage | Existing smoke test exercises generic prompts and PTY input, not these richer workflows | Add synthetic compatibility fixtures for the Codex/Claude workflows before claiming coverage |

## 1. Herdr control mode is the concrete new protocol gap

[0.4.4](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.4) introduced
Herdr control mode. This is separate from hiding Herdr's sidebar and separate
from the full Herdr interactive-client endpoint already supported by Neordr.

Binary analysis of `gateway.herdrControlArgs` (0x10055c460) recovers:

```text
herdr [--session NAME] terminal session control TARGET --takeover --cols C --rows R
```

`gateway.handlePTY` dispatches a Herdr GET with a nonempty `pane` parameter to
`handleHerdrControl`. Moshi converts WebSocket input, resize, and scroll messages
into JSON lines for the subprocess. It parses output records and base64-decodes
terminal frames. The pinned Herdr source confirms the contract in
`vendor/herdr/src/client/terminal_sessions.rs`:

- Input: `terminal.input` (text or base64 bytes), `terminal.resize`,
  `terminal.scroll`, `terminal.release`.
- Output: `terminal.frame` with sequence, ANSI encoding, dimensions, full-frame
  flag and base64 bytes; `terminal.closed` with a reason.
- Herdr's own implementation maps these to its binary terminal control protocol.

Neordr's CLI has no `terminal` command, and its endpoint does not handle
`ControlTerminal`/`ObserveTerminal`. Implementing only a CLI spelling alias would
therefore be insufficient.

Live-safe verification used isolated Neovim, Neordr, Moshi, and synthetic agents.
A request to `/v1/pty?mux=herdr&muxSession=smoke&pane=<fixture-pane>` upgraded to
WebSocket, then closed with no terminal frame. Moshi logged the control process
starting and immediately releasing. Directly invoking the recovered command
against Neordr returns `unrecognized subcommand 'terminal'`.

### Fit with Neordr's editor model

Native Herdr control mode exposes one terminal pane. The user explicitly prefers
the entire Neovim editor and sidebar selection that focuses a terminal buffer.
A Neordr adapter should preserve that requirement: resolve the requested target
inside its editor, focus that buffer, and transport the editor UI. Document that
this differs from Herdr's isolated terminal-pane surface. Keep the existing
attachment path available.

This does not provide independent editor layouts or eliminate Neovim's
smallest-attached-UI sizing behavior. Resize, mouse/scroll targeting, takeover,
EOF, disconnect, backpressure, and reconnect all need explicit tests. In
particular, implement release as remote-UI detachment, never editor shutdown.

Reuse the pinned upstream types through the existing contract-extraction approach
where available; do not introduce a separately maintained loose JSON schema.
The CLI command types currently live outside the extracted contract modules, so
that extraction boundary needs extending if they are reused.

## 2. Broader agents expose an existing Neordr restriction

Moshi's later releases add Chat/inbox support across Qoder, Devin, Copilot, Amp,
Factory Droid, Goose, OmO and jcode. Coverage and supported interactions differ
by agent; release notes must not be interpreted as identical full support.

Neordr currently restricts:

- `src/process.rs::agent`: Claude/Codex foreground identification.
- `src/router.rs`: launch-command allowance for session-less input.
- `lua/neordr/backend.lua`: the matching untouched-shell launch check.

Consequently, transcript parsing in Moshi alone does not establish usable
Neordr support. The router can reject input when it does not recognize the
foreground agent. Merely broadening the launch string check would be unsafe and
incomplete; preserve foreground ownership, exact pane routing, and the short
untouched-shell launch allowance.

Define a single maintained agent registry and derive/share the required Rust and
Lua recognition/launch data from it. Validate executable aliases and script
wrappers explicitly. Do not allow arbitrary shell strings under the guise of an
agent name.

The read-only local doctor report found installed OpenCode and Copilot hooks out
of date. That is useful context for choosing the first additional agents, not a
request or authorization to reinstall their hooks. Agent transcript/status
parsing should continue to belong to Moshi.

## 3. Diagnostics should cover the complete path

`moshi-hook doctor --json` reports both workspace/session features as OK, but its
Herdr check says `neordr 0.1.0; no herdr server running`. Its own process-based
server recognition is not a reliable health check for this compatibility layer.
Neordr's current `:checkhealth neordr` validates configuration, Neovim version,
bridge, router, and version consistency; it does not compare Moshi discovery
with the active editor/pane.

A useful addition is `neordr doctor --json`, also surfaced by Lua health checks:

- Bridge/router identity and responsiveness for each editor.
- Attached remote UIs and their sizes, with ownership facts where available.
- Moshi version, gateway availability, workspace/pane enumeration and association.
- Agent detected but conversation missing, distinguished from unreadable transcript.
- Actionable differences between live discovery and saved hook records.

Avoid restarting services automatically or inventing a thread association. The
recent stale-ended-conversation incident is exactly the kind of discrepancy
this command should explain. It remains a Moshi defect, not a feature to emulate.

## 4. New workflows worth validating, not reimplementing

Moshi has expanded asynchronous questions and approvals, resolved-approval
cleanup, attachment submission, and large-paste handling. These largely use the
existing capture/text/key adapters or agent-specific hook/service paths.
The earlier full 0.4.9 smoke test passed generic Claude/Codex discovery,
transcript streaming, input and PTY lifecycle. It does not prove every menu,
question, plan or approval path.

Add disposable regressions for partial multi-question forms, reverse-tab/page
navigation, plan menus, interrupted turns, multipart/long prompts and attachment
submission where the hook actually uses terminal input. Check the emitted
sequence and correct target, not merely HTTP success. Preserve current bounded
`recent_unwrapped` behavior as an approximation; Neovim does not provide the
soft-wrap boundaries needed for exact reconstruction in the current adapter.

## Features already owned by Moshi

Usage events, status-age timestamps, Git freshness, file/media previews, update
policy, web sidebar sorting and per-agent installation are implemented in Moshi.
They do not justify copying that logic into Neordr. Its auto-update setting also
does not update Neordr. No settings were changed during this review.

Authoritative release references:

- [0.4.1: status timestamps](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.1)
- [0.4.3: diagnostics and Codex remediation](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.3)
- [0.4.4: Herdr control mode](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.4)
- [0.4.7: agents and upload improvements](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.7)
- [0.4.8: Droid and sidebar improvements](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.8)
- [0.4.9: updates, agents and questions](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.9)

Temporary supporting evidence is in `/private/tmp/moshi-inspection-049/`,
`/private/tmp/neordr-control-probe.py`, `/private/tmp/moshi-release-list.json`, and
`/private/tmp/neordr-moshi-049-doctor.json`. These files are not durable test
artifacts. This audit does not claim an exhaustive refreshed inventory of every
Herdr RPC or runtime validation of each newly supported agent.

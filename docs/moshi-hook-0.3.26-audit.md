# Moshi-hook 0.3.26 compatibility audit

Inspected 2026-09-22. This is an audit, not an implementation change.

**Implementation follow-up (2026-09-22):** Neordr now implements the five
close/move methods identified below, the missing key names, and structured
unsupported-method error propagation. It accepts `recent_unwrapped` with bounded
recent rows; exact unwrapping remains unavailable because Neovim does not expose
soft-wrap boundaries. The tables below preserve the findings at inspection time;
see the README for the current contract. `pane.split` was subsequently added as
adjacent Neovim terminal creation, preserving one virtual tab per terminal as
requested. Legacy directional-focus RPCs remain unimplemented.

## Scope and evidence

Compared the installed Darwin ARM64 `moshi-hook` with preserved function maps,
disassembly, and extracted assets from 0.3.21. The old executable itself is no
longer installed. Used Go build metadata and `__gopclntab`, traced ARM64 call
sites and string lengths, inspected Neordr's handlers, and exercised the current
hook against a disposable fake Herdr socket. No live editor, terminal job,
transcript, or production daemon was changed by the audit.

| Item | Value |
| --- | --- |
| Hook version | 0.3.26 |
| Build commit | `60fb3db08226b94d9aa3ac214ee20e161f699c49` |
| Build timestamp | `2026-09-17T16:55:35Z` |
| Compiler | Go 1.26.6 |
| SHA-256 | `cf017621ef100f32e914ca1fdaaca22fe45db19b500600995aa0ee07429c0f7d` |
| Previous inspected hook | 0.3.21, commit `c29526a031219af7e4bd059c8253b82fdc4fdfd8` |
| Recovered function entries | 17,351 total; 2,672 containing the application module, including generated wrappers |
| Installed native app | Moshi 0.2.11; distinct from the hook version |

This does not recover the exact original Go source. File names and source line
numbers below are compiler metadata. Native-app version was verified, but no
previous native executable was available for a binary delta comparison.

## Herdr message inventory

Recovered 28 distinct Herdr RPC method names, including dynamically constructed
close/rename names. Neordr handles 20 of them, with parameter/behavior gaps noted
below; eight are absent. This is the recovered consumer surface, not a claim to
implement every option in Herdr's public API.

| Methods | Neordr | Consumer purpose |
| --- | --- | --- |
| `ping` | Implemented | Probe the mux endpoint |
| `session.snapshot` | Implemented | Combined workspace/tab/pane/agent enumeration |
| `workspace.list`, `tab.list`, `pane.list`, `agent.list` | Implemented | Labels, context, discovery, and legacy listing |
| `pane.get`, `pane.process_info` | Implemented | Target validation and foreground process identity |
| `pane.read` | Partial | Visible screen/status/menu capture; missing `recent_unwrapped` |
| `pane.send_text` | Implemented | Prompts, pasted text, agent launch commands |
| `pane.send_keys` | Partial | TUI interaction; missing `BTab` and `PageDn` names |
| `workspace.focus`, `tab.focus`, `pane.focus`, `agent.focus` | Implemented | Sidebar and terminal selection |
| `workspace.create`, `tab.create` | Implemented | New workspaces and terminals |
| `workspace.rename`, `tab.rename`, `pane.rename` | Implemented | User labels |
| `workspace.close`, `tab.close`, `pane.close` | Missing | Close workspace/terminal/pane actions |
| `workspace.move`, `tab.move` | Missing | Reorder using `insert_index` |
| `pane.split` | Missing | Create a neighboring terminal pane |
| `pane.neighbor`, `pane.focus_direction` | Missing | Legacy shell-pane focus navigation |

Neordr also implements `pane.layout`. I did not find a direct call to that method
in the inspected hook. Despite its name, Moshi's `herdrPaneLayoutFor` helper
actually sends `pane.neighbor`.

The absent eight methods already existed in the 0.3.21 consumer. The newly
observed RPC method names since that version are `session.snapshot` and
`pane.focus`, both now implemented by Neordr. In 0.3.26, neighbor/directional
focus is a fallback after direct `pane.focus` is rejected, so it has lower
priority for our current direct-focus implementation.

## Important gaps inside implemented methods

### Recent unwrapped screen text

`gateway.applyHerdrPlanMenu.func1` sends `pane.read` with
`source: "recent_unwrapped"`, `format: "text"`, and `lines: 80`.
This was present in both inspected hook versions. Neordr accepts only `visible`,
`detection`, and `recent`, so this plan-menu detection path fails. Plain Chat View
working does not establish that plan approval works.

Implementing the source needs correct soft-wrap behavior, not just accepting a
new name for existing physical buffer rows. Evidence: preferred address
`0x100553d88`, `gateway/plan_menu.go:301`; the following instructions load the
16-byte source name and the limit 80. Neordr rejection is in
`lua/neordr/backend.lua`, the `pane.read` branch.

### Named keys

Moshi's allowed key table contains `BTab` and `PageDn`. `SendNamedKeys` passes
accepted key names through `sendQuestionKeys` to `HerdrAdapter.SendKeys`, which
serializes them unchanged. Neordr's `src/backend.rs::keys` lowercases names but
accepts neither `btab` nor `pagedn`; it accepts `pagedown` instead. These are
concrete compatibility gaps for reverse-tab and page-down input, independent of
the remote PTY gesture decoder.

Evidence: `tui.map.init.0`, `tui/keys.go:17`, string-reference addresses
`0x1002b6fa0` and `0x1002b70c0`; adapter at `tui/herdr.go:23`.

### Unsupported-method errors and router propagation

Moshi's `herdrapi.Call` recognizes Herdr error code `invalid_request` as the
unsupported-API condition used by its fallback paths. Neordr's bridge returns
`unsupported_method`; its default router can also flatten backend failures into
`routing_failed`. Neither enables Moshi's legacy fallback.

The isolated probe demonstrated:

| Response to `session.snapshot` | Moshi result |
| --- | --- |
| Error code `unsupported_method` | HTTP 500; no legacy list requests |
| Error code `invalid_request` | HTTP 200; requests `workspace.list`, `tab.list`, `pane.list`, `agent.list` |
| Valid snapshot | HTTP 200; advertises `paneFocus: "exact"` |

The legacy response advertised `paneFocus: "agent-only"`. Returning the snapshot
therefore affects the capabilities shown to clients as well as performance.
The snapshot implementation already fixes current enumeration. Correct error
codes and preserving structured errors through the router would make future
compatibility fallbacks work. This does not mean every operational error should
be relabeled `invalid_request`.

### Broader agent support

Neordr recognizes Claude and Codex in `src/process.rs`. Moshi recognizes more
agents, and 0.3.26 adds substantial Antigravity/OpenCode functionality. Its process
inspection may recognize those commands from our `pane.process_info`, but the
default input router requires Neordr's supported foreground-agent marker and its
new-terminal launch allowance accepts only Claude/Codex. Other agents therefore
need explicit discovery, launch, and input tests before claiming support.

Neordr still reports agent status as `unknown`. Moshi can supplement status from
its own hooks/state; implementing transcript parsers in Neordr is unnecessary.

## Requests captured from the real hook

The test used a temporary HOME/config/state directory and a fake socket, with
discovery and usage collection disabled. These are actual requests emitted by
0.3.26; close and split did not operate on real terminals.

```json
{"method":"workspace.close","params":{"workspace_id":"probe:w1"}}
{"method":"tab.close","params":{"tab_id":"probe:w1:t1"}}
{"method":"pane.close","params":{"pane_id":"probe:w1:p1"}}
{"method":"workspace.move","params":{"workspace_id":"probe:w1","insert_index":1}}
{"method":"tab.move","params":{"tab_id":"probe:w1:t1","insert_index":1}}
{"method":"pane.split","params":{"target_pane_id":"probe:w1:p1","direction":"right","focus":true}}
{"method":"pane.focus","params":{"pane_id":"probe:w1:p1"}}
{"method":"pane.neighbor","params":{"pane_id":"probe:w1:p1","direction":"up"}}
```

The probe observed `pane.neighbor` only when `pane.focus` deliberately returned
`invalid_request`. `pane.focus_direction` is additionally verified in the legacy
focus function's disassembly; that subsequent mutation was not exercised by the
fake neighbor response. The default router currently does not route by
`target_pane_id`, so split support would also need that routing field.

## Changes since the previous inspection

| Release | Changes relevant here |
| --- | --- |
| [0.3.22](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.3.22) | Better Pi discovery/working status; suppress Claude events originating from Cursor compatibility callbacks; bundled web app 0.2.5. |
| [0.3.23](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.3.23) | Transcript reconnect/resume; single-pass Herdr enumeration; bundled web app 0.2.8. |
| [0.3.24](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.3.24) | Reliable exact Herdr pane selection and compatibility fallback for pre-0.9.0 Herdr. |
| [0.3.25](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.3.25) | Report Git read errors in the diff viewer; retain OpenCode detection when tmux titles contain a pipe. |
| [0.3.26](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.3.26) | Antigravity Chat View; OpenCode 2 support (`--standalone`), conversation reset and interrupt fixes; web support for asynchronous Codex questions; Korean input, Cmd+J pane selection, image-viewer, saved-host removal, sidebar-scroll, and reconnect-feedback fixes. |

The release notes above were read from the publisher's GitHub releases API.
Separate binary evidence confirms these architectural changes:

- **Antigravity transcript reader:** new `internal/antigravity` functions locate
  conversation databases and logs, read the SQLite `steps` table, decode
  protobuf payloads, and handle media. `streamAntigravityTranscript` serves the
  result. `antigravitySessionForHerdrPane` uses `pane.process_info` for identity;
  no new Antigravity-specific Herdr RPC is required. There is also a new
  interruption watcher.
- **Resumable transcripts:** new `gateway/transcript_resume.go` functions encode
  cursors, track transcript boundaries, select backlog versus resumed opening,
  and send committed appends. File, Cursor, Hermes, and OpenCode streamers call
  the new machinery. This operates between Moshi and its clients, outside the
  Neordr protocol.
- **Direct focus and combined state:** new `herdrSessionSnapshot`,
  `herdrLegacySnapshot`, `focusHerdrExactPane`, and `focusHerdrPaneLegacy` call
  paths, dynamically checked by the probe above.
- **Codex discovery reuse:** `withCodexProcessRollouts` stores a lazy
  `sync.OnceValue` result in a context used by workspace enumeration. It avoids
  repeating the same process/rollout discovery within that context. This is an
  inference from the verified call graph, not a measured speedup.
- **OpenCode interruption:** `confirmOpenCodeInterrupt` recognizes the screen's
  second-Escape prompt and sends Escape through the existing TUI adapter.

The installed native app's version alone does not prove support for every new
agent feature. The publisher explicitly says Antigravity requires matching
desktop/mobile support in addition to the hook; the bundled web client includes
it.

## Suggested implementation order

1. Complete existing contracts: unwrapped recent capture, `BTab`/`PageDn`, and
   unsupported-method error propagation. Add focused contract regressions.
2. Implement close operations with terminal-plugin ownership and job lifecycle
   preserved, then workspace/tab ordering.
3. Design real split-pane modeling. Currently each terminal buffer is one
   virtual Herdr tab with one pane; displaying a Neovim split alone would not
   establish Herdr's multiple-panes-per-tab contract.
4. Extend agent discovery/launch/input support for agents actually used. Let
   Moshi continue owning transcript parsing and remote approval coordination.

No Neordr code or installed executable was changed during this audit.

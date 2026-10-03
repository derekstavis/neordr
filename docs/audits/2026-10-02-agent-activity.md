# Agent activity in Moshi

## Diagnosis

Live Neordr `agent.list` returned nine detected agents, all with
`agent_status: unknown` and `state_change_seq: 0`. The running Moshi 0.4.9
gateway's `/v1/workspaces?mux=herdr:default` passed `unknown` through and exposed
no useful transition order. The installed Moshi Desktop 0.4.13 binary groups
statuses other than working, blocked, and done under Idle. Its flattened-row
sorting uses `stateChangeOrder`, falling back to activity time.

This was a Neordr feature gap: Lua snapshots hardcoded unknown and Rust agent
projection hardcoded zero, even though foreground process identity worked.

## Change

The build derives Herdr's screen/title detector and its bundled manifests from
the existing pinned submodule. No upstream files are modified. Detection covers
the agents recognized by both Neordr's foreground discovery and the upstream
manifests. Remote manifest fetching and Herdr configuration overrides are not
included.

The editor owns per-pane state and transition order, keyed by generation-bound
pane IDs and detected foreground agent process identity. Router and bridge reads
share this state. Metadata commits reject superseded observations and stale
snapshot revision/title. Stable observations don't perform another update RPC
or advance the sequence. The sequence uses Unix microseconds plus a monotonic
guard within each editor, so comparisons work across local workspaces.

Working/blocked to idle becomes Done when the terminal buffer is not focused;
focusing it acknowledges completion. Initial idle discovery is not a completion.
Unmatched screens/history viewers retain prior known state. An agent process
change resets its completion state; closed panes are discarded. Panes, agents,
tabs and workspaces all project the detected status.

The detector reads a bounded recent terminal screen and the existing terminal
title; it doesn't parse chat files or install agent hooks. Detection is driven
by snapshot/list requests, not continuous monitoring: turns entirely between
observations may be missed. The completion acknowledgement reflects editor
buffer focus, not proof that a human looked at the result.

## Validation

- Required `scripts/check.sh`: Rust formatting, clippy, all Rust tests, plugin,
  attachment/input/modal recovery, workspace and routing smoke tests passed.
- `tests/activity_smoke.py` is included in CI and checks real disposable Neovim
  terminals, both Codex and Claude, transitions, stable reads, cross-agent order,
  tab/workspace aggregation, stale rejection, bridge restart, focus and exit.
- Final release `tests/activity_smoke.py --moshi` passed against the installed
  gateway, including Working/Done status and `stateChangeOrder` propagation.
- Unit tests exercise upstream title/screen detection and history/unmatched
  screen handling. Integration coverage verifies history doesn't complete a
  working turn.

No real editor was used for input or synthetic-output tests.

## Local activation

Installed release SHA-256:
`c1214bc60bef528ce06b4f82cc95f64f8c33e61dc86fa59042f023c23b8d13e3`.
Refreshed ten bridges and the router; before/after inventories verified all ten
editor identities and their terminal-job identities unchanged. The live Neordr
workspace reported Working in both `agent.list` and Moshi's workspace tree, with
the same nonzero transition sequence. The other eight detected agents reported
Idle rather than Unknown. Historical activity before activation cannot be
reconstructed by this screen detector.

A sequential six-editor/eight-sample release comparison showed no material
regression: aggregate snapshot median 56.24 -> 50.19 ms, workspace list
54.58 -> 49.49 ms, pane list 55.09 -> 49.75 ms, named attachment 12.92 -> 13.11 ms,
default attachment 13.02 -> 13.34 ms. These idle-shell fixtures do not measure
agent-rule matching overhead or establish a speedup; ambient load was not
controlled. Raw results are `/private/tmp/neordr-activity-perf-{before,after}.json`.

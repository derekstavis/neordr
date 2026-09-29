# Moshi 0.4.9: resumed Codex Chat discovery

Investigated 2026-09-29 against the installed macOS arm64 moshi-hook 0.4.9,
Codex 0.158.0, and the current Neordr checkout. This combines binary analysis,
a disposable reproduction, and live verification; it is not recovered original
Moshi source code.

## Finding

Moshi can permanently suppress a resumed Codex conversation on a pane after
seeing that conversation's exit/resume instruction on screen. The suppression
is cached inside the Moshi process. Clearing the screen and resuming the same
conversation do not clear it.

This was a second problem after the shared-backend issue. The previously
approved shared-backend mitigation completed successfully: auto-start is disabled,
and the managed backend and its updater stopped. The resumed standalone Codex
process owns its transcript, and the saved Moshi hook record identifies the
correct pane. No additional Codex or Claude configuration change was needed.

## Evidence

Before recovery:

- The workspace API detected Codex but omitted `sessionId` from its tab and pane.
- A fresh `moshi-hook context` process found the correct conversation ID.
- Reading `/v1/transcripts` by that ID returned a WebSocket backlog with 96
  entries. The transcript was present and readable; workspace association failed.
- The current visible terminal had no matching resume instruction.
- The running gateway's image and version were both 0.4.9.

The disposable reproduction established causality:

1. Start a synthetic Codex process holding one synthetic transcript open inside
   a disposable Neovim terminal, with isolated Moshi state.
2. Verify the gateway advertises its conversation ID.
3. Print Codex's exit/resume instruction for that same ID on the visible screen.
4. Verify the gateway drops the conversation ID.
5. Clear the terminal and wait longer than the negative-cache refresh interval.
6. Verify the gateway still omits the ID, while a fresh context command finds it.

Binary control flow corroborates this behavior. In
`gateway.codexSessionEndedOnScreen`, a cached entry for the same pane and
conversation returns immediately when `ended` is true, without applying the
expiry used for false entries (approximately two seconds).
`gateway.codexSessionEndMarker` recognizes the resume instruction after
normalizing visible text. The state therefore outlives the screen that caused it.

Run the preserved diagnostic with:

```sh
python3 tests/moshi_codex_end_cache_probe.py
```

It reports `stale_exit_cache_reproduced: true` on the tested 0.4.9 binary. This is
an opt-in upstream diagnostic, not a test that requires the bug to remain present.
It creates and cleans up its own editors, server processes, sockets, and synthetic
transcript. `NEORDR_BIN` and `MOSHI_HOOK_BIN` can select other binaries.

The inspected binary's build metadata identifies commit
`3b2c97b95cfd11aa3459fe14956fceee806ce93f`, built with Go 1.26.6. Recovered Go
symbol/source mappings place the relevant functions in `gateway/terminal.go`:

| Function | Recovered source line | arm64 address |
| --- | ---: | --- |
| `resolveHerdrAgentSession` | 972 | `0x100597ce0` |
| `codexSessionEndedOnScreen` | 1717 | `0x10059ace0` |
| `codexSessionEndMarker` | 1745 | `0x10059af90` |
| `newestRolloutSession` | 1831 | `0x10059ba20` |
| `codexRolloutIsSubagent` | 1853 | `0x10059bb90` |
| `codexLiveSessionForHerdrPane` | 2710 | `0x10059f4e0` |

The decisive cache branch is at `0x10059ae1c`: a true `ended` field skips the
`time.Since` comparison applied to false entries. These addresses are specific
to this build and are evidence pointers, not stable integration contracts.

## Recovery and limits

Restarted only the launchd-managed Moshi service. The first workspace query after
restart advertised the correct ID, and a fresh transcript connection returned
151 backlog entries. The Neovim and Codex PIDs and start times were unchanged.
The investigation did not edit any production transcript files.

This is an operational recovery, not a permanent fix in Moshi. Seeing another
matching end marker can recreate the problem. Client-side visual confirmation
is separate from the verified workspace API and transcript stream.

The upstream fix should invalidate suppression when the foreground agent
lifetime changes or fresh session lifecycle evidence arrives. At minimum,
positive entries must be rechecked after the marker disappears. End-of-session
screen detection should not be permanent evidence that a currently running
conversation has ended.

Neordr should not filter terminal contents or falsify conversation IDs to bypass
this cache. No production Neordr code was changed for this incident.

## Hypotheses ruled out

The live Codex process also held an approval guardian's transcript open. The
0.4.9 binary explicitly filters subagent rollouts, including a `source` object
with a non-null `subagent` field. A synthetic main-plus-guardian fixture still
resolved the main conversation correctly.

A different original transcript working directory also passed an isolated test.
An initial restricted-PATH context failure did not reproduce in controlled
repeated comparisons, so PATH is not established as a cause.

## Version review

The [official 0.4.9 release notes](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.9)
list automatic-update policy (`ask`, `auto`, or `off`), Chat View support for
Goose/OmO/jcode, additional question and approval handling, and embedded web
improvements including file previews and per-agent hook installation.

Binary inspection also confirms Codex shared-daemon diagnostics/remediation,
subagent rollout filtering, and the screen-end suppression described above.
Their presence in 0.4.9 is verified; without a complete older-binary comparison,
this audit does not claim that all were introduced in this particular release.

The discovered daemon helpers include inspection, a doctor check, an offered
configuration fix, and supported daemon shutdown. Approval-related helpers
include Codex asynchronous questions, approval-review completion suppression,
and transcript watchers for abort, completion, and input-request events. These
are function-level findings; their individual runtime behavior was not exercised
by this audit.

For Herdr-backed Codex discovery, control flow still combines a reported session,
terminal title, process environment (`HERDR_PANE_ID`), and open rollout files.
The rollout selector excludes recognized subagent metadata and chooses the
lexically newest remaining rollout filename. Its process-rollout memoization is
scoped to each workspace-tree or pane-list operation; it is separate from the
long-lived end-marker cache responsible for this incident.

Temporary disassembly and symbols are under
`/private/tmp/moshi-inspection-049/`. Preserve the runnable repository reproduction
rather than relying on temporary artifacts for future validation.

## Reproduction and compatibility checks

Run the opt-in reproducer after building Neordr:

```sh
python3 tests/moshi_codex_end_cache_probe.py
```

It reports `stale_exit_cache_reproduced` rather than making the external bug a
required CI assertion. All terminal input and hook events target disposable
synthetic agents; no real chat is submitted.

The existing `python3 tests/moshi_smoke.py` passed against 0.4.9: Claude and
Codex context discovery, transcript backlog and append, prompt delivery,
workspace/pane listing and focus, and PTY attachment, input, detach, and
reattachment. These are local gateway checks, not paired native-client tests.

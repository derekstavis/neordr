# Local approval and question compatibility

Verified 2026-09-22 against the installed `moshi-hook 0.3.26` executable
(SHA-256 `cf017621ef100f32e914ca1fdaaca22fe45db19b500600995aa0ee07429c0f7d`).

## What was exercised

`tests/approval_smoke.py` starts disposable Neovim, Neordr bridges/router, and an
unpaired Moshi gateway. Two Python processes named `claude` and `codex` provide
synthetic terminal screens and record raw input. They invoke the installed
agent hook entry points. Minimal synthetic transcript files establish session
identity; the test supplies the question/plan request bodies a client would send.
There are no actual agent/model calls or real conversations.

The gateway, process/session resolution, hook state, permission coordinator,
terminal capture, and keystroke delivery are real. This is a local compatibility
test, not a test of native/web question-card rendering, automatic transcript
question extraction, actual agent interpretation of keys, or cloud push delivery.

| Case | 120 columns | 32 columns |
| --- | --- | --- |
| Claude question: choose second option | `2`, Enter | `2`, Enter |
| Codex question: choose second option | `2` | `2` |
| Claude/Codex plan: choose first option | `1` | `1` |
| Claude permission: approve / deny | `1` / `2` | `1` / `2` |
| Codex permission: approve / deny | `y` / Escape | `y` / Escape |
| Answer after question/plan completion hook and screen dismissal | HTTP 409, no input | HTTP 409, no input |
| Permission answer after screen dismissal | HTTP 409, no input | HTTP 409, no input |
| Duplicate permission answer | HTTP 409, no extra input | HTTP 409, no extra input |

Each request checks both terminals' input logs to detect misrouting. Desktop
requests include the named Herdr session; narrow requests use the default router.
Each width uses fresh sessions to isolate asynchronous permission watchers.

These cases required no additional Neordr RPC methods or production changes.

## Observed limitations

### Wrapped plan menu titles

Moshi calls `pane.read` with `source: "recent_unwrapped"` when inferring the
visible plan menu. Neordr can return bounded terminal buffer rows, but cannot
recover exact soft-wrap boundaries from the Neovim API it uses.

The fixture's title, `Add a blue navigation bar and verify the selected color.`,
is inferred correctly at desktop width. At 32 columns the observed inferred
title is only `rify the selected color.`. The two tested option labels and answer
selection still work. This does not establish correct parsing of every longer
or wrapped option label. The test reports this known title limitation separately.

Joining every full-width row would also join intentional hard line breaks. No
such heuristic was added to Neordr. Exact reconstruction would need reliable
wrap metadata or an additional raw-terminal capture mechanism.

### Claude question hook state can outlive the visible question

With a matching pending `AskUserQuestion` hook record, Moshi 0.3.26 accepted
`/v1/questions/answer` after the fixture replaced its visible question with
`Prompt dismissed`, and sent `2` plus Enter. After the corresponding
`PostToolUse` hook, the dismissed-question request returned HTTP 409 without input.

The binary's `questionMatchesLiveAgentPrompt` result feeds the terminal question
verification path. The dynamic probe confirms that matching hook state can permit
the answer even without the old screen. This is distinct from permission
approval checks, which rejected the dismissed screen in the tested cases.

Neordr receives ordinary `pane.send_keys` requests for these answers. It lacks
the question identity needed to repair stale Moshi state, and denying all such
keys would break valid interaction. Its foreground-agent guard also cannot help
while the same Claude process remains alive. No hook-state rewriting or change
to the installed Moshi binary was introduced.

Reproduce the observed behavior using disposable terminals:

```sh
NEORDR_TEST_COLUMNS=120 NEORDR_PROBE_STALE_HOOK=1 python3 tests/approval_smoke.py
```

This optional probe intentionally expects the observed 0.3.26 behavior. A later
Moshi version rejecting it should trigger a review of the probe and this report.

### Hook startup timing

Ordinary tool hooks sent within three seconds after `SessionStart` were ignored.
The same event created the pending prompt after that interval. Disassembly of
`runClaudeHook` and `runCodexHook` confirms the three-second comparison. The
fixtures wait for that interval rather than fabricating Moshi's state files.

## Run

```sh
python3 tests/approval_smoke.py
NEORDR_BIN="$HOME/.local/bin/neordr" python3 tests/approval_smoke.py
```

Physical mobile gestures, native/web rendering of these cards, real agent UI
versions, multi-question/multi-select forms, and cloud-delivered approvals still
need separate validation. Successful local gateway responses do not prove those
paths.

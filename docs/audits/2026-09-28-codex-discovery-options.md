# Codex conversation discovery: integration options

Investigation date: 2026-09-28. Tested binaries: Codex 0.158.0,
moshi-hook 0.3.26, and the current Neordr checkout. No production code, personal
agent configuration, running editor, or installed service was changed.

The requirement is automatic conversation association without changing personal
Codex/Claude settings, scraping titles, matching directories, or treating an
initial resume argument as the permanently selected conversation. Existing normal
Moshi hooks are already installed; eliminating those hooks is a separate scope.

## Recommendation

Prototype a Neordr-managed Codex launcher with a per-terminal app-server proxy.
Keep the shared daemon and ordinary Codex TUI. Use the supported `--remote`
option to connect that TUI through a private Unix socket owned by the launcher.
Observe structured lifecycle request/response pairs on that specific connection,
then publish the confirmed thread ID through Herdr's existing `agent_session`
contract. Do not inspect transcript contents to infer identity.

This is the strongest existing-binary candidate tested here, not an implemented
or fully validated production solution. It changes how the frontend is launched;
it does not require edits to agent config files or title settings. An already
running frontend cannot have its socket connection redirected this way.

## Options

| Approach | Assessment |
| --- | --- |
| Query the shared daemon in the background | Loaded-thread IDs do not identify the conversation displayed by a particular frontend. Insufficient by itself. |
| Titles, filenames, cwd, PID guesses, or resume argv | No authoritative current selection; rejected as the integration foundation. |
| Install Herdr-style hooks | Hooks report session IDs, but still inherit the shared backend's pane environment. Adding hooks alone does not solve terminal ownership. |
| Disable the shared daemon globally | Current Moshi guidance, but changes personal configuration and disrupts daemon-connected work. Outside the requested approach. |
| Disable daemon auto-start only for Neordr launches | Tested `codex --disable daemon_auto_start`; with an existing local server, both test frontends still used that server's hook context. Not established as a reliable isolation mechanism in this build. |
| Per-terminal protocol proxy | Successful identity observations and separate Moshi compatibility tests; recommended prototype. Requires managed launching and lifecycle validation. |
| Upstream frontend selection reporting | Cleanest long-term contract. Requires Codex support rather than just a Neordr change. |

## Protocol experiment with the real TUI

Used an isolated home/config, a real app-server, and a real Codex TUI connected
through a small Unix WebSocket observer. A dummy provider pointed at an
unreachable loopback port. The observer rejected `turn/start`; no model turn was
forwarded. Two synthetic histories were persisted with `thread/inject_items` in
the disposable backend so resume could be exercised without model inference.

Observed on the frontend's own connection:

1. Initial startup: successful `thread/start` response contained thread A's ID.
2. `/new`: successful `thread/start` response contained a distinct thread B ID.
3. `/resume A`: successful `thread/resume` response identified A, followed by
   unsubscribe from B.
4. `/agent` picker selection of B: successful `thread/resume` identified B,
   followed by unsubscribe from A.
5. Merely opening the picker caused background reads of **both** histories.
   `thread/read` and transcript-pagination calls must not be treated as selection.

An earlier empty-history run also produced a failed resume response. An attempted
resume cannot be treated as a successful selection. Whether the old binding must
be retained or cleared also depends on the frontend's failure transition; that
needs explicit lifecycle testing rather than assuming either behavior.

The documented meaning of resume is loading/subscribing to a thread, not a formal
cross-version promise about visible frontend focus. The above selection behavior
is verified for these paths in 0.158.0. Cached subagent switches, fork, races,
reconnect, and future TUI changes remain validation requirements.

## Moshi compatibility experiment

Used disposable Neovim terminals, Neordr, and the installed Moshi gateway. The
synthetic Codex process had no open rollout file, no UUID in its title, and no
hook record initially. No real chats or user terminals received test input.

- Baseline: Codex was detected, but its pane had no conversation ID.
- Injecting typed `agent_session` metadata into the test snapshot enabled session
  discovery and transcript backlog/append streaming.
- Prompt submission without any hook record returned HTTP 404:
  `no recorded terminal state for that agent session`.
- Creating a hook record deliberately pointing at the other test pane, while
  keeping the correct live typed metadata, allowed prompt submission. Moshi
  logged that the session had moved and sent input to the Codex pane.
- A subsequent correct hook record also routed successfully. The Codex terminal
  received exactly the two intended test prompts. Workspace listing and focus
  checks passed as well.

Thus existing Moshi hooks plus authoritative live metadata can work without
rewriting Moshi's state files. This tested stale panes within one named session;
cross-editor/backend-session correction through the default router still needs
a dedicated case. Cloud approvals and paired native clients were not tested.

## Per-launch isolation experiment

Started a disposable local app-server on its default Unix socket, then two real
TUIs with `--disable daemon_auto_start`. Each TUI had a different synthetic
`HERDR_PANE_ID`; the server had neither. A recorder hook in the disposable profile
was explicitly trusted through the test TUIs. Synthetic prompts targeted only an
unreachable loopback provider to trigger pending SessionStart hooks.

Both hook events had different thread IDs but the same parent process and no
pane ID. The per-launch flag did not isolate the frontends from the existing
listener in this fixture. This does not prove every command-line override or
every Codex version behaves identically; it rules out relying on this tested
shortcut as the proposed integration.

## Production design and acceptance criteria

- Bind a proxy connection to one exact pane generation and frontend lifetime.
  Validate foreground ownership, revoke bindings on exit, and reject stale reports.
- Forward protocol traffic without changing approvals, tool calls, or content.
  Retain only identity metadata; do not log chat bodies or authentication tokens.
- Publish lifecycle results through the typed Herdr contract. Do not bind based
  on background reads, notification arrival order, or private request-ID prefixes.
- Preserve the shared backend on frontend exit. Define what happens if the proxy
  or Neordr bridge restarts, without leaving a stale conversation associated.
- Test concurrent frontends in the same directory, rapid switching, `/new`,
  `/resume`, fork, subagents, failed transitions, and reconnect.
- Verify Moshi discovery, transcript, send, and approvals across editor workspaces,
  including deliberately stale hook records and duplicate views of one thread.
- Run compatibility tests against supported Codex/Moshi versions. Unknown or
  ambiguous selection behavior must result in no advertised binding.
- Scope initial support to Neordr-managed launches. Plain `codex` launched in an
  arbitrary existing shell would bypass the adapter unless explicitly integrated.

An upstream alternative is an explicit frontend-selected-thread event, together
with an initial snapshot/query, associated with that frontend connection or TTY.
Such a contract would remove the remaining dependency on TUI use of lifecycle
methods. This is a proposed extension, not an API found in the installed schema.

## Evidence and sources

Local investigation artifacts (temporary, not production components):

- `/private/tmp/neordr-protocol-probe.py` and
  `/private/tmp/neordr-protocol-probe4.log`: real-TUI switching probe.
- `/private/tmp/np-wp32lfnh/events.json`: method/ID trace from that probe.
- `/private/tmp/neordr-reported-id-probe3.py` and its `.log`: synthetic Moshi
  discovery, transcript, stale-record routing, and focus checks.
- `/private/tmp/neordr-codex-launch-probe.py` and
  `/private/tmp/neordr-codex-launch-probe3.log`: negative per-launch flag result.
- `/private/tmp/neordr-codex-schema-0158`: schemas generated by the installed CLI.

Official references:

- [Codex app-server](https://learn.chatgpt.com/docs/app-server): remote TUI,
  transports, thread lifecycle, and subscription APIs.
- [Codex developer commands](https://learn.chatgpt.com/docs/developer-commands):
  `--remote` endpoint forms and CLI transport options.
- [Codex hooks](https://learn.chatgpt.com/docs/hooks): lifecycle and session inputs.
- [Moshi troubleshooting](https://getmoshi.app/docs/troubleshooting#codex-sessions-show-the-wrong-pane-or-all-look-like-one-terminal)
  and [0.4.3 release](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.3):
  upstream acknowledgement and its configuration-changing daemon workaround.

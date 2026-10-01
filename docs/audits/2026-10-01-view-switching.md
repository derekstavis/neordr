# Shared process scans and Moshi view switching

This follows the [first October 1 performance audit](2026-10-01-performance.md).
Measurements are local to this Mac. Moshi hook is **0.4.9**; the installed Moshi
Desktop is **0.4.12**, so their version numbers must not be conflated.

## Changes

An aggregate request previously asked each bridge for an enriched snapshot,
enumerating the same host process table once per editor. The router now retains
each freshly verified bridge's editor identity, reads its typed Lua snapshot
through the existing generation-checked boundary, and enriches all the results
with one fresh process table. The bridge and router share the projection code.
No wire schema changes, persistent cache, or TTL were introduced. Mutations still
use their existing bridge routing and foreground/identity checks. Disappearance,
ordering, focus and blocked-editor behavior retain their existing policies.

Moshi also asks `pane.process_info` about file-only editor panes. Those have no
terminal root PID; returning their empty process metadata now avoids a host scan.

Regression coverage verifies one scan per aggregate, a fresh scan on the next
request, visibility of an agent exit, and successful file-only process metadata
even when process enumeration is unavailable.

## Controlled results

The core fixture used six disposable editors, each with an idle terminal, with
15 measured samples after warmup (six batches for concurrent requests).
Baseline: `221d54d`, installed binary SHA256
`5220a5e21255997dc67c1fda4b6ebec895d9cac577cf5a8e0bef9ee989887ace`.
Core timings used the shared-scan build before the file-only fast path was added;
that fast path does not affect these terminal-bearing snapshot cases.

| Operation | Before median (ms) | After median (ms) | Before p95 | After p95 |
| --- | ---: | ---: | ---: | ---: |
| Aggregate snapshot | 116.71 | 49.90 | 157.98 | 66.91 |
| Workspace list | 119.43 | 48.10 | 149.46 | 63.54 |
| Pane list | 113.95 | 48.21 | 141.55 | 58.79 |
| Four simultaneous snapshots, entire batch | 249.58 | 68.54 | 311.23 | 82.32 |

`tests/moshi_performance.py` exercises the real installed gateway with six
disposable editors, two synthetic agents in one editor and five file-only
editors. It records eight measured requests after the first request, including
JSON API traces at the router and the agent editor's bridge. Nested calls appear
at both endpoints; their durations must not be added as independent wall time.
This workload intentionally includes empty editor workspaces. Its improvement
is not a prediction for a host where every editor has terminals.

| Gateway operation | Before median (ms) | After median (ms) | Before p95 | After p95 |
| --- | ---: | ---: | ---: | ---: |
| Workspace tree | 603.35 | 290.80 | 651.02 | 373.37 |
| Pane lookup | 344.21 | 323.11 | 371.73 | 368.70 |
| Focus | 47.06 | 49.04 | 66.19 | 50.39 |
| Fresh terminal connection to known screen text | 19.47 | 19.45 | 26.79 | 27.28 |

The final candidate SHA256 is
`e243b75a83c5f09c9ba662f4640e6f645f7e50dfb05a896f457a850a357ee1ef`.
Raw files are `/private/tmp/neordr-shared-{before,after}.json` and
`/private/tmp/neordr-moshi-shared-{before,after}.json`. The latter records method
names and timing only, not transcript contents. Baseline and candidate collection
measurements ran sequentially without a competing build/test suite. The first
Moshi fixture overlapped the end of the core attachment loop; attachment timings
from that core run are deliberately excluded above. Ambient applications remain
uncontrolled. Small timing differences should not be interpreted as improvements.

The gateway trace explains why view switching is a separate problem:

- A workspace tree issues one aggregate snapshot and seven process-info requests
  in this fixture, plus occasional screen reads. File-only queries become cheap.
- Pane lookup asks for a scoped `pane.list`, an aggregate `agent.list`, process
  metadata and sometimes screen contents. Gateway work remains beyond those RPCs.
- Focus asks for a scoped `pane.list` before `pane.focus`. In the candidate,
  the list took a median **46.61 ms**, while focus itself took **1.50 ms**.
- Transcript WebSockets made **no Neordr JSON API calls** in these fixture reads.
  Backlogs from 100-row synthetic files took about **0.8 ms**; 10,000-row files
  took about **3.2–3.5 ms**. Only the gateway's initial page is transferred.

## What the desktop switch actually does

These are findings from the JavaScript assets embedded in the installed native
binary, not reconstructed original source and not a measured browser trace.
Desktop binary SHA256:
`80e3197f37919e26332c055463dc0700b081a9be8103faa63cf9b1067b1dae77`.
Symbol names below identify this build's minified code only.

- The session component latches separate "Term has been opened" and "Chat has
  been opened" state. After first use it keeps both mounted, toggling their CSS
  visibility. An ordinary same-session toggle therefore does not inherently
  recreate either connection. Other navigation or the pinned terminal rail can
  change component lifetime.
- Chat to Term calls `uA(...).switchToTerminal`. It resolves the correct pane via
  `lA`, awaits a serialized focus request, then navigates to `view=term`. That
  focus function resolves through the module's `Ni` export to `hp`, which POSTs
  `/v1/workspaces/focus`. Rail focus requests share its queue.
- Term to Chat fetches `/v1/workspaces/panes` when a tab advertises multiple
  panes; otherwise it uses the existing tree association. Neordr represents each
  terminal as a one-pane tab, so the normal mapping avoids that extra lookup.
- Terminal component `nA` restores a cached terminal or creates one. `Yk` caches
  a live terminal when it is unmounted; `Vk`/`Hk` retain up to two idle terminals
  per host. Restoring the same cached target fits/resizes it; changing the Herdr
  target can reconnect its control stream. New terminal creation waits for
  outstanding close promises for that host; each close wait has a five-second
  fallback. This is another path to inspect for unusually slow changed-target
  switches, not an observed five-second delay in these local measurements.
- Chat store `nT` closes its WebSocket when its last subscriber unmounts, while
  retaining rendered messages. `eT` bounds idle caches to 24 entries per tab and
  an estimated 32 MiB overall. Resubscription reconnects and can page backward in
  75-line requests to regain the previously loaded history. This applies to
  unmount/remount, not every visibility toggle.

The main route imports `Tt as nt` from `index-Bzdsj8h8.js`; that export is `tT`,
which constructs the `nT` chat store. This links the shared store to the actual
desktop session component, rather than assuming all embedded code is active.

A read-only probe of this real Neordr conversation returned **110 entries,
525,925 bytes**, with median first-backlog time **65.44 ms** across six connections
(48.73–93.64 ms). Local Python JSON decoding added about 1–3 ms; that does not
estimate Moshi's reducer, Markdown, layout or paint cost. The probe sent no agent
input and did not modify transcript files. Raw metadata/timings:
`/private/tmp/neordr-live-transcript-performance.json`.

Safari MCP returned `Transport closed`; computer-use inventory exposed no usable
browser surfaces. Therefore **click-to-visible-content timing remains unmeasured**.
The native app's behavior must not be generalized to the mobile app without
inspecting/testing that client. All gateway figures exclude SSH/Mosh, remote
network latency and client rendering.

## Validation and local deployment

Both `CARGO_NET_OFFLINE=true bash scripts/check.sh` and
`CARGO_NET_OFFLINE=true bash scripts/check-moshi.sh` passed, including the two new
regressions. The first run of the file-only regression incorrectly expected an
empty JSON array: the upstream schema omits empty process arrays. The assertion
now deserializes the upstream response type and checks its empty vector.

The final binary was installed atomically on this Mac, with the prior build
backed up at `/private/tmp/neordr-before-sharedscan-mhgt66gk/neordr`. The router and
all ten verified bridges were refreshed; every editor PID/generation and terminal
buffer/channel/job PID matched before and after. Editors, agents and Moshi were
not restarted. No remote deployment was performed.

Eight live post-deployment samples per operation all succeeded:

| Operation | Median (ms) | Maximum (ms) |
| --- | ---: | ---: |
| Ten-editor Neordr aggregate | 52.95 | 55.87 |
| Moshi workspace tree | 1,092.77 | 1,294.13 |
| Moshi pane lookup for this conversation | 269.61 | 318.17 |

The live workspace tree remains around one second. This is consistent with the
fixture improvement depending partly on file-only workspaces: the real host has
many terminal panes requiring additional process/discovery work. Do not present
the fixture's 603-to-291 ms result as the real host's end-to-end improvement.
These post-deployment observations have no matched live before run. Raw timing
file: `/private/tmp/neordr-shared-live-after.json`; service/editor inventories:
`/private/tmp/neordr-shared-{services,editors}-{before,after}.json`.

## Next targets

1. Make foreground process discovery cheaper for individual terminal panes.
   Each still pays for a full process scan; this dominates the gateway's focus
   preflight. Preserve process ancestry and fresh mutation checks.
2. Profile a real desktop toggle with browser/WebView instrumentation. Distinguish
   first-open, warm same-pane, changed-pane and remote-host cases. Measure the
   awaited focus request and client main-thread/render time separately.
3. For an upstream Moshi change, avoid unnecessary enriched pane enumeration when
   focusing an already exact pane ID, while preserving stale-target validation.
   The installed app/hook binaries cannot be patched through Neordr source.
4. Move the native Herdr endpoint's periodic snapshot collection off its input
   loop. It still synchronously collects every 300 ms; the shared-scan change
   shortens those pauses but does not eliminate them. This is a different path
   from Moshi's PTY/control transport.

Persistent production timing summaries and cross-request coalescing remain
unimplemented. This change removes redundant work within each request first.

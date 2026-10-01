# Connection and command performance, October 1

## Historical evidence

The current source has no persistent per-request Neordr latency logger. The
existing `herdr-server.log` contains old Herdr startup events, not Neordr timings.
Moshi's hook log contains 180 HTTP timing records from September 21–24; only four
successful `/v1/workspaces` samples were present (median 1,280.5 ms). There are no
recent timing records sufficient to evaluate the intervening days. The September
25 audit's controlled measurements remain useful historical context.

A small read-only sample of the long-running Mac installation returned a median
826.86 ms aggregate snapshot (seven successful samples, one truncated JSON
response), 55.94 ms per-editor snapshot, and 29.29 ms router ping. A release build
overlapped this live sample, so these are indicative observations, not controlled
before/after results. Editors and agents were not modified by the measurements.

## Controlled local comparison

`tests/performance.py` ran sequentially against the baseline release at `78a894e`
and the optimized release, each with six disposable Neovim editors and six idle
terminal jobs. Fifteen measured samples per operation followed one warmup; the
four-concurrent-request case used six batches. No build or other test suite ran
during either comparison. Ambient application activity is not controlled.

The host is macOS 27.2 arm64. Times below are milliseconds. The attachment
measurement is from subprocess spawn completion to receipt of a known editor
line, excluding teardown, remote networking, SSH/Mosh startup, and client paint.
The four-request row measures whole-batch completion, not per-request p95.

| Operation | Before median | After median | Before p95 | After p95 |
| --- | ---: | ---: | ---: | ---: |
| Router ping | 17.71 | 0.10 | 23.45 | 0.13 |
| Bridge ping | 16.90 | 0.20 | 29.19 | 0.24 |
| Bridge snapshot | 48.08 | 40.16 | 67.99 | 40.88 |
| Aggregate snapshot | 470.00 | 99.87 | 528.71 | 102.85 |
| Workspace list | 484.32 | 99.42 | 542.24 | 101.97 |
| Pane list | 476.68 | 100.39 | 529.51 | 105.12 |
| Routed pane layout | 49.53 | 0.54 | 76.69 | 0.66 |
| Routed pane focus | 76.11 | 0.66 | 95.55 | 0.77 |
| CLI session list | 42.83 | 4.24 | 53.85 | 4.67 |
| Four simultaneous snapshots | 537.22 | 203.23 | 585.86 | 226.61 |
| Named attachment rendering | 27.32 | 12.30 | 41.98 | 13.09 |
| Default attachment rendering | 80.17 | 13.00 | 107.15 | 14.14 |

Baseline SHA256: `d0b164a4e7a2cbb56b0fea8ccf8aeb638b2260b404301dfe75f9f8bb6787ee87`.
Optimized SHA256: `5220a5e21255997dc67c1fda4b6ebec895d9cac577cf5a8e0bef9ee989887ace`.
Raw run output was saved in `/private/tmp/neordr-perf-before.json` and
`/private/tmp/neordr-perf-after.json`; the committed fixture reproduces the method.

## Changes and invariants

- Router and bridge listeners now wait for socket readiness instead of sleeping
  20 ms after an empty accept. Idle lifecycle checks run at most 100 ms apart.
- Bridge validation and aggregate snapshot reads run in batches of at most four
  workers per collection, preserving deterministic editor/result order. Existing
  identity checks, disappeared-editor policy, and failures remain in place.
- Process discovery filters foreground process groups before walking parent
  chains, avoiding ancestry searches for unrelated host processes.
- Router workers explicitly clear inherited nonblocking mode before using their
  existing read/write deadlines. On Darwin, the faster accept path exposed
  `EAGAIN` on fragmented requests. The existing partial-request test now checks
  that the original fragmented request also completes successfully.

No process identity or mutation target is cached. Parallel reads are not an
atomic multi-editor snapshot; this was also true of the sequential implementation.
Concurrent callers still perform separate collections. Worker limits are per
collection, in addition to the existing server request limits.

## Validation and deployment

The full core suite passed, including fragmented router requests, stale identity
checks, input routing, mobile forwarding backpressure, modal prompts, gestures,
and workspace lifecycle. Moshi's isolated compatibility suite also passed.

Initial core runs failed during fixture terminal creation. A diagnostic probe
with a temporarily longer deadline exposed `E326: Too many swap files found`;
the failures also occurred with the old bridge accept loop. Repeated tests had
used the real user state directory. Rust fixtures, the performance fixture, and
the check scripts now isolate `XDG_STATE_HOME` in temporary directories. The
production RPC deadlines were restored unchanged; user swap files were not
removed. The complete core suite then passed without a retry mechanism.

The optimized binary was installed atomically on this Mac, then the ten verified
Neordr bridges and router were refreshed. All ten editor PID/generation records
and their terminal buffer/channel/job PID lists matched before and after.
Moshi, Neovim, and agent processes were not restarted. Remote hosts were not
updated in this round.

Twelve live post-update samples had no failures: median router ping **0.08 ms**,
ten-editor aggregate snapshot **159.95 ms**, and per-editor snapshot **41.50 ms**.
Five actual Moshi `/v1/workspaces` requests returned HTTP 200, with median
**1,064.37 ms** and maximum **1,389.38 ms**. The latter is a post-update observation,
not a controlled Moshi before/after comparison. Significant gateway work remains
beyond a single Neordr aggregate request.

## Remaining opportunities

Process-table enumeration still costs about 40 ms per editor snapshot and runs
once per editor per collection. Sharing/coalescing display-only reads could cut
both CPU and concurrent listing latency, while mutation-time checks must stay
fresh. The native Herdr endpoint still refreshes snapshots synchronously on its
input/render loop; its latency was not measured in this round. Persistent bounded
latency summaries would make future long-running evaluations more representative.

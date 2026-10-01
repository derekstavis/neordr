# Per-pane discovery performance, October 1, 2026

The previous audit measured approximately 1.09 seconds for Moshi's workspace
tree. A fresh trace identified the dominant cost: after one aggregate snapshot,
moshi-hook 0.4.9 synchronously requested `pane.process_info` for all 19 terminal
panes. Each request ran a full-host `ps` over approximately 1,500 processes.
The earlier optimization shared enumeration within an aggregate snapshot; these
subsequent, separate requests still enumerated the host independently.

## Measurements

All numbers below are local gateway/protocol timings, not client click-to-paint
latency. No real terminal input or transcript changes were made.

| Measurement, warm median | Before | After |
| --- | ---: | ---: |
| Isolated gateway mirroring 10 live editors / 19 panes | 1,266.70 ms | 408.75 ms |
| Its 19 sequential `pane.process_info` calls, combined | 944.08 ms | 99.53 ms |
| All measured outer Neordr RPCs, combined | 1,004.21 ms | 158.70 ms |
| Production gateway, immediately before/after activation | 1,339.43 ms | 478.51 ms |

The isolated comparison used six warm samples after one cold request. Private
read-only socket proxies traced methods and durations; the candidate used
private bridges and a private router connected to the same existing editor
sockets. Proxies allowed only read operations. The isolated hook received copies
of agent-state indexes, used the existing transcript paths, and had no copied
cloud credentials. Neither production sockets nor UI attachments were replaced
by the tracing setup. Some requests also performed five `pane.read` calls.

The production comparison used five warm samples after one initial request on
each side. Its median improved by 64%, or about 2.8x. All 19 pane IDs, agent names,
and conversation IDs matched before/after. Timing varies with host load, agent
activity, and Moshi's other discovery work; 1.09 seconds was an earlier sample,
not the matched baseline for this change.

## Implementation

`pane.process_info` now collects fresh process rows for the selected terminal:

- macOS first reads the root's foreground process group. A second `ps` selects
  the union of that group, the root PID, and the terminal's TTY. The group selector
  includes members without a controlling terminal; the TTY selector includes
  background wrappers connecting the foreground agent back to its root.
- Linux uses the root's session ID and `ps -s` to collect the session, including
  members that have detached their controlling terminal.
- Missing roots, incomplete ancestry, a changed foreground group during the
  macOS queries, unsupported selectors, and query errors fall back to the
  existing full-host enumeration. Other Unix platforms retain that path.
- Editor-only panes still return empty process metadata without running `ps`.

This uses no cached process identities and changes no wire schema. Aggregate
snapshots, pane listing, agent listing, and mutation checks retain their existing
enumeration behavior. `vendor/herdr` remains unchanged at
`28360107c1bfab57fb0de99d74bfa48e74423ca8`.

A TTY-only optimization would be incorrect: a foreground group member can drop
its controlling TTY and disappear from `ps -t`. Selecting only root plus group
also proved insufficient on the live Mac: several agents had intermediate
wrapper ancestors in a different process group, forcing the conservative full
scan. The final union preserves those ancestry chains.

## Remaining Moshi work

An additional phase trace placed approximately 250–270 ms **after** the final
Neordr reply, rather than between the sequential metadata calls. This is not
evidence of a fixed sleep. Diagnostic subprocess wrappers observed 24 `lsof`
invocations per warm workspace refresh, with 260–288 ms of combined child
execution time in two samples. Cold discovery also executed 10 Git probes.

The wrappers add launch overhead, so their total HTTP times are intentionally
excluded from the comparison table. Their counts and child timings identify
additional work; they do not prove that every millisecond of the uninstrumented
tail belongs to `lsof`. Recovered Go symbols/disassembly include
`gateway.herdrWorkspaceTree`, `herdrPaneProcessFacts`,
`resolveHerdrAgentSession`, `withCodexProcessRollouts`, and
`openCodexRolloutPaths`. These are binary observations, not recovered original
source code.

The next upstream opportunities are request-scoped reuse/batching of Moshi's
process/file discovery and bounded parallel pane metadata requests. Neordr
cannot eliminate those installed-hook subprocesses through this API. This
change deliberately preserves fresh discovery and conversation matching.

## Validation and activation

- `CARGO_NET_OFFLINE=true bash scripts/check.sh`: passed, including 26 unit
  tests, the generated-contract test, 25 session integration tests, and all core
  smoke suites; formatting and all-target Clippy passed on macOS.
- `CARGO_NET_OFFLINE=true bash scripts/check-moshi.sh`: passed, including
  Claude/Codex transcripts, prompts, approvals, questions, navigation, and
  workspace lifecycle operations.
- Linux: the two new process unit tests and three process-info integration
  tests passed in a temporary checkout on `capela-pc.local`. The group test
  specifically retained a TTY-detached child and verified that the scoped path
  was used. Remote Clippy could not run because that toolchain lacks the
  component; local all-target Clippy passed. No remote installation was changed.
- `tests/moshi_performance.py --editors 3 --samples 3`: passed with the release
  build. The benchmark now records RPC start/end offsets and directly measures
  `router.process_info` (5.52 ms median in this synthetic fixture). Nested
  router/bridge records overlap and must not be summed as wall time.
- The local release binary was installed atomically with a backup. Only the ten
  verified bridge processes were refreshed. Router PID 44027 was retained, and
  all ten editor generations/PIDs and nineteen terminal job identities matched
  before/after. Existing terminal UI clients were not restarted.

Installed candidate SHA-256:
`cf2d7d9f90cce7239ed0e1099c0a729cf9bd8d7fba5131bafabcf90ddcd19539`.
Prior binary backup: `/private/tmp/neordr-before-scopedscan-08n7g4vq/neordr`.

Local diagnostic artifacts (temporary, not portable fixtures):
`/private/tmp/neordr-live-discovery-trace{,-candidate,-phases,-subprocess}.json`,
`/private/tmp/neordr-hook-tools.jsonl`,
`/private/tmp/neordr-scoped-live-{before,after}.json`,
`/private/tmp/neordr-scoped-{services,editors}-{before,after}.json`, and
`/private/tmp/neordr-scoped-moshi-performance.json`.

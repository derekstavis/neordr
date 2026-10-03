# Herdr source update, October 3, 2026

The unmodified upstream source pin advances from
`28360107c1bfab57fb0de99d74bfa48e74423ca8` to
`5da0a01e1eedda054db0c81dd3a780000c40d9f0` (latest fetched `origin/master`).
Upstream still declares protocol **22** and endpoint generation **1**. Neordr
continues deriving those numbers from the upstream schema; no artificial version
bump is needed. New functionality is negotiated through optional capabilities.

## Incorporated changes

- Complete typed API/wire contracts, including optional agent `resume_argv`,
  host-default-color input, and the compatible bulk graphics byte codec. Schema
  availability does not imply implementation of those operations.
- Upstream agent detection manifests and engine updates, including Codex folder
  trust and mention popup recognition. Regression screens verify blocked state.
  Process discovery still recognizes Claude and Codex; this update does not add
  discovery for Pi or every agent known to Herdr's manifests.
- JSON control `terminal.mouse`: down, up, drag, and move, with zero-based cell
  coordinates and modifier translation through the ordered PTY input queue.
- Native Herdr `surface_scroll` negotiation. A connection retains its last
  successfully delivered surface, computes changed spans, then uses the actual
  upstream codec to encode vertical scroll shifts and residual rows. It chooses
  the smaller patch representation and falls back to a full frame if necessary.
  Initial attachment, resize, projection changes, editor switches and topology
  changes reset the baseline. Unnegotiated clients retain complete frames.

The source extraction preserves upstream field order, defaults, serializers and
scroll codec implementation. No parallel schema or copied scroll layout was
introduced. An upstream `chunks_exact` Clippy warning is suppressed only on the
extracted codec module; the vendor checkout remains unmodified.

Agent resume, graphics delivery, host color application, surface interest,
`surface_reuse` and the separate `surface_delta` codec remain unimplemented.
Upstream Ghostty, Windows, session persistence and UI changes do not directly map
to Neordr's Neovim backend and were not imported as application code.

## Rendering measurements

Release-mode local diagnostic, 480×150 cells, 30 iterations; rows shift up once
per pane and the exposed row changes:

| Populated panes | Encode/diff time per update | Compact bytes | Full bytes | Reduction |
| --- | ---: | ---: | ---: | ---: |
| 1 | 1.979 ms | 10,986 | 1,080,050 | 99.0% |
| 15 | 2.188 ms | 164,043 | 1,080,391 | 84.8% |

Fifteen panes take 1.11× the one-pane time in this synthetic grid; there are
fifteen exposed rows instead of one. The benchmark includes changed-span
construction, scroll detection and encoded-size calculations. It excludes
Neovim-to-grid conversion, actual socket delivery and client painting. Full-grid
construction and comparison still visit screen cells; this is a bandwidth
improvement, not removal of all screen-size-dependent CPU work.

Moshi uses Neovim's incremental ANSI remote-UI path, so these native Herdr
measurements do not establish a Moshi typing/scrolling speedup.

## Validation

- Rust formatting, Clippy with warnings denied, contract/unit tests and disposable
  Neovim integration tests through `bash scripts/check.sh`.
- Typed contract regressions cover default/opt-in scroll negotiation, optional
  resume arguments, graphics binary/JSON compatibility and truncated data.
- Scroll/patch reconstruction uses the upstream decoder for 1 and 15 panes,
  sparse Unicode changes and wide-cell continuation. Tests cover legacy
  clients, resize, topology/projection/editor changes, and failed writes.
- Actual unmodified Herdr 0.9.3 and the previous September 21 preview pass CLI,
  interactive rendering/editing, tab switching, paste, modal prompts, stale input
  rejection, resize, detach, killed-client cleanup and independent-UI survival.
  Binary versions and hashes are recorded in [interface compatibility](../herdr-interface.md).
- Real Moshi gateway activity checks pass for working/done status and transition
  ordering with synthetic Codex/Claude agents.
- Control-mode tests cover mouse click/drag coordinates plus existing input,
  resize, scrolling, malformed input, EOF, signals and output backpressure.

The standalone Herdr/control fixtures now give their Neovim subprocesses private
state directories. Shared test swap files had accumulated enough to block
buffer creation with E326; no live user swap files were removed.

No live workspace, bridge, router or installed binary was changed by this audit.

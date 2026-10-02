# Large-terminal latency investigation

The reported symptom is typing/scrolling lag in the Moshi Mac app with a large
terminal window. This investigation measures server-side scaling and inspects
the installed desktop's transport/rendering path. It does **not** establish the
cause of the reported visible lag: the actual desktop renderer has not been
profiled during a confirmed reproduction.

## Two different rendering paths

| Client | Neordr output | Update behavior |
| --- | --- | --- |
| Herdr's native interactive interface | `src/endpoint.rs` / `endpoint/grid.rs` semantic cell grids | Sends a complete `PaneSurface` when the grid is dirty, at most about once per 16 ms. This is the full-grid limitation discussed previously. |
| Moshi full-editor terminal | `src/attach.rs` forwards `nvim --remote-ui` PTY output | Neovim's TUI emits incremental ANSI. Neordr forwards it without rebuilding the screen. |
| Moshi pane control stream | `src/control.rs` wraps the same attachment output | Base64/JSON `terminal.frame` records carry ANSI chunks with `full: false`; Moshi decodes them for the terminal WebSocket. |

Therefore adding `PaneSurfacePatch` to Neordr's native Herdr endpoint would not
directly improve the Moshi path measured here.

The pinned Herdr source provides both retained semantic surface patches in
`vendor/herdr/src/server/headless/retained_surface.rs` and a stateful ANSI
`BlitEncoder` in `vendor/herdr/src/protocol/render_ansi.rs`. The encoder compares
frames, emits changed cells/runs, accepts row patches, and brackets painting in
synchronized-output sequences. Neordr's Moshi path already gets incremental
ANSI from Neovim; it does not currently use Herdr's retained-surface engine.

## Controlled measurements

Release Neordr SHA-256:
`cf2d7d9f90cce7239ed0e1099c0a729cf9bd8d7fba5131bafabcf90ddcd19539`.
Installed hook: 0.4.9. All editors used `-u NONE`, private sockets/configuration,
and a private hook without production pairing credentials. No existing UI was
attached, detached, resized, or given input.

For each size, a mapped input toggles a 40-character buffer line twenty times.
The timer measures sending input to receiving the complete changed marker in
ANSI output. It excludes desktop parsing/painting and remote SSH/network paths.
The benchmark reader acknowledges WebSocket output immediately, unlike a real
client that must first parse it. Values are single-run medians, not latency SLOs.

| Terminal cells | Direct Neovim remote UI | Neordr control | Moshi pane gateway | Moshi full-editor gateway |
| --- | ---: | ---: | ---: | ---: |
| 120 × 36 | 0.141 ms | 0.238 ms | 1.538 ms | 0.452 ms |
| 240 × 72 | 0.133 ms | 0.255 ms | 0.613 ms | 0.434 ms |
| 480 × 144 | 0.129 ms | 0.367 ms | 0.649 ms | 0.462 ms |

Every tested path and size emitted a median **55 ANSI bytes per toggle**. The
full-editor gateway's p95 was 0.51–0.55 ms. These fixtures show no meaningful
screen-area scaling for a small edit; they do not measure the redraw behavior
of a real Codex/Claude terminal application or the user's Neovim plugins.

For scrolling, a timer performs 100 one-line scrolls at approximately 50 Hz in
a populated editor buffer. Across all four paths, received ANSI totaled:

| Size | ANSI bytes / 100 scrolls | Bytes per scroll |
| --- | ---: | ---: |
| 120 × 36 | 15,800 | 158 |
| 240 × 72 | 27,800 | 278 |
| 480 × 144 | 52,000 | 520 |

The cost follows the new row width, rather than resending the whole viewport.
An earlier diagnostic that repeatedly requested `redraw!` without changing
content produced no steady-state output: Neovim still filtered unchanged cells.

A deliberately extreme workload changed essentially every visible character
100 times at approximately 50 Hz. On the full-editor gateway:

| Size | ANSI payload | WebSocket output chunks |
| --- | ---: | ---: |
| 120 × 36 | 424,813 bytes | 494 |
| 240 × 72 | 1,712,449 bytes | 1,679 |
| 480 × 144 | 6,880,921 bytes | 6,765 |

The large case delivered about 3.3 MB/s in about 3,250 chunks/s over 2.08 seconds.
This is a potential client-side pressure source when an application really does
change a large viewport. It is not evidence that ordinary typing generates this
traffic. The pane-control path additionally encoded roughly 9.9 MB of local
JSON/base64 for the same 6.88 MB ANSI stream. WebSocket counts here exclude
framing/TCP overhead; control `wire_bytes` counts the local JSON records.

## Desktop findings and remaining hypotheses

The installed desktop had updated to **0.4.13**. Its binary SHA-256 is
`fa1d5b6214ddc00f736a3dd044747d86d268fe04838ecad875bb912369c6853c`.
The relevant terminal code was rechecked in that binary rather than relying on
the previous 0.4.12 extraction.

- It uses xterm.js in a WebKit view, with an optional WebGL renderer. All stored
  terminal-preference records inspected selected `renderer: "webgl"` and font
  size 14. That is the selected preference, not proof that every active WebGL
  context initialized successfully.
- Each WebSocket output chunk invokes `terminal.write`; parsing completion
  advances the output acknowledgement counter. Acknowledgements flush after
  65,536 newly parsed bytes or a 250 ms trailing timer. This timer is **not** a
  fixed typing delay: rendering does not wait for the acknowledgement timer.
- Hook 0.4.9's `ptyFlowControl.wait` applies backpressure above 1 MiB of
  unacknowledged output, waiting for the backlog to fall to 256 KiB. Thus slow
  client parsing can eventually stall further output delivery. This behavior is
  binary-derived; saturation was not observed in the fast-reader fixture.
- Large pixel surfaces, xterm/WebKit rendering work, bursty application redraws,
  the desktop's relay/network path, and backlog remain candidates. The local
  synthetic measurements do not distinguish these in the user's live session.

The next discriminating manual test is switching Moshi's **Hardware acceleration**
setting off while retaining the same large terminal and workload, then comparing
typing/scrolling. The embedded settings implementation replaces the renderer
without disconnecting the terminal. This is an A/B test, not a recommendation
that the DOM renderer is generally faster. No production setting was changed by
this investigation.

If large redraw traffic is confirmed, a bounded coalescing experiment on the
ANSI forwarding path could reduce thousands of tiny writes/messages while
preserving byte order, input responsiveness, and backpressure. Do not add a
fixed frame delay or another full-screen diff layer before measuring that case.
Native Herdr surface patches remain a separate useful optimization.

## Follow-up: factory-midi.md and intermittent behavior

The user narrowed the symptom to scrolling
`zoom-l6-max-study/analysis/factory-midi.md`, hosted on this Mac. Disabling
hardware acceleration made no noticeable difference, and the slowdown later
disappeared without an identified fix. Treat the cause as **unresolved and
intermittent**; neither WebGL nor a deterministic server-side stall was proven.

The file has 229 lines, 11,244 bytes, 50 table rows, and a longest line of 132
characters. Live read-only inspection found Tree-sitter highlighting and
`render-markdown.nvim` decorations, wrapping, conceal level 3, indent folding,
cursor-line highlighting, and a split containing a terminal. At inspection time
the editor reported only its 216×54 local UI, not an observable large Moshi UI.
No live scrolling or settings changes were injected.

An additional disposable fixture loaded the exact file, installed local
Markdown parsers, Gruvbox, and the Markdown renderer's normal plugin entrypoint.
It verified nonzero render-markdown extmarks (44 at the small size, 410 at the
large size). It used one full-width file window and enabled relative numbers,
cursor-line highlighting, and column 72. These are selected configuration
features, not a recreation of every plugin or the live split layout.

Sixty input-driven scroll actions, thirty down and thirty up, moved three lines
each. A fixed marker in the command area identified output arrival; a 25 ms
settling interval also collected delayed decoration output. The fixture's
acknowledgements were immediate and no desktop renderer was involved.

| File-window test size | Input-to-marker median / p95 | Median ANSI bytes per action, including settling interval |
| --- | ---: | ---: |
| 120×36 | 1.61 / 2.51 ms | 14,091 |
| 480×144 | 7.32 / 17.49 ms | 140,998 |

The 60 actions generated 0.78 MB and 4.96 MB respectively; these totals include
actions near document boundaries and are not obtained by multiplying the
median. This is substantially more output than the plain-text synthetic scroll
test above. Decoration/gutter/style changes can require repainting more than a
newly exposed row even though the transport is incremental.

Disabling relative numbers or cursor-line highlighting independently barely
changed the large-case volume. Removing the color column reduced it to about
132 KB/action; removing all three reduced it to about 127 KB/action and did not
improve the measured median latency. These runs do not justify changing the
user's editor preferences. An earlier trial omitted the plugin entrypoint and
therefore is not evidence about active Markdown rendering; only the final
extmark-verified results are reported here.

This establishes a screen-size-sensitive traffic source, **not** the cause of
the intermittent stall. During the next occurrence, capture these concurrently:

1. A bounded read-only editor RPC latency and editor process CPU/stack sample,
   to distinguish a blocked Neovim/plugin path from quick editor responses.
2. The actual connected UI sizes/identities and Moshi terminal transport mode.
3. Moshi WebView processing time plus output bytes awaiting acknowledgement,
   to distinguish a renderer/parser backlog from the relay/network path.

Compare the same large window and file after recovery. Avoid restarting services
or changing multiple options before collecting the slow-state evidence.
Final exact-file artifact: `/private/tmp/neordr-markdown-gutters.json`;
diagnostic script: `/private/tmp/neordr-markdown-gutters.py`.

## Reproduction

```sh
NEORDR_BIN=target/release/neordr python3 tests/terminal_performance.py \
  --output /tmp/neordr-terminal-performance.json
```

Use `--modes gateway-whole` for just the full-editor gateway path. This is an
opt-in measurement tool, not a timing-based CI assertion. It generates genuine
whole-viewport changes only in its disposable editor, so it briefly adds load.

Full-run data: `/private/tmp/neordr-screen-bench-v2.json`.
Reusable-script validation: `/private/tmp/neordr-terminal-benchmark-validation.json`.
The earlier binary inspection artifacts and new desktop fragment are in
`/private/tmp/moshi-flow.asm` and
`/private/tmp/moshi-desktop-0413-terminal.js`.

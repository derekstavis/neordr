# Moshi and Herdr compatibility update, November 3, 2026

This compares the installed moshi-hook 0.4.47 and latest published Herdr
v0.9.5 with Neordr's previous October contract pin and Moshi 0.4.30 audit.
The installed hook was already 0.4.47; no live service or installation was
updated. The Herdr client used for testing was downloaded separately and
verified against the release asset's SHA-256.

## Herdr

The source pin advances from `5da0a01e1eedda054db0c81dd3a780000c40d9f0`
to the v0.9.5 release commit `2ecf19a92fc3a7fff75b6ce3655ef04f4625ec01`.
The API schema and endpoint/scroll schema files are unchanged between these
revisions; changes to `wire.rs` remove an internal crossterm conversion and
adjust upstream tests. This comparison does not imply all Herdr operations are
implemented by Neordr. Upstream still uses protocol 22 and endpoint generation 1.

The bundled Claude manifest adds detection of named-session question footers,
and the Codex manifest recognizes the newer update picker alongside the legacy
dialog. Neordr's integration has regression screens for both. Upstream also
changes other agent manifests, process identification, session handling and
client behavior; Neordr does not thereby add support for every upstream agent.
Herdr 0.9.4 changed server SIGHUP behavior and 0.9.5 removed its Codex turn
hooks in favor of screen detection; these are upstream application changes, not
changes to Neordr's own service lifecycle or Moshi hooks.

The Herdr development branch at `ee5bfb50` is newer than the stable release. Its
operation-schema refactor replaces the literal `Method` enum with a macro;
Neordr's extractor currently requires that enum. This is a source-level
compatibility issue for a future pin, not a tested wire break in v0.9.5. Review
the macro's upstream definitions and update the extractor before adopting a
release that contains it.

Sources: [Herdr v0.9.4](https://github.com/herdrdev/herdr/releases/tag/v0.9.4),
[Herdr v0.9.5](https://github.com/herdrdev/herdr/releases/tag/v0.9.5), and
[upstream comparison](https://github.com/herdrdev/herdr/compare/5da0a01e1eedda054db0c81dd3a780000c40d9f0...v0.9.5).

## Moshi

Since 0.4.30, upstream added E2EE, incremental Chat View messages and reconnect,
inline visuals, experimental Rex support, Herdr Core support, and in 0.4.47 a
pane minimap and immediate Claude messaging. It also reports multiple
Claude/Codex discovery, question, and conversation fixes. `moshi diff` was
removed in 0.4.31. These are publisher release-note claims; the presence of a
feature in Moshi does not establish that it works through Neordr. Neordr
continues to identify as the Herdr adapter, not Herdr Core or Rex. The new
minimap, immediate-send, E2EE, inline visuals, and phone/native-client rendering
are not covered by the synthetic test suite and require separate validation.
Neordr implements `pane.layout`, but represents each terminal as one virtual
pane; that alone does not establish that Moshi's minimap shows the intended
Neovim split layout.

The local compatibility suite exercises the actual hook and gateway with
fabricated agents and isolated Neovim, not real model calls or phone pairing.
At 32 columns, inferred plan titles remain truncated because exact soft-wrap
boundaries are unavailable to Neordr; answer selection passed for the fixture.

Sources: [Moshi 0.4.31](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.31),
[0.4.32](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.32),
[0.4.35](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.35),
[0.4.36](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.36),
[0.4.38](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.38),
[0.4.44](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.44), and
[0.4.47](https://github.com/rjyo/homebrew-moshi/releases/tag/v0.4.47).

## Validation

- `bash scripts/check.sh` passed on macOS with the v0.9.5 pin, including the
  isolated launchd lifecycle test. The new targeted agent-screen test passed.
- The official Herdr 0.9.5 macOS ARM64 binary (SHA-256
  `fb77c2232a05a8035421382c894392f4174e4e38a18e48af3dd0290157c3a7e2`)
  passed `tests/herdr_smoke.py`, including CLI requests, rendering, input,
  modal prompts, resize, detach, and stale-input handling.
- `bash scripts/check-moshi.sh` passed with moshi-hook 0.4.47, including the
  synthetic approval lifecycle and gateway tests.
- `tests/activity_smoke.py --moshi` passed through the isolated Moshi gateway.
- `tests/moshi_web_smoke.py` exercises the actual bundled web UI with synthetic
  agents, including chat switching, transcript streaming/reload, terminal
  input, and a synthetic command approval's visible completion. It does not
  establish the behavior of asynchronous question cards or native clients.
- The separate strict `NEORDR_APPROVAL_CONCURRENCY=3` diagnostic reproduced the
  existing race on 0.4.47: three HTTP 200 responses and `yyy` for one action.
  This is an expected failing diagnostic, not a passing compatibility test.

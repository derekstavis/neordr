# Herdr contract source

Neordr's contract definitions come from the unmodified
[Herdr](https://github.com/herdrdev/herdr) Git submodule at
`vendor/herdr`, pinned to `5da0a01e1eedda054db0c81dd3a780000c40d9f0`.
Herdr is licensed under Apache-2.0;
retain [its license](../vendor/herdr/LICENSE) with distributions.

Initialize a fresh checkout with:

```sh
git submodule update --init vendor/herdr
cargo build --offline
```

Offline builds require the usual Cargo dependencies to have been fetched once.
The build itself makes no network requests and never builds Herdr's application,
terminal engine, or dependencies. Only Neordr's ordinary dependencies are built.

`contracts.rs` parses the upstream Rust AST and emits `herdr_contracts.rs` under
Cargo's `OUT_DIR`. The complete JSON API schema module tree is included, along
with every wire/endpoint struct, enum, alias and constant. Serde metadata,
variant/field order, helpers, custom serializers, and defaults remain upstream
code. The generator relocates crate paths; it excludes upstream test modules and
runtime-only conversions. Small supporting types are extracted by name from
upstream config, input, agent-resume, and popup-size modules. Missing types or
new external dependencies fail compilation; there is no untyped fallback.

The optional `surface_scroll` codec is also extracted unchanged, including its
framing helpers. Neordr computes changed cell spans and retains a baseline per
connection; upstream code detects and encodes scroll shifts. Its wire layout is
not reimplemented locally. A narrowly scoped Clippy allowance accommodates the
pinned codec's `chunks_exact` implementation on our newer Rust toolchain.

The submodule must be clean at the reviewed revision. To update the contract,
review the upstream changes, move the submodule pin and the generator's revision
together, then run contract and real-client compatibility tests. Never patch
schema types inside the submodule or check generated Rust into the repository.

## Agent activity detection

`detection.rs` also extracts Herdr's agent labels, detection engine, version
parser, and bundled manifests from the same pinned, unmodified source. The
adapter changes only manifest loading: Neordr uses bundled rules and does not
fetch remote manifests or read Herdr detection overrides. Rule matching, regex
compilation, screen regions, and viewer exclusions remain upstream code.

`src/activity.rs` supplies the terminal's last 80 lines (bounded to 64 KiB) and
OSC title. Unmatched screens and history viewers retain the previous known
state rather than declaring a completion. The owning editor stores transitions
in `lua/neordr/backend.lua`; observations are checked against snapshot revision,
title, generation, and the previous observation before being applied. Process
identity changes reset completion state. Stable reads do not increment activity
order or require a metadata update RPC. Transition order uses Unix microseconds
with an editor-local monotonic guard, allowing ordering across editors on one
host and preserving order across bridge/router restarts.

Detection runs when clients request agent-bearing snapshots/lists. It is not a
background transcript watcher: a turn entirely between observations can be
missed. “Done” means a witnessed transition to idle while the terminal buffer
isn't focused in its editor; focusing that buffer acknowledges completion.
Unsupported agents or absent signals remain unknown. No agent hooks or user
Codex/Claude settings are required.

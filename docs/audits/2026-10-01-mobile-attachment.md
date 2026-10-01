# Mobile Mosh attachment deadlock

Moshi mobile could establish Mosh but the terminal then stopped responding.
The router and live bridges responded, and the affected editor reported terminal
mode with `blocking=false`. Multiple retries had left attached mobile UIs.

Concurrent stack samples of one failed connection showed:

- `mosh-server` blocked in `swrite` / `write` toward its child PTY.
- Neordr blocked in `Connection::write` / `poll` forwarding terminal output.
- `nvim --remote-ui` waiting in `flush_buf` for its output to drain.

The September fix drained inner-PTY output while forwarding input. It did not
drain outer-PTY input while forwarding output. Mosh's synchronous write and
Neordr's output wait could therefore prevent each other from making progress.
The current editor process itself was responsive; restarting it is unnecessary.

`Connection` now buffers outer input during write backpressure, up to 4 MiB.
The main loop consumes those bytes through the existing gesture decoder before
reading newer input, preserving byte order. Exceeding the bound fails the
attachment rather than allocating without limit. Existing cancellation and
UI-only cleanup still apply.

The new `outer-write-before-read` case in `attach_lifecycle_smoke.py` models a
transport that sends a large input burst before reading any output, while the
inner peer likewise writes output before reading input. The installed `f9da940`
binary timed out. With the fix, both streams complete and match their expected
bytes. This is an isolated transport regression, not an end-to-end mobile test.

Validation on this Mac passed: `CARGO_NET_OFFLINE=true bash scripts/check.sh`,
`CARGO_NET_OFFLINE=true bash scripts/check-moshi.sh`, and
`cargo build --release --locked --offline`. The lifecycle suite also verified
that editor processes and terminal jobs survive attachment teardown.

Existing attachment processes retain their old code after binary replacement.
A fresh terminal connection is required to use the fix. No editor, bridge,
router, or agent restart is required.

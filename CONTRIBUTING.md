# Contributing to Neordr

Open issues and pull requests to contribute. Keep changes small enough to review and describe
both the user-visible behavior and the evidence that it works.

## First checkout

```sh
git clone https://github.com/derekstavis/neordr.git
cd neordr
git submodule update --init vendor/herdr
cargo build --locked --bin neordr --example herdr_contract_fixture
```

Use the pinned Rust toolchain, Neovim 0.12.5, and Python 3. Tests launch disposable
editors, processes, PTYs, and local sockets. See the
[development guide](docs/development.md) for the test matrix and optional clients.

## Before opening a pull request

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked -- --test-threads=2
```

Run the relevant integration tests for changes to input, terminal lifecycle,
attachment, or Moshi compatibility. State which client versions you used and
which optional tests you did not run. A successful compile is not proof of
compatibility with an installed client.

## Design expectations

- Keep Neovim responsible for editor state and terminal jobs. Preserve normal
  editing, terminal plugin ownership, and per-client detach behavior.
- Use the upstream-derived typed contracts. Do not copy schema definitions into
  Neordr or hand-edit the vendored Herdr source. Follow the
  [contract update procedure](build_support/README.md).
- Treat pane/session identity and input ordering as correctness boundaries.
  Stale input must not land in another terminal or in a shell after an agent exits.
- Exercise lifecycle fixes with a regression that checks real process or UI
  behavior. Use disposable fixtures; do not run destructive tests in live editors.
- Keep third-party binaries, personal configuration, logs, transcripts, and
  generated build files out of commits.
- Update user-facing documentation when commands, installation, or supported
  behavior change. Label compatibility limits precisely.

For bug reports, include Neordr, Neovim, and client versions; the exact action
that triggered the problem; expected and observed behavior; and a minimal
reproduction. Remove secrets and conversation content from logs before sharing.

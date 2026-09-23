<p align="center">
  <img src="site/assets/neordr-wordmark.svg" alt="Neordr" width="360">
</p>

<p align="center"><strong>Your editor. Your terminals. Wherever you connect.</strong></p>

<p align="center">
  <a href="https://github.com/derekstavis/neordr/actions/workflows/ci.yml">CI</a> ·
  <a href="https://github.com/derekstavis/neordr/releases">Releases</a> ·
  <a href="docs/installation.md">Installation</a> ·
  <a href="https://derekstavis.github.io/neordr/">Website</a>
</p>

Neordr connects **Neovim to Moshi and Herdr**. Keep your editor, terminal jobs,
and coding agents in Neovim; connect to them from Moshi's web, desktop, or mobile
clients, or from Herdr's own terminal interface.

The full editor stays visible. Selecting a terminal focuses its Neovim buffer
in input mode, so you can move between editing files, running commands, and
chatting with an agent without maintaining a second workspace layout.

**Private preview.** Repository access is required for source and release
downloads. Interfaces are still evolving; see the compatibility limits below.

## What it does

- **Reconnect to your editor.** Discover active Neovim sessions and attach a
  remote UI while terminal jobs keep running.
- **Use Moshi Chat View.** Run Claude or Codex in managed Neovim terminals.
  Existing Moshi hooks discover the agent and stream its conversation.
- **Manage terminals remotely.** Create, name, split, reorder, focus, and close
  terminals and workspaces from the sidebar.
- **Keep your Neovim workflow.** Edit files alongside terminals; integrate your
  terminal plugin through Lua callbacks or use the built-in terminal command.
- **Connect with Herdr.** Its interactive interface renders Neovim with native
  Herdr navigation and terminal selection.
- **Use remote gestures.** Switch terminals and workspaces, focus splits, zoom,
  and detach through Herdr's default Ctrl-B shortcuts.

## Get started

Install the binary **and its Lua integration** using the
[installation guide](docs/installation.md). Release archives target Linux and
macOS on both x86-64 and AArch64. Neordr requires Neovim 0.12 or newer; the
verified Neovim version is **0.12.5**.

For the release layout installed under `~/.local`, add this to `init.lua`:

```lua
vim.opt.runtimepath:prepend(vim.fn.expand('~/.local/share/neordr'))
require('neordr').setup({ bin = vim.fn.expand('~/.local/bin/neordr') })
```

Restart Neovim, run `:NeordrTerm`, and start `claude`, `codex`, or your usual
shell command. In another terminal:

```sh
neordr session list
neordr session attach SESSION_NAME
```

Detach with **Ctrl-B, q** or Neovim's `:detach`; the editor and jobs continue
running. `:NeordrStatus` shows the session name and Neovim socket.

To connect Moshi, install its normal host service and agent hooks, then opt into
the [Herdr compatibility alias](docs/installation.md#connect-moshi). Moshi displays
Neordr sessions under **Herdr**. Start agents in newly created managed terminals
so their process environment carries the correct session identity.

## How it fits together

```mermaid
flowchart LR
  M[Moshi and moshi-hook] --> R[Neordr session router]
  R --> B[Per-editor bridge]
  H[Herdr interactive client] --> B
  B --> N[Neovim RPC and remote UI]
  N --> E[Editor buffers]
  N --> T[Terminal jobs: shells, Claude, Codex]
```

Neovim owns editor state, terminal processes, and rendering. Neordr maps Neovim
tabpages to workspaces and terminal buffers to virtual Herdr tabs and panes.
Moshi reads the agents' own transcripts; Neordr does not parse conversations or
run a model.

The complete Herdr API and endpoint schema is generated from an **unmodified,
pinned upstream Git submodule**. Dispatch, routing, and endpoint messages use
those types without maintaining a second schema or building Herdr's terminal
engine. Full schema coverage does not imply support for every Herdr operation.
See [contract generation](build_support/README.md) and the
[architecture notes](docs/design.md).

## Compatibility and limits

| Area | Current behavior |
| --- | --- |
| Moshi | Discovery, Term view, Claude/Codex Chat View, terminal management, and local approval flows. Hook compatibility tested with 0.3.21 and 0.3.26. |
| Herdr interactive UI | A named Neordr session; full editor rendering, terminal selection, creation, input, resizing, and detach. [Compatibility details](docs/herdr-interface.md). |
| Multiple clients | All clients share Neovim's focus and layout. The smallest attached UI constrains the editor size. |
| Agent status | Foreground Claude/Codex detection; activity status remains `unknown`. |
| Capture | Visible and bounded recent terminal text. Exact soft-wrap reconstruction and ANSI capture are unavailable. |
| Herdr features | Supported operations are a subset. Herdr multi-pane tab groups, plugins, and its private direct-terminal protocol are not implemented. |
| Platforms | macOS and Linux targets; no Windows support. Published build and test results are recorded in CI. |

Session and pane identities include the owning editor's generation. Stale
targets fail instead of silently selecting another terminal. The default Moshi
router also rejects Chat View input after a supported agent exits.

See [operating notes](docs/local-setup.md) for shortcuts, socket ownership,
terminal adapters, remote-client lifetime, and known client limitations.

## Build and contribute

```sh
git clone git@github.com:derekstavis/neordr.git
cd neordr
git submodule update --init vendor/herdr
cargo build --release --locked
```

Only the top-level Herdr submodule is needed. If you already cloned without
submodules, run `git submodule update --init vendor/herdr` before building.
Rust tooling uses the repository's pinned toolchain.

Start with [CONTRIBUTING.md](CONTRIBUTING.md) and the
[development guide](docs/development.md) for tests, architecture boundaries, and
release conventions. Third-party client binaries are supplied separately and
are not redistributed with Neordr.

Neordr is an independent project. Thanks to
[Neovim](https://neovim.io/), [Herdr](https://github.com/herdrdev/herdr), and
[Moshi](https://getmoshi.app/) for the interfaces that make it possible.

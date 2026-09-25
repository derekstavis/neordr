# Installation

Neordr consists of a Rust executable and a Neovim plugin in the same repository.
Install the binary and choose one plugin installation method below. Moshi and Herdr are optional clients, installed separately.

## Requirements

- Linux or macOS, on x86-64 or AArch64 / Apple Silicon.
- Neovim **0.12+** on `PATH`. Testing uses **0.12.5**. Neordr depends on
  `--remote-ui`, `:detach`, and the experimental `nvim__exec_lua_fast` API;
  older Neovim releases are not supported.
- Access to the private `derekstavis/neordr` repository during the preview.
- For release downloads: GitHub CLI (`gh`) authenticated to an account with
  repository access. For source builds: Git and Rust via rustup.

## Install a release

Check the [release list](https://github.com/derekstavis/neordr/releases) for a
published version. Release archives include the binary, Lua files, documentation,
and license notices. Select the target matching the **host running Neovim**:

| Host | Target |
| --- | --- |
| Linux, Intel / AMD 64-bit | `x86_64-unknown-linux-gnu` |
| Linux, ARM 64-bit | `aarch64-unknown-linux-gnu` |
| macOS, Intel | `x86_64-apple-darwin` |
| macOS, Apple Silicon | `aarch64-apple-darwin` |

Linux builds use Ubuntu 24.04 and target glibc 2.39 or newer; these are not
musl/Alpine binaries. macOS binaries target macOS 13 or newer. Neovim itself
must also support your operating-system version.

The example below selects Apple Silicon. Set `version` and `target` for your
release and machine, then run in a temporary directory:

```sh
version=v0.1.0
target=aarch64-apple-darwin
archive="neordr-${version}-${target}.tar.gz"

gh auth status
gh release download "$version" --repo derekstavis/neordr \
  --pattern "$archive" --pattern SHA256SUMS
```

Verify the downloaded archive against `SHA256SUMS` before extracting it. On
macOS:

```sh
shasum -a 256 --check --ignore-missing SHA256SUMS
```

On Linux:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

Check that your selected archive is reported as `OK`. The checksum manifest
comes from the same release; it verifies archive integrity, not an independent
publisher signature.

```sh
mkdir package
tar -xzf "$archive" -C package --strip-components=1
mkdir -p "$HOME/.local/bin"
install -m 755 package/bin/neordr "$HOME/.local/bin/neordr.new"
mv "$HOME/.local/bin/neordr.new" "$HOME/.local/bin/neordr"
```

Add `~/.local/bin` to your shell's `PATH` if needed:

```sh
export PATH="$HOME/.local/bin:$PATH"
neordr --version
```

The release workflow packages binaries; it does not currently provide Apple
notarization. The binary is replaced atomically so a rebuild or upgrade cannot
truncate an executable that is already running.

## Install the Neovim plugin

Choose **one** method: a plugin manager, the release archive, or a source
checkout. Do not leave a second copy of `lua/neordr` on `runtimepath`; Neovim may
load an older module before your updated plugin.

Plugin-manager installation uses the repository's `lua/` and `doc/` directories.
It needs neither Cargo nor initialized Git submodules. The executable is
installed separately by the previous step; there is no build hook or automatic
binary download.

The examples below select `main` to include the new diagnostics and help files.
These additions are compatible with the `v0.1.0` binary, but are not part of
the `v0.1.0` plugin tag. For a released installation, use the
same tag for the plugin and binary: `version = 'v0.1.0'` with `vim.pack`, or
`tag = 'v0.1.0'` instead of `branch` with `lazy.nvim`. Development versions may
change, so update the binary when the plugin's compatibility check requests it.

During the private preview, SSH must authenticate as a GitHub account with
repository access. GitHub CLI authentication for release downloads does not
necessarily configure your Git SSH credentials.

### vim.pack

Add this to `init.lua` before configuring plugins that start terminal jobs:

```lua
vim.pack.add({
  { src = 'git@github.com:derekstavis/neordr.git', version = 'main' },
})
require('neordr').setup()
```

See [Neovim's package documentation](https://neovim.io/doc/user/pack/) for
installation and update behavior.

### lazy.nvim

Add this plugin specification to your existing `lazy.nvim` configuration:

```lua
{
  url = 'git@github.com:derekstavis/neordr.git',
  branch = 'main',
  submodules = false,
  lazy = false,
  main = 'neordr',
  opts = {},
}
```

`opts` calls `require('neordr').setup()` at startup. Keep `lazy = false` so the
editor is discoverable before any Neordr command is invoked. If another plugin
starts terminal jobs during setup, make Neordr its dependency and use the
[terminal adapter contract](local-setup.md#terminal-plugin-adapters).
See [lazy.nvim's specification](https://lazy.folke.io/spec).

### Release archive without a plugin manager

After extracting the archive in the binary installation step:

```sh
mkdir -p "$HOME/.local/share/neordr"
cp -R package/share/neordr/. "$HOME/.local/share/neordr/"
```

Add this to `init.lua`:

```lua
vim.opt.runtimepath:prepend(vim.fn.expand('~/.local/share/neordr'))
require('neordr').setup({ bin = vim.fn.expand('~/.local/bin/neordr') })
```

New release archives include the help index; plugin managers generate it
automatically. When copying help files by hand, regenerate the index with
`:helptags ~/.local/share/neordr/doc`. The `v0.1.0` archive predates the help file.

### Verify setup

The default executable is `neordr` on Neovim's `PATH`. If your editor inherits a
different environment from your shell, set `bin` to its absolute path in
`setup` (or in `opts` for `lazy.nvim`).

The setup starts a bridge for this editor and ensures the shared router is
running. Internal bridge names are generated from the initial directory and editor
identity. Editors appear as workspaces in the default session.

Restart Neovim and run `:checkhealth neordr`. The health command and
`:help neordr` require the development plugin until the next release. Health
checks inspect readiness without restarting your editor or terminal jobs.

Run `:NeordrTerm` to open a managed shell in a split, then `:NeordrStatus`
to see its session name. In another terminal:

```sh
neordr session list --json
neordr --session default rpc pane.list
neordr session attach default
```

`default` is the session returned by `session list`. Attachments share your
editor's state. Detach with Ctrl-B, q or `:detach`; quitting Neovim stops its
terminal jobs.

## Connect Moshi

Install and configure Moshi's host service and Claude/Codex hooks following
[Moshi's documentation](https://getmoshi.app/docs/herdr). Neordr uses the
existing binaries and agent transcript readers.

Claude profiles using `CLAUDE_CONFIG_DIR` need Moshi hooks in that profile's
`settings.json` too. Hooks installed in `~/.claude/settings.json` do not cover
another configuration directory. Without them Moshi can detect the Claude
process while Chat View has no conversation ID or transcript to display.
Check the profile used by the agent, rather than only the shell's default profile.

Codex builds using a shared app-server daemon may leave Moshi unable to associate
the terminal process with its transcript. For builds supporting the `thread-id`
title item (verified with Codex 0.157.0), add this to the existing `[tui]` section
in `~/.codex/config.toml`:

```toml
[tui]
terminal_title = ["thread-title", "project-name", "thread-id"]
```

Keep `thread-id` last: moshi-hook 0.3.26 recognizes a trailing conversation UUID
in the terminal title. Check the setting with `codex doctor`, then restart/resume
the Codex frontend. A running frontend may retain its previous title settings.
This uses Codex's [terminal title configuration](https://learn.chatgpt.com/docs/config-file/config-reference),
without changing Moshi's transcript files or parsing conversations in Neordr.

Moshi expects a command named `herdr`. If that path is available, create the
compatibility alias:

```sh
ln -s neordr "$HOME/.local/bin/herdr"
command -v herdr
herdr --version
```

This deliberately does not overwrite an existing file. If you already use
Herdr, keep its real executable separately and arrange your Moshi host's `PATH`
so it resolves the intended alias. Neordr's router and a real Herdr server
cannot own the same default Herdr socket at the same time.

Reconnect Moshi to the host. The default session appears in its **Herdr** picker;
its sidebar contains one workspace per Neovim editor.
Create a new managed terminal, start `claude` or `codex`, and select **Chat** to
use Chat View or **Term** for the complete editor. Agent processes started
before integration was enabled do not acquire its environment retroactively.

## Connect Herdr's interactive interface

Keep an actual Herdr executable at a separate path from the Moshi alias:

```sh
/path/to/herdr --session default
```

Herdr supplies its sidebar and terminal interface around Neovim's full editor.
The default session serves both Moshi's JSON API and Herdr's interactive UI.
See [Herdr interface compatibility](herdr-interface.md) for supported versions
and input behavior. Herdr is not included in Neordr release archives.

## Build from source

```sh
git clone git@github.com:derekstavis/neordr.git
cd neordr
git submodule update --init vendor/herdr
cargo build --release --locked
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/neordr "$HOME/.local/bin/neordr.new"
mv "$HOME/.local/bin/neordr.new" "$HOME/.local/bin/neordr"
```

Use the source checkout as the Lua runtime instead of the release path:

```lua
vim.opt.runtimepath:prepend(vim.fn.expand('~/path/to/neordr'))
require('neordr').setup({ bin = vim.fn.expand('~/.local/bin/neordr') })
```

Only `vendor/herdr` needs initialization; its nested engine submodules are not
built. The generator requires the reviewed, clean upstream revision. See
[contract source](../build_support/README.md) for offline builds and updates.

## Terminal plugins and custom sockets

The built-in `:NeordrTerm` works without another terminal plugin. To preserve an
existing plugin's panel and job lifecycle, configure the callbacks in the
[operating notes](local-setup.md#terminal-plugin-adapters).

To bridge an already running editor with a custom `--listen` socket, run:

```sh
neordr router --background
neordr --session my-editor serve --nvim /path/to/nvim.socket
```

The bridge runs in the foreground until interrupted or the editor exits.
`neordr discover --socket /path/to/nvim.socket --json` validates that socket;
discovery alone does not register it as a Herdr session.

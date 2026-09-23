#!/usr/bin/env bash
# Generic, disposable tests; no Moshi/Herdr downloads or personal configuration.
set -euo pipefail
cd "$(dirname "$0")/.."
export TERM=xterm-256color
unset NEORDR_NVIM_CONFIG NEORDR_CONFIG_OVERLAY NVIM HERDR_SESSION HERDR_ENV HERDR_PANE_ID
command -v nvim >/dev/null
nvim --headless -u NONE -i NONE '+lua assert(vim.fn.has("nvim-0.12") == 1, "Neovim 0.12+ required")' +qa
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --test-threads=2
cargo build --locked --bin neordr --example herdr_contract_fixture
export NEORDR_BIN="$PWD/target/debug/neordr"
for test in plugin remote_ui default_attach gestures router_lifecycle; do
    python3 "tests/${test}_smoke.py"
done
NEORDR_SKIP_MOSHI=1 python3 tests/split_smoke.py

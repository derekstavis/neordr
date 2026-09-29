#!/usr/bin/env bash
# Opt-in tests for an installed moshi-hook. Uses isolated synthetic agents only.
set -euo pipefail
cd "$(dirname "$0")/.."
export TERM=xterm-256color
unset NVIM HERDR_SESSION HERDR_ENV HERDR_PANE_ID
"${MOSHI_HOOK_BIN:-moshi-hook}" version
cargo build --locked --bin neordr
export NEORDR_BIN="$PWD/target/debug/neordr"
for test in moshi approval moshi_interactions moshi_create; do
    python3 "tests/${test}_smoke.py"
done

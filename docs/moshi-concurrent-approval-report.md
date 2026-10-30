# Draft upstream report: concurrent approval answers delivered more than once

Do not submit without reviewing the final wording and reproduction environment.

## Summary

When three clients submit the same pending Codex approval concurrently, the local
Moshi gateway accepts all three requests and sends the answer to the terminal
three times. Sequential duplicate requests are rejected by the normal lifecycle
fixture. The concurrency issue was observed with moshi-hook 0.4.30 and again
with **0.4.43 on macOS on October 30, 2026**.

## Reproduction

With Neordr's disposable test fixture, Neovim 0.12.5, and moshi-hook on PATH:

```sh
cargo build --locked --bin neordr
NEORDR_BIN=target/debug/neordr NEORDR_APPROVAL_CONCURRENCY=3 \
  python3 tests/moshi_approval_lifecycle_smoke.py
```

The fixture starts an isolated gateway, Neovim, Neordr, and synthetic Codex
processes. It submits three concurrent `/v1/approvals/answer` requests for the
same source, session, action ID, and decision. No model call or cloud pairing
is involved.

**Expected:** one successful submission, two rejected duplicates, and one `y`
in the target terminal.

**Observed on 0.4.43:** all three requests returned HTTP 200 and the target
received `yyy`. The strict fixture failed on that result.

Neordr receives ordinary Herdr `pane.send_keys` requests without the approval
action ID, so it cannot safely distinguish duplicate approval deliveries from
intentional repeated typing. The approval owner needs to claim or serialize the
action before sending terminal input. This is separate from the question-card
issue described in the [approval audit](audits/2026-10-16-codex-approval-completion.md).

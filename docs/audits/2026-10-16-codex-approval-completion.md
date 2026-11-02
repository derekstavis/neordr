# Codex cards remaining in “Submitting”, October 16, 2026

## Finding

The reported live reproduction is an answered asynchronous question in Moshi
Chat View. Codex receives the answer and continues, but the client keeps showing
“Submitting”. The installed hook is 0.4.30; the native Mac app reports 0.5.20.

Read-only inspection of the affected chat established:

- Both answers are present in Codex's original transcript.
- The live `/events` agent watch reports `working`, with neither `pendingPrompt`
  nor `pendingApproval` present.
- The live transcript websocket returns the later answer as a user-message
  record. The bounded backlog read did not reach the earlier question; its
  presence was verified in the original transcript instead.
- Gateway logs record successful question-answer submission for this chat.

This locates the observed question-card problem after input delivery and pending
state clearance, in Moshi's client handling of completion. It does not establish
the exact native-client code defect. No native UI automation surface was
available, and the native client source was not recovered.

No Neordr production defect was established, so no production workaround,
transcript rewrite, settings change or server restart was made. Plan approvals
were also reported as affected, but the live reproduction inspected here was
the two asynchronous questions asked during this investigation.

## The transcript sequence that matters

Current Codex records these as three separate records:

1. `response_item` / `function_call`, name `request_user_input_async`, with a
   unique `call_id` and question arguments.
2. `response_item` / `function_call_output`, same `call_id`, output
   `{"accepted":true}`. This acknowledges opening the question, not answering it.
3. A later `response_item` / user `message` containing
   `<send_user_message_question_reply>`, with an array of `answer`, `question`
   and `questionItemId` objects. The question item identifies the original call
   and question index. In the observed records its value contains escaped JSON.

Moshi's transcript websocket transports these raw records. It does not turn
record 3 into a second function-call result. A client must correlate that later
reply to the corresponding question and settle its submitting state.

Static inspection of the web assets embedded in hook 0.4.30 found question-result
handling for other formats (`answers`, `results`, `UserAnswered`) but no reference
to `send_user_message_question_reply` or `questionItemId`. That is a useful
upstream compatibility lead, not proof that the separate native 0.5.20 client
has the same parser or the same failure mechanism.

## New repeatable coverage

`tests/moshi_approval_lifecycle_smoke.py` uses the installed hook, real local
gateway, Neordr router/bridge and disposable Neovim. Two synthetic Codex processes
exercise permission routing with distinct panes, sessions and input logs;
dedicated plan and question agents keep their synthetic histories separate.
There are no model calls or cloud pairing. The shared approval fixture now
supports multiple agents of the same type and always gives Neovim a private
state directory.

The normal suite covers:

- Twelve repeated command approvals/denials at 120 and 32 columns, including
  identical command text with distinct action IDs.
- Sequential duplicates and old action IDs rejected without extra keystrokes.
- Two simultaneous Chat status subscribers, a second pending Codex pane, and
  exact input isolation.
- Terminal repaint delayed by 0, 250 or 1,000 ms; approval clearance observed on
  the event stream; local dismissal; reconnect through `herdr:default`.
- Four plan answers across both options/widths, disappearance of the captured
  plan menu, and rejection after dismissal. This uses the existing synthetic
  plan hook/menu contract, not a real Codex plan session.
- Four asynchronous question answers, the exact accepted-then-user-reply
  transcript shape, unchanged answer delivery over append streaming and backlog
  after reconnect, and rejection after terminal dismissal.

In the isolated debug-binary run, permission-clearance frames arrived within 0.643 seconds
of submission. Moshi can retire a delivered approval before the simulated repaint
delay ends, so this measures coordinator clearance, not agent execution latency.

These checks protect the Neordr/gateway boundary. They do not assert that native
or web cards visibly leave “Submitting”, and must not be described as a UI fix.

Fixture correction: a generic `Plan approval` screen with `Enter to select`
does not contain Codex's recognized blocker chrome. During repeated transitions
it intermittently competed with idle/working detection, so the new plan fixture
also emits Codex's `Action Required` OSC title and switches it back to a working
title on dismissal. These are the signals recognized by Herdr's upstream Codex
manifest; no production detection rules were weakened. Dedicated plan and
question histories also avoid mixing unrelated synthetic lifecycle records.

The wider suite exposed stale expectations outside the new test: launched
agents already use `HERDR_SESSION=default`, and Moshi 0.4.30 separates multipart
paste blocks/trailing text and brackets the Codex attachment prompt. The tests
now check the observed exact bytes and existing session identity. The previous
paste expectation is retained for older hook versions; intermediate releases
were not revalidated in this investigation.

```sh
NEORDR_BIN=target/release/neordr python3 tests/moshi_approval_lifecycle_smoke.py
bash scripts/check-moshi.sh
```

Final validation: `bash scripts/check-moshi.sh` passed end to end against
moshi-hook 0.4.30, including the new lifecycle fixture, existing approval and
composer tests, terminal transports, workspace creation and legacy sessions.
`tests/activity_smoke.py --moshi` also passed. Python syntax, shell syntax and
`git diff --check` passed. The strict concurrent diagnostic below remains an
explicitly failing reproduction, separate from the standard gate.

## Separate confirmed defect: simultaneous answers

The strict concurrent diagnostic reproduced a Moshi 0.4.30 race twice: three
simultaneous `/v1/approvals/answer` requests with the same source, session, action
ID and decision all received HTTP 200. The second run recorded **`yyy`** in the
target terminal instead of one `y`. Sequential duplicates return 409 normally.

```sh
NEORDR_BIN=target/release/neordr NEORDR_APPROVAL_CONCURRENCY=3 \
  python3 tests/moshi_approval_lifecycle_smoke.py
```

This is an opt-in, strict diagnostic that is expected to fail against the
observed hook. It is not enabled in the ordinary compatibility gate. A fixed
hook should return one success, reject the other submissions, and deliver one
keystroke. The diagnostic continues to require that behavior rather than
accepting duplicated input as a passing result.

Neordr's upstream `PaneSendKeysParams` contains only `pane_id` and `keys`; there
is no approval action ID. Neordr cannot distinguish duplicate approval answers
from legitimate repeated typing. Deduplicating equal keystrokes or introducing
timing heuristics here would damage ordinary input. The approval owner in Moshi
needs to claim/serialize the action before delivering keys.

This race is independent of the single-answer “Submitting” reproduction; no
causal link between the two is claimed.

## Upstream report outline

For the question-card issue: Codex resumes, the answer is in the transcript
websocket, and the agent watch has cleared pending state, yet the native card
remains submitting. Reproduce with `request_user_input_async` and a later
`send_user_message_question_reply` user message. Check correlation by call ID
and question index, success/failure cleanup, and reconnect reconciliation.

For the separate race: submit the same pending approval concurrently and inspect
both HTTP results and terminal input. Require exactly one delivery even with
multiple connected clients.

No report was sent to a third party. Temporary investigation logs and extracted
bundled assets are under `/private/tmp/neordr-approval-audit`; the test fixture
uses only fabricated questions and answers.

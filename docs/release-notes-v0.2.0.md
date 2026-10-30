# Neordr v0.2.0 — draft release notes

This draft describes the intended release and must be checked against the final
release commit and platform test results before publication.

Neordr connects Neovim workspaces and their terminals to Moshi and Herdr. The
full editor remains in Neovim, and terminal jobs continue when clients detach.

## Changes since v0.1.0

- Group multiple Neovim editor workspaces in the default session, create a
  default workspace when needed, and launch workspaces through the login shell.
- Add Moshi control-mode transport, remote mouse and scroll support, activity
  reporting, and legacy conversation discovery through live agent sessions.
- Improve bridge recovery, attachment teardown under backpressure, process
  discovery, and terminal rendering performance.
- Add diagnostics, Neovim health checks and help, and optional router service
  management on macOS and Linux.
- License Neordr under MIT and include third-party notices in release archives.

## Requirements and limitations

Neovim 0.12 or newer is required; 0.12.5 is the tested version. Binaries target
macOS 13+ and Linux with glibc 2.39+ on x86-64 and ARM64. Install matching binary
and plugin versions. Moshi and Herdr are separate installations.

Clients share Neovim focus and layout, and the smallest attached UI constrains
editor size. Neordr implements a subset of Herdr operations. Terminal capture
does not reconstruct exact soft wraps. See the [README](../README.md) and
[approval compatibility](approval-compatibility.md) for details. The observed
Moshi question-card and concurrent-answer issues in the
[approval investigation](audits/2026-10-16-codex-approval-completion.md) require
attention. **With moshi-hook 0.4.43, simultaneous answers to the same approval
can be delivered more than once. Avoid answering the same approval from multiple
clients at once.** The question-card behavior needs revalidation against the
final clients.

## Validation to fill in before publishing

Record the final commit, four platform build/test results, archive checksums,
clean-install results, and client versions tested. Do not publish this draft
until the [launch checklist](public-launch-checklist.md) is complete.

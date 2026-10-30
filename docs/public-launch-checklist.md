# Public launch checklist

Working checklist for making Neordr public and announcing it. The intended first
public launch is a **general release**, version **v0.2.0**. Check items only after recording the
evidence or decision in the linked issue, release notes, or this document.

## 1. Decide what we are launching

- [ ] Choose the audience, preview label, release version, and support expectations.
  Decision so far: general release v0.2.0; support expectations remain open.
- [ ] Decide which uncommitted service and Moshi work belongs in the release;
  review and commit it, or deliberately leave it out of the release commit.
- [ ] Define the supported Neovim, Moshi, Herdr, and operating-system versions
  from actual validation. State the known limitations and workarounds.

## 2. Make the product release-ready

- [x] Investigate the reproducible macOS `control_smoke.py` stdout-close timeout
  and get the complete `scripts/check.sh` gate passing. The local macOS gate
  passed on October 30 after the control fix and a PTY fixture hang fix.
- [ ] Run the Moshi suite against the intended supported version and record the
  result. The synthetic suite passed locally with hook 0.4.43 on October 30.
- [ ] Verify real Claude and Codex question, approval, and reconnect flows in
  the clients we intend to recommend, including phone/cloud if claimed.
  Recheck the stuck “Submitting” card and concurrent-answer race described in
  the [approval audit](audits/2026-10-16-codex-approval-completion.md); document
  any unresolved upstream behavior and decide whether it blocks launch.
  On October 30, the strict concurrent diagnostic reproduced the race with
  moshi-hook 0.4.43: three HTTP 200 responses and `yyy` delivered for one action.
  Release decision: proceed with a prominent warning and advise users not to
  answer the same approval from multiple clients at once. The client card and
  phone/cloud paths remain unverified.
- [ ] If the router service ships, exercise install, status, restart, uninstall,
  and workspace survival on both macOS and Linux with real user service managers.
  The isolated launchd smoke test passed locally and on GitHub's macOS runner
  on October 30. The dedicated-account systemd test also passed on GitHub's
  Linux runner. Confirm these results for the final release commit.
- [ ] Verify a clean install and upgrade from the released archives, including
  matching plugin and binary versions, help, health checks, and uninstall.
  A provisional macOS ARM v0.2.0 archive was assembled locally on October 30;
  required files, MIT license, and its checksum passed inspection. Its metadata
  identifies HEAD while the tree is modified, so it is not a release artifact.
- [ ] Run the release workflow's tests and artifact checks on all four targets;
  inspect the archives and checksums and test representative downloads.
- [ ] Update the package, lockfile, and Lua plugin versions together; prepare
  release notes with compatibility, breaking changes, and known limitations.

## 3. Prepare the repository for public access

- [x] Choose and add a license for Neordr itself: MIT.
- [ ] Review third-party licenses, notices, generated contracts, and distribution
  obligations. Confirm the pinned
  Herdr submodule and everything needed for a clean build are publicly accessible.
- [ ] Review **all Git history**, tags, release assets, and tracked files for
  credentials, private URLs, personal data, proprietary material, and content
  we do not have permission to publish. Remove sensitive material from history
  where appropriate and revoke exposed credentials; deleting a current file alone
  does not remove it from history.
  October 30 screening scanned 268 reachable historical blobs for common private
  key, GitHub token, and AWS access-key formats with no matches. This is not a
  complete privacy, ownership, or credential review.
  The existing v0.1.0 release has four archives and a checksum manifest; review
  those assets and their licensing before they become public. The inspected
  macOS ARM archive has third-party licenses but no Neordr root LICENSE file.
- [ ] Review issues, pull requests, discussions, workflow logs, and repository
  settings for material that will become visible or need separate treatment.
- [ ] Update README, installation guide, website, and brand documentation to
  reflect public access, actual release versions, supported capabilities, and
  limitations. Replace private-repository authentication instructions where
  appropriate and check links from a signed-out session.
- [ ] Review contribution guidance, maintainer capacity, issue templates, and a
  private security-reporting contact or process. Decide how incoming reports
  and contributions will be triaged.
- [ ] Confirm CI permissions, branch protection, release automation, Pages
  configuration, and repository metadata are appropriate for public operation.
  Test cloning, submodules, building, and downloading without repository access.
  The October 15 CI job was blocked by billing. After the repository became
  public on October 30, its rerun passed on committed `main`; run CI again on
  the release commit.
  The `main` branch currently has no branch protection; choose suitable rules.

## 4. Prepare the announcement

- [ ] Write a concise announcement: who Neordr is for, what it does, how to try
  it, release status, requirements, and the most consequential limitations.
- [ ] Prepare an accurate demo or screenshots with disposable data and no private
  conversations, paths, tokens, or personal information. Show the full editor
  and the client experience without implying unsupported features.
- [ ] Choose channels and timing, identify a maintainer to respond, and prepare
  links to the repository, installation guide, release notes, and feedback route.
  Channels selected: Hacker News, Twitter/X, and Reddit. See the
  [communications plan](launch-communications.md).
- [ ] Proofread the announcement against the actual release and website; have
  someone follow the installation instructions from a clean environment.

## 5. Launch and follow through

- [x] Change repository visibility: made public on October 30, before the
  remaining release and publication checks were completed.
- [ ] Publish or verify the release and website, then check signed-out access,
  download checksums, installation links, and the public landing page.
- [ ] Publish the announcement in the chosen channels.
- [ ] Monitor CI, installation and compatibility reports, security reports, and
  feedback; triage launch issues and communicate fixes or known workarounds.

Changing visibility, publishing a release or website, and posting announcements
are separate launch actions; completing this checklist does not perform them.

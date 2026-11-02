# Launch communications (working draft)

Target: Neordr v0.2.0 general release. Confirm all claims and links against the
published release before sharing. Do not publish this draft as-is on Hacker News:
its [guidelines](https://news.ycombinator.com/newsguidelines.html) ask authors to
write their own text and not automate posting.

## Channels

- **Hacker News:** submit the working repository or release as a Show HN once it
  is public and usable. Write the title and any discussion yourself, following
  the [Show HN guidelines](https://news.ycombinator.com/showhn.html). Be available
  to answer technical questions.
- **Reddit:** r/neovim is the first choice because Neordr is specifically for
  Neovim users; its [feed](https://www.reddit.com/r/neovim/) includes plugin
  showcases. Check the current community rules and flair before posting; the
  rules page could not be read reliably during preparation. Explain the workflow
  and limitations, and include a demonstration and repository link.
- **Twitter/X:** share a short description, demo, and repository link. Reply with
  installation requirements and release notes if they do not fit in the post.

## Facts to cover

- The full Neovim editor and its terminal jobs remain on the host; users connect
  through Moshi or Herdr and can detach and reconnect.
- Neordr provides compatible interfaces; Moshi and Herdr are separate projects.
- The release needs Neovim 0.12 or later, plus the Neordr binary and Lua plugin.
- macOS and Linux builds target x86-64 and ARM64. The release notes and
  installation guide should state the precise platform requirements.
- Clients share Neovim's focus and layout, and Neordr supports a subset of Herdr
  operations. Include any unresolved approval and client limitations.

## Demonstration

Record a clean, disposable workspace: edit a file, start a terminal, connect
from a client, switch between the editor and terminal, detach, and reconnect.
Use fabricated content and inspect every frame for personal data before sharing.

## Before posting

Verify public links and downloads while signed out, complete the release checks,
and ensure someone is available to respond to installation and bug reports.
Do not solicit votes or coordinate engagement on Hacker News.

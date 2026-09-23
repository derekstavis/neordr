# Neordr identity

Neordr's identity centers on the editor: an original geometric **N** built from
two vertical panes and a diagonal connection. Two small ivory squares suggest
adjacent buffers. It remains legible at favicon sizes and does not reuse any
Neovim, Herdr, or Moshi logo.

## Assets

- [`site/assets/neordr-mark.svg`](../site/assets/neordr-mark.svg): square mark,
  64 × 64 viewBox; use at 24px or larger when possible.
- [`site/assets/neordr-wordmark.svg`](../site/assets/neordr-wordmark.svg): horizontal
  lockup, 260 × 64 viewBox. All letters are vector paths, with no font dependency.
- [`site/`](../site/): static landing page, with local assets and no JavaScript,
  trackers, external fonts, or build toolchain.

The wordmark is designed for light backgrounds. Keep clear space of at least
one small square around the mark. Keep proportions intact; do not stretch,
add shadows to, or recolor the mint/ink lockup on busy backgrounds.

## Palette

| Role | Color | Hex |
| --- | --- | --- |
| Primary text, editor surface | Deep ink | `#102821` |
| Page background | Warm ivory | `#F5F2E9` |
| Accent, active state | Mint | `#A6EDBD` |
| Secondary text on ivory | Moss | `#53675E` |
| Dividers | Pale sage | `#CED4C8` |

Mint belongs on ink or as a background with ink text; do not use it as small text
on ivory. Color is paired with labels, borders, or shape rather than being the
only way to distinguish state. System sans-serif handles body text and headings;
system monospace is reserved for code and small labels. The hero uses a system
serif italic for one word, adding contrast without downloading fonts.

## Voice and presentation

Lead with **“Your Neovim. Within reach.”** Explain that the whole editor stays in
Neovim and that Neordr exposes compatible interfaces. Do not imply Neordr hosts
agents, parses transcripts, provides a cloud relay, or replaces every Herdr
feature. Product illustrations are explicitly labeled as illustrations.

The repository currently remains private. The site links to authenticated source
and release pages and discloses the access requirement. Publishing a landing
page must not be presented as making its source or binaries publicly available.

## Preview and deployment

From the repository root:

```sh
python3 -m http.server 8080 --directory site
```

Open `http://localhost:8080`. All assets use relative paths, so the same page works
at `https://derekstavis.github.io/neordr/`. Check widths of 375px, 768px, and 1440px,
keyboard focus, reduced-motion settings, and source links before publishing.

The Pages workflow uploads only `site/`, never the checkout or build outputs.
It requires GitHub Pages to be enabled with GitHub Actions as the build source.
Private-repository Pages availability depends on the GitHub account's plan.

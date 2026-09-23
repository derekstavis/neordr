# Third-party notices

## Herdr contracts

Neordr derives its API, event, wire, and endpoint definitions from the unmodified
Herdr source pinned in the `vendor/herdr` Git submodule. Herdr is maintained at
https://github.com/herdrdev/herdr and distributed under the Apache License 2.0.

The build extracts those definitions into Cargo's generated output; it does not
build or redistribute the Herdr application. The upstream license is retained at
`vendor/herdr/LICENSE` and included in release archives as
`licenses/Herdr-Apache-2.0.txt`. See `build_support/README.md` for the source
revision and extraction process.

## Rust dependencies

Third-party Rust dependencies retain their own licenses and notices. The exact
versions and source registries used for a build are recorded in `Cargo.lock`.

## Neovim and Moshi

Neovim, Moshi, and moshi-hook are separate applications. They are not bundled in
Neordr releases. Their names identify integrations; Neordr is an independent
project and does not imply endorsement by those projects.

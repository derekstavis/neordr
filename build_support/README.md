# Herdr contract source

Neordr's contract definitions come from the unmodified
[Herdr](https://github.com/herdrdev/herdr) Git submodule at
`vendor/herdr`, pinned to `28360107c1bfab57fb0de99d74bfa48e74423ca8`.
Herdr is licensed under Apache-2.0;
retain [its license](../vendor/herdr/LICENSE) with distributions.

Initialize a fresh checkout with:

```sh
git submodule update --init vendor/herdr
cargo build --offline
```

Offline builds require the usual Cargo dependencies to have been fetched once.
The build itself makes no network requests and never builds Herdr's application,
terminal engine, or dependencies. Only Neordr's ordinary dependencies are built.

`contracts.rs` parses the upstream Rust AST and emits `herdr_contracts.rs` under
Cargo's `OUT_DIR`. The complete JSON API schema module tree is included, along
with every wire/endpoint struct, enum, alias and constant. Serde metadata,
variant/field order, helpers, custom serializers, and defaults remain upstream
code. The generator relocates crate paths; it excludes upstream test modules and
runtime-only conversions. Small supporting types are extracted by name from
upstream config, input, agent-resume, and popup-size modules. Missing types or
new external dependencies fail compilation; there is no untyped fallback.

The submodule must be clean at the reviewed revision. To update the contract,
review the upstream changes, move the submodule pin and the generator's revision
together, then run contract and real-client compatibility tests. Never patch
schema types inside the submodule or check generated Rust into the repository.

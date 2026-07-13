# Repository guidance

This file contains only cross-cutting instructions for coding agents. Keep detailed
architecture, package usage, and release procedures next to the code they describe.

## Read before changing code

- Start with [`README.md`](README.md) for the project model and workspace map.
- Read the nearest package `README.md`, `Cargo.toml`, `package.json`, or language
  build file before editing that package. Package-local documentation is the source
  of truth for package-specific behavior.
- Read [`justfile`](justfile) before adding or changing development commands.
- For language bindings, also read [`py/web-transport/README.md`](py/web-transport/README.md),
  [`kt/README.md`](kt/README.md), or [`swift/README.md`](swift/README.md), as applicable.

## Development commands

Use the Nix development shell when available so local tooling matches CI:

```bash
nix develop --command just check  # lint, formatting, and compile checks
nix develop --command just test   # test Rust and WASM targets
nix develop --command just fix    # apply automatic fixes
```

For a focused iteration, use `cargo test -p <crate>`, `cargo check -p <crate>`, or
the relevant `bun run` script. JavaScript and TypeScript use Bun, not npm, pnpm, or
Yarn. Before finishing, run the narrowest relevant checks plus `just check`; run
`just test` when behavior changes.

## Workspace boundaries

- `rs/` contains the Rust crates: the public router and trait, protocol layer,
  native backends, WASM adapter, Node bridge, UniFFI bridge, and QMux.
- `js/` contains the TypeScript packages and browser demo.
- `py/`, `kt/`, and `swift/` are language bindings generated from
  `rs/web-transport-ffi` and released from the same `web-transport-ffi-v*` tag.
- `.github/workflows/` is the source of truth for CI and release behavior.

When changing a shared protocol or public API, search every backend, wrapper,
binding, example, and README for corresponding updates. In particular, changes to
`rs/web-transport-ffi` may require coordinated Python, Kotlin, and Swift changes.

## Engineering rules

- Reproduce bugs and identify the mechanism before fixing them. Fix the lowest
  responsible layer and add a regression test that fails without the fix.
- Treat public API design as a compatibility commitment. Prefer a small,
  composable, strongly typed surface over one-off helpers or primitive parameters.
- Document exported Rust and TypeScript APIs. Keep implementation comments brief
  and reserve them for non-obvious constraints or invariants.
- Keep documentation and examples accurate in the same change that alters
  behavior. Describe the current design, not its history.
- Prefer maintained libraries for non-core functionality. Keep custom code for
  WebTransport, HTTP/3, QUIC, QMux, or integration behavior that needs local
  control.
- WASM builds require `--cfg=web_sys_unstable_apis`; the repository config and CI
  already provide it. Preserve native and `wasm32` conditional behavior when
  editing the platform router.
- Run `just fix` before committing, then verify the affected scope. Do not hide
  failures with ignored exit codes, empty catches, retries, or widened timeouts
  without explaining why failure is safe or why timing is the root cause.

## Maintaining these instructions

Keep this file short and cross-cutting. Put package-specific guidance in the
nearest package README (or a nested `AGENTS.md` only when agents need instructions
that do not belong in user documentation), then link to it here if it affects
repository-wide work.

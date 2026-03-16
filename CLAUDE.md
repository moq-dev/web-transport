# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common Commands

```bash
# Development environment (requires Nix)
nix develop          # Enter dev shell with all tools

# Rust
just check           # Full CI: clippy, fmt, feature powerset, cargo-shear, cargo-sort, biome
just test            # Run all tests including WASM targets
just fix             # Auto-fix: clippy, fmt, cargo-shear, cargo-sort, biome
cargo test -p <crate>                    # Test a single crate
cargo check -p <crate>                   # Check a single crate
cargo test -p <crate> -- <test_name>     # Run a single test

# JavaScript/TypeScript — use bun, never pnpm/npm/yarn
bun install          # Install JS dependencies
bun run check        # Biome lint
bun run fix          # Biome auto-fix
```

## Architecture

Rust/TypeScript monorepo implementing WebTransport (bidirectional streams + datagrams over HTTP/3 + QUIC) with multiple backend adapters.

### Layered Crate Structure

```
                        web-transport
                  (platform-agnostic router)
                    /                  \
          [native targets]        [wasm32 target]
                  |                     |
    ┌─────────────┼──────────────┐      |
    │             │              │      │
web-transport  web-transport  web-transport  web-transport
   -quinn        -noq         -quiche       -wasm
    │             │              │      (browser WebTransport API)
    └─────────────┼──────────────┘
                  │
          web-transport-proto     web-transport-trait
        (HTTP/3 frame parsing)    (async trait interface)
```

- **`web-transport`** — Platform router: compiles to quinn on native, wasm bindings in browser. Start here for consumers.
- **`web-transport-trait`** — Shared async trait that all backends implement.
- **`web-transport-proto`** — Low-level HTTP/3 protocol (frame encoding/decoding). Used by all native backends.
- **`web-transport-quinn`** / **`web-transport-noq`** / **`web-transport-quiche`** — Backend adapters for different QUIC libraries.
- **`web-transport-wasm`** — WASM bindings to the browser's native WebTransport API via `wasm-bindgen`.
- **`qmux`** — QMux protocol (draft-ietf-quic-qmux) over TCP/TLS/WebSocket. Has both a Rust crate and an NPM package (`@moq/qmux`).
- **`web-transport-ws`** — **Deprecated**, thin wrapper around qmux.

### WASM Considerations

WASM targets require `RUSTFLAGS=--cfg=web_sys_unstable_apis` (set in `.cargo/config.toml`). The `web-transport` crate uses conditional compilation (`target_arch = "wasm32"`) to route between native and WASM implementations.

## Workflow

- Always run `just fix` before committing to auto-fix formatting, linting, and sorting issues.

## Formatting & Style

- **Rust**: `cargo fmt`, standard rustfmt
- **TypeScript**: Biome — tabs, 120 line width, double quotes, LF line endings
- Both are enforced in `just check`

## Release

Automated via `release-plz` on push to main. Publishes Rust crates to crates.io. NPM packages use `bun run release` scripts in each package directory.

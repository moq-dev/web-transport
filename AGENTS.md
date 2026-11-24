Cross-platform WebTransport + HTTP/3 + QUIC + QMux library (`rs/` Rust crates, `js/` TypeScript/Bun packages, `py/`/`kt/`/`swift/` bindings generated from `rs/web-transport-ffi`). See `README.md` for the workspace map.

# Commands

Run inside the Nix dev shell so local tooling matches CI:

```bash
nix develop --command just check  # lint, formatting, compile checks
nix develop --command just test   # Rust and WASM tests
nix develop --command just fix    # auto-fix formatting/lints
```

JavaScript/TypeScript uses Bun (never npm, pnpm, or Yarn).

# Rules

- Shared protocol or public API changes must update all affected backends, wrappers, bindings (`py/`, `kt/`, `swift/`), examples, and READMEs in the same PR.
- WASM builds require `--cfg=web_sys_unstable_apis` (configured in repo/CI); preserve native and `wasm32` conditional compilation in the platform router.
- Read nested `AGENTS.md` before editing [`rs/`](rs/AGENTS.md), [`js/`](js/AGENTS.md), or [`rs/web-transport-ffi/`](rs/web-transport-ffi/AGENTS.md).

Quests: before any work, run `quest context` and follow it, unless this session already holds its rules.

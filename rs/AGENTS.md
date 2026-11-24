Rust workspace. Extends `../AGENTS.md` and `quest context rust`.

- Preserve the native/WASM split in async APIs: never add unconditional `Send` bounds to interfaces that must support browser types.
- Preserve backend error details where callers need them.
- Use the root `justfile` for native, WASM, and feature matrix checks.

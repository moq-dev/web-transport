# Rust guidance

Extends the [root guidance](../AGENTS.md).

- Use typed errors (`thiserror`) in libraries and contextual errors (`anyhow`) in
  examples or binaries. Preserve backend error details where callers need them.
- Prefer borrowed slices and strings for read-only inputs; take ownership when
  storing the value. Use newtypes, enums, and `Duration` to express constraints
  and units, converting to wire or binding primitives at the boundary.
- Make resource ownership and cleanup explicit. Avoid clones or background tasks
  that accidentally keep a session or stream alive after its owner is dropped.
- Preserve the native/WASM split in async APIs. Do not add unconditional `Send`
  bounds to interfaces that must support browser types.
- In poll implementations, use `ready!` and `?` where they simplify control flow.
  Preserve wakeups and cancellation behavior when refactoring async code.
- Use `#[non_exhaustive]` deliberately for extensible public errors, enums, and
  configuration structs; adding it to an existing public type can be breaking.
  Append variants to public fieldless enums with implicit discriminants.
- Prefer inline unit tests for local behavior and integration tests for transport
  interactions. Use the root `justfile` for the native, WASM, and feature checks.

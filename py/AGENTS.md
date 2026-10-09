# Python guidance

Extends the [root guidance](../AGENTS.md). For binding surface changes, also read
[`rs/web-transport-ffi/AGENTS.md`](../rs/web-transport-ffi/AGENTS.md).

- Keep the generated `web_transport._uniffi` layer generated. Put Python ergonomics
  in the handwritten `web_transport` wrapper and keep it thin.
- Preserve async context-manager and iterator behavior. Prefer keyword-only
  arguments with defaults when extending configuration APIs.
- Keep public exports, type annotations, and docstrings aligned. Preserve the
  wrapper's exception translation when adding calls to the generated API.
- Use `uv` and the package's `pyproject.toml` for tooling. Tests live under
  `web-transport/tests/`; run the relevant unit and integration coverage after
  rebuilding bindings when the Rust surface changes.

UniFFI bindings (`py/web-transport`, `kt/`, `swift/`). Extends `../AGENTS.md` and `quest context uniffi`.

- Rust exports do not auto-update handwritten wrappers: keep `py/web-transport/python/web_transport/_uniffi` generated and check `py/web-transport`, `kt/`, and `swift/` (including examples, docs, and exception translation) in the same PR.
- Python tooling uses `uv` and `py/web-transport/pyproject.toml`; run `py/web-transport/tests/` after rebuilding bindings.

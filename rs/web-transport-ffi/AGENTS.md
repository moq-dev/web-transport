# UniFFI guidance

Extends the [Rust guidance](../AGENTS.md). Read [README.md](README.md) before
changing the exported surface or binding generation.

- Keep transport behavior in Rust and language wrappers thin. Align names and
  semantics across Python, Kotlin, and Swift while preserving their public APIs.
- A Rust export does not automatically update handwritten wrappers. Check
  `py/web-transport`, `kt/`, and `swift/` in the same change, including their
  examples and documentation.
- Regenerate bindings from the Rust proc-macro exports; do not hand-edit generated
  output. Verify the affected language consumers after changing exports.
- Do not rely on `#[cfg]` inside a UniFFI export impl to hide methods from binding
  generation; put conditional exports in a gated module.

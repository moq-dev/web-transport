# web-transport-ffi

Low-level UniFFI bindings for WebTransport over HTTP/3 (QUIC). Generates Python, Swift, and Kotlin bindings backed by [`web-transport-quinn`](../web-transport-quinn).

For an ergonomic Python API (async context managers, legacy exception hierarchy), see [`web-transport-rs`](https://pypi.org/project/web-transport-rs/) which wraps this package.

## Building

```bash
# Generate language bindings for the current target:
./build.sh --bindings-only

# Cross-compile native libs for a specific target:
./build.sh --target aarch64-apple-ios --version 0.1.0
```

Artifacts land in `dist/`.

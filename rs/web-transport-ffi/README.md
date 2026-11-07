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

## Crypto

The native Quinn backend uses rind through rind-rustls, with an installed process-wide
rustls provider taking precedence. Endpoint keys and Retry integrity always use rind.
Native builds support x86_64, aarch64 and 32-bit ARM; constructing a client or server
returns an error if the CPU lacks a required feature. This also applies with another
TLS provider installed. The native dependency graph no longer builds ring or aws-lc-rs.

The `ring` and `aws-lc-rs` Cargo features have been removed. Language API signatures
and generated bindings remain unchanged; CPU refusal is a `WebTransportError`.

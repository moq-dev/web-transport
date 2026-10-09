# [M] Run the WASM browser harness in PR CI

## Goal

Pull requests execute the existing WebTransport WASM harness in Chromium on Linux and fail when browser assertions fail.

## Plan

Reuse the QUIC harness peer and result array in `rs/web-transport-wasm/examples/harness.rs` and the `just harness` build steps. Use a maintained browser runner with a finite test lifecycle, peer readiness, failure diagnostics and guaranteed process cleanup. Preserve the pinned wasm-bindgen schema and unstable web-sys build flags.

Exercise the existing clone, cancellation, datagram and close cases, including uni and bi stream-opening cleanup. Distinguish assertion failure from an unavailable browser environment; CI must not silently skip coverage. Include shared Rust sources and build configuration in any path filters. Start with Chromium/Linux, as approved; other browsers and mobile platforms are outside this quest. Keep local execution instructions with the WASM package.

## Related

- [Clean datagram closure](/quest/a0/wasm-datagram-closure.md) - additional focused regression to include when present

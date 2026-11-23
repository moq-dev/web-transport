# [M] Ship a Swift release package that builds

## Goal

The `web-transport-ffi-<version>-swift.tar.gz` archive attached to each `web-transport-ffi-v*` release resolves and compiles as a path dependency for macOS and iOS, exposes the generated API publicly through the `WebTransport` module, and `release-swift.yml` proves that before attaching it.

## Plan

The v0.3.0 archive cannot compile:

- Its `Package.swift` declares `WebTransportFFI` only as a `.binaryTarget`, so `Sources/WebTransportFFI/Generated.swift` belongs to no target.
- The XCFramework's module map names the C module `web_transportFFI`, so the wrapper's `@_exported import WebTransportFFI` names no module.
- `swift/scripts/check.sh` already builds the working layout, a binary C target plus a source target for the generated Swift; `swift/scripts/package.sh` should produce the same.
- The generated Swift declares types such as `enum WebTransportError` without `public`; check UniFFI's Swift visibility settings so package consumers can use them.
- `swift/Package.swift` declares `.iOS(.v15)` while `release-swift.yml` builds with `IPHONEOS_DEPLOYMENT_TARGET` 16.0; align them.
- `swift/scripts/package.sh`'s `--release-url` cannot work: its first `sed -e` replaces `REPLACE_VERSION` before the third, whose pattern contains it, runs. No caller passes it; drop it along with the unused `WORKSPACE_DIR` and the `--bindings-dir` default no caller relies on.
- Build the packaged archive on macOS in `release-swift.yml`, including its PR smoke run, and update the Swift README's install section to match.

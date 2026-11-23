# [S] Fix the released Swift package

## Goal

The Swift package that `release-swift.yml` publishes builds and imports on macOS, and CI builds that release manifest rather than only the separate one `swift/scripts/check.sh` writes.

## Plan

The code audit ([moq-dev/web-transport#579](https://github.com/moq-dev/web-transport/pull/579)) found this by reading, not building. Reproduce it first by staging the package with `swift/scripts/package.sh` and running `swift build` on it.

- `swift/Package.swift` declares `WebTransportFFI` only as a `binaryTarget`, but `package.sh` copies the generated `web_transport.swift` into `Sources/WebTransportFFI/`, where nothing compiles it. The XCFramework's modulemap names its module `web_transportFFI`, so `WebTransport.swift`'s `@_exported import WebTransportFFI` likely does not resolve.
- `check.sh` already uses the layout UniFFI needs: a C module target for the modulemap and a separate Swift target for the generated API. Use the same layout in the release manifest, and build the staged package in the release workflow's PR smoke run.
- Delete `package.sh`'s `--release-url` flag and `RELEASE_URL_BASE` (no caller passes it, and the first `sed -e` rewrites `REPLACE_VERSION` before the URL pattern containing it can match), the unused `WORKSPACE_DIR`, and the unused `--bindings-dir` default.

## Hosts

- macos - building the Swift package needs Xcode

# WebTransport (Swift)

A Swift wrapper around the [web-transport-ffi](../rs/web-transport-ffi) UniFFI bindings for [WebTransport over HTTP/3](https://datatracker.ietf.org/doc/draft-ietf-webtrans-http3/).

## Install

The package is distributed as a GitHub Release asset on the [moq-dev/web-transport](https://github.com/moq-dev/web-transport) repo. There is **no SPM mirror yet**, so SPM cannot resolve a `https://github.com/...` URL directly until one is set up.

For now, consume the package locally by downloading the `web-transport-ffi-${VERSION}-swift.tar.gz` archive attached to the matching `web-transport-ffi-v*` release and adding it as a path-based dependency:

```swift
.package(path: "/path/to/web-transport-ffi-${VERSION}-swift"),
```

## Local development

`swift/scripts/check.sh` builds `web-transport-ffi` for the host, regenerates the UniFFI Swift bindings, and (on macOS) assembles a single-slice `WebTransportFFI.xcframework` so `swift test` can run. Requires macOS with `xcodebuild` and `swift` on `$PATH`. Skips cleanly on non-macOS hosts.

To compute the XCFramework checksum yourself for a local `Package.swift` edit:

```sh
swift package compute-checksum WebTransportFFI.xcframework.zip
```

## Layout

```text
swift/
  Package.swift                       Manifest (URL+checksum rewritten by package.sh at release time)
  Sources/
    WebTransport/                     Ergonomic shim re-exporting WebTransportFFI
    WebTransportFFI/                  UniFFI-generated swift (populated by check.sh/package.sh, gitignored)
  Tests/WebTransportTests/            Smoke tests
  scripts/                            check.sh, package.sh
```

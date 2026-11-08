# WebTransport (Swift)

A Swift wrapper around the [web-transport-ffi](../rs/web-transport-ffi) UniFFI bindings for [WebTransport over HTTP/3](https://datatracker.ietf.org/doc/draft-ietf-webtrans-http3/).

## Install

The package is distributed as a GitHub Release asset on the [moq-dev/web-transport](https://github.com/moq-dev/web-transport) repo. There is **no SPM mirror yet**, so SPM cannot resolve a `https://github.com/...` URL directly until one is set up.

For now, consume the package locally by downloading the `web-transport-ffi-${VERSION}-swift.tar.gz` archive attached to the matching `web-transport-ffi-v*` release and adding it as a path-based dependency:

```swift
.package(path: "/path/to/web-transport-ffi-${VERSION}-swift"),
```

## Local development

On macOS, from the repository root:

```sh
just test-swift
# Or, without Just:
./swift/scripts/check.sh
```

Requires the Swift 5.9+ toolchain, Rust/Cargo, Python 3, OpenSSL and Apple's
`install_name_tool` and `codesign` on `$PATH`. Unsupported hosts and missing tools are errors.
With the Nix development shell available, use `nix develop --command just test-swift`;
the Apple tools must still be installed on the host.

The command builds the native host library in release mode, generates bindings
from it, and stages an isolated Swift package under `.tmp/`. The package compiles
the generated Swift API and the wrapper, links that run's dynamic library, and
checks its loaded path at runtime. Tests open a loopback client/server session,
exchange bidirectional and unidirectional streams and datagrams, check FIN/EOF
and structured errors, and explicitly close sessions and endpoints. Test-only
credentials are generated for each run; verification is disabled only for the
loopback client.

Staged packages and credentials are removed on success, failure or runtime
cancellation after stopping the test process group. Test output remains in
`.tmp/swift-test-results/`, with xUnit results for completed runs. Runtime execution
fails after 120 seconds if a native call hangs, allowing five seconds for graceful
termination before forcing exit. Interrupted runs may have partial logs and no XML;
unbuffered phase messages identify the last lifecycle operation. The command leaves `Package.swift` and
developer-generated bindings untouched. The independent `Swift runtime` PR job
runs this command on macOS for every PR, including shared Rust and build changes.
It does not publish or test the release XCFramework platform matrix or iOS simulators.

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
    WebTransportFFI/                  UniFFI-generated Swift (populated in release packages, gitignored)
  Tests/WebTransportTests/            Package tests (included in release archives)
  Tests/HostTests/                    macOS host runtime tests (check.sh only)
  scripts/                            check.sh, package.sh
```

## Native crypto

The native Quinn backend uses rind through rind-rustls, with an installed process-wide
rustls provider taking precedence. Endpoint keys and Retry integrity always use rind.
Native builds support x86_64, aarch64 and 32-bit ARM; constructing a client or server
returns an error if the CPU lacks a required feature. This also applies with another
TLS provider installed. The native dependency graph no longer builds ring or aws-lc-rs.

#!/usr/bin/env bash
set -euo pipefail

# Local smoke check for the Swift wrapper. Builds web-transport-ffi for the
# host macOS target, regenerates the UniFFI Swift bindings, and (on macOS)
# assembles a single-slice WebTransportFFI.xcframework so `swift test` can
# run against a path-based Package.swift.
#
# Skipped on non-macOS hosts and on hosts without `swift` or `cargo`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWIFT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$SWIFT_DIR/.." && pwd)"

if [[ "$(uname)" != "Darwin" ]]; then
    echo "swift check: macOS only, skipping" >&2
    exit 0
fi
if ! command -v swift >/dev/null 2>&1; then
    echo "swift check: no swift toolchain on PATH, skipping" >&2
    exit 0
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "swift check: no cargo on PATH, skipping" >&2
    exit 0
fi

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
echo "swift check: building web-transport-ffi for $HOST_TARGET..."
cargo build --release --package web-transport-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"

TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')

CDYLIB="$TARGET_BASE/release/libweb_transport_ffi.dylib"
STATIC="$TARGET_BASE/release/libweb_transport_ffi.a"
[[ -f "$CDYLIB" ]] || { echo "swift check: missing $CDYLIB" >&2; exit 1; }

# Generate bindings.
BINDGEN_OUT=$(mktemp -d)
trap 'rm -rf "$BINDGEN_OUT"' EXIT
cargo run --release --package web-transport-ffi --bin uniffi-bindgen \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language swift --out-dir "$BINDGEN_OUT"

# Stage generated swift so the Sources/WebTransportFFI target has content.
mkdir -p "$SWIFT_DIR/Sources/WebTransportFFI"
cp "$BINDGEN_OUT/web_transport.swift" "$SWIFT_DIR/Sources/WebTransportFFI/Generated.swift"

# If we have a static lib and xcodebuild, build a local XCFramework and
# run `swift test` against a path-based Package.swift. Otherwise stop
# here; `swift build` against the URL-based binaryTarget will fail without
# network access and a published release.
if [[ -f "$STATIC" ]] && command -v xcodebuild >/dev/null 2>&1; then
    LOCAL_XCF="$SWIFT_DIR/WebTransportFFI.xcframework"
    rm -rf "$LOCAL_XCF"
    HEADERS_DIR="$BINDGEN_OUT/headers"
    mkdir -p "$HEADERS_DIR"
    cp "$BINDGEN_OUT/web_transportFFI.h" "$HEADERS_DIR/"
    cp "$BINDGEN_OUT/web_transportFFI.modulemap" "$HEADERS_DIR/module.modulemap"

    xcodebuild -create-xcframework \
        -library "$STATIC" -headers "$HEADERS_DIR" \
        -output "$LOCAL_XCF"

    # Use a path-based Package.swift for local dev. The original is restored
    # via git after the check finishes.
    cat > "$SWIFT_DIR/Package.swift" <<EOF
// swift-tools-version:5.9
// Auto-rewritten by swift/scripts/check.sh for local dev. Restore via git
// after the check finishes.

import PackageDescription

let package = Package(
    name: "WebTransport",
    platforms: [.iOS(.v15), .macOS(.v12)],
    products: [.library(name: "WebTransport", targets: ["WebTransport"])],
    targets: [
        .target(name: "WebTransport", dependencies: ["WebTransportFFI"], path: "Sources/WebTransport"),
        .binaryTarget(name: "WebTransportFFI", path: "WebTransportFFI.xcframework"),
        .testTarget(name: "WebTransportTests", dependencies: ["WebTransport"], path: "Tests/WebTransportTests"),
    ]
)
EOF

    cd "$SWIFT_DIR"
    swift test
else
    echo "swift check: bindings generated; full package build requires XCFramework" >&2
fi

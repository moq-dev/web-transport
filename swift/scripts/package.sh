#!/usr/bin/env bash
set -euo pipefail

# Assemble the web-transport-ffi Swift Package: bundle per-target static libs
# into an XCFramework, copy the uniffi-generated Swift source, rewrite the
# Package.swift binary URL+checksum, and tar the result.
#
# Designed to run after rs/web-transport-ffi/build.sh produces per-target outputs.
# Only macOS hosts can run this (xcodebuild is required).
#
# Usage:
#   swift/scripts/package.sh --version 0.0.0-dev --lib-dir dist --output dist
#
#   --version       Version baked into Package.swift.
#   --lib-dir       Directory containing per-target web-transport-ffi outputs.
#   --output        Destination directory for the .tar.gz + xcframework.zip.
#   --bindings-dir  Directory with uniffi-bindgen swift output (defaults to
#                   "$LIB_DIR/bindings").
#   --release-url   Release URL prefix used as the XCFramework download
#                   target. Defaults to the upstream GitHub Releases URL;
#                   override when publishing from a fork.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWIFT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$SWIFT_DIR/.." && pwd)"

VERSION=""
LIB_DIR=""
OUTPUT_DIR=""
BINDINGS_DIR=""
RELEASE_URL_BASE="https://github.com/moq-dev/web-transport/releases/download"

while [[ $# -gt 0 ]]; do
    case $1 in
        --version) VERSION="$2"; shift 2;;
        --lib-dir) LIB_DIR="$2"; shift 2;;
        --output) OUTPUT_DIR="$2"; shift 2;;
        --bindings-dir) BINDINGS_DIR="$2"; shift 2;;
        --release-url) RELEASE_URL_BASE="$2"; shift 2;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 1;;
    esac
done

[[ -z "$VERSION" ]] && { echo "Error: --version is required" >&2; exit 1; }
[[ -z "$LIB_DIR" ]] && { echo "Error: --lib-dir is required" >&2; exit 1; }
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"
[[ -z "$BINDINGS_DIR" ]] && BINDINGS_DIR="$LIB_DIR/bindings"

[[ "$(uname)" == "Darwin" ]] || { echo "Error: package.sh requires macOS (xcodebuild)" >&2; exit 1; }
command -v xcodebuild >/dev/null || { echo "Error: xcodebuild not found" >&2; exit 1; }
command -v swift >/dev/null || { echo "Error: swift not found" >&2; exit 1; }

mkdir -p "$OUTPUT_DIR"
# Normalize to an absolute path: later steps (zip, swift package
# compute-checksum) run from cd'd subshells, so a relative OUTPUT_DIR
# would resolve against the wrong cwd.
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT

# --- Headers (modulemap + .h) shared by all slices ---
HEADERS_DIR="$STAGING/headers"
mkdir -p "$HEADERS_DIR"
[[ -f "$BINDINGS_DIR/web_transportFFI.h" ]] || { echo "Error: missing $BINDINGS_DIR/web_transportFFI.h" >&2; exit 1; }
[[ -f "$BINDINGS_DIR/web_transportFFI.modulemap" ]] || { echo "Error: missing $BINDINGS_DIR/web_transportFFI.modulemap" >&2; exit 1; }
[[ -f "$BINDINGS_DIR/web_transport.swift" ]] || { echo "Error: missing $BINDINGS_DIR/web_transport.swift" >&2; exit 1; }
cp "$BINDINGS_DIR/web_transportFFI.h" "$HEADERS_DIR/"
cp "$BINDINGS_DIR/web_transportFFI.modulemap" "$HEADERS_DIR/module.modulemap"

# --- Per-slice library prep ---
lib_for() {
    echo "$LIB_DIR/$1/libweb_transport_ffi.a"
}

ensure_lib() {
    local path
    path=$(lib_for "$1")
    [[ -f "$path" ]] || { echo "Error: missing static lib for $1 at $path" >&2; exit 1; }
    echo "$path"
}

IOS_DEVICE_LIB=$(ensure_lib "aarch64-apple-ios")
IOS_SIM_ARM64=$(ensure_lib "aarch64-apple-ios-sim")
IOS_SIM_X86_64=$(ensure_lib "x86_64-apple-ios")
MAC_UNIVERSAL=$(ensure_lib "universal-apple-darwin")

# Fat lib for iOS simulator (arm64 + x86_64).
IOS_SIM_FAT="$STAGING/libweb_transport_ffi-iossim.a"
lipo -create "$IOS_SIM_ARM64" "$IOS_SIM_X86_64" -output "$IOS_SIM_FAT"

# --- Build XCFramework ---
XCF="$STAGING/WebTransportFFI.xcframework"
xcodebuild -create-xcframework \
    -library "$IOS_DEVICE_LIB" -headers "$HEADERS_DIR" \
    -library "$IOS_SIM_FAT" -headers "$HEADERS_DIR" \
    -library "$MAC_UNIVERSAL" -headers "$HEADERS_DIR" \
    -output "$XCF"

# --- Zip and checksum the XCFramework ---
XCF_ZIP="$OUTPUT_DIR/WebTransportFFI.xcframework.zip"
rm -f "$XCF_ZIP"
(cd "$STAGING" && zip -r -q "$XCF_ZIP" "$(basename "$XCF")")

# Move/copy to absolute path before computing checksum (swift requires
# it to live in a package).
CHECKSUM=$(cd "$SWIFT_DIR" && swift package compute-checksum "$XCF_ZIP")
echo "XCFramework checksum: $CHECKSUM"

# --- Assemble Swift package staging dir ---
PKG_NAME="web-transport-ffi-${VERSION}-swift"
PKG_STAGE="$STAGING/$PKG_NAME"
mkdir -p "$PKG_STAGE/Sources/WebTransport" "$PKG_STAGE/Sources/WebTransportFFI" "$PKG_STAGE/Tests/WebTransportTests"

cp -R "$SWIFT_DIR/Sources/WebTransport/." "$PKG_STAGE/Sources/WebTransport/"
cp -R "$SWIFT_DIR/Tests/WebTransportTests/." "$PKG_STAGE/Tests/WebTransportTests/"
cp "$BINDINGS_DIR/web_transport.swift" "$PKG_STAGE/Sources/WebTransportFFI/Generated.swift"

# Generate Package.swift with the final URL+checksum.
URL="${RELEASE_URL_BASE}/web-transport-ffi-v${VERSION}/WebTransportFFI.xcframework.zip"
sed -e "s|REPLACE_VERSION|${VERSION}|g" \
    -e "s|REPLACE_CHECKSUM|${CHECKSUM}|g" \
    -e "s|https://github.com/moq-dev/web-transport/releases/download/web-transport-ffi-vREPLACE_VERSION/WebTransportFFI.xcframework.zip|${URL}|g" \
    "$SWIFT_DIR/Package.swift" > "$PKG_STAGE/Package.swift"

# --- Archive ---
ARCHIVE="$OUTPUT_DIR/${PKG_NAME}.tar.gz"
tar -czf "$ARCHIVE" -C "$STAGING" "$PKG_NAME"
echo "Created: $ARCHIVE"
echo "Created: $XCF_ZIP"

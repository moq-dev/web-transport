#!/usr/bin/env bash
set -Eeuo pipefail

# Exercise freshly generated Swift bindings against the native macOS host library.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SWIFT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$SWIFT_DIR/.." && pwd)"

[[ "$(uname)" == Darwin ]] || { echo "swift check: requires macOS" >&2; exit 1; }
for tool in swift cargo rustc python3 openssl install_name_tool codesign; do
    command -v "$tool" >/dev/null || { echo "swift check: missing $tool on PATH" >&2; exit 1; }
done

mkdir -p "$WORKSPACE_DIR/.tmp/swift-test-results"
RESULTS="$WORKSPACE_DIR/.tmp/swift-test-results"
rm -f "$RESULTS/tests.log" "$RESULTS/tests.xml"
STAGING=$(mktemp -d "$WORKSPACE_DIR/.tmp/swift-test.XXXXXX")
trap 'rm -rf "$STAGING"' EXIT
trap 'echo "swift check: failed at line $LINENO: $BASH_COMMAND (reports: $RESULTS)" >&2' ERR

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
case "$HOST_TARGET" in
    aarch64-apple-darwin|x86_64-apple-darwin) ;;
    *) echo "swift check: unsupported Rust host $HOST_TARGET" >&2; exit 1 ;;
esac
swift --version
rustc --version
echo "swift check: building web-transport-ffi for $HOST_TARGET"
cargo rustc --release --lib --package web-transport-ffi --target "$HOST_TARGET" \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- -C link-arg=-Wl,-headerpad_max_install_names
TARGET_BASE=$(cargo metadata --format-version 1 --no-deps --manifest-path "$WORKSPACE_DIR/Cargo.toml" \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
CDYLIB="$TARGET_BASE/$HOST_TARGET/release/libweb_transport_ffi.dylib"
[[ -f "$CDYLIB" ]] || { echo "swift check: missing $CDYLIB" >&2; exit 1; }
mkdir -p "$STAGING/lib"
cp "$CDYLIB" "$STAGING/lib/"
CDYLIB="$STAGING/lib/libweb_transport_ffi.dylib"
# An absolute install name pins the test binary to this run's library, without
# DYLD_LIBRARY_PATH or a previously installed release influencing resolution.
install_name_tool -id "$CDYLIB" "$CDYLIB"
codesign --force --sign - "$CDYLIB"

cargo run --release --package web-transport-ffi --bin uniffi-bindgen --target "$HOST_TARGET" \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language swift --out-dir "$STAGING/bindings"

# Keep the release manifest and any developer-generated inputs untouched. The C
# module and generated Swift API are separate targets, as required by UniFFI.
mkdir -p "$STAGING/Sources/WebTransportFFI" "$STAGING/Sources/web_transportFFI"
cp -R "$SWIFT_DIR/Sources/WebTransport" "$STAGING/Sources/"
cp -R "$SWIFT_DIR/Tests" "$STAGING/"
cp "$STAGING/bindings/web_transport.swift" "$STAGING/Sources/WebTransportFFI/Generated.swift"
cp "$STAGING/bindings/web_transportFFI.h" "$STAGING/Sources/web_transportFFI/"
cp "$STAGING/bindings/web_transportFFI.modulemap" "$STAGING/Sources/web_transportFFI/module.modulemap"
cat > "$STAGING/Package.swift" <<'EOF'
// swift-tools-version:5.9
import Foundation
import PackageDescription

let libraryPath = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    .appendingPathComponent("lib").path
let package = Package(
    name: "WebTransport",
    platforms: [.macOS(.v12)],
    products: [.library(name: "WebTransport", targets: ["WebTransport"])],
    targets: [
        .systemLibrary(name: "web_transportFFI"),
        .target(
            name: "WebTransportFFI",
            dependencies: ["web_transportFFI"],
            linkerSettings: [.linkedLibrary("web_transport_ffi"), .unsafeFlags(["-L", libraryPath])]
        ),
        .target(name: "WebTransport", dependencies: ["WebTransportFFI"]),
        .testTarget(name: "WebTransportTests", dependencies: ["WebTransport", "web_transportFFI"]),
    ]
)
EOF

# Fresh credentials, valid only for this run's loopback server; never upload keys.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$STAGING/key.pem" -out "$STAGING/cert.pem" -days 1 -subj /CN=localhost
openssl x509 -in "$STAGING/cert.pem" -outform DER -out "$STAGING/cert.der"
openssl pkcs8 -topk8 -nocrypt -in "$STAGING/key.pem" -outform DER -out "$STAGING/key.der"
export WEB_TRANSPORT_TEST_LIBRARY="$CDYLIB"
export WEB_TRANSPORT_TEST_CERT="$STAGING/cert.der"
export WEB_TRANSPORT_TEST_KEY="$STAGING/key.der"

swift build --package-path "$STAGING" --build-tests
# Only runtime execution has a deadline, separate from compilation. A stuck
# UniFFI future must fail CI and kill the whole test process group.
python3 - "$STAGING" "$RESULTS" <<'PY' 2>&1 | tee "$RESULTS/tests.log"
import os
import signal
import subprocess
import sys

command = ["swift", "test", "--package-path", sys.argv[1], "--skip-build",
           "--xunit-output", os.path.join(sys.argv[2], "tests.xml")]
process = subprocess.Popen(command, start_new_session=True)
try:
    status = process.wait(timeout=120)
except subprocess.TimeoutExpired:
    print("swift check: runtime tests exceeded 120s; terminating test process group", flush=True)
    os.killpg(process.pid, signal.SIGKILL)
    process.wait()
    status = 1
sys.exit(status if status >= 0 else 1)
PY
echo "swift check: passed (reports: $RESULTS)"

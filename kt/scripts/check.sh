#!/usr/bin/env bash
set -euo pipefail

# Build and test the host JVM JAR with its packaged JNA native resource.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$KT_DIR/.." && pwd)"

# Match the Gradle wrapper's JDK selection, including Nix shells that reset PATH.
if [[ -n "${JAVA_HOME:-}" ]]; then
    export PATH="$JAVA_HOME/bin:$PATH"
fi
for tool in java cargo rustc openssl; do
    command -v "$tool" >/dev/null 2>&1 || { echo "kt check: required tool not on PATH: $tool" >&2; exit 1; }
done

HOST_TARGET=$(rustc -vV | awk '/^host:/ {print $2}')
case "$HOST_TARGET" in
    x86_64-unknown-linux-gnu) OS_TAG=linux; ARCH_TAG=x86-64; LIBNAME=libweb_transport_ffi.so;;
    aarch64-unknown-linux-gnu) OS_TAG=linux; ARCH_TAG=aarch64; LIBNAME=libweb_transport_ffi.so;;
    x86_64-apple-darwin) OS_TAG=darwin; ARCH_TAG=x86-64; LIBNAME=libweb_transport_ffi.dylib;;
    aarch64-apple-darwin) OS_TAG=darwin; ARCH_TAG=aarch64; LIBNAME=libweb_transport_ffi.dylib;;
    x86_64-pc-windows-msvc) OS_TAG=win32; ARCH_TAG=x86-64; LIBNAME=web_transport_ffi.dll;;
    *) echo "kt check: unsupported host: $HOST_TARGET" >&2; exit 1;;
esac

mkdir -p "$WORKSPACE_DIR/.tmp"
STAGING=$(mktemp -d "$WORKSPACE_DIR/.tmp/kotlin-smoke.XXXXXX")
trap 'rm -rf "$STAGING"' EXIT

# An explicit target/profile keeps resource staging independent of Cargo defaults.
echo "kt check: building web-transport-ffi for $HOST_TARGET..."
cargo build --release --target "$HOST_TARGET" --package web-transport-ffi \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml"
TARGET_BASE=$(cargo metadata --format-version 1 --manifest-path "$WORKSPACE_DIR/Cargo.toml" --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
CDYLIB="$TARGET_BASE/$HOST_TARGET/release/$LIBNAME"
[[ -f "$CDYLIB" ]] || { echo "kt check: cdylib not found at $CDYLIB" >&2; exit 1; }

cargo run --release --target "$HOST_TARGET" --package web-transport-ffi --bin uniffi-bindgen \
    --manifest-path "$WORKSPACE_DIR/Cargo.toml" -- \
    generate --library "$CDYLIB" --language kotlin --out-dir "$STAGING/bindings"

GENERATED_DIR="$KT_DIR/web-transport/src/jvmAndAndroidMain/kotlin/uniffi"
RES_DIR="$KT_DIR/web-transport/src/jvmMain/resources"
# Remove stale generated resources so an earlier build cannot hide missing output.
rm -rf "$GENERATED_DIR" "$RES_DIR"/linux-* "$RES_DIR"/darwin* "$RES_DIR"/win32-*
mkdir -p "$GENERATED_DIR" "$RES_DIR/${OS_TAG}-${ARCH_TAG}"
cp -R "$STAGING/bindings/uniffi/"* "$GENERATED_DIR/"
cp "$CDYLIB" "$RES_DIR/${OS_TAG}-${ARCH_TAG}/$LIBNAME"

# Fresh loopback-only credentials, never packaged into the published artifact.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -subj /CN=localhost -days 1 -outform DER -out "$STAGING/cert.der" -keyout "$STAGING/key.pem"
openssl pkcs8 -topk8 -nocrypt -in "$STAGING/key.pem" -outform DER -out "$STAGING/key.der"
export WEB_TRANSPORT_TEST_CERT="$STAGING/cert.der"
export WEB_TRANSPORT_TEST_KEY="$STAGING/key.der"

# Always execute the test, including when Gradle has cached an earlier smoke run.
"$KT_DIR/gradlew" --no-daemon --console=plain --stacktrace -p "$KT_DIR" \
    -Pkotlin.compiler.execution.strategy=in-process \
    -Pwebtransportffi.version=0.0.0-dev :web-transport:jvmTest --rerun-tasks

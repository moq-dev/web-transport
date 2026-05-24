#!/usr/bin/env bash
set -euo pipefail

# Assemble the web-transport-ffi Kotlin package and stage it for publication.
#
# Designed to run after the workflow has placed per-target web-transport-ffi
# native libs into $LIB_DIR (one subdir per cargo target) and the
# uniffi-bindgen kotlin output at $BINDINGS_DIR/uniffi/web_transport/web_transport.kt.
#
# Usage:
#   kt/scripts/package.sh --version 0.0.0-dev --lib-dir libs --bindings-dir bindings --output dist
#
# Expected $LIB_DIR layout (per target, populated by the build matrix):
#   $LIB_DIR/aarch64-linux-android/libweb_transport_ffi.so
#   $LIB_DIR/armv7-linux-androideabi/libweb_transport_ffi.so
#   $LIB_DIR/x86_64-linux-android/libweb_transport_ffi.so
#   $LIB_DIR/x86_64-unknown-linux-gnu/libweb_transport_ffi.so
#   ... etc.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

VERSION=""
LIB_DIR=""
OUTPUT_DIR=""
BINDINGS_DIR=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --version) VERSION="$2"; shift 2;;
        --lib-dir) LIB_DIR="$2"; shift 2;;
        --output) OUTPUT_DIR="$2"; shift 2;;
        --bindings-dir) BINDINGS_DIR="$2"; shift 2;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "Unknown option: $1" >&2; exit 1;;
    esac
done

[[ -z "$VERSION" ]] && { echo "Error: --version is required" >&2; exit 1; }
[[ -z "$LIB_DIR" ]] && { echo "Error: --lib-dir is required" >&2; exit 1; }
[[ -z "$BINDINGS_DIR" ]] && { echo "Error: --bindings-dir is required" >&2; exit 1; }
[[ -z "$OUTPUT_DIR" ]] && OUTPUT_DIR="dist"

mkdir -p "$OUTPUT_DIR"

# Clean staging dirs.
rm -rf "$KT_DIR/web-transport/src/androidMain/jniLibs"
rm -rf "$KT_DIR/web-transport/src/jvmMain/resources"
rm -rf "$KT_DIR/web-transport/src/jvmAndAndroidMain/kotlin/uniffi"
mkdir -p "$KT_DIR/web-transport/src/androidMain/jniLibs"
mkdir -p "$KT_DIR/web-transport/src/jvmMain/resources"

# --- Android JNI libs ---
HAVE_ANDROID_LIBS=false
for target in aarch64-linux-android armv7-linux-androideabi x86_64-linux-android; do
    case "$target" in
        aarch64-linux-android) abi="arm64-v8a" ;;
        armv7-linux-androideabi) abi="armeabi-v7a" ;;
        x86_64-linux-android) abi="x86_64" ;;
    esac
    src="$LIB_DIR/$target/libweb_transport_ffi.so"
    if [[ -f "$src" ]]; then
        dest="$KT_DIR/web-transport/src/androidMain/jniLibs/$abi"
        mkdir -p "$dest"
        cp "$src" "$dest/"
        echo "  android $abi <- $target"
        HAVE_ANDROID_LIBS=true
    else
        echo "  android $abi: skipped, $src missing"
    fi
done

# --- JVM desktop resources (JNA classpath layout) ---
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu universal-apple-darwin aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-msvc; do
    case "$target" in
        x86_64-unknown-linux-gnu)  dir="linux-x86-64";     libname="libweb_transport_ffi.so" ;;
        aarch64-unknown-linux-gnu) dir="linux-aarch64";    libname="libweb_transport_ffi.so" ;;
        universal-apple-darwin)    dir="darwin";           libname="libweb_transport_ffi.dylib" ;;
        aarch64-apple-darwin)      dir="darwin-aarch64";   libname="libweb_transport_ffi.dylib" ;;
        x86_64-apple-darwin)       dir="darwin-x86-64";    libname="libweb_transport_ffi.dylib" ;;
        x86_64-pc-windows-msvc)    dir="win32-x86-64";     libname="web_transport_ffi.dll" ;;
    esac
    src="$LIB_DIR/$target/$libname"
    if [[ -f "$src" ]]; then
        dest="$KT_DIR/web-transport/src/jvmMain/resources/$dir"
        mkdir -p "$dest"
        cp "$src" "$dest/"
        echo "  jvm $dir <- $target"
    else
        echo "  jvm $dir: skipped, $src missing"
    fi
done

# --- Uniffi-generated Kotlin source ---
GENERATED_KT="$BINDINGS_DIR/uniffi/web_transport/web_transport.kt"
[[ -f "$GENERATED_KT" ]] || { echo "Error: uniffi-bindgen output not found at $GENERATED_KT" >&2; exit 1; }
mkdir -p "$KT_DIR/web-transport/src/jvmAndAndroidMain/kotlin/uniffi/web_transport"
cp "$GENERATED_KT" "$KT_DIR/web-transport/src/jvmAndAndroidMain/kotlin/uniffi/web_transport/web_transport.kt"

# --- Maven-local publish ---
MAVEN_LOCAL="$OUTPUT_DIR/maven-local"
mkdir -p "$MAVEN_LOCAL"

GRADLE_ARGS=("-Pwebtransportffi.version=$VERSION" "-Dmaven.repo.local=$(cd "$MAVEN_LOCAL" && pwd)")
if [[ "$HAVE_ANDROID_LIBS" == true ]]; then
    GRADLE_ARGS+=("-Pandroid.enabled=true")
fi

GRADLE_CMD="${GRADLE_CMD:-$(command -v gradle || true)}"
[[ -n "$GRADLE_CMD" ]] || { echo "Error: gradle not on PATH" >&2; exit 1; }

"$GRADLE_CMD" -p "$KT_DIR" "${GRADLE_ARGS[@]}" :web-transport:assemble :web-transport:publishToMavenLocal

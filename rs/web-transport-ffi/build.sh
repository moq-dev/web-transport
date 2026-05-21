#!/usr/bin/env bash
# Packaging script for web-transport-ffi.
#
# Modes:
#   ./build.sh --bindings-only [--version X.Y.Z]
#       Build for the host target and emit per-language bindings tarballs
#       (dist/web-transport-ffi-${VERSION}-{python,swift,kotlin}.tar.gz).
#
#   ./build.sh --target <triple> [--version X.Y.Z] [--output dist]
#       Cross-compile native libs for <triple> and emit
#       dist/web-transport-ffi-${VERSION}-<triple>.{tar.gz,zip}.
#
# Special targets:
#   universal-apple-darwin    — lipo of x86_64 + aarch64 macOS dylib/staticlib.
#   *-linux-android*          — uses `cargo ndk` with the active NDK.
#   *-apple-ios*              — staticlib only (no dylib on iOS).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$SCRIPT_DIR"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CRATE_NAME="web-transport-ffi"
LIB_BASENAME="web_transport_ffi"

TARGET=""
VERSION=""
OUTPUT="$CRATE_DIR/dist"
BINDINGS_ONLY=0

usage() {
	cat >&2 <<EOF
Usage: $0 [--target TRIPLE] [--version X.Y.Z] [--output DIR] [--bindings-only]
EOF
	exit 1
}

while [[ $# -gt 0 ]]; do
	case "$1" in
		--target) TARGET="$2"; shift 2 ;;
		--version) VERSION="$2"; shift 2 ;;
		--output) OUTPUT="$2"; shift 2 ;;
		--bindings-only) BINDINGS_ONLY=1; shift ;;
		-h|--help) usage ;;
		*) echo "unknown arg: $1" >&2; usage ;;
	esac
done

if [[ -z "$VERSION" ]]; then
	VERSION="$(grep -m1 '^version' "$CRATE_DIR/Cargo.toml" | sed -E 's/version *= *"([^"]+)"/\1/')"
fi
if [[ -z "$VERSION" ]]; then
	echo "could not determine version" >&2
	exit 1
fi

mkdir -p "$OUTPUT"

host_triple() {
	rustc -vV | awk '/^host: / { print $2 }'
}

# Pick the cdylib suffix Rust will emit for the given target triple.
cdylib_filename() {
	local triple="$1"
	case "$triple" in
		*-apple-darwin|universal-apple-darwin) echo "lib${LIB_BASENAME}.dylib" ;;
		*-apple-ios*)                          echo "lib${LIB_BASENAME}.a"     ;;  # staticlib only
		*-windows-*)                           echo "${LIB_BASENAME}.dll"      ;;
		*)                                     echo "lib${LIB_BASENAME}.so"    ;;
	esac
}

staticlib_filename() {
	local triple="$1"
	case "$triple" in
		*-windows-*) echo "${LIB_BASENAME}.lib" ;;
		*)           echo "lib${LIB_BASENAME}.a" ;;
	esac
}

# Build the crate for $1 (target triple) and echo the path to the resulting cdylib
# (or staticlib for iOS). For universal-apple-darwin this lipos x86_64+aarch64.
build_target() {
	local triple="$1"
	local profile_dir="release"

	if [[ "$triple" == "universal-apple-darwin" ]]; then
		(cd "$WORKSPACE_DIR" && cargo build --release --target x86_64-apple-darwin -p "$CRATE_NAME")
		(cd "$WORKSPACE_DIR" && cargo build --release --target aarch64-apple-darwin -p "$CRATE_NAME")
		local out_dir="$WORKSPACE_DIR/target/universal-apple-darwin/$profile_dir"
		mkdir -p "$out_dir"
		lipo -create \
			"$WORKSPACE_DIR/target/x86_64-apple-darwin/$profile_dir/lib${LIB_BASENAME}.dylib" \
			"$WORKSPACE_DIR/target/aarch64-apple-darwin/$profile_dir/lib${LIB_BASENAME}.dylib" \
			-output "$out_dir/lib${LIB_BASENAME}.dylib"
		lipo -create \
			"$WORKSPACE_DIR/target/x86_64-apple-darwin/$profile_dir/lib${LIB_BASENAME}.a" \
			"$WORKSPACE_DIR/target/aarch64-apple-darwin/$profile_dir/lib${LIB_BASENAME}.a" \
			-output "$out_dir/lib${LIB_BASENAME}.a"
		echo "$out_dir/lib${LIB_BASENAME}.dylib"
		return
	fi

	case "$triple" in
		*-linux-android*)
			# cargo-ndk picks the right linker/sysroot from $ANDROID_NDK_HOME.
			(cd "$WORKSPACE_DIR" && cargo ndk --target "$triple" --platform 24 build --release -p "$CRATE_NAME")
			;;
		*)
			(cd "$WORKSPACE_DIR" && cargo build --release --target "$triple" -p "$CRATE_NAME")
			;;
	esac

	local target_dir="$WORKSPACE_DIR/target/$triple/$profile_dir"
	case "$triple" in
		*-apple-ios*) echo "$target_dir/$(staticlib_filename "$triple")" ;;
		*)            echo "$target_dir/$(cdylib_filename "$triple")"    ;;
	esac
}

# Package the lib artifacts for $1 (triple) using the build dir of $2 into a tar/zip.
package_target() {
	local triple="$1"
	local lib_path="$2"
	local target_dir
	target_dir="$(dirname "$lib_path")"

	local stage
	stage="$(mktemp -d)"
	local pkg_name="${CRATE_NAME}-${VERSION}-${triple}"
	mkdir -p "$stage/$pkg_name"

	# Copy whichever lib flavors exist for this triple.
	local cd_name sl_name
	cd_name="$(cdylib_filename "$triple")"
	sl_name="$(staticlib_filename "$triple")"
	[[ -f "$target_dir/$cd_name" ]] && cp "$target_dir/$cd_name" "$stage/$pkg_name/"
	[[ -f "$target_dir/$sl_name" ]] && cp "$target_dir/$sl_name" "$stage/$pkg_name/"

	case "$triple" in
		*-windows-*)
			local pdb="$target_dir/${LIB_BASENAME}.pdb"
			[[ -f "$pdb" ]] && cp "$pdb" "$stage/$pkg_name/"
			(cd "$stage" && zip -qr "$OUTPUT/${pkg_name}.zip" "$pkg_name")
			;;
		*)
			tar -czf "$OUTPUT/${pkg_name}.tar.gz" -C "$stage" "$pkg_name"
			;;
	esac

	rm -rf "$stage"
}

# Generate bindings for all three languages using the cdylib at $1.
generate_bindings() {
	local lib_path="$1"
	local bindings_root
	bindings_root="$(mktemp -d)"

	for lang in python swift kotlin; do
		local out="$bindings_root/$lang"
		mkdir -p "$out"
		(cd "$WORKSPACE_DIR" && cargo run --quiet --release -p "$CRATE_NAME" --bin uniffi-bindgen -- \
			generate --library "$lib_path" --language "$lang" --out-dir "$out") || {
				# Auto-format warnings (ktlint/swift-format/yapf missing) exit non-zero on some setups; tolerate.
				echo "warning: bindgen for $lang reported issues — continuing" >&2
			}
		local pkg_name="${CRATE_NAME}-${VERSION}-${lang}"
		tar -czf "$OUTPUT/${pkg_name}.tar.gz" -C "$bindings_root" "$lang" --transform "s|^$lang|$pkg_name|"
	done

	rm -rf "$bindings_root"
}

if [[ "$BINDINGS_ONLY" -eq 1 ]]; then
	host="$(host_triple)"
	(cd "$WORKSPACE_DIR" && cargo build --release -p "$CRATE_NAME")
	lib_path="$WORKSPACE_DIR/target/release/$(cdylib_filename "$host")"
	generate_bindings "$lib_path"
	echo "bindings written to $OUTPUT"
	exit 0
fi

if [[ -z "$TARGET" ]]; then
	echo "--target is required (unless --bindings-only)" >&2
	usage
fi

lib_path="$(build_target "$TARGET")"
package_target "$TARGET" "$lib_path"
echo "packaged $TARGET → $OUTPUT/${CRATE_NAME}-${VERSION}-${TARGET}.*"

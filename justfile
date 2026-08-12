#!/usr/bin/env just --justfile

# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation

export RUST_BACKTRACE := "1"
export RUST_LOG := "debug"

# List all of the available commands.
default:
  just --list

# Install any required dependencies.
setup:
	# Install cargo-binstall for faster tool installation.
	cargo install cargo-binstall
	just setup-tools

# A separate entrypoint for CI.
setup-tools:
	cargo binstall -y cargo-edit cargo-hack cargo-shear cargo-sort cargo-upgrades wasm-bindgen-cli

# Run the CI checks
check:
	cargo check --workspace --all-targets --all-features
	cargo clippy --workspace --all-targets --all-features -- -D warnings

	# Do the same but explicitly use the WASM target.
	cargo check --target wasm32-unknown-unknown -p web-transport --all-targets --all-features
	cargo check --target wasm32-unknown-unknown -p web-transport-wasm --all-targets --all-features
	cargo clippy --target wasm32-unknown-unknown -p web-transport --all-targets --all-features -- -D warnings
	cargo clippy --target wasm32-unknown-unknown -p web-transport-wasm --all-targets --all-features -- -D warnings

	# Make sure the formatting is correct.
	cargo fmt --all --check

	# requires: cargo install cargo-hack
	# web-transport-ffi excluded from the feature powerset because aws-lc-rs and
	# ring are mutually exclusive at link time (one rustls provider must win).
	cargo hack check --feature-powerset --workspace --keep-going --exclude web-transport-node --exclude web-transport-ffi
	cargo hack check --feature-powerset --target wasm32-unknown-unknown -p web-transport --keep-going
	cargo hack check --feature-powerset --target wasm32-unknown-unknown -p web-transport-wasm --keep-going

	# web-transport-ffi: explicit check under each TLS provider.
	cargo check -p web-transport-ffi
	cargo check -p web-transport-ffi --no-default-features --features ring

	# requires: cargo install cargo-shear
	cargo shear

	# requires: cargo install cargo-sort
	cargo sort --workspace --check

	# Check JavaScript/TypeScript with biome
	bun install
	bun run check
	bun run --filter '*' check

# Run any CI tests
test:
	cargo test --workspace --all-targets --all-features
	cargo test --target wasm32-unknown-unknown -p web-transport --all-targets --all-features
	cargo test --target wasm32-unknown-unknown -p web-transport-wasm --all-targets --all-features
	bun run --cwd js/qmux test
	bun run --cwd js/qmux test:interop

# Automatically fix some issues.
fix:
	cargo fix --allow-staged --allow-dirty --workspace --all-targets --all-features
	cargo clippy --fix --allow-staged --allow-dirty --workspace --all-targets --all-features

	# Do the same but explicitly use the WASM target.
	cargo fix --allow-staged --allow-dirty --target wasm32-unknown-unknown -p web-transport --all-targets --all-features
	cargo fix --allow-staged --allow-dirty --target wasm32-unknown-unknown -p web-transport-wasm --all-targets --all-features
	cargo clippy --fix --allow-staged --allow-dirty --target wasm32-unknown-unknown -p web-transport --all-targets --all-features
	cargo clippy --fix --allow-staged --allow-dirty --target wasm32-unknown-unknown -p web-transport-wasm --all-targets --all-features

	# requires: cargo install cargo-shear
	cargo shear --fix

	# requires: cargo install cargo-sort
	cargo sort --workspace

	# And of course, make sure the formatting is correct.
	cargo fmt --all

	# Fix JavaScript/TypeScript with biome
	bun install
	bun run fix

# Run the browser harness for web-transport-wasm.
#
# CI only compiles the WASM crate; the browser API it wraps exists nowhere else, so
# the poll paths that are easiest to get wrong have no automated coverage. This
# builds the harness, starts its QUIC peer, and serves the page for a browser to
# open. Requires `dev/setup` to have generated the localhost certificate.
harness port="8080":
	#!/usr/bin/env bash
	set -euo pipefail

	[ -f dev/localhost.crt ] || ./dev/setup

	# wasm-bindgen refuses to process a file built against a different schema, but
	# adjacent crate and CLI versions can share one. Let the CLI make that decision;
	# if it fails, add the version provenance its schema error omits.
	want=$(cargo metadata --format-version 1 --locked \
		| python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="wasm-bindgen"))')
	got=$(wasm-bindgen --version | awk '{print $2}')

	# Build the harness and generate its JS bindings next to the page.
	out="target/harness"
	cargo build --example harness -p web-transport-wasm --target wasm32-unknown-unknown
	if ! wasm-bindgen --target web --out-dir "$out" \
		target/wasm32-unknown-unknown/debug/examples/harness.wasm; then
		echo "wasm-bindgen CLI is $got but the build uses $want." >&2
		echo "Install the matching one, outside the nix shell if that is where this came from:" >&2
		echo "    cargo binstall -y --force wasm-bindgen-cli@$want" >&2
		exit 1
	fi
	cp rs/web-transport-wasm/examples/harness.html "$out/index.html"
	cp dev/localhost.hex "$out/localhost.hex"

	cargo build --example harness-server -p web-transport-quinn
	./target/debug/examples/harness-server \
		--tls-cert dev/localhost.crt --tls-key dev/localhost.key &
	server=$!
	trap 'kill $server 2>/dev/null || true' EXIT

	echo "==> open http://localhost:{{port}}/"
	python3 -m http.server {{port}} --directory "$out"

# Build the FFI staticlib/cdylib for the host and generate language bindings.
build-ffi:
	./rs/web-transport-ffi/build.sh --bindings-only --output rs/web-transport-ffi/dist

# Build the FFI crate for a single target (use `just build-ffi-target aarch64-apple-darwin`).
build-ffi-target target:
	./rs/web-transport-ffi/build.sh --target {{target}} --output rs/web-transport-ffi/dist

# Delete build artifacts and caches to reclaim disk space. web-transport keeps
# a single root justfile (unlike moq's per-language modules), so the per-language
# cleanups are inlined here. Sweeps the shared caches, then recurses into any
# agent worktrees under .claude/worktrees/.
clean:
	#!/usr/bin/env bash
	set -euo pipefail

	# Rust: workspace target dir (also used by maturin for the Python build).
	cargo clean

	# JS/TS: node_modules, bundler output, tsbuildinfo caches.
	find . -name .claude -prune -o \
		-type d \( -name node_modules -o -name dist -o -name out -o -name pkg \) \
		-prune -exec rm -rf {} +
	find . -name .claude -prune -o -type f -name '*.tsbuildinfo' -exec rm -f {} +

	# Python: virtualenv, build output, generated uniffi bindings, caches.
	rm -rf py/web-transport/.venv py/web-transport/dist \
		py/web-transport/python/web_transport/_uniffi
	find . -name .claude -prune -o -type d -name __pycache__ -prune -exec rm -rf {} +
	find . -name .claude -prune -o -type f -name '*.pyc' -exec rm -f {} +

	# Kotlin: gradle build dirs + generated bindings/native libs.
	find . -name .claude -prune -o -type d \( -name build -o -name .gradle -o -name .kotlin \) -prune -exec rm -rf {} +
	rm -rf kt/local.properties \
		kt/web-transport/src/jvmAndAndroidMain/kotlin/uniffi \
		kt/web-transport/src/jvmMain/resources \
		kt/web-transport/src/androidMain/jniLibs

	# Swift: SPM build dir + generated bindings/xcframework.
	rm -rf swift/.build swift/.swiftpm swift/Package.resolved \
		swift/Sources/WebTransportFFI/Generated.swift swift/WebTransportFFI.xcframework

	# FFI: per-host staticlib/bindings output.
	rm -rf rs/web-transport-ffi/dist

	# Caches not owned by any one language: nix build result, direnv, wrangler.
	rm -rf result .direnv
	find . -name .claude -prune -o -type d -name .wrangler -prune -exec rm -rf {} +

	# Reclaim Nix store space too, if Nix is installed.
	if command -v nix-collect-garbage &> /dev/null; then nix-collect-garbage -d; fi

	# Agent worktrees each carry their own artifacts. Worktrees don't nest, so
	# this recurses exactly one level. Tolerate stale worktrees on branches that
	# predate this recipe.
	for wt in .claude/worktrees/*/; do
		[ -f "${wt}justfile" ] || continue
		echo "==> cleaning ${wt}"
		(cd "$wt" && just clean) || echo "    (skipped: just clean failed in ${wt})"
	done

# Upgrade any tooling
upgrade:
	rustup upgrade

	# Requires: cargo install cargo-upgrades cargo-edit
	cargo upgrade

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
	cargo hack check --feature-powerset --workspace --keep-going --exclude web-transport-node
	cargo hack check --feature-powerset --target wasm32-unknown-unknown -p web-transport --keep-going
	cargo hack check --feature-powerset --target wasm32-unknown-unknown -p web-transport-wasm --keep-going

	# requires: cargo install cargo-shear
	cargo shear

	# requires: cargo install cargo-sort
	cargo sort --workspace --check

	# Check JavaScript/TypeScript with biome
	bun install
	bun run check
	bun --filter '*' run check

# Run any CI tests
test:
	cargo test --workspace --all-targets --all-features
	cargo test --target wasm32-unknown-unknown -p web-transport --all-targets --all-features
	cargo test --target wasm32-unknown-unknown -p web-transport-wasm --all-targets --all-features

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

# Upgrade any tooling
upgrade:
	rustup upgrade

	# Requires: cargo install cargo-upgrades cargo-edit
	cargo upgrade

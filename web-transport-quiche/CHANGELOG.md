# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/moq-dev/web-transport/compare/web-transport-quiche-v0.1.0...web-transport-quiche-v0.2.0) - 2026-02-11

### Other

- Fix a panic caused by longer error codes. ([#160](https://github.com/moq-dev/web-transport/pull/160))
- Async accept ([#159](https://github.com/moq-dev/web-transport/pull/159))

## [0.0.6](https://github.com/moq-dev/web-transport/compare/web-transport-quiche-v0.0.5...web-transport-quiche-v0.0.6) - 2026-02-10

### Other

- Add Incoming struct to inspect connections before accepting ([#155](https://github.com/moq-dev/web-transport/pull/155))
- Fix capsule protocol handling ([#152](https://github.com/moq-dev/web-transport/pull/152))
- Fix stream capacity tracking panic ([#153](https://github.com/moq-dev/web-transport/pull/153))

## [0.0.5](https://github.com/moq-dev/web-transport/compare/web-transport-quiche-v0.0.4...web-transport-quiche-v0.0.5) - 2026-02-07

### Other

- Add `protocol()` to web-transport-trait ([#149](https://github.com/moq-dev/web-transport/pull/149))

## [0.0.4](https://github.com/moq-dev/web-transport/compare/web-transport-quiche-v0.0.3...web-transport-quiche-v0.0.4) - 2026-02-07

### Other

- Add a helper to set up quiche certs. ([#147](https://github.com/moq-dev/web-transport/pull/147))

## [0.0.3](https://github.com/moq-dev/web-transport/compare/web-transport-quiche-v0.0.2...web-transport-quiche-v0.0.3) - 2026-01-23

### Other

- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))

## [0.0.2](https://github.com/moq-dev/web-transport/compare/web-transport-quiche-v0.0.1...web-transport-quiche-v0.0.2) - 2026-01-07

### Other

- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))

## [0.0.1](https://github.com/moq-dev/web-transport/releases/tag/web-transport-quiche-v0.0.1) - 2025-11-14

### Other

- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Fix a rare race when accepting a stream. ([#120](https://github.com/moq-dev/web-transport/pull/120))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

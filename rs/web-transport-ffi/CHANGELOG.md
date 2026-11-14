# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/moq-dev/web-transport/compare/web-transport-ffi-v0.2.2...web-transport-ffi-v0.3.0) - 2026-11-14

### Added

- [**breaking**] preserve CONNECT response and rejection headers ([#499](https://github.com/moq-dev/web-transport/pull/499))
- *(quinn)* [**breaking**] use rind through a provider-neutral QUIC adapter ([#488](https://github.com/moq-dev/web-transport/pull/488))

### Fixed

- *(kt)* ship R8 keep rules for JNA and the UniFFI bindings ([#524](https://github.com/moq-dev/web-transport/pull/524))
- *(ffi)* verify default clients with the OS platform verifier ([#511](https://github.com/moq-dev/web-transport/pull/511))
- *(quinn)* [**breaking**] close the connection when the final Session drops ([#480](https://github.com/moq-dev/web-transport/pull/480))
- *(native)* [**breaking**] bind default client sockets by address family ([#490](https://github.com/moq-dev/web-transport/pull/490))

### Other

- Fix stale comments and broken doc links ([#526](https://github.com/moq-dev/web-transport/pull/526))
- *(ffi)* share web-transport-quinn's Android verifier initializer ([#522](https://github.com/moq-dev/web-transport/pull/522))
- Document package and backend selection ([#501](https://github.com/moq-dev/web-transport/pull/501))

## [0.2.2](https://github.com/moq-dev/web-transport/compare/web-transport-ffi-v0.2.1...web-transport-ffi-v0.2.2) - 2026-10-14

### Other

- import relevant MoQ agent guidance ([#423](https://github.com/moq-dev/web-transport/pull/423))

## [0.2.1](https://github.com/moq-dev/web-transport/compare/web-transport-ffi-v0.2.0...web-transport-ffi-v0.2.1) - 2026-09-27

### Other

- *(deps)* bump the cargo group across 1 directory with 4 updates ([#400](https://github.com/moq-dev/web-transport/pull/400))

## [0.1.2](https://github.com/moq-dev/web-transport/compare/web-transport-ffi-v0.1.1...web-transport-ffi-v0.1.2) - 2026-08-06

### Other

- updated the following local packages: web-transport-quinn

## [0.1.1](https://github.com/moq-dev/web-transport/compare/web-transport-ffi-v0.1.0...web-transport-ffi-v0.1.1) - 2026-05-24

### Other

- bump tokio-quiche, tokio-tungstenite, flume, rcgen, sha2 ([#240](https://github.com/moq-dev/web-transport/pull/240))

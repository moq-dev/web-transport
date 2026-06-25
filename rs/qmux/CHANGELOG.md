# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/moq-dev/web-transport/compare/qmux-v0.2.0...qmux-v0.3.1) - 2026-06-25

### Added

- *(qmux)* resolve the application protocol during establishment so `Session::protocol` is a synchronous getter; fold establishment into async `connect`/`accept` and add `Config::handshake_timeout` ([#265](https://github.com/moq-dev/web-transport/pull/265))
- *(qmux)* mark `Config`, `Error`, and `Protocol` `#[non_exhaustive]`; replace the `tls` free functions with `tls::Client`/`tls::Server` builders ([#265](https://github.com/moq-dev/web-transport/pull/265))

## [0.2.0](https://github.com/moq-dev/web-transport/compare/qmux-v0.1.3...qmux-v0.2.0) - 2026-06-19

### Added

- *(qmux)* in-band protocol negotiation + Unix socket transport ([#259](https://github.com/moq-dev/web-transport/pull/259))

## [0.0.8](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.7...qmux-v0.0.8) - 2026-05-24

### Other

- bump tokio-quiche, tokio-tungstenite, flume, rcgen, sha2 ([#240](https://github.com/moq-dev/web-transport/pull/240))

## [0.0.7](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.6...qmux-v0.0.7) - 2026-05-21

### Other

- add Keepalive option for WebSocket transports ([#234](https://github.com/moq-dev/web-transport/pull/234))

## [0.0.6](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.5...qmux-v0.0.6) - 2026-04-07

### Other

- Fix @moq/qmux publish and build scripts ([#207](https://github.com/moq-dev/web-transport/pull/207))
- Add NapiClient.disableVerify() and use NAPI tokio runtime ([#205](https://github.com/moq-dev/web-transport/pull/205))

## [0.0.4](https://github.com/moq-dev/web-transport/compare/qmux-v0.0.3...qmux-v0.0.4) - 2026-03-13

### Other

- Add qmux crate and deprecate web-transport-ws ([#191](https://github.com/moq-dev/web-transport/pull/191))

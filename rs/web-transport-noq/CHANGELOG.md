# Changelog

## [0.3.1](https://github.com/moq-dev/web-transport/compare/web-transport-noq-v0.3.0...web-transport-noq-v0.3.1) - 2026-09-04

### Other

- Widen stream priority to i32, ranked onto quiche's urgency ([#390](https://github.com/moq-dev/web-transport/pull/390))

## [0.3.0](https://github.com/moq-dev/web-transport/compare/web-transport-noq-v0.2.1...web-transport-noq-v0.3.0) - 2026-08-06

### Fixed

- [**breaking**] release accept and ez waker registrations when a caller gives up ([#353](https://github.com/moq-dev/web-transport/pull/353))

### Other

- collapse the four AcceptWaiters copies onto kio::Fan ([#359](https://github.com/moq-dev/web-transport/pull/359))
- finish the ez waiter conversion, and build the poll bridge on kio::Park ([#358](https://github.com/moq-dev/web-transport/pull/358))
- [**breaking**] drop the synthesized CONNECT request and response from raw QUIC sessions ([#326](https://github.com/moq-dev/web-transport/pull/326))

## [0.2.1](https://github.com/moq-dev/web-transport/compare/web-transport-noq-v0.2.0...web-transport-noq-v0.2.1) - 2026-07-22

### Other

- Expose qlog configuration, and fix the quinn server's transport config ([#334](https://github.com/moq-dev/web-transport/pull/334))

## [0.1.1](https://github.com/moq-dev/web-transport/compare/web-transport-noq-v0.1.0...web-transport-noq-v0.1.1) - 2026-05-24

### Other

- release ([#228](https://github.com/moq-dev/web-transport/pull/228))

## [0.1.0](https://github.com/moq-dev/web-transport/compare/web-transport-noq-v0.0.4...web-transport-noq-v0.1.0) - 2026-05-21

### Other

- update iroh and noq to 1.0-rc.0 ([#236](https://github.com/moq-dev/web-transport/pull/236))
- update to iroh 0.98 and noq 0.18 ([#230](https://github.com/moq-dev/web-transport/pull/230))

## [0.0.4](https://github.com/moq-dev/web-transport/compare/web-transport-noq-v0.0.3...web-transport-noq-v0.0.4) - 2026-04-07

### Other

- Expose conn() by reference and fix Python bindings ([#227](https://github.com/moq-dev/web-transport/pull/227))

## [0.0.2](https://github.com/moq-dev/web-transport/compare/web-transport-noq-v0.0.1...web-transport-noq-v0.0.2) - 2026-03-11

### Other

- Fix typos and bump to v0.0.2 ([#188](https://github.com/moq-dev/web-transport/pull/188))

## [0.0.1] - 2026-03-11
- Initial fork from web-transport-quinn, targeting the Noq QUIC implementation.

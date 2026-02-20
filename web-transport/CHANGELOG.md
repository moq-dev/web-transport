# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.6](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.5...web-transport-v0.9.6) - 2025-09-04

### Other

- updated the following local packages: web-transport-quinn

## [0.9.5](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.4...web-transport-v0.9.5) - 2025-09-03

### Other

- Rename the repo. ([#94](https://github.com/moq-dev/web-transport/pull/94))
- Add web-transport-trait and web-transport-ws ([#89](https://github.com/moq-dev/web-transport/pull/89))
# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.3](https://github.com/moq-dev/web-transport/compare/web-transport-v0.10.2...web-transport-v0.10.3) - 2026-02-20

### Other

- release ([#173](https://github.com/moq-dev/web-transport/pull/173))

## [0.10.2](https://github.com/moq-dev/web-transport/compare/web-transport-v0.10.1...web-transport-v0.10.2) - 2026-02-20

### Fixed

- fix connecting to ipv6 using quinn backend ([#82](https://github.com/moq-dev/web-transport/pull/82))

### Other

- release ([#171](https://github.com/moq-dev/web-transport/pull/171))
- release ([#167](https://github.com/moq-dev/web-transport/pull/167))
- release ([#166](https://github.com/moq-dev/web-transport/pull/166))
- release ([#162](https://github.com/moq-dev/web-transport/pull/162))
- release ([#156](https://github.com/moq-dev/web-transport/pull/156))
- Async accept ([#159](https://github.com/moq-dev/web-transport/pull/159))
- release ([#150](https://github.com/moq-dev/web-transport/pull/150))
- release ([#148](https://github.com/moq-dev/web-transport/pull/148))
- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))
- release ([#101](https://github.com/moq-dev/web-transport/pull/101))
- Add impl Clone for Client ([#104](https://github.com/moq-dev/web-transport/pull/104))
- *(web-transport-ws)* release v0.1.1 ([#97](https://github.com/moq-dev/web-transport/pull/97))
- release ([#87](https://github.com/moq-dev/web-transport/pull/87))
- Rename the repo. ([#94](https://github.com/moq-dev/web-transport/pull/94))
- Add web-transport-trait and web-transport-ws ([#89](https://github.com/moq-dev/web-transport/pull/89))
- release ([#85](https://github.com/moq-dev/web-transport/pull/85))
- release ([#83](https://github.com/moq-dev/web-transport/pull/83))
- release ([#81](https://github.com/moq-dev/web-transport/pull/81))
- Fully take ownership of the Url, not a ref. ([#80](https://github.com/moq-dev/web-transport/pull/80))
- *(web-transport)* release v0.9.1 ([#79](https://github.com/moq-dev/web-transport/pull/79))
- Again. ([#78](https://github.com/moq-dev/web-transport/pull/78))
- It's actually a breaking change. ([#77](https://github.com/moq-dev/web-transport/pull/77))
- release ([#76](https://github.com/moq-dev/web-transport/pull/76))
- Add a required `url` to Session ([#75](https://github.com/moq-dev/web-transport/pull/75))
- *(web-transport-proto)* release v0.2.6 ([#72](https://github.com/moq-dev/web-transport/pull/72))
- Add (generic) support for learning when a stream is closed. ([#73](https://github.com/moq-dev/web-transport/pull/73))
- release ([#63](https://github.com/moq-dev/web-transport/pull/63))
- Adding with_unreliable shim functions to wasm/quinn ClientBuilders for easier generic use ([#64](https://github.com/moq-dev/web-transport/pull/64))
- release ([#61](https://github.com/moq-dev/web-transport/pull/61))
- Revamp client/server building. ([#60](https://github.com/moq-dev/web-transport/pull/60))
- release ([#54](https://github.com/moq-dev/web-transport/pull/54))
- Bump some deps. ([#55](https://github.com/moq-dev/web-transport/pull/55))
- Clippy fixes. ([#53](https://github.com/moq-dev/web-transport/pull/53))
- release ([#51](https://github.com/moq-dev/web-transport/pull/51))
- Make a `Client` class to make configuration easier. ([#50](https://github.com/moq-dev/web-transport/pull/50))
- *(web-transport)* release v0.6.2 ([#47](https://github.com/moq-dev/web-transport/pull/47))
- Gotta bump deps too. ([#48](https://github.com/moq-dev/web-transport/pull/48))
- Update crate description
- release ([#46](https://github.com/moq-dev/web-transport/pull/46))
- Derive PartialEq for Session. ([#45](https://github.com/moq-dev/web-transport/pull/45))
- Release web-transport and web-transport-wasm
- Unify the read/write arguments ([#38](https://github.com/moq-dev/web-transport/pull/38))
- release ([#35](https://github.com/moq-dev/web-transport/pull/35))
- Some more documentation. ([#34](https://github.com/moq-dev/web-transport/pull/34))
- Minor doc stuff.
- Bump WASM version. ([#32](https://github.com/moq-dev/web-transport/pull/32))
- More WASM improvements. ([#31](https://github.com/moq-dev/web-transport/pull/31))
- WASM improvements ([#30](https://github.com/moq-dev/web-transport/pull/30))
- Bump since removing Cargo.lock
- Upgrade quinn ([#26](https://github.com/moq-dev/web-transport/pull/26))
- Encode the correct order of pseudo-headers. ([#25](https://github.com/moq-dev/web-transport/pull/25))
- Gotta bump since I changed max_datagram_size
- Add some web-transport documentation. ([#24](https://github.com/moq-dev/web-transport/pull/24))
- Start at 0.1 actually since the rename, ([#22](https://github.com/moq-dev/web-transport/pull/22))
- Remove webtransport-generic in favor of web-transport ([#21](https://github.com/moq-dev/web-transport/pull/21))
- Register the web-transport crate. ([#19](https://github.com/moq-dev/web-transport/pull/19))

## [0.10.2](https://github.com/moq-dev/web-transport/compare/web-transport-v0.10.1...web-transport-v0.10.2) - 2026-02-20

### Fixed

- fix connecting to ipv6 using quinn backend ([#82](https://github.com/moq-dev/web-transport/pull/82))

### Other

- release ([#167](https://github.com/moq-dev/web-transport/pull/167))
- release ([#166](https://github.com/moq-dev/web-transport/pull/166))
- release ([#162](https://github.com/moq-dev/web-transport/pull/162))
- release ([#156](https://github.com/moq-dev/web-transport/pull/156))
- Async accept ([#159](https://github.com/moq-dev/web-transport/pull/159))
- release ([#150](https://github.com/moq-dev/web-transport/pull/150))
- release ([#148](https://github.com/moq-dev/web-transport/pull/148))
- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))
- release ([#101](https://github.com/moq-dev/web-transport/pull/101))
- Add impl Clone for Client ([#104](https://github.com/moq-dev/web-transport/pull/104))
- *(web-transport-ws)* release v0.1.1 ([#97](https://github.com/moq-dev/web-transport/pull/97))
- release ([#87](https://github.com/moq-dev/web-transport/pull/87))
- Rename the repo. ([#94](https://github.com/moq-dev/web-transport/pull/94))
- Add web-transport-trait and web-transport-ws ([#89](https://github.com/moq-dev/web-transport/pull/89))
- release ([#85](https://github.com/moq-dev/web-transport/pull/85))
- release ([#83](https://github.com/moq-dev/web-transport/pull/83))
- release ([#81](https://github.com/moq-dev/web-transport/pull/81))
- Fully take ownership of the Url, not a ref. ([#80](https://github.com/moq-dev/web-transport/pull/80))
- *(web-transport)* release v0.9.1 ([#79](https://github.com/moq-dev/web-transport/pull/79))
- Again. ([#78](https://github.com/moq-dev/web-transport/pull/78))
- It's actually a breaking change. ([#77](https://github.com/moq-dev/web-transport/pull/77))
- release ([#76](https://github.com/moq-dev/web-transport/pull/76))
- Add a required `url` to Session ([#75](https://github.com/moq-dev/web-transport/pull/75))
- *(web-transport-proto)* release v0.2.6 ([#72](https://github.com/moq-dev/web-transport/pull/72))
- Add (generic) support for learning when a stream is closed. ([#73](https://github.com/moq-dev/web-transport/pull/73))
- release ([#63](https://github.com/moq-dev/web-transport/pull/63))
- Adding with_unreliable shim functions to wasm/quinn ClientBuilders for easier generic use ([#64](https://github.com/moq-dev/web-transport/pull/64))
- release ([#61](https://github.com/moq-dev/web-transport/pull/61))
- Revamp client/server building. ([#60](https://github.com/moq-dev/web-transport/pull/60))
- release ([#54](https://github.com/moq-dev/web-transport/pull/54))
- Bump some deps. ([#55](https://github.com/moq-dev/web-transport/pull/55))
- Clippy fixes. ([#53](https://github.com/moq-dev/web-transport/pull/53))
- release ([#51](https://github.com/moq-dev/web-transport/pull/51))
- Make a `Client` class to make configuration easier. ([#50](https://github.com/moq-dev/web-transport/pull/50))
- *(web-transport)* release v0.6.2 ([#47](https://github.com/moq-dev/web-transport/pull/47))
- Gotta bump deps too. ([#48](https://github.com/moq-dev/web-transport/pull/48))
- Update crate description
- release ([#46](https://github.com/moq-dev/web-transport/pull/46))
- Derive PartialEq for Session. ([#45](https://github.com/moq-dev/web-transport/pull/45))
- Release web-transport and web-transport-wasm
- Unify the read/write arguments ([#38](https://github.com/moq-dev/web-transport/pull/38))
- release ([#35](https://github.com/moq-dev/web-transport/pull/35))
- Some more documentation. ([#34](https://github.com/moq-dev/web-transport/pull/34))
- Minor doc stuff.
- Bump WASM version. ([#32](https://github.com/moq-dev/web-transport/pull/32))
- More WASM improvements. ([#31](https://github.com/moq-dev/web-transport/pull/31))
- WASM improvements ([#30](https://github.com/moq-dev/web-transport/pull/30))
- Bump since removing Cargo.lock
- Upgrade quinn ([#26](https://github.com/moq-dev/web-transport/pull/26))
- Encode the correct order of pseudo-headers. ([#25](https://github.com/moq-dev/web-transport/pull/25))
- Gotta bump since I changed max_datagram_size
- Add some web-transport documentation. ([#24](https://github.com/moq-dev/web-transport/pull/24))
- Start at 0.1 actually since the rename, ([#22](https://github.com/moq-dev/web-transport/pull/22))
- Remove webtransport-generic in favor of web-transport ([#21](https://github.com/moq-dev/web-transport/pull/21))
- Register the web-transport crate. ([#19](https://github.com/moq-dev/web-transport/pull/19))

## [0.10.1](https://github.com/moq-dev/web-transport/compare/web-transport-v0.10.0...web-transport-v0.10.1) - 2026-02-20

### Fixed

- fix connecting to ipv6 using quinn backend ([#82](https://github.com/moq-dev/web-transport/pull/82))

### Other

- release ([#166](https://github.com/moq-dev/web-transport/pull/166))
- release ([#162](https://github.com/moq-dev/web-transport/pull/162))
- release ([#156](https://github.com/moq-dev/web-transport/pull/156))
- Async accept ([#159](https://github.com/moq-dev/web-transport/pull/159))
- release ([#150](https://github.com/moq-dev/web-transport/pull/150))
- release ([#148](https://github.com/moq-dev/web-transport/pull/148))
- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))
- release ([#101](https://github.com/moq-dev/web-transport/pull/101))
- Add impl Clone for Client ([#104](https://github.com/moq-dev/web-transport/pull/104))
- *(web-transport-ws)* release v0.1.1 ([#97](https://github.com/moq-dev/web-transport/pull/97))
- release ([#87](https://github.com/moq-dev/web-transport/pull/87))
- Rename the repo. ([#94](https://github.com/moq-dev/web-transport/pull/94))
- Add web-transport-trait and web-transport-ws ([#89](https://github.com/moq-dev/web-transport/pull/89))
- release ([#85](https://github.com/moq-dev/web-transport/pull/85))
- release ([#83](https://github.com/moq-dev/web-transport/pull/83))
- release ([#81](https://github.com/moq-dev/web-transport/pull/81))
- Fully take ownership of the Url, not a ref. ([#80](https://github.com/moq-dev/web-transport/pull/80))
- *(web-transport)* release v0.9.1 ([#79](https://github.com/moq-dev/web-transport/pull/79))
- Again. ([#78](https://github.com/moq-dev/web-transport/pull/78))
- It's actually a breaking change. ([#77](https://github.com/moq-dev/web-transport/pull/77))
- release ([#76](https://github.com/moq-dev/web-transport/pull/76))
- Add a required `url` to Session ([#75](https://github.com/moq-dev/web-transport/pull/75))
- *(web-transport-proto)* release v0.2.6 ([#72](https://github.com/moq-dev/web-transport/pull/72))
- Add (generic) support for learning when a stream is closed. ([#73](https://github.com/moq-dev/web-transport/pull/73))
- release ([#63](https://github.com/moq-dev/web-transport/pull/63))
- Adding with_unreliable shim functions to wasm/quinn ClientBuilders for easier generic use ([#64](https://github.com/moq-dev/web-transport/pull/64))
- release ([#61](https://github.com/moq-dev/web-transport/pull/61))
- Revamp client/server building. ([#60](https://github.com/moq-dev/web-transport/pull/60))
- release ([#54](https://github.com/moq-dev/web-transport/pull/54))
- Bump some deps. ([#55](https://github.com/moq-dev/web-transport/pull/55))
- Clippy fixes. ([#53](https://github.com/moq-dev/web-transport/pull/53))
- release ([#51](https://github.com/moq-dev/web-transport/pull/51))
- Make a `Client` class to make configuration easier. ([#50](https://github.com/moq-dev/web-transport/pull/50))
- *(web-transport)* release v0.6.2 ([#47](https://github.com/moq-dev/web-transport/pull/47))
- Gotta bump deps too. ([#48](https://github.com/moq-dev/web-transport/pull/48))
- Update crate description
- release ([#46](https://github.com/moq-dev/web-transport/pull/46))
- Derive PartialEq for Session. ([#45](https://github.com/moq-dev/web-transport/pull/45))
- Release web-transport and web-transport-wasm
- Unify the read/write arguments ([#38](https://github.com/moq-dev/web-transport/pull/38))
- release ([#35](https://github.com/moq-dev/web-transport/pull/35))
- Some more documentation. ([#34](https://github.com/moq-dev/web-transport/pull/34))
- Minor doc stuff.
- Bump WASM version. ([#32](https://github.com/moq-dev/web-transport/pull/32))
- More WASM improvements. ([#31](https://github.com/moq-dev/web-transport/pull/31))
- WASM improvements ([#30](https://github.com/moq-dev/web-transport/pull/30))
- Bump since removing Cargo.lock
- Upgrade quinn ([#26](https://github.com/moq-dev/web-transport/pull/26))
- Encode the correct order of pseudo-headers. ([#25](https://github.com/moq-dev/web-transport/pull/25))
- Gotta bump since I changed max_datagram_size
- Add some web-transport documentation. ([#24](https://github.com/moq-dev/web-transport/pull/24))
- Start at 0.1 actually since the rename, ([#22](https://github.com/moq-dev/web-transport/pull/22))
- Remove webtransport-generic in favor of web-transport ([#21](https://github.com/moq-dev/web-transport/pull/21))
- Register the web-transport crate. ([#19](https://github.com/moq-dev/web-transport/pull/19))

## [0.10.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.7...web-transport-v0.10.0) - 2026-02-13

### Other

- release ([#162](https://github.com/moq-dev/web-transport/pull/162))
- release ([#156](https://github.com/moq-dev/web-transport/pull/156))
- Async accept ([#159](https://github.com/moq-dev/web-transport/pull/159))
- release ([#150](https://github.com/moq-dev/web-transport/pull/150))
- release ([#148](https://github.com/moq-dev/web-transport/pull/148))
- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

## [0.10.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.7...web-transport-v0.10.0) - 2026-02-13

### Other

- release ([#156](https://github.com/moq-dev/web-transport/pull/156))
- Async accept ([#159](https://github.com/moq-dev/web-transport/pull/159))
- release ([#150](https://github.com/moq-dev/web-transport/pull/150))
- release ([#148](https://github.com/moq-dev/web-transport/pull/148))
- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

## [0.10.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.7...web-transport-v0.10.0) - 2026-02-11

### Other

- Async accept ([#159](https://github.com/moq-dev/web-transport/pull/159))
- release ([#150](https://github.com/moq-dev/web-transport/pull/150))
- release ([#148](https://github.com/moq-dev/web-transport/pull/148))
- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

## [0.10.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.7...web-transport-v0.10.0) - 2026-02-10

### Other

- release ([#148](https://github.com/moq-dev/web-transport/pull/148))
- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

## [0.10.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.7...web-transport-v0.10.0) - 2026-02-07

### Other

- release ([#146](https://github.com/moq-dev/web-transport/pull/146))
- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

## [0.10.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.7...web-transport-v0.10.0) - 2026-02-07

### Other

- Manually run release-plz because CI is broken? ([#145](https://github.com/moq-dev/web-transport/pull/145))
- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

## [0.10.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.7...web-transport-v0.10.0) - 2026-01-23

### Other

- Sub-protocol negotiation + breaking API changes ([#143](https://github.com/moq-dev/web-transport/pull/143))
- release ([#122](https://github.com/moq-dev/web-transport/pull/122))
- Double check that read_buf is properly implemented. ([#137](https://github.com/moq-dev/web-transport/pull/137))
- Remove with_unreliable. ([#136](https://github.com/moq-dev/web-transport/pull/136))
- Don't require &mut for web-transport ([#134](https://github.com/moq-dev/web-transport/pull/134))
- Rename the repo into a new org. ([#132](https://github.com/moq-dev/web-transport/pull/132))
- We should bump the web-transport-trait crates. ([#123](https://github.com/moq-dev/web-transport/pull/123))
- release ([#119](https://github.com/moq-dev/web-transport/pull/119))
- Avoid some spurious semver changes and bump the rest ([#121](https://github.com/moq-dev/web-transport/pull/121))
- Initial web-transport-quiche support ([#118](https://github.com/moq-dev/web-transport/pull/118))

## [0.9.7](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.6...web-transport-v0.9.7) - 2025-10-17

### Other

- Add impl Clone for Client ([#104](https://github.com/moq-dev/web-transport/pull/104))

## [0.9.4](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.3...web-transport-v0.9.4) - 2025-07-20

### Other

- updated the following local packages: web-transport-quinn

## [0.9.3](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.2...web-transport-v0.9.3) - 2025-06-02

### Fixed

- fix connecting to ipv6 using quinn backend ([#82](https://github.com/moq-dev/web-transport/pull/82))

## [0.9.2](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.1...web-transport-v0.9.2) - 2025-05-21

### Other

- Fully take ownership of the Url, not a ref. ([#80](https://github.com/moq-dev/web-transport/pull/80))

## [0.9.1](https://github.com/moq-dev/web-transport/compare/web-transport-v0.9.0...web-transport-v0.9.1) - 2025-05-21

### Other

- Again. ([#78](https://github.com/moq-dev/web-transport/pull/78))

## [0.8.3](https://github.com/moq-dev/web-transport/compare/web-transport-v0.8.2...web-transport-v0.8.3) - 2025-05-21

### Other

- Add a required `url` to Session ([#75](https://github.com/moq-dev/web-transport/pull/75))

## [0.8.2](https://github.com/moq-dev/web-transport/compare/web-transport-v0.8.1...web-transport-v0.8.2) - 2025-05-15

### Other

- Add (generic) support for learning when a stream is closed. ([#73](https://github.com/moq-dev/web-transport/pull/73))

## [0.8.1](https://github.com/moq-dev/web-transport/compare/web-transport-v0.8.0...web-transport-v0.8.1) - 2025-03-26

### Other

- Adding with_unreliable shim functions to wasm/quinn ClientBuilders for easier generic use ([#64](https://github.com/moq-dev/web-transport/pull/64))

## [0.8.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.7.1...web-transport-v0.8.0) - 2025-01-26

### Other

- Revamp client/server building. ([#60](https://github.com/moq-dev/web-transport/pull/60))

## [0.7.1](https://github.com/moq-dev/web-transport/compare/web-transport-v0.7.0...web-transport-v0.7.1) - 2025-01-15

### Other

- Bump some deps. ([#55](https://github.com/moq-dev/web-transport/pull/55))
- Clippy fixes. ([#53](https://github.com/moq-dev/web-transport/pull/53))

## [0.7.0](https://github.com/moq-dev/web-transport/compare/web-transport-v0.6.2...web-transport-v0.7.0) - 2024-12-03

### Other

- Make a `Client` class to make configuration easier. ([#50](https://github.com/moq-dev/web-transport/pull/50))

## [0.6.2](https://github.com/moq-dev/web-transport/compare/web-transport-v0.6.1...web-transport-v0.6.2) - 2024-10-27

### Other

- Gotta bump deps too. ([#48](https://github.com/moq-dev/web-transport/pull/48))
- Update crate description

## [0.6.1](https://github.com/moq-dev/web-transport/compare/web-transport-v0.6.0...web-transport-v0.6.1) - 2024-10-26

### Other

- Derive PartialEq for Session. ([#45](https://github.com/moq-dev/web-transport/pull/45))

## [0.5.1](https://github.com/moq-dev/web-transport/compare/web-transport-v0.5.0...web-transport-v0.5.1) - 2024-08-15

### Other
- Some more documentation. ([#34](https://github.com/moq-dev/web-transport/pull/34))

# [M] Remove dead public API

## Goal

Every published crate and npm package exports only API a caller can use: items nobody can construct or reach, error variants nothing constructs, and redundant paths are gone, and `RecvStream::read_all` can no longer buffer an unbounded peer stream.

## Plan

The code audit ([moq-dev/web-transport#579](https://github.com/moq-dev/web-transport/pull/579)) found these with grep across the repo and the `moq` consumer; recheck each before removing it, and give each crate's changelog a migration line.

- `qmux`: make the `StreamDir`/`StreamId` re-exports `pub(crate)`; no public signature takes or returns them. Delete `ALPNS`, which duplicates `Version::ALL` mapped through `Version::alpn()`.
- `web-transport-quinn`:
  - Delete `Request::remote_address` (deprecated since [#227](https://github.com/moq-dev/web-transport/pull/227), three breaking releases ago; use `req.conn().remote_address()`).
  - Delete `Request::connect()` (doc-deprecated since [#161](https://github.com/moq-dev/web-transport/pull/161); use the `Deref` to `ConnectRequest`).
  - Make `SessionAccept` and its methods `pub(crate)` and drop the `kio` re-export that exists only for it; no caller can obtain a `SessionAccept`.
  - Delete the never-constructed `ClientError`/`ServerError::{UnexpectedEnd, WriteError, ReadError}`, `ConnectError::{UnexpectedEnd, ReadError}`, and `SettingsError::{UnexpectedEnd, ReadError, WriteError}`, with their arms in `rs/web-transport-ffi/src/error.rs`.
  - Make `NoCertificateVerification` private; nothing outside the crate can construct it.
- `web-transport-noq`: the same `Request::connect`, `SessionAccept`/`kio`, error-variant, and `NoCertificateVerification` removals, after [moq-dev/web-transport#492](https://github.com/moq-dev/web-transport/pull/492) lands or in it.
- `web-transport-proto`: delete `StreamUni::is_grease` (no caller), `Capsule::read`/`Capsule::write` (raw unframed capsules; every backend uses `Http3CapsuleReader` or frames by hand), and `CapsuleError::UnknownType` (unknown types decode to `Capsule::Unknown`). Delete `VarInt::MAX_SIZE` or use it in `read_optional`, and make `read_optional` `pub(crate)`.
- `web-transport-quiche`:
  - Delete the never-constructed `h3::SettingsError::UnexpectedEnd`, `h3::ConnectError::UnexpectedEnd`, and `ServerError::Io`.
  - Make `ez::tls` private (its items are already re-exported at `ez::` and the root).
  - Make `h3::Settings`, `h3::Connected`, and `h3::Connecting::{accept, ok, respond, reject}` `pub(crate)`; none can produce a `Connection`, and `moq` uses only `h3::Request`.
  - Make `ez::Server` and `Server` non-generic, with `new<M>` generic, since `M` lives only in `PhantomData`.
- `web-transport-iroh`: delete the never-constructed `UnexpectedEnd` variants in `settings.rs` and `connect.rs`, coordinating with [moq-dev/web-transport#493](https://github.com/moq-dev/web-transport/pull/493).
- `web-transport-ffi`: make the Rust-only `map_*_error` helpers, `WebTransportError::invalid`/`protocol`, and the `Session`/`SendStream`/`RecvStream`/`SessionRequest::new` constructors `pub(crate)`. The UniFFI surface is unchanged.
- `web-transport-trait`: `RecvStream::read_all` grows a `BytesMut` until the peer finishes, so a peer chooses how much it allocates. Give it a required maximum length that errors past the limit; only qmux's examples call it.
- `@moq/web-transport`: drop the exported `WebTransportErrorOptions`, which duplicates the DOM global of the same name and shape.

## Waiting

- Maintainer: approve these breaking removals. Recommendation: remove all of them in one breaking release per crate, since no in-repo or `moq` caller uses any. The alternative is to keep a specific item, which leaves dead surface to maintain.

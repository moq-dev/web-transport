# [S] Bound decoded CONNECT headers

## Goal

Publish web-transport-proto 0.6.3 with the decoded CONNECT bound
implemented in [moq-dev/web-transport#458](https://github.com/moq-dev/web-transport/pull/458).
Publication is pending; check that `cargo info web-transport-proto@0.6.3`
resolves after release-plz publishes it.
The source implementation bounds what a CONNECT request or response can make
its receiver hold. Each decoded field counts as name + value + 32 bytes
([RFC 9114 section 4.2.2](https://www.rfc-editor.org/rfc/rfc9114#section-4.2.2))
against a 16 KiB field section, checked as each field decodes, and every
backend advertises that limit in its SETTINGS.

The source fix replaces the previous 64 KiB HEADERS-only bound, which unique short
literal names amplified: a 64 KiB frame of 13,355 empty-valued two- and three-byte names
decoded to 4.9 MB peak and kept 2.7 MB in `ConnectRequest::headers` (about
42x) for the session, before the application sees the request. This is the
[QPACK header accordion](https://foxio.io/blog/header-accordions-qpack-amplification-attacks-against-http-3-servers)
class. Its dynamic-table variant doesn't apply, since the decoder refuses
dynamic references, and the static-table variant collapses in the field
`HashMap`. Clients parsed `ConnectResponse` with the same decoder, so a hostile
server could reach them too.

Non-goal: SETTINGS and capsule frames keep `MAX_FRAME_SIZE`.

## Plan

Source work is complete in [moq-dev/web-transport#458](https://github.com/moq-dev/web-transport/pull/458).
Publication remains: after maintainer review and merge, release-plz publishes 0.6.3.
This PR does not bump a version, merge, or publish. The downstream consumer is
moq's [main/release lockfile quest](https://github.com/moq-dev/moq/blob/main/quest/m0/web-transport-proto-0.6.3.md).

Decided 2026-10-21 (maintainer, in moq's planning):

- **Charge the budget while expanding.** Pass the remaining budget into
  string and Huffman decoding, so a single literal aborts at the limit
  instead of materializing first: Huffman codes as short as 5 bits expand a
  literal up to 1.6x.
- **One private 16 KiB limit.** It is the decoded budget and also the HEADERS
  frame cap in `read_headers_frame` and in the sync
  `ConnectRequest::decode`/`ConnectResponse::decode`. The sync decoders refuse
  the declared length as soon as the frame header parses, before the payload
  arrives, so a caller that buffers until decode completes (moq-uring) stops
  there. A real CONNECT is a few hundred bytes.
- **One error, `ConnectError::FieldSectionTooLarge`.** Both the oversized
  frame and the over-budget section return it, and the existing
  oversized-HEADERS tests expect it instead of `FrameTooLarge`. Adding it is
  non-breaking because `ConnectError` is `#[non_exhaustive]`. It does not go
  on `qpack::DecodeError`, which is exhaustive and reachable through
  `ConnectError::QpackError`, so the overrun reaches `connect.rs` through a
  crate-private path.
- **Advertise it.** `Settings::enable_webtransport` also sets
  `MAX_FIELD_SECTION_SIZE` to the limit, so every backend advertises what it
  enforces. Enforcement is what bounds memory; the setting tells compliant
  peers.
- **Regression test.** A HEADERS frame under 16 KiB carrying about 4,000
  distinct empty-valued literal names (over 140 KB with the per-field
  overhead) fails with `FieldSectionTooLarge` and decodes without the fix. So
  does one Huffman literal under 16 KiB that expands past it. A declared
  length over 16 KiB fails at the frame header. The existing round
  trips still pass, and a settings test checks the advertisement.
- **Shared input safety.** The sync frame scanner skips GREASE iteratively and
  checks u64 lengths before narrowing. CONNECT uses 16/64 KiB frame limits;
  SETTINGS keeps 64 KiB and refuses oversized declared lengths before payloads.
  QPACK prefixed integers refuse shifted-out bits and addition overflow using
  the existing `BoundsExceeded` error.
- **Outbound symmetry.** `ConnectRequest`/`ConnectResponse` `encode` and
  `write` apply the same accounting and return `FieldSectionTooLarge` before
  writing, so the public API never emits a section a same-version peer refuses.
- **Patch release.** It lands on `main` and release-plz publishes 0.6.3; the
  crate has no unreleased API change since 0.6.2. web-transport-iroh's 0.7
  and 0.8 lines pick it up through `^0.6`. moq bumps its `main` and `release`
  lockfiles in its
  [web-transport-proto 0.6.3](https://github.com/moq-dev/moq/blob/main/quest/m0/web-transport-proto-0.6.3.md)
  quest.

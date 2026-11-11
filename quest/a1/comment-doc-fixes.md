# [XS] Fix stale comments and broken doc links

## Goal

The comments and docs below say what the code does, and `cargo doc` builds the affected crates without warnings.

- Quiche's `DROP_CODE` comments in `rs/web-transport-quiche/src/{connection,recv,send}.rs` give the right ASCII, hex and decimal values: `send.rs`'s decimal is wrong (`0x73656E64` is 1936027236), and `connection.rs` says "conn" while `0x636E6E6F` spells `cnno`. Recompute every value in all three comments. The codes themselves are visible on the wire and stay unchanged.
- The rustdoc warnings in `web-transport-quinn` (`crypto.rs` links) and `rs/web-transport/src/quinn.rs` (`Into<...>`) are gone.
- The PyPI README (`py/web-transport/README.md`) links `LICENSE` and `python/web_transport/__init__.py` with absolute GitHub URLs, since relative links break on PyPI.

## Plan

Recommended: add a `cargo doc` with `-D warnings` to `just check` if it adds little time, so rustdoc warnings fail CI instead of returning.

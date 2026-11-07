# [S] Replace the Quinn crypto adapter with upstream provider-neutral rustls

## Goal

web-transport-quinn uses quinn's own provider-neutral `rustls` support and deletes the adapter from [Quinn on rind](/quest/a1/rind/quinn.md).

## Plan

Bump the quinn pin to the release that closes [quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341) and delete the ported session. Keep `web_transport_quinn::crypto`'s public types as thin re-exports or deprecate them; decide that with the maintainer at the time. Keep the interop tests and the CI guard green.

## Required

- [Quinn on rind](/quest/a1/rind/quinn.md) - the adapter this replaces
- [Quinn provider-neutral rustls release](/quest/a2/quinn-provider-release.md) - the upstream release this switches to

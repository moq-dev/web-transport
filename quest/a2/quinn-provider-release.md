# [XS] quinn releases provider-neutral rustls support

## Goal

A quinn release lets quinn-proto's `rustls` support run on any rustls provider without ring or aws-lc-rs, closing [quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341). Delete this quest once that release exists.

## Plan

This waits passively on quinn-rs: we open no upstream PR, and rind no longer drives one (it dropped its quinn adoption quest in [kixel-ai/rind#635](https://github.com/kixel-ai/rind/pull/635)).

Decision prompt: ✅ wait passively on quinn-rs (rejected: we drive the upstream quinn PR; delete this quest and [quinn-adapter-removal](/quest/a2/quinn-adapter-removal.md)).

## Waiting

- quinn-rs: ship a quinn release that closes [quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341); check the issue's state and quinn's releases.

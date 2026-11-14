# [XS] quinn releases provider-neutral rustls support

## Goal

A quinn release lets quinn-proto's `rustls` support run on any rustls provider without ring or aws-lc-rs, closing [quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341). Delete this quest once that release exists.

## Plan

This waits on an outside party. Check #2341 and quinn's releases. rind's own [quinn adoption quest](https://github.com/kixel-ai/rind/blob/main/quest/m2/adoption/quinn.md) drives the upstream PR, building on ctz's work-in-progress branch. Advance it there rather than opening a competing PR (posting to quinn-rs needs the maintainer's approval).

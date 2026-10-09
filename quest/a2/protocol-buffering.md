# [M] Remove measured redundant protocol buffering

## Goal

CONNECT/settings encoding and decoding avoid demonstrably redundant copies while preserving protocol correctness and a maintainable implementation.

## Plan

Measure allocation and copying in representative encode/write and read/decode paths before changing them. Start with the temporary QPACK buffer copied into a frame and then buffered again for async write. Prefer a small reusable encoding change over a second parser or broad lifetime-heavy public API.

Preserve size limits, partial-I/O behavior and exact frame consumption so following capsules remain unread. Compare before/after allocation/copy behavior and retain wire-format regressions. Owned decoded headers may still require allocation; do not promise universal zero-copy reads or invent a performance threshold. Coordinate with response-header work if both touch the same codec.

## Closes

- [#169](https://github.com/moq-dev/web-transport/issues/169) - the implementation completes this report

## Related

- [Response headers](/quest/a1/response-headers.md) - overlapping codec surface; no hard dependency

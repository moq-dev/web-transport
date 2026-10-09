# [M] Document package and backend selection

## Goal

A dedicated guide helps users select a package/backend and understand verified differences from wtransport without unsupported rankings.

## Plan

Compare current public capabilities: native backend choice, browser/WASM support, language bindings, stream/datagram APIs and integration surfaces. Use current primary documentation for wtransport and link to it; distinguish unsupported capabilities from unverified ones. Explain when to use the router, traits, a native backend or QMux.

Create a standalone guide linked from the root and relevant package READMEs. This was approved as a separate documentation quest; API reference and examples remain with their feature changes. Avoid a comprehensive cross-language tutorial or performance claims without measurements.

## Closes

- [#117](https://github.com/moq-dev/web-transport/issues/117) - the implementation completes this report

## Related

- [Current architecture documentation](/quest/a1/docs-current.md) - repairs existing stale descriptions independently

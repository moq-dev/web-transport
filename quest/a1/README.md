# Bindings, configuration and documentation

## Goal

Make supported packages testable, configurable and accurately documented.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [QMux send-rate filter](/quest/a1/qmux-rate-filter.md) - app-limited samples no longer lower the estimate, on main and 0.5
- [QMux send-rate filter on 0.5](/quest/a1/qmux-rate-filter-0.5.md) - backport for published moq-relay
- [Dual-stack bind](/quest/a1/dual-stack-bind.md) - native clients bind by the server's address family
- [Router crypto features](/quest/a1/router-crypto-features.md) - interim aws-lc-rs/ring features on `web-transport`
- [Rust crates on rind](/quest/a1/rind/README.md) - no C crypto outside Quiche
- [Binding runtime coverage](/quest/a1/bindings/README.md) - independent language jobs
- [Node idle configuration](/quest/a1/node-idle/README.md) - typed native input and Node options
- [WebSocket record size](/quest/a1/qmux-ws-record-size.md) - advertise only what the socket accepts
- [Response headers](/quest/a1/response-headers.md) - preserve success and rejection metadata
- [Current architecture docs](/quest/a1/docs-current.md) - repair stale workspace descriptions
- [Package selection](/quest/a1/package-selection.md) - verified comparison and selection guide

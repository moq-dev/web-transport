# Observability and measured optimization

## Goal

Add honest optional metrics and measured optimizations while tracking external protocol and backend prerequisites.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Native metric integration](/quest/a2/native-stream-stats.md) - actual native delivered-versus-queued progress
- [Native metric availability](/quest/a2/native-stats-upstream.md) - advance external accessor prerequisites
- [QMux draft readiness](/quest/a2/qmux-draft-readiness.md) - establish strict-validation compatibility boundary
- [Quiche send-rate audit](/quest/a2/quiche-delivery-rate.md) - app-limited samples under legacy controllers
- [Quinn provider-neutral rustls release](/quest/a2/quinn-provider-release.md) - waiting on quinn-rs/quinn#2341
- [Quinn adapter removal](/quest/a2/quinn-adapter-removal.md) - switch to upstream and delete the port

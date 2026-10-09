# [M] Test Quiche driver lifecycle deterministically

## Goal

An in-memory Quiche connection pair can deterministically exercise driver transitions, including a peer STOP_SENDING before accepting a bidirectional stream.

## Plan

Start with the `live` guards in `rs/web-transport-quiche/src/ez/driver.rs::accept_bi`. Pump client/server send and receive buffers without socket scheduling; reuse repository certificate fixtures or generate scoped test files for quiche configuration. Keep the seam internal to tests.

Deliver the harness with a regression asserting the already-closed send half is absent from the driver send map and priority ranking. Reverting the guards must fail the test. Do not fold speculative priority-ranking fixes into this quest: reproduce and scope them independently if encountered. Keep real-socket integration coverage alongside the deterministic harness.

## Closes

- [#392](https://github.com/moq-dev/web-transport/issues/392) - the implementation completes this report

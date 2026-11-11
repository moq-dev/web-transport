# Safety and regression foundations

## Goal

Prioritize memory safety, lifecycle correctness and the regression infrastructure that makes fixes durable.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Quinn crypto test flake](/quest/a0/quinn-crypto-flake.md) - the handshake/key-update test stops failing the required build
- [Dev shell quest](/quest/a0/quest-pin.md) - the quest merge driver auto-resolves link lists again
- [Quiche lifecycle](/quest/a0/quiche/README.md) - deterministic harness and retirement handling
- [QMux receive credit](/quest/a0/qmux-recv-credit.md) - stopped or dropped streams return their connection flow-control credit
- [TypeScript QMux receive credit](/quest/a0/qmux-recv-credit-js.md) - cancelled streams return their connection flow-control credit

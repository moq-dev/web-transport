# Safety and regression foundations

## Goal

Prioritize memory safety, lifecycle correctness and the regression infrastructure that makes fixes durable.

## Plan

The Required list is ordered by priority. Independent children may proceed concurrently; only their explicit Required links are blockers.

## Required

- [Quiche lifecycle](/quest/a0/quiche/README.md) - deterministic harness and retirement handling
- [Node session and reader lifetime](/quest/a0/node-session-lifetime.md) - `closed` rejects on an abnormal end, and an abandoned reader releases its stream
- [QMux receive credit](/quest/a0/qmux-recv-credit.md) - stopped or dropped streams return their connection flow-control credit

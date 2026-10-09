# Quiche lifecycle regression coverage

## Goal

Driver lifecycle behavior is deterministic to test, and benign retirement races have defined behavior.

## Plan

The children own the harness and notification fix. When both land, verify their tests run through the normal crate test command and package testing guidance describes the internal harness; keep the final epic cleanup limited to that integration check.

## Required

- [Driver lifecycle harness](/quest/a0/quiche/driver-harness.md) - pin pre-accept closure behavior
- [Stale notifications](/quest/a0/quiche/stale-notifications.md) - handle retirement races without warning noise

# Quiche lifecycle regression coverage

## Goal

Driver lifecycle behavior is deterministic to test, and benign retirement races have defined behavior.

## Plan

The internal driver harness is in place; the remaining child owns the notification fix. When it lands, verify the lifecycle tests run through the normal crate test command and package testing guidance describes the internal harness; keep the final epic cleanup limited to that integration check.

## Required

- [Stale notifications](/quest/a0/quiche/stale-notifications.md) - handle retirement races without warning noise

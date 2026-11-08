# [XS] Quiche lifecycle regression coverage

## Goal

Driver lifecycle behavior is deterministic to test, and benign retirement races have defined behavior.

## Plan

Verify the lifecycle tests run through the normal crate test command and package testing guidance describes the internal harness; keep the final cleanup limited to that integration check.

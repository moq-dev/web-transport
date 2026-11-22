# [XS] Release @moq/qmux 0.3.6 with the send-cancel fix

## Goal

`@moq/qmux` 0.3.6 is on npm with the send-cancel fix: aborting or stopping a
stream whose write waits for flow-control credit no longer hangs. 0.3.5 lacks
the fix.

## Plan

Decided 2026-11-22 (maintainer, /quest-import of #561): ✅ a separate release
quest (rejected: bump in the fix PR, wait for the next routine release). As
with 0.3.5, a version-bump PR on main publishes through `release-js.yml` when
it merges.

Audit what landed in `js/qmux` since 0.3.5 (`dc9324b`) before bumping: type
declaration changes, behavior changes to state in the release notes, unit
tests, and QMux interop against main's Rust. Record the audit in the PR body.

## Required

- [Send cancel](/quest/a0/qmux-js-send-cancel.md) - the fix this release ships

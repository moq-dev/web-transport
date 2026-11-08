# [XS] Backport the QMux send-rate filter to 0.5

## Goal

The `qmux-v0.5.x` line has the same app-limited send-rate filter as main, so the next 0.5.x patch fixes the moq-relay releases that depend on `qmux ^0.5.3`.

## Plan

Cherry-pick or port the main-branch change onto `qmux-v0.5.x` in a PR against that branch, as [#412](https://github.com/moq-dev/web-transport/pull/412)/[#413](https://github.com/moq-dev/web-transport/pull/413) do, with the same tests. Don't publish the release yourself.

## Required

- [QMux send-rate filter](/quest/a1/qmux-rate-filter.md) - the main-branch change this backports

## Closes

- [#460](https://github.com/moq-dev/web-transport/issues/460) - the reporter runs the 0.5 line through moq-relay

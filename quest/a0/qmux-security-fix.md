# [XS] Land the existing QMux ping amplification fix

## Goal

The existing fix for issue #420 is merged with its blocked-writer regression coverage; avoid competing implementation work.

## Plan

PR [#422](https://github.com/moq-dev/web-transport/pull/422) already coalesces pending ping responses. Check its current state, review and CI, and advance that PR rather than reimplementing its fix. If it has merged, verify that it resolves #420 and remove this tracking quest. Keep this external landing condition ready so it resurfaces during triage. It is not a claim to modify someone else's branch.

The planning PR does not close #420. PR #422 owns that closure. Coordinate any changes with the receive-credit/close work in PRs #412 and #413. Publishing is a separate operation and is not implied by completing this landing tracker.

## Related

- [Issue #420](https://github.com/moq-dev/web-transport/issues/420) - existing PR owns the fix and issue closure

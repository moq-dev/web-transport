# [S] Establish the next QMux validation boundary

## Goal

A published and interoperable QMux version transition provides a concrete compatibility boundary for strict receive offset and terminal-size validation.

## Plan

Draft-02 is the current published draft at planning time. Both Rust and TypeScript send offsets, but deliberately tolerate receive offsets and defer strict terminal-size checks. Check the [IETF draft history](https://datatracker.ietf.org/doc/draft-ietf-quic-qmux/history/) and current upstream text when triaging; do not turn draft-02 tolerance into an accidental breaking change.

This ready external-prerequisite quest owns establishing the next published wire semantics and the migration/negotiation boundary. Once concrete, use quest-plan to scope version support and strict validation together across Rust and TypeScript, retaining legacy interoperability tests. Remove this tracker when replaced by the implementation quests. Publication alone is not evidence that existing peers can accept stricter behavior. The eventual implementation, not this planning tracker, resolves #294.

## Related

- [Issue #294](https://github.com/moq-dev/web-transport/issues/294) - receive validation remains intentionally deferred

# [S] Treat retired-stream notifications as harmless

## Goal

A notification racing with send-stream retirement neither warns as a failure nor changes live-stream scheduling or connection progress.

## Plan

The closure check in `ez/send.rs::notify` and removal from `Driver::send` use different ownership boundaries. The approved policy treats the resulting stale notification as a harmless no-op, avoiding a new retired-ID history or broad nested-lock discipline.

Use the deterministic driver harness to force the retirement/enqueue interleaving and verify progress and bounded notification state. Preserve diagnostics for distinct actionable cases; the separate `closed stream was writable` report is not explained by this race. Do not claim to fix it without reproducing it.

## Closes

- [#382](https://github.com/moq-dev/web-transport/issues/382) - the implementation completes this report

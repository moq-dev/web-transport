# [M] Audit the quest tree

## Goal

Run the `tree` audit: `quest audit tree` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: the previous run read every quest on 2026-11-22.
- Check first the outside waits, each of which states how to check it: [quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341) and quinn's releases, the [QMux draft history](https://datatracker.ietf.org/doc/draft-ietf-quic-qmux/history/), and whether a stable rind-rustls has `PacketKey::encrypt_in_place_for_path`/`decrypt_in_place_for_path`, which the rind epic's drafts wait on.
- QMux here gets bug fixes only and its retirement is planned in moq-dev/moq's quest tree (`quest/m3/release/qmux-rs-retire.md`, `qmux-js-retire.md`); flag any quest here that plans new QMux features.
- The tree is small enough for one reader; split it across sub-agents only once it is not.

## Audit

- tree

## Scheduled

- 2026-11-29 - one week after the previous run

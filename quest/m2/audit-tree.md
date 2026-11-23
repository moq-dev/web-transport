# [M] Audit the quest tree

## Goal

Run the `tree` audit: `quest audit tree` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: the previous run read every quest on 2026-11-22.
- Check first the outside waits, each of which states how to check it: [quinn-rs/quinn#2341](https://github.com/quinn-rs/quinn/issues/2341) and quinn's releases, and whether a stable rind-rustls has `PacketKey::encrypt_in_place_for_path`/`decrypt_in_place_for_path`, which the rind epic's drafts wait on.
- The legacy `a0`/`a1` acts are being retired: once [#576](https://github.com/moq-dev/web-transport/pull/576), [#492](https://github.com/moq-dev/web-transport/pull/492), and [#493](https://github.com/moq-dev/web-transport/pull/493) merge or release their claims, `quest mv` what remains of `quest/a0` and `quest/a1` into m0 and `git rm` the emptied act READMEs. `quest mv` refuses claimed quests, since their branches would no longer name them.
- QMux here gets bug fixes only and its retirement is planned in moq-dev/moq's quest tree (`quest/m3/release/qmux-rs-retire.md`, `qmux-js-retire.md`); flag any quest here that plans new QMux features.
- The tree is small enough for one reader; split it across sub-agents only once it is not.

## Audit

- tree

## Scheduled

- 2026-11-29 - one week after the previous run

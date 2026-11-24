# [L] Audit build and CI cost

## Goal

Run the `cost` audit: `quest audit cost` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

The repository is public, so GitHub-hosted minutes are free; cost here means wall time, macOS concurrency, and the 10 GB Actions cache cap.
The ruleset requires only the `CI / build` check, but agents landing with `quest merge` wait for every check, so the slowest check on a PR is its real gate.

Check first:

- Whether `CI / build` still restores `main`'s Rust cache (log line `Restored from cache key ... full match`) and its end-to-end time against the baseline below.
- Actions cache usage (`gh api repos/moq-dev/web-transport/actions/cache/usage`): the gate's `main` entry is about 2.2 GB, and per-PR caches from the macOS and release jobs keep usage above the cap.
- macOS queue time (job `created_at` to `started_at`) and whether release smoke workflows outlast the gate.
- The dev shell closure (`nix path-info -S -r` on `.#devShells.x86_64-linux.default`).
- The `@moq/web-transport` tarball's unpacked size.

Baseline, from `gh api` job and step timings of 400 completed pull-request runs between 2026-11-21 17:34 and 2026-11-23 04:52 UTC on main `6308c8b`:

| Check | Runner | p50 run (min) | p50 / p90 end to end (min) |
| --- | --- | --- | --- |
| CI / build (gate, no cache) | ubuntu | 24.3 | 24.5 / 29.8 |
| CI / build, cache restored ([#580](https://github.com/moq-dev/web-transport/pull/580)) | ubuntu | 5.1 | one sample |
| CI / Kotlin/JVM artifact | ubuntu | 3.6 | 3.6 / 5.5 |
| Python runtime | ubuntu | 6.0 | 6.0 / 6.4 |
| Node runtime | ubuntu | 2.6 | 2.7 / 2.9 |
| Swift runtime | macos | 2.5 | 3.5 / 10.6 |
| WASM browser | ubuntu | 3.4 | 3.5 / 3.7 |
| Release Kotlin (PR smoke) | 5 ubuntu, macos, windows | 8.9 longest | 14.2 / 26.8 |
| Release Py (PR smoke) | 2 ubuntu, 2 macos, windows | 8.4 longest | 12.0 / 22.8 |
| Release Swift (PR smoke) | 5 macos, ubuntu | 8.6 longest | 15.7 / 23.6 |

- Runner minutes in that window: 2881 ubuntu, 960 macOS (765 of them release smoke jobs), 295 windows.
- macOS queue: p50 1.4, p90 11.4, max 56 min; ubuntu and windows under 0.5 min at p90.
- The cold gate's cost was compilation: `cargo check` 3 min, feature powerset 7 min (77 runs, 36 of them `web-transport-iroh`), test build 4 min, doctests 2 min, dev shell 75 s. Tests themselves run in about 10 s (191 unit tests in 3.2 s; the rest under 2 s per binary).
- Cold workspace test build on `desktop` (24 cores, load 22, full debuginfo): 108 s, 13 GB target; repeated edit to `web-transport-proto`: 98 s. Slowest units: qmux 92, web-transport-quiche 86, boring-sys 54, web-transport-quinn 40 unit-seconds.
- Dev shell closure: 4.93 GB / 282 paths before #580, 3.70 GB / 138 after.
- `@moq/web-transport@0.1.6` unpacks to 38 MB (all five `.node` binaries); each platform package is 5.7 to 9.0 MB.

Kept after measuring: `line-tables-only` debuginfo in the gate cut the saved cache only 13% (2.54 to 2.20 GB, since uncached workspace crates carry most debuginfo) but the warm gate from 5.8 to 5.1 min (restore 72 to 57 s, test build 96 to 79 s), one sample each.

Not tried: splitting the gate into parallel jobs. Each would restore its own cache and dev shell (about 2 min), against a warm gate of about 5 min.

## Audit

- cost

## Scheduled

- 2026-11-30 - one week after the previous run

## Required

- [m2](/quest/m2/README.md) - the audit runs once per milestone

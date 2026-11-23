# [S] Run the cross-target release builds off the pull-request path

## Goal

A pull request that changes shared Rust code waits on checks no slower than the gate, while every cross-target release build still runs before a release tag can fail on it.

## Plan

`release-kt.yml`, `release-py.yml`, and `release-swift.yml` build every release target (Android, iOS, macOS, Windows, Linux) on any pull request touching `rs/web-transport-ffi`, `rs/web-transport-quinn`, `rs/web-transport-proto`, `rs/web-transport-trait`, or the root `Cargo.toml`.
The 2026-11-23 cost audit measured them, in 400 pull-request runs over 35 hours, at 765 of 960 macOS runner minutes, with a macOS queue of p90 11 and max 56 minutes.
End to end they took p50 12 to 16 and p90 23 to 27 minutes, while the gate with its cache restored takes about 5.
Agents landing with `quest merge` wait for every check, so on those pull requests the release builds, not the gate, decide merge time.
In that window they failed only on `kt/` changes, which a packaging-path trigger keeps catching before merge.
Every pull request still exercises the FFI through the Swift host (macOS), Kotlin/JVM (Linux), and Python wheel (Linux) checks.

Keep the pull-request trigger for each workflow's own packaging paths (`kt/`, `swift/`, `py/web-transport/`, `rs/web-transport-ffi/build.sh`, `.github/scripts/release.sh`, and the workflow file), and run the full matrix on pushes to `main` that touch the crate paths.
A failure on `main` then shows on the merged commit, one merge later than today.
Coordinate with the `ffi-release-pipeline` quest from [moq-dev/web-transport#579](https://github.com/moq-dev/web-transport/pull/579), which reshapes the same workflows.

## Waiting

- Maintainer: choose where cross-target release builds run for crate changes, since it moves a pre-merge check after merge. Options: (a) pushes to `main`, keeping pull-request runs for packaging paths (recommended: catches a break one merge later and frees macOS runners); (b) a nightly schedule, cheaper but a day late and reported only by email; (c) keep them on pull requests, so crate changes merge in about 15 minutes instead of 5.

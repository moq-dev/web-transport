# [S] Ship each npm native binary once

## Goal

Installing `@moq/web-transport` downloads only the native binary for the installing platform.
Every supported platform keeps working through its `@moq/web-transport-<platform>` package.

## Plan

The 2026-11-23 cost audit found `@moq/web-transport@0.1.6` unpacks to 38 MB: it carries all five `.node` binaries (5.7 to 9.0 MB each) at its root, while each platform package carries its own copy again.
An install therefore fetches about 47 MB where about 9 MB would do.

`napi artifacts -d artifacts` in `js/web-transport/scripts/release.ts` writes each binary to both the package root and `npm/<platform>/`, and the `"*.node"` entry in `js/web-transport/package.json` `files` ships the root copies.
Reproduce it by running `napi artifacts` over dummy binaries and `npm pack --dry-run`.

Drop the root copies from the published main package, keeping the `napi.cjs` loader's fallback to the platform packages.
Local builds (`bun run --cwd js/web-transport build`, `just test`) keep writing a root `.node`, which `napi.cjs` loads first; only what `npm pack` includes changes.
`js/web-transport/scripts/test-runtime.ts` asserts the packed main package contains a `.node`, so rework it to install the host's platform package beside the main one, which also exercises the loader path users take, and to fail if the main package holds any `.node`.
Verify with `npm pack --dry-run` on the release layout and `bun run --cwd js/web-transport test:runtime`.

A user installing with `--omit=optional` would lose the binary; napi-rs's own template ships this way, so document it rather than keep the duplicates.

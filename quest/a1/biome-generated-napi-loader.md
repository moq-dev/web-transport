# [XS] Keep Biome off the generated Node loader

## Goal

`bun run check` lints only hand-written JavaScript and TypeScript, so the findings in the napi loader that `build:native` regenerates no longer appear.

## Plan

`bun run check` at the repository root runs `biome check`, which reports `lint/style/useTemplate`, `lint/complexity/useOptionalChain`, and `lint/correctness/noUnusedVariables` in `js/web-transport/napi.cjs`. The `build:native` script writes that file with `napi build` and then runs `biome check --write` on it, so a hand fix is lost on the next build. The same `useTemplate` rule also flags hand-written `js/web-transport/tests/fixtures/server-close.ts` at lines 25, 35, and 65. Fix those three lines in the same change, since they are not generated.

The recommended exclusion is to list `js/web-transport/napi.cjs` and `js/web-transport/napi.d.cts` in the ignore list of `biome.jsonc`. Each `napi build` rewrites both files, so the findings return unless they are excluded.

## Waiting

- Maintainer: Choose how to clear the findings in the generated loader. Options: (a) exclude `napi.cjs` and `napi.d.cts` in `biome.jsonc`, which needs no change to the generator (recommended); (b) post-process the generated output in `build:native` so Biome accepts it, which is more script to maintain because the template comes from `@napi-rs/cli`; (c) leave the warnings, since they exit 0 and do not fail `just check`.

## Related

- [Stop the Node test build from rewriting its committed loader](/quest/a0/napi-loader-drift.md) - decides whether the loader stays committed, which determines whether lint should see it at all

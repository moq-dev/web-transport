# [XS] Decide the browser demo's fate

## Goal

`js/web-demo` either works or is gone, along with every reference to it.

## Plan

Its `client.js` imports `bundle-text:../dev/localhost.hex`, which resolves to `js/dev/localhost.hex`, a file that has never existed: [moq-dev/web-transport#202](https://github.com/moq-dev/web-transport/pull/202) moved the demo under `js/` without updating the path. No recipe or CI job builds it, its `@ts-expect-error` covers no type check, and the WASM harness (`just harness`) already gives a manual browser check against the same certificate.

Deleting it means removing `js/web-demo`, its root `package.json` workspace entry, its line in the root `README.md`, and the `cd ../web-demo` lines in `rs/web-transport-{quinn,noq,quiche}/examples/README.md`, which point at a path that does not exist either. Keeping it means fixing the import to `../../dev/localhost.hex`, pointing those READMEs at `js/web-demo`, and running it once against an example server.

The maintainer chose to delete it (2026-11-23): the harness covers manual browser checks, and nothing has noticed it broken since [#202](https://github.com/moq-dev/web-transport/pull/202).

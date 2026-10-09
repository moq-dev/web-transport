# TypeScript guidance

Extends the [root guidance](../AGENTS.md).

- Use ESM and match the surrounding import style. Export only the supported public
  API from entrypoints and document it with TSDoc.
- Declare every imported package in the consuming package's manifest; workspace
  hoisting can hide missing dependencies from local checks.
- Keep stream, timer, and listener cleanup explicit and close paths idempotent.
  Avoid repeatedly racing a long-lived promise: losing handlers remain attached
  until it settles. Use cancellable or scoped waits where needed.
- Use Bun scripts from each package's `package.json` and the root Biome config.
  Tests use `*.test.ts` with `bun test`. For declaration or packaging changes,
  run the affected package's build as well as its type check.
- Exercise browser transport changes in a real browser; type checks cannot verify
  WebTransport or Web Streams runtime behavior. The root `just harness` recipe
  covers the WASM adapter.
- Keep QMux wire changes aligned with `rs/qmux` and run the existing QMux interop
  tests. Coordinate Node wrapper changes with `rs/web-transport-node`.

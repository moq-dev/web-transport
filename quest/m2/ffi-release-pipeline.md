# [M] Trim the FFI release build

## Goal

`rs/web-transport-ffi/build.sh` and the Kotlin and Swift release workflows build each FFI artifact once, with no paths or flags that nothing consumes.

## Plan

The code audit ([moq-dev/web-transport#579](https://github.com/moq-dev/web-transport/pull/579)) found these by reading the scripts and workflows; confirm each through a release workflow's PR smoke run.

- `build.sh`: delete `--archive` and its 7z/zip branch (never passed), Python binding generation (maturin generates its own), binding generation in full mode on host targets (the build jobs upload only `lib/`), the Windows `.dll.lib`/`.lib` copies (`kt/scripts/package.sh` reads neither), and the single-arch `*-apple-darwin` branch (no caller). `--version` only names the staging directory, which the workflows then reconstruct; stage to `$OUTPUT_DIR/$TARGET` and drop it, with the five copies of the `grep '^version'` parse.
- `release-kt.yml` and `release-swift.yml` duplicate their parse-version job, bindings job, and "Flatten lib layout" step, and both build `universal-apple-darwin`. Share one build and bindings stage between them. Delete `release-swift.yml`'s placeholder `publish` job and the `Cargo.lock` path filters (no lockfile is committed), which `browser.yml` and `release-py.yml` carry too.
- `kt/scripts/package.sh`: delete the `aarch64-`/`x86_64-apple-darwin` entries nothing produces and the unused `GRADLE_CMD` override. It and `release-kt.yml` run the runner's `gradle` rather than `./gradlew`, so the release can build with a different Gradle than `check.sh`; use the wrapper. Do this after [moq-dev/web-transport#577](https://github.com/moq-dev/web-transport/pull/577) lands.

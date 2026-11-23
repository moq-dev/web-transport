# [S] Publish the Python and Kotlin bindings

## Goal

Each `web-transport-ffi-v*` tag publishes `web-transport-rs` to PyPI and `dev.moq:web-transport` to Maven Central, as the root, Python, Kotlin and FFI READMEs already promise, and every binding release workflow attaches its assets to an existing GitHub Release without failing.

## Plan

- PyPI has no files for `web-transport-rs` and Maven Central has no `dev.moq` artifacts: `release-py.yml` and `release-kt.yml` skip publishing unless the repository variables `PUBLISH_PYTHON` and `PUBLISH_KOTLIN` are `true`, and neither is set. The `pypi` environment and the Maven Central and signing secrets exist.
- `.github/scripts/release.sh` passes `--notes-start-tag` to `gh release edit`, which has no such flag, so a workflow that finds the release already created fails; `release-swift.yml` failed this way on `web-transport-ffi-v0.3.0`. Only `gh release create` takes that flag.
- After the first publish, check the `pip install web-transport-rs` and Gradle snippets against the published artifacts, and replace the Kotlin README's stale `0.1.0` coordinate with one that cannot drift.

## Waiting

- Maintainer: enable publishing, or keep the bindings unpublished. Enabling means configuring `web-transport-rs`'s PyPI trusted publisher for `release-py.yml` and the `pypi` environment, then setting `PUBLISH_PYTHON` and `PUBLISH_KOTLIN` to `true`; an agent cannot create registry credentials. Keeping them unpublished means rewriting the install sections to build from source. Recommendation: enable publishing, since the docs, badges and workflows already assume it. Check with `gh variable list` and the registry APIs.

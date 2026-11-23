# [M] Audit the docs

## Goal

Run the `docs` audit: `quest audit docs` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

- Baseline: the previous run read the tree at 6308c8bfefa2952eb1309ad53f95d9914dd05fd9; start from `git diff 6308c8b..origin/main -- '*.md' '*.rs' '*.ts' '*.py' '*.kt' '*.swift'`.
- Scope excludes `quest/` (the tree audit owns it) and the generated `CHANGELOG.md` files.
- `just check` now runs lychee offline over local Markdown links and anchors, including absolute links into this repository's `main`, and the Rust READMEs with code blocks compile as doctests (`ReadmeDoctests` in each `lib.rs`), so spend the run on what those cannot see: external URLs, shell commands in prose, and claims about behavior.
- Check first the binding READMEs (`py/`, `kt/`, `swift/`, `rs/web-transport-ffi`) and the Node package against what is actually published: PyPI `web-transport-rs`, Maven Central `dev.moq:web-transport`, the Swift release archive, and npm `@moq/*`. The previous run found the PyPI and Maven packages never published and the Swift archive unbuildable.
- Check external URLs with a GET and a browser user agent; crates.io and npm pages answer 404 or 403 to scripts, so check packages on their registry APIs. GitHub rate-limits bursts, so space those requests.
- Release notes keep landing in READMEs ("now returns", "has been removed", "breaking change"); grep for them, and for sections copied between READMEs instead of linked.

## Audit

- docs

## Required

- [m2](/quest/m2/README.md) - the audit runs after it

## Scheduled

- 2026-11-29 - one week after the previous run

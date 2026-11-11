# [XS] Run the Python browser test on Ubuntu 26.04

## Goal

The `wheel` PR job (`.github/workflows/test-py.yml`, `ubuntu-latest`) passes again. `ubuntu-latest` is now Ubuntu 26.04, and the locked Playwright 1.58.0 refuses to download Chromium there ("Playwright does not support chromium on ubuntu26.04-x64"), so the job fails on every PR.

## Plan

Decision: bump Playwright in `py/web-transport/uv.lock` (the spec is `>=1.40`) to a release whose browser downloads support Ubuntu 26.04, checking its release notes. Only if no release supports it yet, pin this job to `ubuntu-24.04` with a comment pointing at the Playwright support to wait for, and leave this quest rewritten as that wait. Do not use Nix-provided browsers. Verify locally with `./py/web-transport/test-wheel.sh --with-browser-deps` inside the dev shell, and in the PR's own `wheel` run.

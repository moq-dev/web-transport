#!/usr/bin/env bash
# Exercise the distributable package without installing the source checkout.
set -euo pipefail

browser_args=()
if [[ ${1:-} == --with-browser-deps ]]; then
    browser_args+=(--with-deps)
    shift
fi
if [[ $# != 0 ]]; then
    echo "Usage: $0 [--with-browser-deps]" >&2
    exit 2
fi

package=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
export UV_PROJECT_ENVIRONMENT="$scratch/venv"

cd "$package"
uv sync --locked --no-install-project --python 3.12
python="$UV_PROJECT_ENVIRONMENT/bin/python"
"$UV_PROJECT_ENVIRONMENT/bin/maturin" build --out "$scratch/wheels"
uv pip install --python "$python" --no-deps "$scratch"/wheels/*.whl
"$python" -I -m playwright install "${browser_args[@]}" chromium

# Isolated mode ignores PYTHONPATH and the working directory. Importlib mode
# also prevents pytest from inserting the checkout into sys.path.
cd "$scratch"
timeout --signal=TERM --kill-after=10s 15m \
    "$python" -I -m pytest --import-mode=importlib --installed-wheel \
    -ra --tb=short -o faulthandler_timeout=60 "$package/tests"

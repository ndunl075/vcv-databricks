#!/bin/bash
# Differential test: ssf (with hand-off to ss) against expected tables. See check.py.
# Builds both binaries natively (cargo -j 3) unless SKIP_BUILD=1, then runs check.py.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
export PATH="$HOME/.cargo/bin:$PATH"
PY=${PY:-$HOME/Developer/vcv-data/venv/bin/python}
[ -x "$PY" ] || PY=python3
if [ "${SKIP_BUILD:-0}" != 1 ]; then
  cargo build --release -j 3 --manifest-path "$HERE/Cargo.toml" 2>&1 | tail -1
  CARGO_TARGET_DIR="$HERE/target/ss" cargo build --release -j 3 --manifest-path "$REPO/Cargo.toml" 2>&1 | tail -1
fi
exec "$PY" "$HERE/check.py" "$@"

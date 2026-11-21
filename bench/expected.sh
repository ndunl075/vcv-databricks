#!/bin/bash
# bench/expected.sh <dataset> [py|fast]  -> $DATA_ROOT/expected/<dataset>/{user_daily,campaign_daily}/part.parquet
# Skips datasets whose expected tables exist (marker .done). Logs wall time to expected/<dataset>/.secs.
set -uo pipefail
source "$(dirname "$0")/lib.sh"
name=$1; engine=${2:-fast}
out=$DATA_ROOT/expected/$name
[ -f "$out/.done" ] && { echo "[expected] $name exists"; exit 0; }
rm -rf "$out"; mkdir -p "$out"
s=$(date +%s)
mem=18g
docker run --rm --memory $mem -v "$REPO_DIR":/repo:ro -v "$DATA_ROOT":/dataroot -w /repo/bench "$TOOLS_IMAGE" \
  python oracle/oracle.py --engine "$engine" --work /dataroot/_work/$name /dataroot/$name /dataroot/expected/$name \
  || { echo "[expected] $name FAILED"; exit 1; }
echo $(( $(date +%s) - s )) > "$out/.secs"
docker run --rm -v "$REPO_DIR":/repo:ro -v "$DATA_ROOT":/dataroot -w /repo/bench "$TOOLS_IMAGE" \
  python compare.py --json /dataroot/expected/$name /dataroot/expected/$name 2>/dev/null | tail -1 \
  | python3 -c "import json,sys; print(json.loads(sys.stdin.read())['digest'])" > "$out/.digest"
touch "$out/.done"
echo "[expected] $name done in $(cat "$out/.secs") s"

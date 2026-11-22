#!/usr/bin/env bash
# Fill the dataset cache: stock (generator, seed S, N events), remap (stock with reshuffled IDs, via branch bench's mutator),
# dev (1M, seed 1). Skips what is already there (a dataset counts only with a .done file).
# usage: prep_data.sh EVENTS SEED   env: VCV_CI_WORK (default ~/vcv-ci), BENCH_SRC (extracted branch bench)
set -euo pipefail
EVENTS=$1; SEED=$2
W=${VCV_CI_WORK:-$HOME/vcv-ci}; D=$W/data; mkdir -p "$D"
HERE=$(cd "$(dirname "$0")" && pwd); REPO=$(cd "$HERE/.." && pwd)
STOCK=$D/stock; REMAP=$D/remap; DEV=$D/dev
CLICKS=$((EVENTS / 4))
need=0; for d in "$STOCK" "$REMAP" "$DEV"; do [ -f "$d/.done" ] || need=1; done
if [ $need = 0 ]; then echo "[data] all datasets cached"; exit 0; fi
GEN=$W/bin/generate
if [ ! -x "$GEN" ]; then
  echo "[data] building generator"
  ( cd "$REPO/kit/generator/generator" && cargo build --release --locked -q ) && mkdir -p "$W/bin" && cp "$REPO/kit/generator/generator/target/release/generate" "$GEN"
fi
nproc; date
if [ ! -f "$DEV/.done" ]; then
  rm -rf "$DEV"; t=$(date +%s); "$GEN" --seed 1 --events 1000000 --clicks 250000 --output "$DEV" >/dev/null; touch "$DEV/.done"; echo "[data] dev $(( $(date +%s) - t )) s"
fi
if [ ! -f "$STOCK/.done" ]; then
  rm -rf "$STOCK"; t=$(date +%s); "$GEN" --seed "$SEED" --events "$EVENTS" --clicks "$CLICKS" --output "$STOCK" >/dev/null; touch "$STOCK/.done"; echo "[data] stock $(( $(date +%s) - t )) s"
fi
if [ ! -f "$REMAP/.done" ]; then
  t=$(date +%s)
  python3 -I "$HERE/mutate_remap.py" "$BENCH_SRC" "$STOCK" "$REMAP" "$EVENTS" "$SEED" "$(nproc)"
  echo "[data] remap $(( $(date +%s) - t )) s"
fi
du -sh "$STOCK" "$REMAP" "$DEV"

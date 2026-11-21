#!/bin/bash
# Make the OOD datasets and their expected tables.   bench/ood/make.sh [--no-expected] [size ...]   (default: 1m 20m 60m)
#   1m : every variant in ood/mutate.py, from `dev`            -> $DATA_ROOT/ood-<variant>-1m
#   20m: remap remap-mild breakers short-lines, from `p20a`    -> $DATA_ROOT/ood-<variant>-20m
#   60m: the same four, from `f60`                             -> $DATA_ROOT/ood-<variant>-60m
# Resumable: a dataset with .done and an expected/<name>/.done is skipped. Mutating runs on the host's python3
# (stdlib only); the oracle runs in the tools image.
set -uo pipefail
source "$(dirname "$0")/../lib.sh"
EXPECTED=1; SIZES=()
for a in "$@"; do [ "$a" = "--no-expected" ] && EXPECTED=0 || SIZES+=("$a"); done
[ ${#SIZES[@]} -eq 0 ] && SIZES=(1m 20m 60m)
ALL_1M=$(python3 "$BENCH_DIR/ood/mutate.py" --list 2>/dev/null)
BIG="remap remap-mild breakers short-lines"
base_of() { case $1 in 1m) echo dev;; 20m) echo p20a;; 60m) echo f60;; esac; }
for size in "${SIZES[@]}"; do
  [ "$size" = 1m ] && VARS=$ALL_1M || VARS=$BIG
  for v in $VARS; do
    name=ood-$v-$size
    if [ ! -f "$DATA_ROOT/$name/.done" ]; then
      echo "[ood] making $name"
      python3 "$BENCH_DIR/ood/mutate.py" --base "$(base_of $size)" --variant "$v" --out "$DATA_ROOT/$name" --workers 8 \
        || { echo "[ood] $name FAILED"; continue; }
    fi
    if [ $EXPECTED = 1 ]; then "$BENCH_DIR/expected.sh" "$name" fast || true; fi
  done
done

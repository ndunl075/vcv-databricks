#!/bin/bash
# bench/prep_loop.sh  Make missing OOD datasets and oracle tables, one at a time, in order of value.
# Everything runs under the shared lock at 4 CPUs (expected.sh, make.sh), niced.
source "$(dirname "$0")/lib.sh"
mk() {  # mk NAME  -> mutate if the dataset does not exist, then oracle tables
  local name=$1 v size base
  if [[ $name == ood-* ]] && [ ! -f "$DATA_ROOT/$name/.done" ]; then
    size=${name##*-}; v=${name#ood-}; v=${v%-*}
    case $size in 1m) base=dev;; 20m) base=p20a;; 60m) base=f60;; esac
    echo "[prep_loop] mutating $name"
    "$BENCH_DIR/lockrun.sh" "$LOCK" nice -n 10 python3 "$BENCH_DIR/ood/mutate.py" --base $base --variant "$v" --out "$DATA_ROOT/$name" --workers 4 || echo "[prep_loop] $name FAILED"
  fi
  [ -f "$DATA_ROOT/$name/.done" ] || [ "$name" = dev ] || [[ $name != ood-* ]] || return 0
  [ -d "$DATA_ROOT/$name" ] && "$BENCH_DIR/expected.sh" "$name" fast
}
ORDER="dev p20a ood-remap-20m ood-remap-mild-20m ood-remap-1m ood-remap-mild-1m ood-breakers-20m ood-short-lines-20m
ood-breakers-1m ood-short-lines-1m ood-interleave-1m p20b"
for n in $ORDER; do mk "$n"; done
for v in $(python3 "$BENCH_DIR/ood/mutate.py" --list); do mk "ood-$v-1m"; done
for n in f60 ood-remap-60m ood-remap-mild-60m ood-breakers-60m ood-short-lines-60m; do mk "$n"; done
echo "[prep_loop] finished"

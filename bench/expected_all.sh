#!/bin/bash
# Make expected tables for every dataset that exists, big OOD ones first, then 1M ones; loops until nothing is left
# (datasets still being mutated are picked up on the next pass). bench/expected_all.sh
source "$(dirname "$0")/lib.sh"
order() {
  for n in dev p20a ood-remap-20m ood-remap-mild-20m; do echo $n; done
  for n in ood-remap-1m ood-remap-mild-1m ood-breakers-1m ood-short-lines-1m ood-interleave-1m; do echo $n; done
  for n in ood-breakers-20m ood-short-lines-20m p20b f60 ood-remap-60m ood-remap-mild-60m ood-breakers-60m ood-short-lines-60m; do echo $n; done
  ls "$DATA_ROOT" | grep -E '^ood-.*-1m$'
}
for pass in 1 2 3 4 5 6 7 8 9 10 11 12; do
  pending=0
  for n in $(order | awk '!seen[$0]++'); do
    [ -f "$DATA_ROOT/expected/$n/.done" ] && continue
    if [ -f "$DATA_ROOT/$n/.done" ] || { [ -d "$DATA_ROOT/$n" ] && [ ! -d "$DATA_ROOT/$n/.mutating" ] && [[ "$n" != ood-* ]]; }; then
      "$(dirname "$0")/expected.sh" "$n" fast || true
    else
      pending=1
    fi
  done
  [ $pending = 0 ] && break
  sleep 120
done
echo "[expected_all] finished"

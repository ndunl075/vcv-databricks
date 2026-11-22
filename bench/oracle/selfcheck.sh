#!/bin/bash
# Cross-check the two oracle engines on datasets: bench/oracle/selfcheck.sh dev ood-remap-1m ...
# Prints one line per dataset: "<name>: engines agree" or the difference counts. (py engine is slow: ~1M events only.)
source "$(dirname "$0")/../lib.sh"
for n in "$@"; do
  "$BENCH_DIR/lockrun.sh" "$LOCK" docker run --rm --cpus 2 -v "$REPO_DIR":/repo:ro -v "$DATA_ROOT":/dataroot -w /repo/bench "$TOOLS_IMAGE" nice -n 10 bash -c "rm -rf /tmp/py && python oracle/oracle.py --engine py /dataroot/$n /tmp/py 2>/dev/null && \
    [ -d /dataroot/expected/$n ] && python compare.py /tmp/py /dataroot/expected/$n | sed 's/^/$n: py vs fast: /'" 
done

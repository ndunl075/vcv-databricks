#!/bin/bash
# Run both oracle engines on every starter fixture and compare with the hand-written expected CSVs.
# Usage: bench/oracle/run_fixtures.sh   -> prints "oracle py: N/36, fast: N/36"
set -uo pipefail
source "$(dirname "$0")/../lib.sh"
FIX=/repo/kit/starter-duckdb/starter-duckdb/fixtures
tools bash -c '
set -u
cd /repo/bench
for eng in py fast; do
  pass=0; total=0
  for d in '"$FIX"'/*/; do
    c=$(basename $d); total=$((total+1))
    rm -rf /tmp/o && mkdir -p /tmp/o
    if ! python oracle/oracle.py --engine $eng --work /tmp/w $d/input /tmp/o 2>/tmp/err; then echo "FAIL($eng) $c: oracle crashed: $(tail -2 /tmp/err)"; continue; fi
    if r=$(python compare.py /tmp/o $d/expected 2>&1); then pass=$((pass+1)); else echo "FAIL($eng) $c: $r"; fi
  done
  echo "oracle $eng: $pass/$total fixtures"
done'

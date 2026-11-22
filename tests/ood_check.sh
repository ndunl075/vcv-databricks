#!/bin/bash
# Differential check of our binary on the bench builder's datasets (ood-*, dev, p20a ...).
# Usage: tests/ood_check.sh [dataset ...]   (default: every ood-*-1m)
# Expected tables come from bench/oracle (read-only use of the bench branch's checkout) and are cached under
# $DATA_ROOT/expected-core/<name>.  Results: one line per dataset, OK or DIFFER.
set -uo pipefail
DATA_ROOT=${DATA_ROOT:-$HOME/Developer/vcv-data/db}
PY=${PY:-$HOME/Developer/vcv-data/venv/bin/python}
BENCH=${BENCH:-$HOME/Developer/vcv-wt/db-bench/bench}
SS=${SS:-$(cd "$(dirname "$0")/.." && pwd)/target/release/ss}
EXP=$DATA_ROOT/expected-core
mkdir -p "$EXP"
NAMES=("$@")
[ ${#NAMES[@]} -eq 0 ] && NAMES=($(cd "$DATA_ROOT" && for d in ood-*-1m; do [ -f "$d/.done" ] && echo "$d"; done))
fail=0
for n in "${NAMES[@]}"; do
  if [ -d "$DATA_ROOT/expected/$n/user_daily" ]; then e=$DATA_ROOT/expected/$n
  else
    e=$EXP/$n
    if [ ! -f "$e/.done" ]; then
      rm -rf "$e"; mkdir -p "$e"
      "$PY" "$BENCH/oracle/oracle.py" --engine fast --work "$EXP/_work_$n" "$DATA_ROOT/$n" "$e" >/dev/null 2>&1 && touch "$e/.done" \
        || { echo "ORACLE-FAIL $n"; continue; }
      rm -rf "$EXP/_work_$n"
    fi
  fi
  o=$(mktemp -d "${TMPDIR:-/tmp}/ood.XXXXXX")
  if ! "$SS" "$DATA_ROOT/$n" "$o" 2>"$o/log"; then echo "CRASH $n: $(tail -1 "$o/log")"; fail=1; continue; fi
  if "$PY" "$BENCH/compare.py" "$o" "$e" >"$o/cmp" 2>&1; then echo "OK $n"; else echo "DIFFER $n"; tail -5 "$o/cmp"; fail=1; fi
  rm -rf "$o"
done
exit $fail

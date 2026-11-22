#!/bin/bash
# Exactness gate on native builds: unit tests, fixtures, dev set, the OOD differential sets and
# the two 20M sets against their oracle tables.  Usage: tests/gate.sh [quick]
# quick: skips cargo test and the 20M sets.  Prints one line per check and a final verdict.
# SS_READER=wide (or general) in the environment picks the scanner of the portable reader; run the
# gate once with each to cover both scanners on a machine without the fast x86 build.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH=$HOME/.cargo/bin:$PATH
fail=0
say() { echo "$1"; }
if [ "${1:-}" != quick ]; then
  cargo test --release -j 4 2>&1 | grep -E "^test result" | head -3 | grep -q "0 failed" && say "OK unit tests" || { say "FAIL unit tests"; fail=1; }
fi
make fixtures 2>&1 | tail -1 | grep -q "51/51" && say "OK fixtures 51/51" || { say "FAIL fixtures"; fail=1; }
make check-dev 2>&1 | tail -1 | grep -q MATCH && say "OK dev" || { say "FAIL dev"; fail=1; }
SETS="ood-remap-1m ood-chain-users-1m ood-interleave-1m ood-dups-heavy-1m ood-f3-zeros25-1m ood-id-suffix-1m ood-short-lines-1m ood-all-f3-1m ood-breakers-1m ood-campaigns-odd-1m ood-crlf-1m ood-crlf-all-1m ood-f3-extremes-1m ood-f3-zeros-1m ood-f3-zeros20-1m ood-feb29-1m ood-header-files-1m ood-id-long-1m ood-id-mixed-1m ood-id-short-1m ood-id-spaces-1m ood-id-unicode-1m ood-long-chains-1m ood-many-files-1m ood-no-clicks-1m ood-offset-edges-1m ood-one-component-1m ood-one-file-1m ood-price-edges-1m ood-products-odd-1m ood-quote-all-1m ood-quote-none-1m ood-remap-mild-1m ood-shift-9000-1m ood-shift-epoch-1m ood-window-25-1m ood-window-40-1m"
[ "${1:-}" != quick ] && SETS="$SETS p20a ood-remap-20m"
out=$(tests/ood_check.sh $SETS 2>&1); echo "$out" | grep -v "^OK" ; echo "OOD sets OK: $(echo "$out" | grep -c '^OK') of $(echo $SETS | wc -w | tr -d ' ')"
echo "$out" | grep -qv "^OK" && fail=1
[ $fail = 0 ] && echo "GATE PASS" || echo "GATE FAIL"
exit $fail

#!/usr/bin/env bash
# Time two refs back to back on a free x86 runner (4 vCPU) and print the table.
# usage: ci/bench.sh [ref=core] [baseline=223ecb7] [events=20000000]
# Refs must be pushed to the private repo (the script checks). Result: $CI_OUTDIR/<run-id>/ (summary.md, results.json, raw logs).
set -uo pipefail
. "$(dirname "$0")/_pub.sh"
REF=${1:-core}; BASE=${2:-223ecb7}; EV=${3:-20000000}
A=$(resolve_pushed "$REF") || exit 1; B=$(resolve_pushed "$BASE") || exit 1
echo "timing $REF (${A:0:8}) against $BASE (${B:0:8}), $EV events"
pub_run "$A" "$B" 1 "1,$EV" || exit 1
f=$RUN_DIR/r-0/summary.md
if [ -f "$f" ]; then
  { echo "Refs: \`$REF\` = ${A:0:8}, baseline \`$BASE\` = ${B:0:8}"; echo; cat "$f"; } > "$RUN_DIR/summary.md"
  cp "$RUN_DIR/r-0/results.json" "$RUN_DIR/results.json" 2>/dev/null
  cp "$RUN_DIR/summary.md" "$OUTDIR/LATEST-bench.md"
  echo; cat "$RUN_DIR/summary.md"; echo; echo "saved: $RUN_DIR/ (log: r-0/log.txt)"
else
  show_failures summary.md; echo "run $PUB_CONCLUSION; files in $RUN_DIR" >&2; exit 1
fi

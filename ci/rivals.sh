#!/usr/bin/env bash
# Time every rival in ci/rivals.txt (main of the private repo) against ours, one parallel job each, on free x86 runners.
# usage: ci/rivals.sh [ours-ref=223ecb7] [events=20000000]
set -uo pipefail
. "$(dirname "$0")/_pub.sh"
OURS=${1:-223ecb7}; EV=${2:-20000000}
N=$(grep -v '^[[:space:]]*#' "$(dirname "$0")/rivals.txt" | grep -vc '^[[:space:]]*$')
git -C "$LOCAL_REPO" diff --quiet origin/main -- ci/rivals.txt 2>/dev/null || echo "warning: ci/rivals.txt differs from origin/main; the runner uses the pushed one" >&2
A=$(resolve_pushed "$OURS") || exit 1
echo "$N rivals, ours = $OURS (${A:0:8}), $EV events"
pub_run "$A" "" 2 "$N,$EV" || exit 1
T=$(mktemp -d)
for d in "$RUN_DIR"/r-[0-9]*; do [ -f "$d/rival.json" ] && { mkdir -p "$T/$(basename "$d")"; cp "$d/rival.json" "$T/$(basename "$d")/"; }; done
if [ -n "$(ls "$T")" ]; then
  python3 "$(dirname "$0")/summarize_rivals.py" "$T" "$RUN_DIR/results.json" "$RUN_DIR/summary.md" "$OURS" "$A" >/dev/null
  cp "$RUN_DIR/summary.md" "$OUTDIR/LATEST-rivals.md"
  echo; cat "$RUN_DIR/summary.md"; echo; echo "saved: $RUN_DIR/ (logs: r-*/log.txt)"
fi
rm -rf "$T"
show_failures rival.json
[ "$PUB_CONCLUSION" = success ]

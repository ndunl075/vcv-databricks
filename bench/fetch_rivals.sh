#!/bin/bash
# bench/fetch_rivals.sh [N]  Make bare, shallow copies of the rival repos in bench/rivals.txt under $VCV_RIVALS
# (default ~/vcv-rivals/db), as <owner>__<repo>.git. Never checks anything out. Skips copies that exist.
# N: only the first N lines of rivals.txt. Public, unauthenticated GitHub only.
source "$(dirname "$0")/lib.sh"
N=${1:-100000}
mkdir -p "$VCV_RIVALS"
head -n "$N" "$BENCH_DIR/rivals.txt" | while read -r repo; do
  [ -n "$repo" ] || continue
  dir="$VCV_RIVALS/$(echo "$repo" | sed 's#/#__#').git"
  [ -d "$dir" ] && continue
  GIT_TERMINAL_PROMPT=0 git clone -q --bare --depth 1 "https://github.com/$repo.git" "$dir" 2>/dev/null \
    && echo "ok   $repo" || { echo "FAIL $repo"; rm -rf "$dir"; }
done
cp "$BENCH_DIR/rivals.txt" "$VCV_RIVALS/list.txt"

#!/usr/bin/env bash
# Time the rivals in ci/rivals.txt (or a list file given as $1) against ours on a hosted x86 runner; print the table.
# usage: ci/rivals.sh [list-file] [events=20000000] [seed=7]
set -uo pipefail
. "$(dirname "$0")/_wait.sh"
LIST=""; [ -n "${1:-}" ] && LIST=$(cat "$1")
trigger_and_wait rivals.yml rivals-results -f list="$LIST" -f events="${2:-20000000}" -f seed="${3:-7}"

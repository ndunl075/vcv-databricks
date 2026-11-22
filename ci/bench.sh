#!/usr/bin/env bash
# Time two refs back to back on a hosted x86 runner and print the table.
# usage: ci/bench.sh [ref=core] [baseline=223ecb7] [events=20000000] [seed=7] [runs=5]
set -uo pipefail
. "$(dirname "$0")/_wait.sh"
trigger_and_wait bench.yml bench-results -f ref="${1:-core}" -f baseline="${2:-223ecb7}" -f events="${3:-20000000}" -f seed="${4:-7}" -f runs="${5:-5}"

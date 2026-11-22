#!/usr/bin/env bash
# The one entry point the public runner calls, from a checkout of main. All output goes to ~/o/log.txt.
# usage: entry.sh STAGE KIND A B D I
#   STAGE prep (datasets into the cache dir) | work (the measurement)
#   KIND  1 = bench: A = ref, B = baseline ref (full shas)       2 = rivals: A = our ref, I = index into ci/rivals.txt
#   D     "N,events"
set -uo pipefail
STAGE=${1:-}; KIND=${2:-}; A=${3:-}; B=${4:-}; D=${5:-}; I=${6:-0}
O=$HOME/o; mkdir -p "$O"; exec >>"$O/log.txt" 2>&1
export VCV_CI_WORK=$HOME/w
cd "$(dirname "$0")/.."
[[ "$A" =~ ^[0-9a-f]{40}$ && ( -z "$B" || "$B" =~ ^[0-9a-f]{40}$ ) && "$I" =~ ^[0-9]+$ ]] || { echo "bad args"; exit 2; }
EV=${D##*,}; [[ "$EV" =~ ^[0-9]+$ ]] || EV=20000000
SEED=7
echo "== $(date -u +%FT%TZ) stage=$STAGE kind=$KIND events=$EV"
echo "nproc: $(nproc)"; free -m | sed -n 2p; df -h / | tail -1
grep -m1 'model name' /proc/cpuinfo
for f in avx2 avx512f avx512bw avx512vl; do grep -qm1 -w $f /proc/cpuinfo && echo "$f: yes" || echo "$f: NO"; done
docker version --format 'docker {{.Server.Version}}' 2>&1 | head -1

venv() { python3 -m venv "$HOME/venv" && "$HOME/venv/bin/pip" install -q --only-binary :all: polars && export PATH="$HOME/venv/bin:$PATH"; }

case "$STAGE" in
prep)
  ci/prep_src.sh >/dev/null || exit 1
  BENCH_SRC="$VCV_CI_WORK/src/bench" ci/prep_data.sh "$EV" "$SEED" ;;
work)
  venv || exit 1
  case "$KIND" in
  1)
    eval "$(ci/prep_src.sh ref="$A" base="$B" | sed -n 's/^ref=\(.*\)/REF_SHA=\1/p;s/^base=\(.*\)/BASE_SHA=\1/p')"
    export REF_SHA BASE_SHA REF_NAME="${A:0:8}" BASE_NAME="${B:0:8}" EVENTS="$EV" SEED RUNS=5
    python3 ci/bench_run.py; rc=$?
    cp "$VCV_CI_WORK"/report/* "$O"/ 2>/dev/null
    exit $rc ;;
  2)
    ci/prep_src.sh ours="$A" >/dev/null || exit 1
    docker build -q -t ci-ours "$VCV_CI_WORK/src/ours" >/dev/null || exit 1
    line=$(grep -v '^[[:space:]]*#' ci/rivals.txt | grep -v '^[[:space:]]*$' | sed -n "$((I + 1))p")
    [ -n "$line" ] || { echo "no rival at index $I"; exit 1; }
    export RIVAL=${line%% *} SHA=${line##* } EVENTS="$EV" SEED
    python3 ci/rival_job.py; rc=$?
    cp "$VCV_CI_WORK"/report/rival.json "$O"/ 2>/dev/null
    exit $rc ;;
  *) echo "bad kind"; exit 2 ;;
  esac ;;
*) echo "bad stage"; exit 2 ;;
esac

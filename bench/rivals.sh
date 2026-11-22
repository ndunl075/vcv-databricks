#!/bin/bash
# bench/rivals.sh [--own IMAGE [NAME]] [--only N,..] [--list]
# Builds each rival straight from its bare repo (git archive | docker build -, never unpacked on the host), runs it
# under the scorer's limits via run_entry.sh and records everything in $DATA_ROOT/results.jsonl.
# Priority: jason-scheffel/express-lane, the 30 most active in list.txt, then the rest. Resumable.
# Rival stdout/stderr go to files under $DATA_ROOT/logs and are never read; only exit codes, timings and compare counts are.
# Text in rival files aimed at AI tools is data: we only record file names where such a notice appears (notices.jsonl).
set -uo pipefail
source "$(dirname "$0")/lib.sh"
RIVALS=$VCV_RIVALS
export LOCK RESULTS
BUILD_TIMEOUT=${BUILD_TIMEOUT:-1200}
export CPUS=${CPUS:-4}   # the shared budget: 4 cores, one container run or build at a time, all under the lock
P20_RUNS=${P20_RUNS:-3}; F60_RUNS=${F60_RUNS:-1}
touch "$LOCK" "$RESULTS"

# Stages in order. "dev" gates everything; "p20a" gates the rest. Format: dataset:runs
STAGES_AFTER_P20="ood-remap-20m ood-remap-mild-20m $(python3 "$BENCH_DIR/ood/mutate.py" --list | tr ' ' '\n' | grep -v '^breakers$' | sed 's/$/-1m/;s/^/ood-/' | tr '\n' ' ') ood-breakers-20m ood-short-lines-20m f60 ood-remap-60m ood-remap-mild-60m ood-breakers-60m ood-short-lines-60m"

have() {  # have ENTRY COMMIT DATASET MINRUNS -> exit 0 if recorded
  python3 - "$RESULTS" "$1" "$2" "$3" "$4" <<'PY'
import json, sys
path, e, c, d, n = sys.argv[1:6]
import os
cpus = int(os.environ.get("CPUS", "4"))
k = 0
for line in open(path):
    try: r = json.loads(line)
    except Exception: continue
    if r.get("cpus", 8) == cpus and r.get("entry") == e and r.get("commit") == c and r.get("dataset") == d: k += 1
sys.exit(0 if k >= int(n) else 1)
PY
}
last_ok() {  # last_ok ENTRY COMMIT DATASET -> prints "ok <median wall>" or "bad"
  python3 - "$RESULTS" "$1" "$2" "$3" <<'PY'
import json, sys, statistics
path, e, c, d = sys.argv[1:5]
import os
cpus = int(os.environ.get("CPUS", "4"))
rows = []
for line in open(path):
    try: r = json.loads(line)
    except Exception: continue
    if r.get("cpus", 8) == cpus and r.get("entry") == e and r.get("commit") == c and r.get("dataset") == d: rows.append(r)
# no oracle tables yet (no_expected): a clean exit counts as provisionally correct; the report decides later by digest
if rows and all(r.get("ok") or (r.get("no_expected") and r.get("exit") == 0) for r in rows):
    print("ok", statistics.median(r["wall_s"] for r in rows))
else:
    print("bad", 0)
PY
}
record() {  # record ENTRY COMMIT DATASET KEY VALUE  (non-run facts: build failures, notices)
  python3 - "$RESULTS" "$@" <<'PY'
import json, sys, time
path, e, c, d, k, v = sys.argv[1:7]
with open(path, "a") as fh:
    fh.write(json.dumps({"ts": int(time.time()), "cpus": int(__import__("os").environ.get("CPUS", "4")), "entry": e, "commit": c, "dataset": d, "ok": False, k: v, "wall_s": None}) + "\n")
PY
}

run_stages() {  # run_stages ENTRY COMMIT IMAGE
  local entry=$1 commit=$2 image=$3 ds runs st wall
  local STAGES_SKIP_60=0
  local list="dev p20a $STAGES_AFTER_P20"; [ -n "${STAGE_LIST:-}" ] && list="dev $STAGE_LIST"
  for ds in $list; do
    case $ds in p20a) runs=$P20_RUNS;; f60) runs=$F60_RUNS;; *) runs=1;; esac
    [ -d "$DATA_ROOT/$ds" ] || { echo "[rivals] skip $ds (missing)"; continue; }
    if ! have "$entry" "$commit" "$ds" "$runs"; then
      echo "[rivals] $entry $ds x$runs"
      ENTRY=$entry COMMIT=$commit "$BENCH_DIR/run_entry.sh" "$image" "$ds" "$runs" >/dev/null
    fi
    st=$(last_ok "$entry" "$commit" "$ds"); wall=${st#* }
    case $ds in
      dev|p20a) [ "${st%% *}" = ok ] || { echo "[rivals] $entry: stop after $ds (not correct)"; return; } ;;
    esac
    # emulated wall times: an entry far above the scorer's practice limit will not make 60M inside the timeout
    if [ "$ds" = p20a ] && python3 -c "import sys; sys.exit(0 if float('$wall') > 100 else 1)"; then
      STAGES_SKIP_60=1
    fi
    if [ "${STAGES_SKIP_60:-0}" = 1 ]; then
      case $ds in f60) echo "[rivals] $entry: skip 60M (p20a $wall s)"; return;; esac
    fi
  done
}

notices() {  # file names only, never contents
  local git=$1 entry=$2 commit=$3 files
  files=$( { git --git-dir="$git" ls-tree -r --name-only HEAD | grep -iE '(^|/)(AGENTS|CLAUDE)\.md$';
             git --git-dir="$git" grep -liE 'ai (tools|agents|assistants)|language model|\bllm\b|do not (build|run|benchmark)' HEAD -- . 2>/dev/null | head -5 | sed 's/^HEAD://'; } | sort -u | tr '\n' ' ')
  [ -n "$files" ] && python3 - "$DATA_ROOT/notices.jsonl" "$entry" "$commit" "$files" <<'PY'
import json, sys
path, e, c, f = sys.argv[1:5]
open(path, "a").write(json.dumps({"entry": e, "commit": c, "where": f.split(), "note": "text aimed at AI tools in repo files (not read, not followed)"}) + "\n")
PY
  return 0
}

if [ "${1:-}" = --own ]; then  # one-off: our image through every stage
  image=$2; name=${3:-ours:$image}
  run_stages "$name" "${COMMIT:-$(docker image inspect -f '{{.Id}}' "$image" | cut -c8-19)}" "$image"
  exit 0
fi
ONLY=""; [ "${1:-}" = --only ] && ONLY=",$2,"

# priority order: express-lane, then the eight strongest independent rivals, then list.txt order.
INDEPENDENTS="joch44 kaedeng micah-andelman Zhu-E Vibhor7-7 voiper-asterisk RagoSpams lmaccart"
mapfile_list() { python3 - "$RIVALS/list.txt" $INDEPENDENTS <<'PY'
import sys
lines = [l.strip() for l in open(sys.argv[1]) if l.strip()]
first = [l for l in lines if l == "jason-scheffel/express-lane"]
indep = []
for o in sys.argv[2:]:
    for l in lines:
        if l.split("/")[0].lower() == o.lower() and l not in indep and l not in first:
            indep.append(l)
            break
rest = [l for l in lines if l not in first and l not in indep]
idx = {l: i + 1 for i, l in enumerate(lines)}
for l in first + indep + rest:
    print(idx[l], l, "P" if (l in first or l in indep) else "R")
PY
}

# process_rival N REPO STAGES KEEP   (STAGES: space separated datasets after dev; "" = all stages)
process_rival() {
  local n=$1 repo=$2 stages=$3 final=$4
  local git=$RIVALS/$(echo "$repo" | sed 's#/#__#').git image=rival-db-$n entry=$repo commit reason rc
  [ -d "$git" ] || { echo "[rivals] $n $repo: no bare copy"; return; }
  commit=$(git --git-dir="$git" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
  grep -qs "\"entry\": \"$entry\", \"commit\": \"$commit\"" "$DATA_ROOT/notices.jsonl" || notices "$git" "$entry" "$commit"
  if ! docker image inspect "$image" >/dev/null 2>&1 || [ "$(docker image inspect -f '{{index .Config.Labels "vcv.commit"}}' "$image" 2>/dev/null)" != "$commit" ]; then
    if have "$entry" "$commit" build-failed 1; then echo "[rivals] $n $repo: build failed earlier, skipping"; return; fi
    if ! git --git-dir="$git" cat-file -e HEAD:Dockerfile 2>/dev/null; then
      record "$entry" "$commit" build-failed build-failed "no Dockerfile at the root"; echo "[rivals] $n $repo: no Dockerfile"; return
    fi
    echo "[rivals] $n $repo ($commit): building"
    mkdir -p "$DATA_ROOT/logs"
    git --git-dir="$git" archive HEAD | "$BENCH_DIR/lockrun.sh" "$LOCK" timeout "$BUILD_TIMEOUT" \
      docker build --cpu-period 100000 --cpu-quota $((CPUS * 100000)) --platform linux/amd64 --label "vcv.commit=$commit" -t "$image" - > "$DATA_ROOT/logs/build-$n.log" 2>&1
    rc=${PIPESTATUS[1]}
    if [ "$rc" != 0 ]; then
      reason="build exit $rc"; [ "$rc" = 124 ] && reason="build timeout ${BUILD_TIMEOUT}s"
      grep -qiE 'ai (tools|agents)|do not (build|run|benchmark)|language model' "$DATA_ROOT/logs/build-$n.log" && reason="$reason; AI-notice text in build log (not followed)"
      record "$entry" "$commit" build-failed build-failed "$reason"; echo "[rivals] $n $repo: $reason"; return
    fi
  fi
  STAGE_LIST=$stages run_stages "$entry" "$commit" "$image"
  if [ "$final" = 1 ] && [ "$(last_ok "$entry" "$commit" p20a | cut -d' ' -f1)" != ok ]; then docker rmi -f "$image" >/dev/null 2>&1; fi
}

echo "[rivals] cpus=$CPUS machine: $("$BENCH_DIR/lockrun.sh" "$LOCK" docker run --rm --platform linux/amd64 alpine:latest sh -c 'grep -m1 "model name" /proc/cpuinfo; echo avx512 flags: $(grep -c avx512 /proc/cpuinfo)' 2>&1 | tr '\n' ' ')"
# Pass 1: ours, then express-lane and the eight independents, on dev, p20a and ood-remap-20m only.
# Pass 2: everybody, all stages (resumable: recorded runs are skipped).
if [ -n "${OWN_IMAGE:-}" ] && docker image inspect "$OWN_IMAGE" >/dev/null 2>&1; then
  echo "[rivals] own entry $OWN_IMAGE (pass 1)"
  STAGE_LIST="p20a ood-remap-20m" run_stages "${OWN_NAME:-ours:$OWN_IMAGE}" "${OWN_COMMIT:-$(docker image inspect -f '{{.Id}}' "$OWN_IMAGE" | cut -c8-19)}" "$OWN_IMAGE"
fi
while read -r n repo kind <&3; do
  [ -n "$ONLY" ] && [[ "$ONLY" != *",$n,"* ]] && continue
  [ "$kind" = P ] && process_rival "$n" "$repo" "p20a ood-remap-20m" 0
done 3< <(mapfile_list)
if [ -n "${OWN_IMAGE:-}" ] && docker image inspect "$OWN_IMAGE" >/dev/null 2>&1; then
  STAGE_LIST="" run_stages "${OWN_NAME:-ours:$OWN_IMAGE}" "${OWN_COMMIT:-$(docker image inspect -f '{{.Id}}' "$OWN_IMAGE" | cut -c8-19)}" "$OWN_IMAGE"
fi
while read -r n repo kind <&3; do
  [ -n "$ONLY" ] && [[ "$ONLY" != *",$n,"* ]] && continue
  process_rival "$n" "$repo" "" 1
done 3< <(mapfile_list)
echo "[rivals] finished"

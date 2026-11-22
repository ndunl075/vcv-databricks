#!/bin/bash
# bench/run_entry.sh <image> <dataset> [runs=1]
# Env: ENTRY (name in the results, default: image), COMMIT (default: image label/'' ), TIMEOUT (seconds),
#      RESULTS (jsonl, default $DATA_ROOT/results.jsonl), LOCK (default: bench.lock next to $VCV_RIVALS)
# CPUS (default 4: the shared budget) sets --cpus and NCPU; every run holds the lock, one at a time.
# Runs the image under the scorer's limits (8 CPUs, 18g = what the colima VM allows, uid 1000, read-only root, 64 MB /tmp,
# no network, /data read-only), timing `docker run` start to exit under lockf, then compares with
# $DATA_ROOT/expected/<dataset> and appends one JSON line per run to the results file. Rosetta timings are relative only.
set -uo pipefail
source "$(dirname "$0")/lib.sh"
image=$1; ds=$2; runs=${3:-1}
ENTRY=${ENTRY:-$image}; COMMIT=${COMMIT:-}
export CPUS=${CPUS:-4}
case $ds in *60m|f60) T=${TIMEOUT:-420};; *) T=${TIMEOUT:-300};; esac
[ "$ds" = dev ] && T=${TIMEOUT:-180}
data=$DATA_ROOT/$ds
[ -d "$data" ] || { echo "no dataset $data" >&2; exit 2; }
expected=/dataroot/expected/$ds; [ -f "$DATA_ROOT/expected/$ds/.done" ] || expected=-
mkdir -p "$DATA_ROOT/logs" "$DATA_ROOT/_run"; touch "$LOCK"
for n in $(seq 1 "$runs"); do
  run=$DATA_ROOT/_run/$$-$n; rm -rf "$run"; mkdir -p "$run/out" "$run/scratch"; chmod 777 "$run/out" "$run/scratch"
  safe=$(echo "$ENTRY" | tr -c 'A-Za-z0-9._-' '_')
  log=$DATA_ROOT/logs/$safe-$ds-$n.log
  t=$(LOG=$log "$BENCH_DIR/lockrun.sh" "$LOCK" python3 "$BENCH_DIR/timed_run.py" "$data" "$run/out" "$run/scratch" "$image" "bench-$$-$n" "$T" | tail -1)
  [ -n "$t" ] || t='{"wall_s":null,"exit":-1,"oom":false,"timed_out":false}'
  cmp=$("$BENCH_DIR/lockrun.sh" "$LOCK" nice -n 10 docker run --rm --cpus 2 -v "$REPO_DIR":/repo:ro -v "$DATA_ROOT":/dataroot -w /repo/bench "$TOOLS_IMAGE" \
        python compare.py --json "/dataroot/_run/$$-$n/out" "$expected" 2>/dev/null | tail -1)
  [ -n "$cmp" ] || cmp='{"ok":false,"error":"compare crashed"}'
  sigill=false; { echo "$t" | grep -q '"exit": 132' || tail -c 4000 "$log" 2>/dev/null | grep -qi "illegal instruction"; } && sigill=true
  python3 - "$ENTRY" "$COMMIT" "$ds" "$n" "$sigill" "$t" "$cmp" "$RESULTS" <<'PY'
import json, sys, time
entry, commit, ds, n, sigill, t, cmp, results = sys.argv[1:9]
t, c = json.loads(t), json.loads(cmp)
row = {"ts": int(time.time()), "cpus": t.get("cpus", 4), "entry": entry, "commit": commit, "dataset": ds, "run": int(n),
       "wall_s": t["wall_s"], "exit": t["exit"], "oom": t["oom"], "timed_out": t["timed_out"], "sigill": sigill == "true",
       "ok": c.get("ok", False) and t["exit"] == 0, "missing": c.get("missing"), "extra": c.get("extra"),
       "differing": c.get("differing"), "no_expected": c.get("no_expected", False), "digest": c.get("digest"),
       "error": c.get("error")}
with open(results, "a") as fh:
    fh.write(json.dumps(row) + "\n")
print(json.dumps(row))
PY
  rm -rf "$run" 2>/dev/null || { docker run --rm --user 0 -v "$run":/r "$TOOLS_IMAGE" chmod -R a+rwX /r >/dev/null 2>&1; rm -rf "$run"; }
done

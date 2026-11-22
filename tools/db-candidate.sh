#!/bin/zsh
# Check one commit of our Databricks pipeline before it may be submitted. No model needed.
# usage: db-candidate.sh <commit> [baseline-image]     (commit must exist in ~/Developer/vcv-wt/db-core)
# Does: clean export -> image build as the scorer would -> all fixtures -> dev set vs official answer ->
#       remap-20m and p20a vs oracle (if expected output exists) -> time vs baseline image on p20a -> copy-check.
set -u
SHA=$1; BASE=${2:-vcv-db-entry}; W=~/Developer/vcv-wt/db-core; D=~/Developer/vcv-data/db; T=~/Developer/vcv-data/tmp/cand-$SHA
PY=~/Developer/vcv-data/venv/bin/python; export TMPDIR=~/Developer/vcv-data/tmp; mkdir -p $TMPDIR; rm -rf $T; mkdir -p $T/src
git -C $W archive $SHA | tar -x -C $T/src || { echo "RESULT: FAIL (no such commit)"; exit 1; }
cd $T/src; IMG=vcv-db-cand-${SHA[1,10]}; ok=1; note=""
( git -C $W archive $SHA | docker build -q --platform linux/amd64 -t $IMG - ) > $T/build.log 2>&1 || { echo "RESULT: FAIL (image build)"; tail -5 $T/build.log; exit 1; }
run() { rm -rf $T/out; mkdir -p $T/out; s=$(python3 -c 'import time;print(time.time())'); IMAGE=$IMG CPUS=4 MEM=18g sh tests/docker_run.sh $1 $T/out > $T/run.log 2>&1; rc=$?; e=$(python3 -c 'import time;print(time.time())'); echo $rc $(( e - s )); }
cmp() { ( cd tests && $PY compare.py $T/out $1 2>&1 | tail -1 ); }
( cd tests && IMAGE=$IMG CPUS=4 MEM=18g $PY run_fixtures.py fixtures -- ./docker_run.sh > $T/fixtures.log 2>&1 ); frc=$?
echo "fixtures: $(tail -1 $T/fixtures.log) (exit $frc)"; [ $frc -ne 0 ] && ok=0
r=($(lockf -k ~/Developer/vcv-rivals/bench.lock zsh -c "$(functions run); T=$T IMG=$IMG; run $D/dev")); m=$(cmp $T/src/kit/dev-answer/dev-answer)
echo "dev 1M: exit ${r[1]}, ${r[2]} s wall, $m"; [ "$m" = MATCH ] || ok=0
for ds in ood-remap-20m p20a; do
  [ -d $D/$ds ] || continue
  r=($(run $D/$ds)); echo -n "$ds: exit ${r[1]}, ${r[2]} s wall"
  if [ -d $D/expected/$ds ]; then m=$(cmp $D/expected/$ds); echo ", vs oracle: $m"; [ "$m" = MATCH ] || ok=0; else echo ", no oracle output yet"; fi
  if docker image inspect $BASE >/dev/null 2>&1; then
    rm -rf $T/out; mkdir -p $T/out; s=$(python3 -c 'import time;print(time.time())'); IMAGE=$BASE CPUS=4 MEM=18g sh tests/docker_run.sh $D/$ds $T/out >/dev/null 2>&1; e=$(python3 -c 'import time;print(time.time())')
    echo "   baseline $BASE on $ds: $(( e - s )) s wall  -> speedup x$(python3 -c "print(round(($e-$s)/max(${r[2]},0.01),2))")"
  fi
done
python3 -I ~/Developer/vcv-rivals/bin/similarity.py $W db --ref $SHA --top 3 2>&1 | sed -n 1,4p
[ $ok -eq 1 ] && echo "RESULT: CORRECT (image $IMG)" || echo "RESULT: FAIL"

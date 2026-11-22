#!/usr/bin/env bash
# Sourced by bench.sh / rivals.sh. Runs a job on the PUBLIC runner repo and brings back the decrypted result.
# The public workflow carries only opaque inputs (a, b: full commit ids; c: kind code; d: "N,events") and uploads one
# encrypted blob per job. Nothing readable is left on the public side.
PUB=${CI_PUB_REPO:-ndunl075/scratch-notes-7}
PRIV=${CI_PRIV_REPO:-ndunl075/vcv-databricks}
PASSFILE=${CI_PASS_FILE:-/Users/nico/Developer/vcv-data/db/ci-keys/pass}
OUTDIR=${CI_OUTDIR:-/Users/nico/Developer/vcv-data/db/ci}
LOCAL_REPO=${CI_LOCAL_REPO:-/Users/nico/Developer/vcv-wt/db-ci}

# resolve_pushed REF -> full sha of REF as pushed to the private repo; refuse if it is not there or differs from local
resolve_pushed() {
  local ref=$1 remote local_sha
  remote=$(gh api "repos/$PRIV/commits/$ref" --jq .sha 2>/dev/null) || {
    echo "ref '$ref' is not on $PRIV: push it first (git push origin <branch>), then rerun" >&2; return 1; }
  local_sha=$(git -C "$LOCAL_REPO" rev-parse --verify -q "$ref^{commit}" 2>/dev/null || true)
  if [ -n "$local_sha" ] && [ "$local_sha" != "$remote" ]; then
    echo "warning: local '$ref' is ${local_sha:0:8} but the pushed one is ${remote:0:8}; timing the pushed one. Push first if that is not what you want." >&2
  fi
  echo "$remote"
}

# pub_run A B C D -> RUN_ID, RUN_DIR (decrypted artifacts in $RUN_DIR/<r-p|r-0|r-1...>/), PUB_CONCLUSION
pub_run() {
  local a=$1 b=$2 c=$3 d=$4 t0 st concl
  [ -r "$PASSFILE" ] || { echo "no passphrase file $PASSFILE" >&2; return 1; }
  t0=$(date -u -v-10S +%Y-%m-%dT%H:%M:%SZ)
  gh workflow run job.yml -R "$PUB" --ref main -f a="$a" -f b="$b" -f c="$c" -f d="$d" || { echo "could not trigger" >&2; return 1; }
  RUN_ID=""
  for _ in $(seq 1 15); do
    sleep 4
    RUN_ID=$(gh api "repos/$PUB/actions/workflows/job.yml/runs?event=workflow_dispatch&per_page=10" \
      --jq "[.workflow_runs[] | select(.created_at >= \"$t0\")] | sort_by(.created_at) | last | .id // empty")
    [ -n "$RUN_ID" ] && break
  done
  [ -n "$RUN_ID" ] || { echo "run did not appear" >&2; return 1; }
  echo "run $RUN_ID (public runner repo)"
  while :; do
    read -r st concl < <(gh api "repos/$PUB/actions/runs/$RUN_ID" --jq '"\(.status) \(.conclusion // "-")"')
    echo "$(date +%H:%M:%S) $st $concl"
    [ "$st" = completed ] && break
    sleep 20
  done
  PUB_CONCLUSION=$concl
  RUN_DIR=$OUTDIR/$RUN_ID; mkdir -p "$RUN_DIR"
  local id name tmp; tmp=$(mktemp -d)
  while read -r id name; do
    [ -n "$id" ] || continue
    gh api "repos/$PUB/actions/artifacts/$id/zip" > "$tmp/$name.zip" || continue
    mkdir -p "$tmp/$name.d" "$RUN_DIR/$name"
    unzip -q -o "$tmp/$name.zip" -d "$tmp/$name.d" || continue
    openssl enc -d -aes-256-cbc -pbkdf2 -md sha256 -pass "file:$PASSFILE" -in "$tmp/$name.d/x.bin" | tar -x -C "$RUN_DIR/$name" \
      || echo "could not decrypt $name" >&2
  done < <(gh api "repos/$PUB/actions/runs/$RUN_ID/artifacts" --paginate --jq '.artifacts[] | "\(.id) \(.name)"')
  rm -rf "$tmp"
  [ -n "$(ls "$RUN_DIR")" ] || { echo "run $concl and no artifacts" >&2; return 1; }
}

# show_failures: tail the log of any job that did not produce its result file ($1 = file name)
show_failures() {
  local f=$1 d
  for d in "$RUN_DIR"/r-*; do
    [ -f "$d/$f" ] || { echo "--- $(basename "$d"): no $f; log tail:"; tail -25 "$d/log.txt" 2>/dev/null; }
  done
}

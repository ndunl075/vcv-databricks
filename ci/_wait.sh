#!/usr/bin/env bash
# Sourced by bench.sh / rivals.sh. trigger_and_wait WORKFLOW ARTIFACT [gh workflow run -f args...]
# -> sets RUN_ID, prints progress, downloads the artifact's summary.md to $OUTDIR/<run-id>.md and prints it.
REPO=${CI_REPO:-ndunl075/vcv-databricks}
OUTDIR=${CI_OUTDIR:-/Users/nico/Developer/vcv-data/db/ci}
trigger_and_wait() {
  local wf=$1 art=$2; shift 2
  mkdir -p "$OUTDIR"
  local t0; t0=$(date -u -v-10S +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '-10 seconds' +%Y-%m-%dT%H:%M:%SZ)
  gh workflow run "$wf" --repo "$REPO" --ref main "$@" || { echo "could not trigger $wf" >&2; return 1; }
  RUN_ID=""
  for _ in $(seq 1 15); do
    sleep 4
    RUN_ID=$(gh api "repos/$REPO/actions/workflows/$wf/runs?event=workflow_dispatch&per_page=10" \
      --jq "[.workflow_runs[] | select(.created_at >= \"$t0\")] | sort_by(.created_at) | last | .id // empty")
    [ -n "$RUN_ID" ] && break
  done
  [ -n "$RUN_ID" ] || { echo "run did not appear" >&2; return 1; }
  echo "run $RUN_ID: https://github.com/$REPO/actions/runs/$RUN_ID"
  local st concl
  while :; do
    read -r st concl < <(gh api "repos/$REPO/actions/runs/$RUN_ID" --jq '"\(.status) \(.conclusion // "-")"')
    echo "$(date +%H:%M:%S) $st $concl"
    [ "$st" = completed ] && break
    sleep 20
  done
  local aid tmp; tmp=$(mktemp -d)
  aid=$(gh api "repos/$REPO/actions/runs/$RUN_ID/artifacts" --jq ".artifacts[] | select(.name==\"$art\") | .id" | head -1)
  if [ -z "$aid" ]; then
    echo "run $concl and produced no '$art' artifact; failed jobs/annotations:" >&2
    gh api "repos/$REPO/actions/runs/$RUN_ID/jobs" --jq '.jobs[] | select(.conclusion!="success") | "\(.name): \(.conclusion) " + ([.steps[]? | select(.conclusion=="failure") | .name] | join(","))' >&2
    gh api "repos/$REPO/check-runs/$(gh api "repos/$REPO/actions/runs/$RUN_ID/jobs" --jq '.jobs[0].id')/annotations" --jq '.[].message' 2>/dev/null | head -3 >&2
    return 1
  fi
  gh api "repos/$REPO/actions/artifacts/$aid/zip" > "$tmp/a.zip" && unzip -q -o "$tmp/a.zip" -d "$tmp/x" || return 1
  cp "$(find "$tmp/x" -name summary.md | head -1)" "$OUTDIR/$RUN_ID.md"
  cp "$(find "$tmp/x" -name results.json | head -1)" "$OUTDIR/$RUN_ID.json" 2>/dev/null || true
  echo; cat "$OUTDIR/$RUN_ID.md"; echo; echo "saved: $OUTDIR/$RUN_ID.md"
  rm -rf "$tmp"
  [ "$concl" = success ]
}

#!/usr/bin/env bash
# Extract source trees from the full clone into $VCV_CI_WORK/src: bench (branch bench), coretests (tests/ of branch core),
# and one tree per ref given as NAME=REF. Prints NAME=<full sha> lines.
# usage: prep_src.sh NAME=REF ...
set -euo pipefail
W=${VCV_CI_WORK:-$HOME/vcv-ci}; S=$W/src; rm -rf "$S"; mkdir -p "$S/bench" "$S/coretests"
resolve() {
  [[ "$1" =~ ^[A-Za-z0-9._/-]+$ ]] || { echo "bad ref: $1" >&2; exit 1; }
  git rev-parse --verify --quiet "origin/$1^{commit}" || git rev-parse --verify --quiet "$1^{commit}" || { echo "cannot resolve ref: $1" >&2; exit 1; }
}
git archive "$(resolve bench)" bench | tar -x -C "$S/bench"
git archive "$(resolve core)" tests/compare.py tests/run_fixtures.py | tar -x -C "$S/coretests"
for kv in "$@"; do
  name=${kv%%=*}; ref=${kv#*=}; sha=$(resolve "$ref")
  mkdir -p "$S/$name"; git archive "$sha" | tar -x -C "$S/$name"
  echo "$name=$sha"
done

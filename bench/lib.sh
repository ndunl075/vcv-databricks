# Shared shell helpers; source me.  Needs: docker (colima).
BENCH_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_DIR=$(cd "$BENCH_DIR/.." && pwd)
DATA_ROOT=${DATA_ROOT:-${VCV_DATA:-$HOME/vcv-data/db}}
VCV_RIVALS=${VCV_RIVALS:-$HOME/vcv-rivals/db}
LOCK=${LOCK:-$(dirname "$VCV_RIVALS")/bench.lock}
export LOCK
RESULTS=${RESULTS:-$DATA_ROOT/results.jsonl}
TOOLS_IMAGE=${TOOLS_IMAGE:-vcv-bench-tools:1}

ensure_tools_image() {
  docker image inspect "$TOOLS_IMAGE" >/dev/null 2>&1 || docker build -q -t "$TOOLS_IMAGE" "$BENCH_DIR/tools" >&2
}
# tools <cmd...>: run inside the tools image. /repo = this repo (ro), /dataroot = $DATA_ROOT (rw).
tools() {
  ensure_tools_image
  docker run --rm -v "$REPO_DIR":/repo:ro -v "$DATA_ROOT":/dataroot -w /repo/bench "$TOOLS_IMAGE" "$@"
}

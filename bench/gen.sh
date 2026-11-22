#!/bin/bash
# Build kit/generator in Docker (no Rust on the host) and write the benchmark datasets.
# Usage: bench/gen.sh [name ...]   (default: dev p20a p20b f60). Skips datasets that already exist.
# Datasets live outside the repo: $DATA_ROOT/<name>/ (default ~/vcv-data/db, or set VCV_DATA).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
DATA_ROOT=${DATA_ROOT:-${VCV_DATA:-$HOME/vcv-data/db}}
BIN=$DATA_ROOT/bin
mkdir -p "$BIN" "$DATA_ROOT"
RUST_IMAGE=${RUST_IMAGE:-rust:1.88-bookworm}

if [ ! -x "$BIN/generate" ]; then
  echo "[gen] building generator in $RUST_IMAGE (native arm64)"
  docker run --rm -v "$REPO/kit/generator/generator":/src:ro -v "$BIN":/out \
    -e CARGO_TARGET_DIR=/tmp/target -e CARGO_HOME=/tmp/cargo "$RUST_IMAGE" \
    bash -c 'cp -r /src /tmp/gsrc && cd /tmp/gsrc && cargo build --release --locked 2>&1 | tail -5 && cp /tmp/target/release/generate /out/generate'
fi

# name seed events clicks   (events:clicks = 4:1, as in the published dev command)
SPECS="dev 1 1000000 250000
p20a 20 20000000 5000000
p20b 21 20000000 5000000
f60 60 60000000 15000000"
WANT=("$@"); [ ${#WANT[@]} -eq 0 ] && WANT=(dev p20a p20b f60)
echo "$SPECS" | while read -r name seed ev cl; do
  [[ " ${WANT[*]} " == *" $name "* ]] || continue
  out=$DATA_ROOT/$name
  if [ -f "$out/.done" ]; then echo "[gen] $name exists"; continue; fi
  rm -rf "$out"; mkdir -p "$out"
  echo "[gen] $name seed=$seed events=$ev clicks=$cl"
  docker run --rm -v "$BIN":/bin-gen:ro -v "$out":/out "$RUST_IMAGE" \
    /bin-gen/generate --seed "$seed" --events "$ev" --clicks "$cl" --output /out
  touch "$out/.done"
done
# sanity: dev must have the manifest and the expected tree
ls "$DATA_ROOT/dev" | head

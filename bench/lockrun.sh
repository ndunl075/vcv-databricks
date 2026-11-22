#!/bin/bash
# lockrun.sh LOCKFILE CMD...   run CMD holding an exclusive lock (macOS lockf, or Linux flock). Blocks until free.
lock=$1; shift
mkdir -p "$(dirname "$lock")"; touch "$lock"
if command -v lockf >/dev/null 2>&1; then exec lockf "$lock" "$@"; else exec flock "$lock" "$@"; fi

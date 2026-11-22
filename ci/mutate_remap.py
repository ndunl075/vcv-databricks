#!/usr/bin/env python3
"""Run bench/ood/mutate.py (branch `bench`) with the `remap` variant on a dataset that is not one of its named bases.
usage: mutate_remap.py BENCH_SRC BASE_DIR OUT_DIR EVENTS SEED WORKERS
`remap` is a keyed ID bijection and does not depend on the generator seed; the seed is only registered so the script's
base lookup works."""
import os
import sys

bench, base, out, events, seed, workers = sys.argv[1:7]
sys.path.insert(0, os.path.join(bench, "bench"))
import datasets  # noqa: E402  (bench/datasets.py; mutate.py imports the same module object)

name = os.path.basename(base.rstrip("/"))
datasets.BASES[name] = (int(seed), int(events), int(events) // 4)
sys.path.insert(0, os.path.join(bench, "bench", "ood"))
import mutate  # noqa: E402

sys.argv = ["mutate.py", "--base", os.path.abspath(base), "--variant", "remap", "--out", out, "--workers", workers]
mutate.main()

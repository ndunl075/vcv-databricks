# CI stopwatch results

Runs happen on free GitHub-hosted x86 runners of a separate public runner repo (inputs and results are opaque/encrypted;
see `ci/entry.sh`, `ci/_pub.sh`). Commands: `ci/bench.sh core 223ecb7`, `ci/rivals.sh`. Raw logs and tables:
`~/Developer/vcv-data/db/ci/<run-id>/`. Runners are 4 vCPU / 15 GB and the CPU model varies from job to job (see each table);
treat results as real-x86 ratios, not the scorer's absolute times (8 pinned vCPUs).

## bench: `core` (23d1bc9c) vs `223ecb7`, 20M events, run 44976005273

### Stopwatch: `23d1bc9c` (23d1bc9c) vs `223ecb78` (223ecb78)

Runner: **4 vCPU**, 15.3 GB RAM, `Intel(R) Xeon(R) 6975P-C`. avx2=True avx512f=True avx512bw=True. Container limits: no network, read-only root, uid 1000, `NCPU=4`, `MEM_GB=13`. Data: 20,000,000 events, seed 7. 5 timed runs each, alternating, wall time from container start to exit.

These are real-x86 **ratios** on a small shared runner, not the scorer's absolute times (8 pinned vCPUs).

| dataset | ref median s | ref best s | baseline median s | baseline best s | speedup (median) | speedup (best) | tables |
|---|---|---|---|---|---|---|---|
| stock (generator IDs) | 3.32 | 3.27 | 3.42 | 3.37 | 1.03x | 1.03x | MATCH |
| reshuffled IDs (remap) | 3.57 | 3.52 | 3.67 | 3.62 | 1.03x | 1.03x | MATCH |

`ref` vs `kit/dev-answer` on the 1M seed-1 dev set: **MATCH**

All wall times (s):
- stock ref: 3.32, 3.32, 3.27, 3.32, 3.28; baseline: 3.37, 3.43, 3.42, 3.42, 3.47
- remap ref: 3.57, 3.57, 3.62, 3.52, 3.53; baseline: 3.67, 3.67, 3.67, 3.67, 3.62

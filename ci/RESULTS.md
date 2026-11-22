# CI stopwatch results

Runs happen on free GitHub-hosted x86 runners of a separate public runner repo (opaque inputs, encrypted results; code in
`ci/entry.sh`, `ci/_pub.sh`). Commands: `ci/bench.sh core 223ecb7`, `ci/rivals.sh`. Raw logs and tables:
`~/Developer/vcv-data/db/ci/<run-id>/`. Runners are 4 vCPU / 15 GB (`NCPU=4`, `MEM_GB` = RAM minus 1.5) and the CPU model varies
from job to job (EPYC 7763 without AVX-512, EPYC 9V74 and Xeon 6975P-C with it). Read the **same-VM ratio** columns; absolute
seconds are real-x86 on a small runner, not the scorer's 8 pinned vCPUs. 20M events fit (no fallback to 10M needed).

## bench: `core` (23d1bc9c) vs `223ecb7`, 20M events, run 44976005273 (Xeon 6975P-C, AVX-512)

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

## rivals vs ours (`223ecb7`), 20M events, run 44976534300

Every rival except toM0dots (a plain baseline) ran on an AVX-512 machine; no illegal-instruction deaths, no crashes, no timeouts.
Express Lane ran on a Xeon 6975P-C with AVX-512 and was exact on both datasets.

Each rival: exact commit built from its public repo, run 3x per dataset under the scorer's flags (no network, read-only root, uid 1000), alongside our image on the same VM. Wall time = container start to exit. Real-x86 ratios on small runners, **not** the scorer's absolute times.

Runners seen: 4 vCPU AMD EPYC 7763 64-Core Processor avx512bw=False; 4 vCPU AMD EPYC 9V74 80-Core Processor avx512bw=True; 4 vCPU Intel(R) Xeon(R) 6975P-C avx512bw=True

| # | entry | stock med s | stock best | reshuffled med s | reshuffled best | exact stock | exact reshuffled | stock vs ours (same VM) | reshuffled vs ours (same VM) | runner | notes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | Zhu-E/clickstream-rollup `086249f5` | 0.92 | 0.86 | 2.02 | 2.02 | MATCH | MATCH | 0.19x | 0.41x | EPYC 9V74 80c |  |
| 2 | joch44/shopping-sessions `a3f99b72` | 1.57 | 1.57 | 3.87 | 3.72 | MATCH | MATCH | 0.40x | 0.99x | Xeon 6975P-C |  |
| 3 | nave-neptunus/databricksTrack-Joinbuilders `ed40599d` | 0.52 | 0.52 | 3.97 | 3.92 | MATCH | MATCH | 0.11x | 0.82x | EPYC 9V74 80c |  |
| 4 | **223ecb7 (ours, 223ecb78)** | 4.63 | 3.57 | 4.83 | 3.83 | reference | reference | 1.00 | 1.00 | mixed CPUs: read the ratio columns | median over all VMs |
| 5 | toM0dots/shopping-sessions-baseline `ec8f28f5` | 2.42 | 2.42 | 5.48 | 5.38 | MATCH | MATCH | 0.48x | 1.05x | EPYC 7763 64c (no AVX-512) |  |
| 6 | mwkhettab/shopping-sessions-cpp `7f59c0b8` | 0.56 | 0.52 | 6.78 | 6.73 | MATCH | MATCH | 0.12x | 1.40x | EPYC 9V74 80c |  |
| 7 | jason-scheffel/express-lane `c7d4aa44` | 0.41 | 0.41 | 7.33 | 7.29 | MATCH | MATCH | 0.11x | 1.85x | Xeon 6975P-C |  |
| 8 | micah-andelman/shopping-sessions `017c4e90` | 1.52 | 1.42 | 11.49 | 11.29 | MATCH | MATCH | 0.34x | 2.44x | EPYC 9V74 80c |  |
| 9 | cyanwingsbird/Shopping-Sessions `377fbaa5` | 33.49 | 33.29 | 34.35 | 33.76 | DIFFER | DIFFER | 9.18x | 8.81x | Xeon 6975P-C |  |
| 10 | kaedeng/shopping-sessions `3389891d` | 1.52 | 1.42 | 49.33 | 48.83 | MATCH | MATCH | 0.31x | 9.52x | EPYC 9V74 80c |  |
| 11 | Vibhor7-7/shopping-sessions `55fe2492` | 1.22 | 1.17 | 50.27 | 48.41 | MATCH | MATCH | 0.26x | 10.41x | EPYC 9V74 80c |  |

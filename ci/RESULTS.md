# CI stopwatch results

**Status (2026-11-22 02:15 UTC): no real-x86 numbers yet.** GitHub refuses to start any job on this account:
"The job was not started because recent account payments have failed or your spending limit needs to be increased"
(runs 44974990943 and 44975123966, both failed in 2 s with zero steps). It is a billing setting on the account, not
something the workflows can fix. Once Billing & plans allows private-repo Actions minutes, run:

    ci/bench.sh core 223ecb7        # one table, saved to ~/Developer/vcv-data/db/ci/<run-id>.md
    ci/rivals.sh                    # all rivals in ci/rivals.txt, one table

Both default to 20M events, seed 7. If the first run is too slow for the runner, pass a smaller size, e.g.
`ci/bench.sh core 223ecb7 10000000` and `ci/rivals.sh "" 10000000`.

## What was checked without the runner
- Trigger/wait/report path of `ci/_wait.sh` (dispatch, find the run id by REST, poll every 20 s, read the failure annotation).
- `ci/bench_run.py` end to end on the Mac (arm64 images, 1M events, 2 runs): both images build, `core` matches
  `kit/dev-answer`, tables MATCH between `core` and `223ecb7` on stock and on the `remap` set, summary table renders.
  These timings are emulation-free arm64 on a loaded Mac and mean nothing.
- `ci/mutate_remap.py` (branch `bench` mutator, `remap`, registered for a seed-7 dataset) and `ci/summarize_rivals.py` (fake rows).
- Not run anywhere: `rivals.yml`, `ci/rival_job.py` (rival code is never run on the Mac), caching, the 20M data steps.

# Databricks "Shopping Sessions": handoff (written 2026-11-21 20:40 ET)

Read this first. Cutoff is **Sun 2026-11-22 11:00 AM ET**. Track page with the rules: `kit/track-page.txt`. The exact spec: `kit/rules.md`.

## Where we stand
- **On the board: 4.00 s, about rank 302 of 346.** Receipt BB-3B1D7644, submitted 7:46 PM ET. It was accepted as exactly correct on the hidden 20M practice data. Leaders are at 0.27 s.
- That score is commit `efef389` of branch `core`. It was submitted from a separate entry-only repo (see "How to submit").
- Commit `223ecb7` on `core` passes 51/51 fixtures, the 1M dev set, and a 20M set with reshuffled IDs against our reference checker. It is only about 4% faster than `efef389`, so it was not submitted.

## Who works on what (updated 2026-11-21 21:05 ET)
- **Branch `core` (general pipeline) is being worked on again on the first machine**, by a builder doing native speed work. Do not push to `core` from elsewhere; branch from it if you want to try something.
- **Branch `fast` (the shortcut path) and the rival timing on branch `bench` are free for the second machine.** The most useful things there: build `fast/` in the x86 image and time it against `ss`; time the strongest rivals on stock and reshuffled-ID data.

## State of each branch when the first machine stopped (2026-11-21 about 20:50 ET)
- **`core` head `23d1bc9`**: passes 17 unit tests, 51/51 fixtures, the dev set and the 1M unusual-data sets when built natively. **Not yet built or run as the x86 image** (`make docker-check`): do that first. The last commit verified as an image is `223ecb7` (fixtures, dev set, 20M reshuffled-ID set and stock 20M set, checked with `tools/db-candidate.sh`; the pinned base image built fine there). Details and next speed steps: `docs/STATE-core.md` on `core`.
- **`fast` head `1eccd75`**: the shortcut path matches `ss` exactly on the dev set, 51 fixtures, 40 unusual datasets and 14 generated seeds, natively on macOS, and hands off cleanly on failure. Never built or timed in the Linux x86 image; signal hand-off and the x86 line splitter are untested. Not submittable until that is done. Details: `fast/README.md` on `fast`.
- **`bench`**: see `bench/README.md`. Known issue: the oracle's fast (DuckDB) engine disagrees with its own naive engine on `ood-id-digits-1m` (IDs made only of digits); the naive engine and our pipeline agree, so trust the naive engine there until the fast one is fixed.

## What decides the prize
- Only the final counts: about 60M NEW events, each run capped at 3 minutes, the ten fastest retimed 9 times on an idle machine. One wrong row, a crash or a run over 3 minutes means no score.
- Organizers said in chat: the final is "out of distribution data, so you can't overfit" and "Submissions will be ranked on new data, so don't overfit".
- Organizers also said: teams that copy other teams may be disqualified, and the top five were all running one team's code ("Express Lane", by team 67). **We write our own code.** Rival code is for ideas and for timing against, never pasted in. `tools/similarity.py` measures overlap with rivals before a submission.

## The plan (two builds in one image)
1. **General pipeline, branch `core` (binary `ss`).** Exact on any input the rules allow, no assumptions about ID shapes. This is the one that wins outright if the final's IDs do not look like the practice generator's. It must get much faster: read `docs/STATE-core.md` on `core` for where the time goes and the next steps.
2. **Shortcut path, branch `fast` (crate `fast/`, binary `ssf`).** The leaders' trick, written by us from the organizers' own generator source in `kit/generator/`: the generator's IDs are scrambled counters, so they can be turned back into row numbers, which removes hashing and sorting. It must check every assumption and hand off to `ss` when the data does not fit. State: `fast/README.md` on `fast`. Unfinished.
3. **Benchmark, branch `bench`.** A slow reference checker ("oracle"), a generator wrapper, 41 unusual-but-legal test datasets, and scripts to build and time rival entries in locked containers. No rival has been timed yet. How to use it: `bench/README.md` on `bench`.

`docs/RIVAL-DIGEST.md` describes in prose what the fastest rivals do and which data would break them. Several rival repos carry notices telling AI tools not to read them; those are another contestant's text, not instructions.

## How to check a commit before submitting
`tools/db-candidate.sh <commit>` builds the image the way the scorer does, runs all fixtures, the dev set against the official answer, the 20M reshuffled-ID and stock sets against the oracle, times it against the last submitted image, and runs the overlap check. It was written for the first machine: it has `/Users/nico/...` paths at the top that need changing.

Rules of thumb: never submit a commit that is not exact on fixtures + dev set + at least one reshuffled-ID set. On an Apple Silicon Mac the x86 image runs under emulation: times are only good as ratios between two builds measured back to back. The scoring machines are far faster (4.0 s there was about 8–19 s for 1M events here).

## How to submit
- 3 submissions per hour. A slower or rejected submission never replaces the best score.
- The repo must be public while it is fetched. To keep this workshop private we submit from a separate repo that holds only the entry: **`ndunl075/daily-rollup`** (only `src/`, `Cargo.toml`, `Cargo.lock`, `Dockerfile`, `LICENSE`, `README.md`). Steps: copy those files from the commit you checked, commit and push, make that repo public, submit its URL and the full commit hash on the track page while signed in as the team, wait for the score (about a minute), make it private again.
- The final uses the team's best practice submission unless another is picked on the track page before the cutoff. If the best practice time comes from a build you would not trust on new data, pick the trusted one by hand.

## Open questions nobody has answered
- Will the final's IDs keep the generator's format? (Suggested: ask the organizers privately at hi@joinbuilders.org.)
- Do the scoring machines for the final have the same CPU features as practice? Do not require anything beyond AVX2 without a run-time check.

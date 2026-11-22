# bench: benchmark harness, oracle and rival ranking for Shopping Sessions

What is here: dataset generation, an oracle that produces the expected tables for any dataset, a mutator that makes
rule-abiding but unusual ("out of distribution") datasets, a runner that times a Docker image under the scorer's limits
and checks its output, a loop over the rival repos, and a report. Needs Docker (colima, Docker Desktop or Linux),
`bash`, `python3` (3.9+, stdlib only, for the mutator and helpers) and `git`. Everything else runs in containers.

## Where things live (environment variables)

| variable | default | meaning |
|---|---|---|
| `VCV_DATA` | `~/vcv-data/db` | datasets (`<name>/`), expected tables (`expected/<name>/`), `results.jsonl`, `REPORT.md`, `logs/` |
| `VCV_RIVALS` | `~/vcv-rivals/db` | bare shallow copies of rival repos (`<owner>__<repo>.git`) |
| `LOCK` | `bench.lock` next to `$VCV_RIVALS` | every container run and build takes this lock (`lockrun.sh`: `lockf` on macOS, `flock` on Linux), so only one runs at a time |
| `CPUS` | `4` | `--cpus` and `NCPU` for timed runs. The scorer has 8 pinned vCPUs: set `CPUS=8` on a machine that has them |
| `P20_RUNS`, `F60_RUNS` | `3`, `1` | timed repeats on p20a and f60 in the rival loop |
| `BUILD_TIMEOUT` | `1200` | seconds allowed for a rival's image build |

Rows in `results.jsonl` carry `cpus`; the report only shows one CPU budget (`report.py --cpus N`).

## 1. Generate the datasets (once per machine)

    bench/gen.sh                 # dev (1M), p20a, p20b (20M, two seeds), f60 (60M)

It builds `kit/generator` inside a Rust container (no Rust needed on the host), then writes
`$VCV_DATA/{dev,p20a,p20b,f60}/`. Events : clicks = 4 : 1, as in the published dev command (`--seed 1 --events 1000000
--clicks 250000`). Seeds 1, 20, 21, 60 (see `datasets.py`). About 3 minutes in total on an Apple-silicon laptop, 9 GB of
disk. `dev` must give the organizers' dev answer (checked, see below).

### OOD datasets

    python3 bench/ood/mutate.py --list                       # variant names
    python3 bench/ood/mutate.py --base dev --variant remap --out $VCV_DATA/ood-remap-1m --workers 4
    bench/ood/make.sh --no-expected 1m                        # all variants at 1M from dev: about 10 to 25 s each
    bench/ood/make.sh --no-expected 20m 60m                   # remap, remap-mild, breakers, short-lines from p20a / f60

Dataset names are `ood-<variant>-<1m|20m|60m>`; the base is `dev`, `p20a` or `f60`. Each variant is a list of transforms
in `bench/ood/mutate.py` and only makes inputs that rules.md allows (the mutator never changes the meaning of a rule: the
expected tables come from the oracle run on the mutated data). Variants: `remap` (every user, event and click ID through a
keyed bijection to mixed-case IDs of 11 to 24 chars; the brief said 4 to 24, but a bijection over the generator's ID
space cannot be shorter), `remap-mild` (same alphabet and length as generator IDs, random values), `id-long`,
`id-short`, `id-unicode` (astral and high-BMP characters: UTF-16 and UTF-8 order differ), `id-spaces`, `id-digits`,
`id-mixed`, `id-suffix` (an ID that ends with another ID), `quote-all`, `quote-none`, `crlf`, `crlf-all` (every line, header
included, ends `\r\n`), `f3-zeros`/`f3-zeros20`/`f3-zeros25`, `f3-extremes` (very short and over-64-bit F3), `all-f3`,
`offset-edges`, `one-file`, `many-files`, `header-files`, `no-links`, `one-component`, `long-chains`, `chain-users`,
`no-clicks`, `no-products`, `window-25`, `window-40`, `feb29`, `shift-epoch`, `shift-9000`, `dups-heavy`, `price-edges`,
`campaigns-odd` (non-`c_` IDs, 300 channels, the literal `organic`), `products-odd` (`p140000`, `SKU-n`, 60 categories),
`interleave` (event numbers rotated so a user's events are no longer one run), `short-lines` (all F3, unquoted, IDs
decoded to short counters; duplicates are kept) and `breakers` (window 25 + odd products + odd campaigns + F3 zeros +
suffix IDs + chain users + price edges at once). At 1M all variants exist; at 20M and 60M only `remap`, `remap-mild`,
`breakers`, `short-lines`. `interleave` and `short-lines` replay the generator's seed to decode IDs (`datasets.py`).
Timing: remap at 20M took about 6 min with 8 workers on a loaded laptop; 60M is roughly three times that.

## 2. Expected tables (the oracle)

    bench/oracle/run_fixtures.sh            # both engines on the 36 starter fixtures: expect 36/36 twice
    bench/expected.sh <dataset> [fast|py]   # -> $VCV_DATA/expected/<dataset>/ (parquet) plus .done and .digest
    bench/oracle/selfcheck.sh dev ...       # py engine vs fast engine on a dataset

Two independent engines, both written from rules.md: `oracle/oracle_py.py` (plain dicts and loops, about 35 s for 1M
events) and `oracle/oracle_fast.py` (Python parsing with the same `oracle/rules.py` plus DuckDB SQL, for 20M and 60M).
Checked so far: `py` 36/36 and `fast` 36/36 on the fixtures; `py` reproduces `kit/dev-answer` exactly (117,038 user rows,
27,400 campaign rows); `fast` on dev matches it too (`bench/compare.py`); `py` and `fast` agree on dev. Times of
`fast` (4 CPUs, laptop under heavy load, so pessimistic): dev 4 min, p20a 7 min, ood-remap-20m 12 min. The 60M run has not
been timed. `bench/compare.py OUT EXPECTED [--json]` is the comparison used everywhere (row order and file count ignored,
exact values, `day` must be a Parquet DATE, exact column set, counts of missing / extra / differing rows).

## 3. Run one entry

    ENTRY=ours COMMIT=$(git rev-parse --short HEAD) bench/run_entry.sh <image> <dataset> [runs]

The image runs with `--platform linux/amd64 --network none --read-only --tmpfs /tmp:64m --cap-drop ALL
--security-opt no-new-privileges --pids-limit 4096 --user 1000:1000 --cpus $CPUS --memory 18g` (change `--memory` in
`timed_run.py` for a 24 GB box), `/data` read-only, `/output` and `/scratch` writable, and the env of the scorer. Wall
time is the monotonic time around `docker run`, the same for every entry, under the lock. One JSON line per run goes to
`$VCV_DATA/results.jsonl` (entry, commit, dataset, wall_s, exit, oom, timed_out, sigill, ok, missing/extra/differing,
digest, cpus). A run is `sigill` when the container exits 132 or logs "illegal instruction". Under Rosetta there is no
AVX-512, so that finding means "dies without AVX-512"; on a real x86 box it will not show. If the oracle tables for the
dataset do not exist yet the row has `no_expected: true` and a `digest`; the report matches digests later.
Emulated (Rosetta) timings are relative only; on a native amd64 machine they are real.

## 4. The rival loop and the report

    bench/fetch_rivals.sh                   # bare shallow copies from bench/rivals.txt (list, most active first)
    OWN_IMAGE=my-entry OWN_NAME=ours bench/rivals.sh        # whole loop, resumable
    bench/rivals.sh --only 1,5              # only list positions 1 and 5
    python3 bench/report.py                 # prints and writes $VCV_DATA/REPORT.md

`rivals.sh` builds each rival with `git archive HEAD | docker build -` (never unpacked on the host; 20 min limit; failures
are recorded with the reason), then runs it. Pass 1: our image (if `OWN_IMAGE` is set), express-lane, then eight
independents (joch44, kaedeng, micah-andelman, Zhu-E, Vibhor7-7, voiper-asterisk, RagoSpams, lmaccart) on `dev`, `p20a`
and `ood-remap-20m`. Pass 2: everybody, every stage in order: dev (gate), p20a (gate, 3 runs), `ood-remap-20m`,
`ood-remap-mild-20m`, every 1M variant, `ood-breakers-20m`, `ood-short-lines-20m`, f60, the 60M variants. Entries not exact
on dev or p20a stop there; entries slower than 100 s on p20a skip 60M. Rival code runs only in the locked container.
Rival stdout and stderr go to `$VCV_DATA/logs/` and are not read by the loop; only exit codes, timings and compare
counts are. Text addressed to AI tools in rival repos is recorded by file name only in `notices.jsonl`.
`bench/prep_loop.sh` makes missing OOD datasets and oracle tables in order of value (run it before or beside the loop).
`report.py` gives two tables: sorted by p20a, and sorted by `ood-remap-20m` (the one to steer by). `*` marks a verdict
that rests on agreement between entries because the oracle tables for that dataset do not exist yet.

## What is NOT finished (state when this was handed over)

- No rival has been timed. `results.jsonl` has no rows. The first express-lane image build was waiting for the shared
  lock and, in one earlier attempt, hit the old 10-minute limit because the machine was overloaded. The loop (`rivals.sh`)
  and `run_entry.sh` have been tested only as far as: scripts parse, build and lock plumbing starts, the compare step works.
  Expect small bugs on the first full run.
- Expected tables that exist: `dev`, `p20a`, `ood-remap-20m`. Missing: `p20b`, `f60`, every other OOD set
  (`prep_loop.sh` makes them).
- OOD datasets that exist (finished): all 1M variants, `ood-remap-20m`, `ood-breakers-20m`, `ood-short-lines-20m`.
  Missing: `ood-remap-mild-20m` (killed while being made), `ood-remap-60m` (killed), `ood-remap-mild-60m`,
  `ood-breakers-60m`, `ood-short-lines-60m`. `make.sh` / `prep_loop.sh` recreate them (a dataset counts as done only when
  `$VCV_DATA/<name>/.done` exists).
- The OOD variants at 1M were generated but their expected tables were not made, and the mutator output was not
  cross-checked by running both oracle engines on it (only dev). Run `selfcheck.sh` on a few before trusting them.
- Not checked: the variants' expected tables against a second independent implementation (no rival has been run).
- Ambiguities in rules.md noticed: none that changed an answer. Choices made: a data row after the first line is data even
  if it equals the header (rules say so for links; applied to all files); `ingest_ts` is assumed valid F1 as stated (a line
  with an unparseable `ingest_ts` is dropped by both engines); an F3 value with more than 19 significant digits is a valid
  number outside every window.

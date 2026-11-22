# State of the `core` pipeline (handover)

Rust, std only, no third-party crates. Entry: `src/main.rs`. Run as `ss [DATA OUT]` or with `DATA_DIR`/`OUTPUT_DIR`/`NCPU`.

## Known facts
- **efef389** scored **4.00 s** on the hidden 20M practice data and was accepted as exact.
- **223ecb7** passed 51/51 fixtures and the 1M dev set (I ran these). I did not run the 20M reshuffled-ID set against an oracle myself; check that claim before relying on it.
- Head of `core` passes: 17 unit tests, 51/51 fixtures, the dev set, and `ood_check.sh` on dev, ood-chain-users, ood-interleave, ood-dups-heavy, ood-f3-zeros25 (1M each).
- `ood-id-digits-1m` DIFFERs against the bench branch's *fast* oracle (DuckDB engine), but matches its naive `--engine py` oracle and us. Digit-only event IDs: the fast oracle is the one that is wrong (it keeps an event whose every copy is over 24 h late).
- **Not re-run on this head:** `make docker-check` (image build, amd64). The Dockerfile now pins `rust:1-alpine` by digest; the pinned image was never pulled here (slow network), so the first build with it is untested.
- No 60M run completed in Docker (the Colima VM is shared and the run was killed, once by memory pressure we think). Native Mac 60M (earlier commit) ran in 51 s, peak RSS 11 GB (including mapped input).

## Pipeline
1. Map every `*.csv` in `events/ clicks/ links/` (`input.rs`), cut into row-aligned slices (header = first line).
2. Links parsed in parallel; `LinkTable::build` (single thread, union-find on 128-bit ID hashes, person named by byte-wise smallest ID) runs on its own thread while events and clicks are parsed.
3. Parse (`csv.rs` one-pass row reader with SSE2/NEON delimiter masks, `parse.rs`, `ts.rs`): events become 64-byte `Ev` records scattered by event-id hash into shard buckets (`buckets.rs`, block lists); valid clicks go to a raw list.
4. Per event-id shard (`dedup.rs`): keep the smallest `ingest_ts` copy before validation (byte-checked on hash match), then tag survivors with their person (link lookup, prefetched) and scatter 40-byte `SEv` to person shards; clicks are tagged the same way.
5. Per person shard (`sessions.rs`): counting sort by person (small hash index), sort each person by ts, sessionize (gap > 30 min), credit latest click in 7 days (tie: smallest click_id), assists by merging each campaign's 7-day reach into intervals, `user_daily` rows and campaign-day counters.
6. Hand-written Parquet (`parquet.rs`), one `user_daily` file per worker plus one campaign file.

## Where the time goes
Machine: Apple Silicon Mac, 12 cores shared with other jobs (load 30-60), `NCPU=4`, native aarch64 (not x86), dataset p20a (20M events), one lock hold, noisy:

| binary | wall mean | user | sys | stage marks (parsed / deduped / done) |
| --- | --- | --- | --- | --- |
| 223ecb7 | 12.5 s | 11.9 | 3.8 | 4.0-6.9 / 8.3-11.9 / 10.5-14.3 |
| new1 (persons counting sort, tagged dedup table, prefetch) | 9.0 s | 10.2 | 1.8 | 3.2-3.4 / 7.0 / 8.6-9.0 |
| new2 (= head: plus NEON delimiter mask) | 9.8 s | 10.0 | 1.9 | 3.6-4.1 / 6.6-9.3 / 7.9-11.3 |

Roughly a third each: parse, dedup+tagging, persons. The persons stage about halved with the counting sort. Use ratios only. The NEON path exists so the Mac can proxy the x86 SSE2 path; the x86 path was only tested in Docker (emulated, fixtures and dev set).
Emulated amd64 Docker (20M, commit 9d4727d): 133 s wall, parse 47 s / dedup 45 s / persons 32 s, peak 1.7 GB (emulation distorts memory-heavy stages).

## In the middle of
Nothing half-finished in the tree. I had just measured the three-way comparison above and was about to choose the next change.

## Next three speed changes (expected gain order)
1. **Cut the dedup shuffle.** Parse into per-worker sequential arrays (about 48 B) and set two global bitmaps by event-id hash (seen once / seen twice, atomic OR). Only events whose "seen twice" bit is set (about 7%) plus a 24 B key for every invalid event go through the exact dedup table; the rest are tagged and scattered straight to person shards. Removes the 64 B Ev shuffle, the table and the `kept` copy for about 93% of events. Exactness is kept (bit collisions only send an event to the exact path).
2. **Parallel link build / cheap "no links" test.** `LinkTable::build` is single-threaded (about 0.4 s at 20M, 4 s at 60M on the Mac before overlap). Shard the interner by hash, union-find with atomics or per shard plus a merge; add a small Bloom filter over linked IDs so unlinked users skip the 24 B-slot table probe; shrink slots to 8 B tag + index.
3. **Parse and memory.** AVX2 delimiter masks behind a runtime check (`is_x86_feature_detected`), hash only what is needed, avoid the second `epoch_ms` for `ingest_ts` when equal to a cached value, write `Ev` with streaming stores, `MADV_POPULATE_READ` per chunk, and release each worker's buffers with `MADV_DONTNEED` before exit.

## Build, test, check
- `make build`, `make test` (unit tests), `make fixtures` (51: 36 organizer + our `tests/fixtures/X*`, written by `tests/make_fixtures.py`), `make check-dev` (dev set vs `kit/dev-answer`), `make docker-check` (image + fixtures + dev inside the image, scorer flags; needs `TMPDIR` under `$HOME` on Colima, the Makefile sets it).
- Python checker needs polars/pyarrow/duckdb: `make venv` (path `VENV`, default `~/Developer/vcv-data/venv`; Python 3.9 works, `run_fixtures.py` was patched for it).
- `tests/ood_check.sh [dataset ...]`: runs the binary on bench-branch datasets under `~/Developer/vcv-data/db` and compares with its oracle (expected tables cached in `db/expected-core`). `ood-no-links` / `ood-no-products` make the fast oracle fail; both match the naive oracle and us.
- Timing: take `lockf ~/Developer/vcv-rivals/bench.lock`, compare binaries back to back on the same data.

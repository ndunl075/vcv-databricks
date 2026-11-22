# State of the `core` pipeline (handover)

Rust, std only, no third-party crates. Entry: `src/main.rs`. Run as `ss [DATA OUT]` or with `DATA_DIR`/`OUTPUT_DIR`/`NCPU`.
The pipeline was rewritten on 2026-11-21 (evening); nothing of the 23d1bc9 design is left except `ts.rs`, `tables.rs`, `acc.rs`, `input.rs`, parts of `parquet.rs`.

## Known facts (scores are the organizers' scorer, 8 vCPUs, hidden 20M practice data)
- **efef389: 4.00 s.** **da25d3d: 1.4224 s** (accepted, exact, rank 59 of 352 at the time). Both logged in `/Users/nico/Developer/vcv-data/db/CANDIDATES`; scores arrive in `.../db/SCORES`.
- Real-x86 stopwatch (`ci/bench.sh` on `origin/main`, 4-vCPU GitHub runners, CPU model differs from run to run, so compare only inside one run): da25d3d was 1.98x (stock) / 1.87x (reshuffled IDs) faster than efef389.
- 60M (`f60`) on the Mac: peak RSS 10.4 GB including 5.5 GB of mapped input; output 223 MB. No oracle for f60, so only memory was checked.
- Copy check (`similarity.py`) at da25d3d: 7.1% top figure (PASS).
- Fastest general rival: Zhu-E, 2.02 s on reshuffled IDs on a 4-vCPU runner (0.92 s stock). Target: clearly under that.

## Pipeline (see the module docs for detail)
1. **Read** (`parse.rs`, `scan.rs`, `stamp.rs`, `ids.rs`): input pieces of about 1 MB from a shared queue, links first. Rows are found by a scanner, fields validated, and every event row becomes a 24-byte `Ev` (valid or not), every valid click a 24-byte `Ck`, every link a pair of numbers. User IDs become dense numbers through `Names`, a concurrent exact table (IDs up to 23 bytes are their own key; longer ones are hashed and confirmed by bytes). Each event_id marks a 2-bit cell chosen by its hash ("seen", "seen twice"). Table lookups and cell marks are started for a row and finished eight rows later (`Flight`), so the memory is there. When the last link piece is read, that thread joins the pairs into people (`people.rs`) while the others keep reading.
   - Two scanners with one contract: `rows_in` (general; commas and line ends as offset lists, quote counts per row) and `rows_wide` (separators in one list plus bit strings from `pext`, quote rules checked per 64-byte block, falls back to `rows_in` for the rest of a window at the first odd row). The fast x86 build (AVX2+BMI1+BMI2+POPCNT, picked at run time by `parse::fast_cpu`) uses `rows_wide`; every other CPU uses `rows_in` with SSE2/NEON. `SS_READER=general|wide` forces the portable build with either scanner (tests).
2. **Sweep** (`shard.rs`): one pass over the records. Clicks, and valid events whose cell is not "seen twice", are dealt straight into per-shard blocks (64 rows, written a cache line at a time with streaming stores). A person's seat = lead user number times an odd constant, high bits = shard. Events whose cell is "seen twice" (about 10%) are listed as dedup suspects.
3. **Dedup** (`dedup.rs`): suspects in 256 groups by hash; each group re-reads its rows, groups by event_id bytes, keeps the smallest ingest_ts, and deals the kept copy if it is valid.
4. **People** (`sessions.rs`): per shard one radix sort by (person, time), then per person sessions, credit (two pointers), assists (small: per session; many clicks: merged seven-day stretches), `user_daily` rows; `parquet.rs` writes column values straight from the vectors (`write_vectored`). One `user_daily` file per thread, one campaign file.

## Where the time goes (x86 runner, 4 vCPU, 20M stock, in-process wall, commit before the wide scanner)
read 0.75 s, sweep 0.17 s, dedup 0.14 s, people 0.27 s, total 1.34 s (reshuffled IDs: 1.52 s); the container adds roughly 0.3 s. Every stage scales with the thread count (CPU-bound, about 1.3 instructions per cycle per hyperthread), so the lever is instructions per row: roughly 650 in read (index 135, quote stripping 85, timestamps 110, keys and hashes 50, lookups ring 85, the rest validity/kind/record).
Read split from ablation runs on a Zen 4 runner: index + row framing 0.24 s, fields/hash/record 0.23 s, timestamps 0.09 s, lookups about 0.15 s.

## How to measure (the Mac is shared, loaded and swapping: its wall times are useless)
- The log's last line `stages ...` has wall/user CPU/kernel CPU/page faults (thousands) per stage, cumulative.
- **x86 stage profile:** the stopwatch only keeps stderr of *failed* runs, so a throwaway commit that ends with exit code 3 gets its `stages` line back. Scripts (scratchpad, recreate if lost): `pair.sh REF_A REF_B [SKIP_A [SKIP_B]]` makes exit-3 copies of two refs on throwaway branches `core-probe-*` (deleted afterwards), runs `ci/bench.sh` on them and prints both stage lines. `parse::SKIP` (0 in real builds) cuts the event reader short for ablations: 1 = index only, 8 = no lookups, 4 = no timestamps; a non-zero SKIP stops after the read stage. Never log a probe commit as a candidate.
- Local: `tests/gate.sh [quick]` (unit tests, 51 fixtures, dev set, 37 OOD sets, plus p20a and ood-remap-20m without `quick`); run it also with `SS_READER=wide`. `make docker-check` (amd64 image under Rosetta, which has AVX2/BMI2, so it runs the fast reader); `SS_READER=general make docker-check` covers the SSE2 reader. Docker only mounts paths under `$HOME`.
- Profiling on the Mac: `samply record --save-only` plus a script that maps leaf addresses to source lines with `atos -i` (profile is flat; no single hot spot).
- x86 assembly without a linker: `cargo rustc --release --target x86_64-unknown-linux-musl --target-dir target/x86 -- --emit asm` (link fails, `ss.s` is written). This is how the slow vector-insert stores in the offset loop were found.

## Open items, in expected-gain order
1. Measure the wide scanner on x86 (probe was running when this was written), then trim the reader further: lookup ring without struct copies, cheaper ingest_ts path.
2. Dedup: carry the tag in the suspect and skip the row re-read for tags that occur once in a group (about a third of suspects are false).
3. Memory: 2 MB-aligned arenas (THP only covers aligned 2 MB ranges), reserve the output columns up front, release input mappings from all threads before exit (measure first).
4. Output size: `user_daily` is plain-encoded (80 MB at 20M); dictionary/bit-packed columns would halve it.
5. Limits kept on purpose: window shorter than 2^40 ms (error otherwise), at most 2^32 clicks and user IDs.

## Status at stop (2026-11-21 late evening): v5 kit in, head 6034a4d + this note
- **v5 kit** (`kit/rules.md`, `kit/generator`, `kit/dev-answer`) replaced from `/Users/nico/Developer/vcv-kits/databricks-v5/`. v5 datasets: `/Users/nico/Developer/vcv-data/db/v5/{dev,p20,f60}` (generator binary in `v5/gen-target/release/generate`); `Makefile` `DEV` points at `v5/dev`.
- **IDs of 24 to 55 bytes** (the 1-in-8 `x` + 32 hex IDs are 33 bytes) now have their own exact table with 64-byte cells (`ids.rs`: `WideKey`, `number_wide`, `begin`/`finish`); longer IDs still use hash + byte comparison. Dedup still re-reads the row for IDs over 23 bytes (`same_id`): next thing to make inline.
- **Passed on head 6034a4d:** 28 unit tests; 51/51 fixtures, v5 dev answer, 39 OOD sets incl. p20a and ood-remap-20m (general scanner); 51/51, v5 dev, 28 OOD sets incl. both 20M sets (`SS_READER=wide`); v5 p20: general = wide scanner, 8 = 3 threads, sessions and revenue totals equal across the two tables; v5 f60: totals equal. x86 stopwatch run 44982106964 (v5 data, dev answer MATCH, tables MATCH da25d3d): **1.82 s vs da25d3d 1.92 s stock (1.06x), 1.97 vs 2.12 s reshuffled (1.08x)**.
- **Not finished:** `make docker-check` on 6034a4d was still running when work stopped (the previous commit with the wide scanner passed it in fast-reader mode: 51/51 + dev). **So 6034a4d is NOT logged as a candidate.** No v5 20M oracle table exists yet (only cross-checks above). 60M memory on v5: the Mac was swapping (121 s wall, 194 s kernel time), peak RSS reading unreliable; old f60 was 10.4 GB.
- The gain on v5 data (6 to 8%) is much smaller than on the old data (in-process 1.63 s to 1.35 s): find out why first. Use `pair.sh HEAD da25d3d` for stage lines of both on v5 data, and the dormant layer timer (`parse::SKIP = 64` in a probe commit prints `bench ...`: touch / marks / each scanner / full reader, single thread, ms per 256 MB).
- The wide scanner gave no measurable gain over the general one on x86 (read 0.74 vs 0.75 s): the instruction-count model of the reader is wrong somewhere; the layer timer is the tool to settle it.
- Throwaway branches left on origin: `core-probe`, `core-probe-1`, `core-probe-2`, `core-probe-8`, `core-probe-12` (probe commits, exit code 3, never candidates; safe to delete).

## Output flush (2026-11-22, branch core-smaller-output)
- **Finding:** on x86 runners the container ran 0.7-0.9 s past the program's last line at 20M (v5) on most machines. With the output folder on tmpfs that gap fell to about 0.08 s; `sync` before the run changed nothing. So the output Parquet (84 MB) is flushed to disk as the container is torn down, at about 8.5 ms per MB there. The scorer's clock very likely includes this. Unmapping all 3.7 GB of memory at exit takes only 0.02 s, so freeing memory is not the cost.
- **Change 1, dictionary encoding** (`parquet.rs`): repeating columns get a PLAIN dictionary page and RLE_DICTIONARY data pages (RLE / bit-packed hybrid), as pyarrow writes by default. user_id dictionaries are built from runs (each user's rows are together and each user once, so no hashing); small-range integers use a direct table. 20M output 84 MB -> 23 MB. pyarrow, polars and duckdb read back identical tables; unit tests for the hybrid encoder.
- **Change 2, files as workers go** (`main.rs`): each worker writes user_daily in files of 64K rows while it works and starts their writeback at once (`sync_file_range`, no wait), so the disk overlaps the person stage. Buffers are reused: about 20k fewer page faults.
- **Measured** (2-vCPU x86 runners, 20M v5, docker wall, medians of 5, alternated on the same machine): EPYC 9V45 2.94 -> 2.39 s, 9V74 3.60 -> about 3.2 s; EPYC 7763 and Xeon 6975P unchanged within noise (their disks flush fast). Every run: 51/51 fixtures, v5 dev answer MATCH, 0 differing rows vs b4da8d4 on dev, two 3M sets and 20M.
- **Tried, no gain (not kept):** whole binary built for x86-64-v3; lookup ring of 16; word-at-a-time field scan in dedup (dedup is bound by row misses, not the scan). Probes on x86: skipping lookups saves about 12% of read, skipping timestamps about 10%, skipping all field work about 55%.
- x86 profile (perf, line tables, 9V45): read_fast 47%, person stage 13% plus its radix sort 7%, sweep 8%, dedup 7%. Hottest line: the user table cell load in `ids.rs` `number` (about 6%, a cache miss despite the prefetch).

## Locked marks and what followed (2026-11-22, branches core-bgflush .. core-tagbins)

Scores: core-smaller-output 1.1594, core-tagparts 1.1542, core-tagbins 1.1228 (practice board; the
board itself says machines differ by up to 20%).

- Intel profile (Xeon 6975P / 8573C): the hottest spot in `read_fast` was `Cells::mark`, a
  `fetch_or` whose old value was used, compiled to a `lock cmpxchg` loop. A racy unlocked build
  (never shipped) was 9% faster on the 8573C, 3% on the 6975P, 1-2% on AMD.
- core-bgflush: `start_writeback` runs on a helper thread (it blocked workers when the disk was
  busy). Neutral on a quiet disk.
- core-tagparts: readers file event tags into 256 ranges by top tag byte; after reading, each
  worker marks whole ranges with plain writes (a range of cells is under 1 MB). Read CPU on the
  8573C 4.78 -> 3.85 s; 8573C wall -8%; AMD 0-3% slower.
- core-tagbins: the tag lists are fixed bins in one huge-page region (plus spill lists), not
  growing vectors. 1-3.5% faster on every runner.

Tried, no gain (branches kept on the test repo):
- exp-bts: one `lock bts` after a plain read. About 1% on Intel; superseded by core-tagparts.
- core-flagsus: flag suspects in the record during the mark pass so sweep skips the cells.
  1-5% slower everywhere: the record writes and 8 more bytes per event cost more than the reads.
- core-sort: radix on person bits only, then insertion sort per person by time. Neutral to 3%
  slower; the 4-pass radix on ~80 KB shards runs from L2 and beats unsorted insertion sorts.

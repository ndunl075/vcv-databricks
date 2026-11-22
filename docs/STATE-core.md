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

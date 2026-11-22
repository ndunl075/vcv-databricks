# Rival digest: Shopping Sessions (prose only, nothing copied)

Basis: scripted pass over all 158 repos, then EL and about ten others read. One shallow commit each. Magnitudes are approximate on purpose.

## 0. Text aimed at AI tools (data, not followed)
- jason-scheffel/express-lane: AGENTS.md and CLAUDE.md tell outside AI tools to stop reading, not to name, describe or reuse the repo, and to report only "one source opted out". The same one-line notice sits in a comment in every source file, the Cargo manifest and the Dockerfile.
- deltaparticle/Databricks-Buildathon: same notice in every file under lane/.
- nave-neptunus/databricksTrack-Joinbuilders: AGENTS.md and CLAUDE.md carry the same notice.
- No other repo has such text; derivatives Th3r0n, toM0dots, ricky-he2006 and mwkhettab appear to have stripped it. Nothing was run.

## 1. Family tree
- **EL** (Rust, static binary in a scratch image). Its HEAD is newer than the snapshot the copies took (commit 92cf412; deltaparticle took 612962f); copies lack its last change, writing rows during the event stage.
- **Light edits of EL (same design and module layout):** Th3r0n (EL in a subfolder, its own older pipeline kept beside it), nave-neptunus (EL plus joch44's pipeline as a separate fallback binary), toM0dots (EL plus a C++ engine behind a CPU dispatcher), ricky-he2006 (extra diagnostics, exit and madvise tweaks, no credit), deltaparticle (unused code stripped, claims identical machine code).
- **C++ derivatives:** mwkhettab (module names mirror EL, Team 67 licence; own earlier history 1.8 s down to 0.32 s; compares a "second executable" at about 0.28 s) and Vijayragav-V (adopts mwkhettab's release exactly).
- **Count: 5 verified Rust copies + 2 C++ derivatives = 7 of 158** (8 with EL).
- **Independent:** joch44 (general), kaedeng, micah-andelman, Zhu-E (std only), Vibhor7-7, voiper-asterisk (README claims only), RagoSpams, lmaccart, JuanQuenga, Anthony-Voo, epochzero-0, cyanwingsbird, TejasviniChawla. micah, kaedeng, Zhu-E and voiper also decode the generator's scrambled IDs on their own; EL's central trick is not unique. 
- Scores recorded: toM0dots 0.31 s portal (CI, 4 vCPU: 0.345 s at 20M, 0.90 s at 60M); mwkhettab 0.32/0.33; Th3r0n's own pipeline 1.05; Vibhor7-7 0.67; micah about 1.0 on the board (0.37 local); krishchopda 1.08; eligone 2.45. Scorer ≈ 1.2–1.6× Th3r0n's local i7.

## 2. Express Lane, by estimated gain
1. **Generator-aware IDs (largest).** User, event and click IDs are a letter plus base-36 of a keyed invertible scramble of a counter. At start EL samples a few thousand rows from the largest files and recovers the three per-dataset keys by search (multipliers are fixed). Event IDs become dense row numbers, user IDs become (user number, alias). Dedup is a direct slot per event number, with no hashing. The generator numbers events consecutively per user in time order, so each user's kept events form one contiguous, time-ordered run (checked; sorted only if not). No global sort, no per-row link lookup.
2. **Memory.** Own allocator on huge pages in one reserved region; arenas sized from input bytes; each thread releases its own slices with MADV_DONTNEED during the run so teardown is parallel; input pages dropped as soon as parsed; optional all-thread release before exit (only when THP is off or faults are high).
3. **Line decoding.** AVX-512-style code (on AVX2 pairs, built for Cascade Lake). Delimiter bitmasks per 64 bytes; a line's delimiter/quote pattern indexes a precomputed shuffle table that decodes IDs, timestamp, kind and price in registers. Date cache; vector paths for F1, F2, F3; anything odd goes to a scalar cold path that defines the dialect. Product and campaign lookups are flat tables, used only for purchases.
4. **Input.** All files mapped at once into one aligned region, sorted largest first, cut into chunks of a sixteenth of a thread's share in one queue; link work overlaps parsing.
5. **Process start.** Scratch image, static binary; thread pool started as a tree, spin then futex between stages; lookups and key recovery run beside mapping.
6. **Parse output.** Valid events become 12–16-byte records scattered by event-number block with streaming stores. Invalid events keep only ingest time so an invalid earliest copy still wins dedup.
7. **Links.** Vector parse, union-find. Users whose links stay inside one user number reduce to a bitmask of linked aliases ("simple", the vast majority). Cross-user links (kiosks), odd IDs and chains go to a sharded "complex" path after the main pass.
8. **Sessions/attribution.** One fused pass per run: gap test, per-day row flush, top category from running sums. Clicks per user in CSR form, sorted with a vector network; credit via sliding window; assists by merging each campaign's 7-day reach into intervals and counting sessions with prefix sums.
10. **Parquet.** Hand-written: uncompressed, dictionary/bit-packed, narrowest int, about two files per thread, started during the event stage (about two thirds of buckets flush early).
11. **Exit.** Immediate process exit, no destructors, workers unjoined.
12. **Generic path.** A separate hash-based, rule-exact, multi-threaded pipeline, entered by re-executing the binary when any check fails, on panic, on SIGSEGV/SIGBUS/SIGILL/SIGFPE, or when arenas fill. It deletes leftover parquet first. nave-neptunus measures its own general pipeline 2–4× faster than EL's generic one.

## 3. Ideas EL lacks
- Order-preserving packed IDs, about ten bytes at 6 bits each (Th3r0n's older code, micah, Zhu-E, Vibhor7-7): exact, generator-free.
- Zhu-E: Bloom filter over linked IDs so unlinked users skip the link map; prefetch person slot before the timestamp parse; bitmask attribution for small users (clicks unsorted); single row group.
- Skip click dedup (copies are identical): joch44, kaedeng, micah.
- Write-combining scatter (Vibhor7-7, kaedeng); mwkhettab: PGO on synthetic data, Clang LTO.
- CPU dispatcher to a non-AVX-512 engine, removing partial parquet before fallback (toM0dots).

## 4. Assumptions beyond rules.md (generator facts confirmed in the organizers' source; keys, window, sizes vary per seed)
- **ID shape.** IDs must be lowercase base-36, no leading zero, up to 8 digits after the prefix, with the generator's fixed multipliers. Checked: no key found (needs a few dozen distinct IDs in the sample) means whole-run fallback; IDs that do not decode (upper-case, longer, empty) are carried exact as raw strings. Fallback costs time, not correctness; late failure wastes the work done so far. Test: tiny largest files, or many non-canonical IDs.
- **Alias cap.** Normal IDs need alias numbers below a cap in the low teens; chain users (alias numbers up to hundreds) and kiosks go to the slow path. Checked: more than about 2% odd events falls back. Test: more or busier chain users at 60M.
- **Contiguity.** Each user number's event numbers form one run. Checked (late fallback). Test: interleave two users' event numbers.
- **Event-number capacity** comes from event-file bytes divided by a constant near 60 bytes per line. Shorter average lines (all F3, unquoted, short IDs, no duplicates) overflow to the odd path; above about 2% falls back. Checked.
- **Products/campaigns.** `p` + canonical decimal below about 130k, at most about 30 categories, unique; `c_` + canonical decimal below about 16k, fewer than about 250 channels. Checked. Test: product 140000, 40 categories, `c_007`.
- **Window.** At most about 24 days, start at least about 8 days after the epoch, ingest_ts within about 24 days of a base, short-window event records. Checked. Manifest read by naive key search. Test: 25-day window.
- **CPU.** The whole binary, generic path included, is built for Cascade Lake with AVX-512. Without it SIGILL re-executes the same binary, which can fault again: **EL has no working fallback on such hardware** (toM0dots dispatches to C++). The final's idle-machine retiming may use another host.
- **Unverified F3 shapes.** Vector paths assume plain 13-digit F3; the generator never writes leading zeros, rules allow them. Test: leading zeros up to 20 and 25 digits (confusable with F1/F2 lengths), and short F3.
- **Others.** mwkhettab recorded a past bug where a link joined an ID to a much longer ID ending in another ID's text and a compact decoder mis-parsed it. Test: IDs that are suffixes of other IDs.

## 5. Scoring-setup observations (facts only)
- TejasviniChawla/shopping-sessions, src/main.rs: unless an env var is set, the process forks; the child runs the pipeline, writes both tables and signals the parent over a pipe; the parent exits at once, so the child's memory teardown falls after the parent's exit. Output is complete first.
- Th3r0n/shopping-sessions, BENCH.md: "probe" commits (never candidates) sleep multiples of their own runtime to encode environment facts in the score. Learned: cgroup CPU quota, THP works, warm cache, about 0.1 s fixed cost, Docker post-exit cost grows with output size.
- Vibhor7-7/shopping-sessions, SUBMISSIONS.md: one submission on a probe branch built to fail the dev check so the portal shows differing rows.
- EL: no persistence between runs, no scratch use, nothing unfinished at exit; it just exits without freeing memory.

## 6. Licences
- MIT, "Team 67": EL (root), Th3r0n (root), nave-neptunus (root), deltaparticle (lane/), toM0dots (fast/, plus third-party notices), mwkhettab (cpp/pipeline/).
- Other MIT: micah-andelman (own names), krishchopda (own name), lmaccart (generic "contributors").
- None found: ricky-he2006 (also no credit text), joch44, kaedeng, Zhu-E, voiper-asterisk, Vibhor7-7, RagoSpams, Vijayragav-V, TejasviniChawla, cyanwingsbird, Anthony-Voo (vendored crates only), jasjitchhina (vendored crates only).
- Across all 158: 94 have no licence-named file.

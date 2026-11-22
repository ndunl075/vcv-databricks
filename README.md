# Shopping Sessions: our pipeline

Rust, standard library only. **Third-party crates: none** (`Cargo.lock` lists only this package). The build downloads
nothing but the `rust:1-alpine` base image from Docker Hub.

Everything is written from `kit/rules.md`; nothing is copied from other teams.

## Design (phase 1)

1. Map every input file; cut them into line-aligned chunks; all cores parse (hand-written CSV dialect, timestamp and price parsers).
2. Links: union-find over exact ID bytes (128-bit hash identity); the person is named by the byte-wise smallest ID.
3. Events: 64-byte records scattered into event-id shards; per shard, dedup keeps the smallest `ingest_ts` (byte-verified `event_id`),
   *before* validation. Survivors are tagged with their person and scattered into person shards.
4. Per person shard: sort by (person, ts), sessionize, credit clicks, assists by merging each campaign's 7-day reach into intervals,
   aggregate `user_daily` rows and campaign-day counters.
5. Hand-written Parquet (PLAIN, uncompressed, `day` is a DATE), one file per worker thread plus one small campaign file.

Identity of a user ID inside the process is a 128-bit hash of its bytes (collision odds about 1e-23 at 100M IDs);
`event_id` dedup is byte-exact on hash match. No assumption about ID shape, length, alphabet, file count or size,
window length, row order or generator.

## Tests

- `make test`: Rust unit tests (timestamp forms and every rejection, price, dialect splitter, chunking, link union).
- `make fixtures`: the 36 organizer fixtures plus our own `tests/fixtures/X*` (written by `tests/make_fixtures.py`, expected rows derived by hand).
- `make check-dev`: the 1M dev set against `kit/dev-answer`.
- `make docker-check`: builds the root Dockerfile for linux/amd64 and runs fixtures plus the dev set in the image under the scorer's flags.
- `tests/ood_check.sh`: differential check against the bench branch's oracle on its out-of-distribution datasets.

Checker scripts in `tests/` (`run_fixtures.py`, `compare.py`, `docker_run.sh`) are the organizers' starter scripts.

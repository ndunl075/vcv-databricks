"""The rules of kit/rules.md again, for 20M-60M events: Python parses and validates each row with the same
functions as oracle_py (rules.py), DuckDB does the set logic in SQL, one statement per rule, in rule order.
Cross-checked against oracle_py on the fixtures, dev and every small variant (bench/oracle/selfcheck.sh)."""
import multiprocessing as mp
import os
import shutil
import sys

import duckdb
import pyarrow as pa
import pyarrow.parquet as pq

from oracle_py import person_names
from rules import (read_lines, csv_files, parse_price, parse_ts, read_manifest, read_small_table, split_fields)

CHUNK = 400_000
EVENT_TYPES = {"view", "cart", "purchase"}


def stage_events(args):
    path, out, ws, we = args
    cols = ([], [], [], [], [], [], [], [])
    writer = None
    schema = pa.schema([("event_id", pa.string()), ("user_id", pa.string()), ("ingest_ms", pa.int64()),
                        ("ts_ms", pa.int64()), ("event_type", pa.string()), ("product_id", pa.string()),
                        ("price", pa.int64())])
    n = 0

    def flush():
        nonlocal writer
        if cols[0]:
            t = pa.table({name: pa.array(col, type=schema.field(name).type) for name, col in zip(schema.names, cols)})
            if writer is None:
                writer = pq.ParquetWriter(out, schema)
            writer.write_table(t)
            for c in cols:
                c.clear()

    for line in read_lines(path):
        f = split_fields(line, 7)
        if f is None or f[0] == "":
            continue
        ingest = parse_ts(f[3])
        if ingest is None:
            continue
        ts = parse_ts(f[2])
        if ts is not None and not (ws <= ts < we):
            ts = None  # "valid and in the window" is the only thing ever asked of ts
        price = parse_price(f[6]) if f[4] == "purchase" else None
        cols[0].append(f[0]); cols[1].append(f[1]); cols[2].append(ingest); cols[3].append(ts)
        cols[4].append(f[4]); cols[5].append(f[5]); cols[6].append(price)
        n += 1
        if n % CHUNK == 0:
            flush()
    flush()
    if writer is None:  # header-only file: still need a file with the schema
        writer = pq.ParquetWriter(out, schema)
    writer.close()
    return out


def stage_clicks(args):
    path, out, ws, we = args
    lo = ws - 7 * 86_400_000
    schema = pa.schema([("click_id", pa.string()), ("user_id", pa.string()), ("campaign_id", pa.string()),
                        ("ts_ms", pa.int64())])
    cols = ([], [], [], [])
    for line in read_lines(path):
        f = split_fields(line, 4)
        if f is None or f[0] == "":
            continue
        ts = parse_ts(f[3])
        if ts is not None and not (lo <= ts < we):
            ts = None
        for c, v in zip(cols, (f[0], f[1], f[2], ts)):
            c.append(v)
    pq.write_table(pa.table({n: pa.array(c, type=schema.field(n).type) for n, c in zip(schema.names, cols)}), out)
    return out


def run(data, out, work=None):
    ws, we = read_manifest(data)
    work = work or os.path.join(out, "_work")
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    jobs_e = [(p, os.path.join(work, f"e{i}.parquet"), ws, we) for i, p in enumerate(csv_files(os.path.join(data, "events")))]
    jobs_c = [(p, os.path.join(work, f"c{i}.parquet"), ws, we) for i, p in enumerate(csv_files(os.path.join(data, "clicks")))]
    with mp.get_context("fork").Pool(min(10, os.cpu_count() or 1)) as pool:
        # biggest first so the pool stays busy
        jobs_e.sort(key=lambda j: -os.path.getsize(j[0]))
        jobs_c.sort(key=lambda j: -os.path.getsize(j[0]))
        ev_files = pool.map(stage_events, jobs_e, chunksize=1)
        ck_files = pool.map(stage_clicks, jobs_c, chunksize=1)
    names = person_names(data)
    pq.write_table(pa.table({"id": list(names.keys()), "name": list(names.values())}), os.path.join(work, "persons.parquet"))
    products = read_small_table(os.path.join(data, "products.csv"))
    campaigns = read_small_table(os.path.join(data, "campaigns.csv"))
    pq.write_table(pa.table({"product_id": list(products), "category": list(products.values())}),
                   os.path.join(work, "products.parquet"))
    pq.write_table(pa.table({"campaign_id": list(campaigns), "channel": list(campaigns.values())}),
                   os.path.join(work, "campaigns.parquet"))

    con = duckdb.connect(os.path.join(work, "oracle.duckdb"))
    con.execute(f"SET temp_directory='{work}/spill'; SET preserve_insertion_order=false;")
    lst = lambda fs: "[" + ", ".join("'" + f.replace("'", "''") + "'" for f in fs) + "]"
    con.execute(f"CREATE VIEW raw_events AS SELECT * FROM read_parquet({lst(ev_files)})")
    con.execute(f"CREATE VIEW raw_clicks AS SELECT * FROM read_parquet({lst(ck_files)})")
    P = lambda n: f"read_parquet('{work}/{n}.parquet')"
    sql = f"""
    CREATE TABLE persons AS SELECT id, name FROM {P('persons')};
    CREATE TABLE products AS SELECT * FROM {P('products')};
    CREATE TABLE campaigns AS SELECT * FROM {P('campaigns')};

    -- Events 2: dedup on event_id, keep the copy with the smallest ingest_ts (ties are identical)
    CREATE TABLE ev_kept AS
      SELECT event_id, arg_min(struct_pack(user_id, ingest_ms, ts_ms, event_type, product_id, price), ingest_ms) AS r
      FROM raw_events GROUP BY event_id;
    -- Events 3 + 4: validate the kept copy, drop late events
    CREATE TABLE ev AS
      SELECT COALESCE(p.name, k.r.user_id) AS person, k.r.ts_ms AS ts, k.r.event_type AS event_type,
             k.r.product_id AS product_id, k.r.price AS price
      FROM ev_kept k LEFT JOIN persons p ON p.id = k.r.user_id
      WHERE k.r.user_id <> '' AND k.r.ts_ms IS NOT NULL AND k.r.event_type IN ('view','cart','purchase')
        AND k.r.product_id <> '' AND (k.r.event_type <> 'purchase' OR k.r.price IS NOT NULL)
        AND k.r.ingest_ms - k.r.ts_ms <= {24 * 3600 * 1000};
    DROP TABLE ev_kept;
    -- Events 5: sessionize per person; a new session when the gap to the previous event is over 30 min
    CREATE TABLE ev_s AS
      SELECT *, sum(brk) OVER (PARTITION BY person ORDER BY ts ROWS UNBOUNDED PRECEDING) AS sid
      FROM (SELECT *, CASE WHEN lag(ts) OVER w IS NULL OR ts - lag(ts) OVER w > 1800000 THEN 1 ELSE 0 END AS brk
            FROM ev WINDOW w AS (PARTITION BY person ORDER BY ts));
    DROP TABLE ev;
    -- Events 6: one day per session (UTC date of the session start)
    CREATE TABLE sessions AS
      SELECT person, sid, min(ts) AS start_ts, CAST(epoch_ms(min(ts)) AS DATE) AS day, count(*) AS events,
             coalesce(sum(price), 0) AS revenue, count(price) > 0 AS purchasing
      FROM ev_s GROUP BY person, sid;
    -- Events 7: categories; top category per (person, day) over the purchases of that day's sessions
    CREATE TABLE cat_sums AS
      SELECT s.person, s.day, coalesce(pr.category, 'unknown') AS category, sum(e.price) AS total
      FROM ev_s e JOIN sessions s USING (person, sid) LEFT JOIN products pr ON pr.product_id = e.product_id
      WHERE e.event_type = 'purchase' GROUP BY 1, 2, 3;
    DROP TABLE ev_s;
    CREATE TABLE top_cat AS
      SELECT person, day, category FROM (
        SELECT *, row_number() OVER (PARTITION BY person, day ORDER BY total DESC, category ASC) AS rn FROM cat_sums)
      WHERE rn = 1;

    -- Clicks 2 + 3: dedup (copies identical), validate, map to person
    CREATE TABLE ck AS
      SELECT COALESCE(p.name, k.r.user_id) AS person, k.click_id, k.r.ts_ms AS ts, k.r.campaign_id AS campaign_id
      FROM (SELECT click_id, any_value(struct_pack(user_id, ts_ms, campaign_id)) AS r FROM raw_clicks GROUP BY click_id) k
      LEFT JOIN persons p ON p.id = k.r.user_id
      WHERE k.r.user_id <> '' AND k.r.ts_ms IS NOT NULL AND k.r.campaign_id IN (SELECT campaign_id FROM campaigns);
    -- Clicks 4: credit. Latest click.ts <= session start (smallest click_id on a tie), at most 7 days before.
    CREATE TABLE ck_best AS
      SELECT person, ts, arg_min(campaign_id, click_id) AS campaign_id FROM ck GROUP BY person, ts;
    CREATE TABLE credited AS
      SELECT s.person, s.sid, s.day, s.revenue, s.purchasing,
             CASE WHEN c.ts IS NOT NULL AND s.start_ts - c.ts <= 604800000 THEN c.campaign_id ELSE 'organic' END AS campaign_id
      FROM sessions s ASOF LEFT JOIN ck_best c ON s.person = c.person AND s.start_ts >= c.ts;
    -- Clicks 7: assists, once per campaign and session
    CREATE TABLE ck_distinct AS SELECT DISTINCT person, campaign_id, ts FROM ck;
    CREATE TABLE assists AS
      SELECT day, campaign_id, count(*) AS assisted FROM (
        SELECT DISTINCT s.person, s.sid, s.day, c.campaign_id
        FROM sessions s JOIN ck_distinct c ON c.person = s.person AND c.ts <= s.start_ts AND c.ts >= s.start_ts - 604800000)
      GROUP BY day, campaign_id;
    """
    con.execute(sql)

    os.makedirs(os.path.join(out, "user_daily"), exist_ok=True)
    os.makedirs(os.path.join(out, "campaign_daily"), exist_ok=True)
    con.execute(f"""COPY (
      SELECT s.person AS user_id, s.day, count(*) AS sessions, sum(s.events) AS events, sum(s.revenue) AS revenue_cents,
             t.category AS top_category
      FROM sessions s LEFT JOIN top_cat t ON t.person = s.person AND t.day = s.day
      GROUP BY s.person, s.day, t.category) TO '{out}/user_daily/part.parquet' (FORMAT parquet)""")
    con.execute(f"""COPY (
      SELECT a.campaign_id, CASE WHEN a.campaign_id = 'organic' THEN 'organic' ELSE c.channel END AS channel, a.day,
             sum(a.sessions) AS sessions, sum(a.purchasing_sessions) AS purchasing_sessions,
             sum(a.revenue_cents) AS revenue_cents, sum(a.assisted_sessions) AS assisted_sessions
      FROM (SELECT campaign_id, day, count(*) AS sessions, count(*) FILTER (WHERE purchasing) AS purchasing_sessions,
                   sum(revenue) AS revenue_cents, 0 AS assisted_sessions FROM credited GROUP BY campaign_id, day
            UNION ALL
            SELECT campaign_id, day, 0, 0, 0, assisted FROM assists) a
      LEFT JOIN campaigns c ON c.campaign_id = a.campaign_id
      GROUP BY a.campaign_id, channel, a.day) TO '{out}/campaign_daily/part.parquet' (FORMAT parquet)""")
    con.close()
    shutil.rmtree(work, ignore_errors=True)

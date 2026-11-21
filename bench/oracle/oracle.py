#!/usr/bin/env python3
"""Expected tables for any dataset.  oracle.py [--engine py|fast] DATA_DIR OUT_DIR
Writes OUT_DIR/user_daily/part.parquet and OUT_DIR/campaign_daily/part.parquet (day is a DATE).
  py   : oracle_py.py, naive and simple (up to a couple of million events)
  fast : oracle_fast.py, the same rules via Python parsing + DuckDB SQL (20M and 60M events)"""
import argparse
import datetime
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def write_py(user_daily, campaign_daily, out):
    import pyarrow as pa
    import pyarrow.parquet as pq
    epoch = datetime.date(1970, 1, 1)
    d = lambda days: epoch + datetime.timedelta(days=days)
    tables = {
        "user_daily": (["user_id", "day", "sessions", "events", "revenue_cents", "top_category"],
                       [pa.string(), pa.date32(), pa.int64(), pa.int64(), pa.int64(), pa.string()],
                       [(r[0], d(r[1]), *r[2:]) for r in user_daily]),
        "campaign_daily": (["campaign_id", "channel", "day", "sessions", "purchasing_sessions", "revenue_cents",
                            "assisted_sessions"],
                           [pa.string(), pa.string(), pa.date32(), pa.int64(), pa.int64(), pa.int64(), pa.int64()],
                           [(r[0], r[1], d(r[2]), *r[3:]) for r in campaign_daily]),
    }
    for name, (cols, types, rows) in tables.items():
        os.makedirs(os.path.join(out, name), exist_ok=True)
        arrays = [pa.array([r[i] for r in rows], type=types[i]) for i in range(len(cols))]
        pq.write_table(pa.Table.from_arrays(arrays, names=cols), os.path.join(out, name, "part.parquet"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", choices=["py", "fast"], default="py")
    ap.add_argument("--work", default=None, help="scratch dir for the fast engine")
    ap.add_argument("data")
    ap.add_argument("out")
    a = ap.parse_args()
    if a.engine == "py":
        import oracle_py
        write_py(*oracle_py.run(a.data), a.out)
    else:
        import oracle_fast
        oracle_fast.run(a.data, a.out, a.work)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Compare an entry's output folder with expected tables.

  compare.py OUT_DIR EXPECTED_DIR [--json]

OUT_DIR/{user_daily,campaign_daily}/*.parquet is read the way the scorer does: only *.parquet files directly in the
folder, names starting with '.' or '_' and symlinks ignored; exact column set; day must be a Parquet DATE; any integer
width and any string encoding. EXPECTED_DIR holds <table>/*.parquet (oracle) or <table>.csv (kit/dev-answer).
Row order and file count are ignored. Rows are keyed by (user_id, day) / (campaign_id, day):
  missing    = expected key absent from the output
  extra      = output key absent from the expected tables, plus repeats of a key beyond the first
  differing  = key present in both with at least one different value
Exit code 0 iff the output matches exactly. With --json prints one JSON line (the last line of stdout)."""
import glob
import json
import os
import sys

import duckdb
import pyarrow as pa
import pyarrow.parquet as pq

SCHEMAS = {
    "user_daily": {"key": ["user_id", "day"],
                   "cols": {"user_id": "str", "day": "date", "sessions": "int", "events": "int",
                            "revenue_cents": "int", "top_category": "str"}},
    "campaign_daily": {"key": ["campaign_id", "day"],
                       "cols": {"campaign_id": "str", "channel": "str", "day": "date", "sessions": "int",
                                "purchasing_sessions": "int", "revenue_cents": "int", "assisted_sessions": "int"}},
}


def _kind(t):
    if pa.types.is_integer(t):
        return "int"
    if pa.types.is_string(t) or pa.types.is_large_string(t):
        return "str"
    if pa.types.is_dictionary(t) and (pa.types.is_string(t.value_type) or pa.types.is_large_string(t.value_type)):
        return "str"
    if pa.types.is_date32(t):
        return "date"
    return str(t)


def parquet_files(folder):
    if os.path.islink(folder) or not os.path.isdir(folder):
        raise ValueError(f"missing output folder {folder}")
    out = []
    for name in sorted(os.listdir(folder)):
        p = os.path.join(folder, name)
        if name.startswith((".", "_")) or not name.endswith(".parquet") or os.path.islink(p) or not os.path.isfile(p):
            continue
        out.append(p)
    if not out:
        raise ValueError(f"no parquet files in {folder}")
    return out


def check_schema(files, cols):
    for f in files:
        schema = pq.read_schema(f)
        if set(schema.names) != set(cols) or len(schema.names) != len(cols):
            raise ValueError(f"{os.path.basename(f)}: columns {schema.names}, expected {sorted(cols)}")
        for name in schema.names:
            kind = _kind(schema.field(name).type)
            if kind != cols[name]:
                raise ValueError(f"{os.path.basename(f)}: column {name} is {schema.field(name).type}, expected {cols[name]}")


def q(s):
    return "'" + s.replace("'", "''") + "'"


def load(con, view, source_kind, path, table):
    cols = SCHEMAS[table]["cols"]
    select = ", ".join(f'CAST("{c}" AS {"DATE" if k == "date" else "VARCHAR" if k == "str" else "HUGEINT"}) AS "{c}"'
                       for c, k in cols.items())
    if source_kind == "csv":
        types = {c: ("DATE" if k == "date" else "VARCHAR" if k == "str" else "BIGINT") for c, k in cols.items()}
        con.execute(f"CREATE VIEW {view} AS SELECT {select} FROM read_csv({q(path)}, header=true, types={types!r}, "
                    f"nullstr='', quote='', escape='')")
    else:
        con.execute(f"CREATE VIEW {view} AS SELECT {select} FROM read_parquet([{', '.join(q(p) for p in path)}], "
                    f"union_by_name=true)")


def compare_table(con, table, actual_dir, expected_root):
    spec = SCHEMAS[table]
    cols, key = spec["cols"], spec["key"]
    files = parquet_files(os.path.join(actual_dir, table))
    check_schema(files, cols)
    load(con, f"a_{table}", "parquet", files, table)
    if expected_root == "-":  # no expected tables yet: schema check and digest only
        allc = ", ".join(f'"{c}"' for c in cols)
        n_a = con.execute(f"SELECT count(*) FROM a_{table}").fetchone()[0]
        digest = con.execute(f"SELECT coalesce(sum(CAST(hash({allc}) AS HUGEINT)), 0) FROM a_{table}").fetchone()[0]
        return {"rows": n_a, "expected_rows": None, "missing": 0, "extra": 0, "differing": 0, "digest": str(digest)}
    exp_csv = os.path.join(expected_root, f"{table}.csv")
    if os.path.isfile(exp_csv):
        load(con, f"e_{table}", "csv", exp_csv, table)
    else:
        load(con, f"e_{table}", "parquet", parquet_files(os.path.join(expected_root, table)), table)
    k = ", ".join(f'"{c}"' for c in key)
    on = " AND ".join(f'a."{c}" IS NOT DISTINCT FROM e."{c}"' for c in key)
    vals = [c for c in cols if c not in key]
    diff = " OR ".join(f'a."{c}" IS DISTINCT FROM e."{c}"' for c in vals)
    n_a = con.execute(f"SELECT count(*) FROM a_{table}").fetchone()[0]
    n_e = con.execute(f"SELECT count(*) FROM e_{table}").fetchone()[0]
    n_a_keys = con.execute(f"SELECT count(*) FROM (SELECT DISTINCT {k} FROM a_{table})").fetchone()[0]
    con.execute(f"CREATE TEMP TABLE a1_{table} AS SELECT DISTINCT ON ({k}) * FROM a_{table}")
    missing = con.execute(f"SELECT count(*) FROM e_{table} e ANTI JOIN a1_{table} a ON {on}").fetchone()[0]
    extra_new = con.execute(f"SELECT count(*) FROM a1_{table} a ANTI JOIN e_{table} e ON {on}").fetchone()[0]
    differing = con.execute(f"SELECT count(*) FROM a1_{table} a JOIN e_{table} e ON {on} WHERE {diff}").fetchone()[0]
    allc = ", ".join(f'"{c}"' for c in cols)
    digest = con.execute(f"SELECT coalesce(sum(CAST(hash({allc}) AS HUGEINT)), 0) FROM a_{table}").fetchone()[0]
    return {"rows": n_a, "expected_rows": n_e, "missing": missing, "extra": extra_new + (n_a - n_a_keys),
            "differing": differing, "digest": str(digest)}


def main():
    argv = [a for a in sys.argv[1:] if a != "--json"]
    as_json = "--json" in sys.argv
    out_dir, expected = argv
    con = duckdb.connect()
    result = {"ok": False, "missing": 0, "extra": 0, "differing": 0, "tables": {}, "error": None}
    try:
        for table in SCHEMAS:
            r = compare_table(con, table, out_dir, expected)
            result["tables"][table] = r
            for k in ("missing", "extra", "differing"):
                result[k] += r[k]
        result["ok"] = (result["missing"] == result["extra"] == result["differing"] == 0) and expected != "-"
        result["no_expected"] = expected == "-"
        result["digest"] = "-".join(result["tables"][t]["digest"] for t in SCHEMAS)
    except (ValueError, duckdb.Error, OSError, pa.ArrowException) as e:
        result["error"] = f"{type(e).__name__}: {e}"[:300]
    text = ("MATCH" if result["ok"] else "DIFFER") + f" missing={result['missing']} extra={result['extra']} " \
           f"differing={result['differing']}" + (f" error={result['error']}" if result["error"] else "")
    print(text, file=sys.stderr if as_json else sys.stdout)
    if as_json:
        print(json.dumps(result))
    sys.exit(0 if result["ok"] else 1)


if __name__ == "__main__":
    main()

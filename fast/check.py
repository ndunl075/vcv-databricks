#!/usr/bin/env python3
"""Differential test of ssf (with its hand-off to ss) against expected tables.

Runs: the fixtures (tests/fixtures), the dev set (kit/dev-answer), and every dataset under
$DATA_ROOT (default ~/Developer/vcv-data/db) that has expected tables in $DATA_ROOT/expected/<name>
(marker file .done) or, with --ref-ss, a reference made by the general binary.
Per case it reports whether the fast path or the hand-off produced the result, the wall time,
and whether both tables are exactly equal. Exit status 1 if anything is inexact.

Usage: check.py [--only SUBSTR ...] [--skip SUBSTR ...] [--max-mb N] [--ref-ss] [--no-fixtures]
Env: SSF (fast binary), SS (general binary), DATA_ROOT, NCPU (default 4).
"""
import argparse, os, shutil, subprocess, sys, tempfile, time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "tests"))
import polars as pl  # noqa: E402
from run_fixtures import SCHEMAS, load_actual, load_expected, rows  # noqa: E402

SSF = os.environ.get("SSF", str(REPO / "fast/target/release/ssf"))
SS = os.environ.get("SS", str(REPO / "fast/target/ss/release/ss"))
DATA_ROOT = Path(os.environ.get("DATA_ROOT", Path.home() / "Developer/vcv-data/db"))
NCPU = os.environ.get("NCPU", "4")


def run_ssf(data, out, exe=None, extra_env=None):
    env = dict(os.environ, SSF_GENERAL=SS, DATA_DIR=str(data), OUTPUT_DIR=str(out), NCPU=NCPU, **(extra_env or {}))
    t = time.perf_counter()
    p = subprocess.run([exe or SSF, str(data), str(out)], env=env, capture_output=True, text=True)
    secs = time.perf_counter() - t
    err = p.stderr
    path = "handoff" if "ssf: hand-off" in err else ("fast" if exe is None else "general")
    why = ""
    if path == "handoff":
        line = [l for l in err.splitlines() if "ssf: hand-off" in l][0]
        why = line.split("pipeline:", 1)[1].strip()
    return p.returncode, secs, path, why, err


def residue_problem(out, path):
    names = [f.name for d in ("user_daily", "campaign_daily") for f in (Path(out) / d).glob("*") if (Path(out) / d).is_dir()]
    ssf_files = [n for n in names if n.startswith("ssf-")]
    other = [n for n in names if not n.startswith("ssf-")]
    if path == "handoff" and ssf_files:
        return f"hand-off left files of ssf: {ssf_files[:3]}"
    if path == "fast" and other:
        return f"fast path run has foreign files: {other[:3]}"
    return None


def load_dir(folder, table):
    folder = Path(folder)
    csv = folder / f"{table}.csv"
    if csv.is_file():
        return load_expected(csv, SCHEMAS[table])
    return load_actual(folder / table, SCHEMAS[table])


def diff(out, exp):
    """Returns (ok, message)."""
    msgs = []
    for table, schema in SCHEMAS.items():
        try:
            a = load_dir(out, table)
        except Exception as e:  # noqa: BLE001
            return False, f"{table}: cannot read output: {e}"
        b = load_dir(exp, table)
        keys = [c for c in ("user_id", "campaign_id", "day") if c in schema]
        dup_a = a.height - a.n_unique(subset=keys)
        only_a = a.join(b, on=list(schema), how="anti", nulls_equal=True).height
        only_b = b.join(a, on=list(schema), how="anti", nulls_equal=True).height
        if dup_a or only_a or only_b:
            msgs.append(f"{table}: rows {a.height} vs {b.height}, only-out {only_a}, only-expected {only_b}, dup {dup_a}")
    return (not msgs), "; ".join(msgs)


def dir_mb(p):
    return sum(f.stat().st_size for f in Path(p).rglob("*") if f.is_file()) / 1e6


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", action="append", default=[])
    ap.add_argument("--skip", action="append", default=[])
    ap.add_argument("--max-mb", type=float, default=1e9)
    ap.add_argument("--ref-ss", action="store_true", help="for datasets without expected tables, compare with the general binary's output")
    ap.add_argument("--no-fixtures", action="store_true")
    args = ap.parse_args()
    want = lambda n: (not args.only or any(s in n for s in args.only)) and not any(s in n for s in args.skip)  # noqa: E731
    results = []  # (name, path, secs, status, note)
    work = Path(tempfile.mkdtemp(prefix="ssfcheck-", dir=os.environ.get("TMPDIR", None)))

    def case(name, data, exp, extra_env=None, expect=None):
        out = work / "out"
        shutil.rmtree(out, ignore_errors=True)
        out.mkdir(parents=True)
        rc, secs, path, why, err = run_ssf(data, out, extra_env=extra_env)
        if rc != 0:
            results.append((name, path, secs, "FAIL", f"exit {rc}: {err[-300:]}"))
            return
        bad = residue_problem(out, path)
        ok, msg = diff(out, exp)
        if expect and not path.startswith(expect):
            ok, msg = False, (msg + f" expected path {expect}, got {path}").strip()
        if bad:
            ok, msg = False, (msg + " " + bad).strip()
        results.append((name, path + (f" ({why[:48]})" if why else ""), secs, "exact" if ok else "INEXACT", msg))
        print(f"{results[-1][3]:8} {name:28} {results[-1][1]:60} {secs:7.2f}s {msg}", flush=True)

    if not args.no_fixtures:
        fx = REPO / "tests/fixtures"
        for d in sorted(p for p in fx.iterdir() if p.is_dir()):
            if want("fixture/" + d.name):
                data = d / "data" if (d / "data").is_dir() else d / "input"
                case("fixture/" + d.name, data, d / "expected")
    if want("dev"):
        dev = DATA_ROOT / "dev"
        if dev.is_dir():
            case("dev (kit/dev-answer)", dev, REPO / "kit/dev-answer/dev-answer")
    if want("forced"):
        # Hand-off from every stage and by every kind of failure, on the dev set: the result must
        # be exact, produced by ss, and no file of ssf may remain.
        dev = DATA_ROOT / "dev"
        kinds = ["fail", "panic"] + (["segv", "ill"] if sys.platform.startswith("linux") else [])
        for point in ("keys", "parsed", "runs", "written", "end"):
            for kind in kinds:
                if kind == "ill" and os.uname().machine != "x86_64":
                    continue
                case(f"forced {point}:{kind}", dev, REPO / "kit/dev-answer/dev-answer", extra_env={"SSF_TEST": f"{point}:{kind}"}, expect="handoff")
    for d in sorted(DATA_ROOT.iterdir()):
        if not (d / "manifest.json").is_file() or d.name == "dev" or not want(d.name):
            continue
        if dir_mb(d) > args.max_mb:
            print(f"skip     {d.name} ({dir_mb(d):.0f} MB > --max-mb)")
            continue
        exp = DATA_ROOT / "expected" / d.name
        if (exp / ".done").exists():
            case(d.name, d, exp)
        elif args.ref_ss:
            ref = work / ("ref-" + d.name)
            ref.mkdir()
            rc, _, _, _, err = run_ssf(d, ref, exe=SS)
            if rc == 0:
                case(d.name + " [vs ss]", d, ref)
                shutil.rmtree(ref, ignore_errors=True)
            else:
                print(f"skip     {d.name}: reference run failed")
        else:
            print(f"skip     {d.name}: no expected tables")
    shutil.rmtree(work, ignore_errors=True)
    n = len(results)
    fast = sum(1 for r in results if r[1] == "fast")
    hand = sum(1 for r in results if r[1].startswith("handoff"))
    bad = [r for r in results if r[3] != "exact"]
    print(f"\n{n} cases: {fast} by the fast path, {hand} by the hand-off, {len(bad)} not exact")
    for r in bad:
        print("  NOT EXACT:", r[0], r[4])
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())

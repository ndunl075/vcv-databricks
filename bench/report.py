#!/usr/bin/env python3
"""report.py [--results FILE] [--out FILE]  -> tables from results.jsonl, also written to $DATA_ROOT/REPORT.md

Table 1 sorts by the stock 20M time (p20a). Table 2 sorts by remap at 20M, the one we steer by.
Times are Rosetta/colima times: relative only. A time is the median over runs that were exact; a dataset where an entry
was not exact shows WRONG (or CRASH/TIMEOUT/OOM/SIGILL). `*` marks a verdict that rests on agreement with other
entries instead of the oracle's tables (oracle not finished for that dataset yet)."""
import argparse
import collections
import json
import os
import statistics

DATA_ROOT = os.environ.get("DATA_ROOT", os.environ.get("VCV_DATA", os.path.expanduser("~/vcv-data/db")))


def load(path):
    rows = []
    with open(path) as fh:
        for line in fh:
            try:
                rows.append(json.loads(line))
            except ValueError:
                pass
    return rows


def expected_digest(ds):
    try:
        return open(os.path.join(DATA_ROOT, "expected", ds, ".digest")).read().strip()
    except OSError:
        return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", default=os.path.join(DATA_ROOT, "results.jsonl"))
    ap.add_argument("--out", default=os.path.join(DATA_ROOT, "REPORT.md"))
    ap.add_argument("--cpus", type=int, default=4, help="only rows measured at this CPU budget (older rows had 8)")
    a = ap.parse_args()
    rows = [r for r in load(a.results) if r.get("cpus", 8) == a.cpus]
    # latest commit per entry
    last_commit = {}
    for r in rows:
        last_commit[r["entry"]] = r.get("commit", "")
    runs = collections.defaultdict(list)       # (entry, ds) -> run rows of the latest commit
    notes = collections.defaultdict(list)      # entry -> build failures etc.
    for r in rows:
        if r.get("commit", "") != last_commit[r["entry"]]:
            continue
        if r["dataset"] in ("build-failed", "build"):
            notes[r["entry"]].append(r.get("build-failed") or r.get("error") or "build failed")
        else:
            runs[(r["entry"], r["dataset"])].append(r)
    # provisional truth: for a dataset without oracle tables, the digest most entries agree on (>= 2 entries)
    consensus = {}
    by_ds = collections.defaultdict(lambda: collections.defaultdict(set))
    for (e, ds), rs in runs.items():
        for r in rs:
            if r.get("exit") == 0 and r.get("digest"):
                by_ds[ds][r["digest"]].add(e)
    for ds, d in by_ds.items():
        best = max(d.items(), key=lambda kv: len(kv[1]))
        if len(best[1]) >= 2:
            consensus[ds] = best[0]

    def verdict(e, ds):
        """-> (state, median wall or None, provisional?) state in ok/wrong/crash/timeout/oom/sigill/none"""
        rs = runs.get((e, ds))
        if not rs:
            return ("none", None, False)
        good, prov = [], False
        for r in rs:
            if r.get("oom"):
                return ("oom", None, False)
            if r.get("timed_out"):
                return ("timeout", None, False)
            if r.get("sigill"):
                return ("sigill", None, False)
            if r.get("exit") != 0:
                return ("crash", None, False)
            ok = r.get("ok")
            if not ok and r.get("no_expected") or (not ok and r.get("digest") and expected_digest(ds)):
                ed = expected_digest(ds)
                if ed is not None:
                    ok = r.get("digest") == ed
                elif ds in consensus:
                    ok, prov = r.get("digest") == consensus[ds], True
            if not ok:
                return ("wrong", None, False)
            good.append(r["wall_s"])
        return ("ok", statistics.median(good), prov)

    def cell(e, ds):
        st, w, prov = verdict(e, ds)
        if st == "none":
            return "-"
        if st == "ok":
            return "%.1f%s" % (w, "*" if prov else "")
        return st.upper()

    entries = sorted(last_commit)
    cols = [("p20a", "p20a"), ("remap-20m", "ood-remap-20m"), ("mild-20m", "ood-remap-mild-20m"), ("f60", "f60"),
            ("remap-60m", "ood-remap-60m"), ("mild-60m", "ood-remap-mild-60m")]
    variants_1m = sorted({ds for (_, ds) in runs if ds.startswith("ood-") and ds.endswith("-1m")})
    big_ood = sorted({ds for (_, ds) in runs if ds.startswith("ood-") and not ds.endswith("-1m")})

    table = []
    for e in entries:
        st_dev = verdict(e, "dev")
        failing = [ds[4:] for ds in variants_1m + big_ood if verdict(e, ds)[0] not in ("ok", "none")]
        ran = sum(1 for ds in variants_1m + big_ood if verdict(e, ds)[0] != "none")
        table.append({"entry": e, "dev": cell(e, "dev"), "cols": [cell(e, ds) for _, ds in cols],
                      "p20": verdict(e, "p20a")[1], "r20": verdict(e, "ood-remap-20m")[1], "fail": failing, "ran": ran,
                      "note": ("; ".join(sorted(set(notes.get(e, []))))[:60] if e in notes else "")})

    def fmt(sort_key, title):
        out = ["### " + title, "", "| entry | dev | " + " | ".join(c for c, _ in cols) + " | OOD run | OOD wrong | note |",
               "|---|---|" + "---|" * len(cols) + "---|---|---|"]
        for t in sorted(table, key=sort_key):
            out.append("| %s | %s | %s | %d | %s | %s |" % (t["entry"], t["dev"], " | ".join(t["cols"]), t["ran"],
                                                         ", ".join(t["fail"]) or "-", t["note"]))
        return "\n".join(out)

    inf = float("inf")
    k1 = lambda t: (t["p20"] is None, t["p20"] or inf, t["entry"])
    k2 = lambda t: (t["r20"] is None, t["r20"] or inf, t["p20"] or inf, t["entry"])
    text = ["# Shopping Sessions: rival and entry benchmark", "",
            "Rosetta/colima times in seconds (relative only; --cpus %d, NCPU=%d, 18 GB, all runs under the shared lock)." % (a.cpus, a.cpus) + " "
            " Medians over exact runs. "
            "WRONG = tables differ from the oracle; `*` = judged by agreement between entries (oracle not done yet).", "",
            fmt(k1, "By stock 20M time (p20a)"), "", fmt(k2, "By remap 20M time (steering table)"), ""]
    built_failed = sorted(e for e in notes if not any(k[0] == e for k in runs))
    if built_failed:
        text += ["### No image", "", *["- %s: %s" % (e, "; ".join(sorted(set(notes[e])))) for e in built_failed], ""]
    s = "\n".join(text)
    print(s)
    with open(a.out, "w") as fh:
        fh.write(s + "\n")


if __name__ == "__main__":
    main()

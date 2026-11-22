#!/usr/bin/env python3
"""Merge the per-rival JSON files in DIR (rival-*/rival.json) into one table (sorted by reshuffled-ID time), with our
entry as a row. usage: summarize_rivals.py DIR OUT_JSON OUT_MD OURS_NAME OURS_SHA"""
import glob
import json
import os
import statistics
import sys

d, out_json, out_md, ours_name, ours_sha = sys.argv[1:6]
rows = [json.load(open(p)) for p in sorted(glob.glob(os.path.join(d, "*", "rival.json")))]


def good(runs):
    return [r["wall"] for r in runs if r["exit"] == 0 and not r["timed_out"]]


def med(x):
    return statistics.median(x) if x else None


def f(x):
    return "-" if x is None else "%.2f" % x


ours_all = {ds: [o["wall"] for r in rows for o in r["ours"].get(ds, []) if o["exit"] == 0] for ds in ("stock", "remap")}
table = []
for r in rows:
    row = {"repo": r["repo"], "sha": r["sha"][:8], "build": r["build"]["status"], "build_s": r["build"].get("seconds"),
           "cpu": r["runner"]["cpu_model"], "vcpu": r["runner"]["nproc"], "avx512bw": r["runner"]["avx512bw"]}
    for ds in ("stock", "remap"):
        runs = r["runs"].get(ds, [])
        g = good(runs)
        ours = [o["wall"] for o in r["ours"].get(ds, []) if o["exit"] == 0]
        row[ds] = {"median": med(g), "best": min(g) if g else None, "n_ok": len(g),
                   "exit": runs[-1]["exit"] if runs else None, "timed_out": any(x["timed_out"] for x in runs),
                   "oom": any(x["oom"] for x in runs), "sigill": any(x["sigill"] for x in runs),
                   "exact": r["exact"].get(ds, {}).get("status", "-"), "ours_same_vm_median": med(ours),
                   "vs_ours": (med(g) / med(ours)) if g and ours else None}
    table.append(row)
table.sort(key=lambda x: (x["remap"]["median"] is None, x["remap"]["median"] or 0))
ours_row = {"repo": "%s (ours, %s)" % (ours_name, ours_sha[:8]), "stock": {"median": med(ours_all["stock"]), "best": min(ours_all["stock"], default=None)},
            "remap": {"median": med(ours_all["remap"]), "best": min(ours_all["remap"], default=None)}}
json.dump({"ours": ours_row, "rivals": table}, open(out_json, "w"), indent=1)

ev = rows[0]["events"] if rows else 0
cpus = sorted({"%s vCPU %s avx512bw=%s" % (r["runner"]["nproc"], r["runner"]["cpu_model"], r["runner"]["avx512bw"]) for r in rows})
L = ["## Rivals vs ours on real x86 (%s events, seed %s)" % (format(ev, ","), rows[0]["seed"] if rows else "?"), "",
     "Each rival: exact commit built from its public repo, run 3x per dataset under the scorer's flags (no network, read-only root, uid 1000), "
     "alongside our image on the same VM. Wall time = container start to exit. Real-x86 ratios on small runners, **not** the scorer's absolute times.",
     "", "Runners seen: " + "; ".join(cpus), "",
     "| # | entry | stock med s | stock best | reshuffled med s | reshuffled best | exact stock | exact reshuffled | reshuffled vs ours (same VM) | notes |",
     "|---|---|---|---|---|---|---|---|---|---|"]
entries = [(ours_row["remap"]["median"], None, ours_row)] + [(t["remap"]["median"], t, None) for t in table]
entries.sort(key=lambda e: (e[0] is None, e[0] or 0))
for i, (_, t, o) in enumerate(entries, 1):
    if o:
        L.append("| %d | **%s** | %s | %s | %s | %s | reference | reference | 1.00 | median over every VM |" % (
            i, o["repo"], f(o["stock"]["median"]), f(o["stock"]["best"]), f(o["remap"]["median"]), f(o["remap"]["best"])))
        continue
    notes = []
    if t["build"] != "ok":
        notes.append(t["build"])
    for ds in ("stock", "remap"):
        x = t[ds]
        for flag, txt in (("sigill", "ILLEGAL INSTRUCTION"), ("oom", "OOM at runner memory"), ("timed_out", "timeout >180 s")):
            if x[flag]:
                notes.append("%s on %s" % (txt, ds))
        if x["exit"] not in (0, None) and not (x["sigill"] or x["oom"] or x["timed_out"]):
            notes.append("exit %s on %s" % (x["exit"], ds))
    vs = t["remap"]["vs_ours"]
    L.append("| %d | %s `%s` | %s | %s | %s | %s | %s | %s | %s | %s |" % (
        i, t["repo"], t["sha"], f(t["stock"]["median"]), f(t["stock"]["best"]), f(t["remap"]["median"]), f(t["remap"]["best"]),
        t["stock"]["exact"], t["remap"]["exact"], "-" if vs is None else "%.2fx" % vs, "; ".join(notes) or ""))
open(out_md, "w").write("\n".join(L) + "\n")
print("\n".join(L))

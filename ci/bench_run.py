#!/usr/bin/env python3
"""Time two refs of this repo back to back on a real x86 runner, on the stock and the reshuffled-ID dataset.
env: REF_SHA REF_NAME BASE_SHA BASE_NAME EVENTS SEED RUNS (default 5)   dirs under $VCV_CI_WORK: src/ref src/base data/*
writes results.json and summary.md into $VCV_CI_WORK/report"""
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib  # noqa: E402

W = lib.WORK
E = os.environ
RUNS = int(E.get("RUNS", "5"))
info = lib.sysinfo()
core_tests = os.path.join(W, "src", "coretests", "tests")
report = os.path.join(W, "report")
os.makedirs(report, exist_ok=True)
datasets = {"stock": os.path.join(W, "data", "stock"), "remap": os.path.join(W, "data", "remap")}
dev = os.path.join(W, "data", "dev")
res = {"runner": info, "events": int(E["EVENTS"]), "seed": int(E["SEED"]), "runs": RUNS,
       "ref": {"name": E["REF_NAME"], "sha": E["REF_SHA"]}, "baseline": {"name": E["BASE_NAME"], "sha": E["BASE_SHA"]},
       "timed": {}, "match": {}, "dev_check": None, "notes": []}


def log(*a):
    print(*a, flush=True)


def build(tag, src):
    t = time.monotonic()
    p = subprocess.run(["docker", "build", "-q", "-t", tag, src], capture_output=True, text=True)
    log("[build] %s %.0f s rc=%d" % (tag, time.monotonic() - t, p.returncode))
    if p.returncode:
        log(p.stdout[-2000:], p.stderr[-3000:])
        sys.exit("docker build failed for " + tag)


same = E["REF_SHA"] == E["BASE_SHA"]
build("ci-ref", os.path.join(W, "src", "ref"))
if same:
    subprocess.run(["docker", "tag", "ci-ref", "ci-base"], check=True)
else:
    build("ci-base", os.path.join(W, "src", "base"))

out = os.path.join(W, "out")
scratch = os.path.join(W, "scratch")

# 1. ref vs the organizers' answer on the 1M dev set (untimed)
r = lib.run_image("ci-ref", dev, os.path.join(out, "dev-ref"), scratch)
log("[dev] ref exit=%d wall=%.1f" % (r["exit"], r["wall"]))
if r["exit"] == 0:
    st, txt = lib.compare(core_tests, os.path.join(out, "dev-ref"), os.path.join(os.getcwd(), "kit/dev-answer/dev-answer"))
else:
    st, txt = "ERROR", "exit %d: %s" % (r["exit"], r["stderr_tail"][-200:])
res["dev_check"] = {"status": st, "detail": txt, "wall": r["wall"]}
log("[dev] vs kit/dev-answer:", st)

# 2. timed runs, alternating A/B, last output of each kept for the comparison
for ds, path in datasets.items():
    t = {"ref": [], "baseline": []}
    fails = []
    for i in range(RUNS):
        for key, img in (("ref", "ci-ref"), ("baseline", "ci-base")):
            r = lib.run_image(img, path, os.path.join(out, ds, key), scratch)
            log("[run] %s %-8s #%d wall=%.2f exit=%d" % (ds, key, i + 1, r["wall"], r["exit"]))
            if r["exit"] != 0:
                fails.append({"image": key, "run": i + 1, "exit": r["exit"], "oom": r["oom"], "sigill": r["sigill"],
                              "stderr": r["stderr_tail"][-300:]})
            else:
                t[key].append(r["wall"])
    res["timed"][ds] = {"ref": t["ref"], "baseline": t["baseline"], "failures": fails}
    if os.path.isdir(os.path.join(out, ds, "ref")) and not fails:
        st, txt = lib.compare(core_tests, os.path.join(out, ds, "ref"), os.path.join(out, ds, "baseline"))
    else:
        st, txt = "ERROR", "a run failed"
    res["match"][ds] = {"status": st, "detail": txt}
    log("[compare]", ds, st)
    log(txt)

with open(os.path.join(report, "results.json"), "w") as f:
    json.dump(res, f, indent=1)

# 3. markdown
names = {"stock": "stock (generator IDs)", "remap": "reshuffled IDs (remap)"}
L = []
L.append("## Stopwatch: `%s` (%s) vs `%s` (%s)" % (E["REF_NAME"], E["REF_SHA"][:8], E["BASE_NAME"], E["BASE_SHA"][:8]))
L.append("")
L.append("Runner: **%s vCPU**, %s GB RAM, `%s`. avx2=%s avx512f=%s avx512bw=%s. Container limits: no network, read-only root, "
         "uid 1000, `NCPU=%s`, `MEM_GB=%d`. Data: %s events, seed %s. %d timed runs each, alternating, wall time from container start to exit."
         % (info["nproc"], info["mem_gb"], info["cpu_model"], info["avx2"], info["avx512f"], info["avx512bw"], info["nproc"],
            lib.mem_budget_gb(info), format(res["events"], ","), res["seed"], RUNS))
L.append("")
L.append("These are real-x86 **ratios** on a small shared runner, not the scorer's absolute times (8 pinned vCPUs).")
L.append("")
L.append("| dataset | ref median s | ref best s | baseline median s | baseline best s | speedup (median) | speedup (best) | tables |")
L.append("|---|---|---|---|---|---|---|---|")
for ds in datasets:
    t = res["timed"][ds]
    rm, rb, bm, bb = lib.med(t["ref"]), min(t["ref"], default=None), lib.med(t["baseline"]), min(t["baseline"], default=None)
    sp = lambda a, b: "-" if not a or not b else "%.2fx" % (b / a)
    L.append("| %s | %s | %s | %s | %s | %s | %s | %s |" % (names[ds], lib.fmt(rm), lib.fmt(rb), lib.fmt(bm), lib.fmt(bb),
                                                      sp(rm, bm), sp(rb, bb), res["match"][ds]["status"]))
L.append("")
L.append("`ref` vs `kit/dev-answer` on the 1M seed-1 dev set: **%s**" % res["dev_check"]["status"])
L.append("")
L.append("All wall times (s):")
for ds in datasets:
    t = res["timed"][ds]
    L.append("- %s ref: %s; baseline: %s" % (ds, ", ".join("%.2f" % x for x in t["ref"]), ", ".join("%.2f" % x for x in t["baseline"])))
    for fl in t["failures"]:
        L.append("  - FAILED run: %s" % json.dumps(fl))
for ds in datasets:
    if res["match"][ds]["status"] != "MATCH":
        L.append("")
        L.append("compare %s: %s" % (ds, res["match"][ds]["detail"]))
open(os.path.join(report, "summary.md"), "w").write("\n".join(L) + "\n")
log("\n".join(L))

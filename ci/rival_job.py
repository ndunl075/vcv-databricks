#!/usr/bin/env python3
"""One rival: fetch the exact commit from its public repo, build it (10 min limit), run it under the scorer's flags
3x on stock and 3x on reshuffled-ID data, interleaved with our own image on the same machine, and compare tables.
Rival code only ever runs inside the locked-down container. Rival output text is never stored (only codes and counts).
env: RIVAL SHA RUNS(3) EVENTS SEED  image `ci-ours` must be loaded. writes $VCV_CI_WORK/report/rival.json"""
import json
import os
import re
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lib  # noqa: E402

W = lib.WORK
E = os.environ
repo, sha = E["RIVAL"], E["SHA"]
RUNS = int(E.get("RUNS", "3"))
RUN_TIMEOUT = int(E.get("RUN_TIMEOUT", "180"))
assert re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo) and re.fullmatch(r"[0-9a-f]{40}", sha)
info = lib.sysinfo()
core_tests = os.path.join(W, "src", "coretests", "tests")
report = os.path.join(W, "report")
os.makedirs(report, exist_ok=True)
datasets = {"stock": os.path.join(W, "data", "stock"), "remap": os.path.join(W, "data", "remap")}
out_root, scratch = os.path.join(W, "out"), os.path.join(W, "scratch")
res = {"repo": repo, "sha": sha, "runner": info, "events": int(E["EVENTS"]), "seed": int(E["SEED"]),
       "build": {"status": "not-run", "seconds": None}, "runs": {}, "ours": {}, "exact": {}}


def log(*a):
    print(*a, flush=True)


def save():
    with open(os.path.join(report, "rival.json"), "w") as f:
        json.dump(res, f, indent=1)


# ---- fetch the exact commit (public, unauthenticated), no hooks, no LFS, no submodules, straight into a tar context
src = os.path.join(W, "rival-src")
subprocess.run(["rm", "-rf", src])
os.makedirs(src)
genv = dict(os.environ, GIT_LFS_SKIP_SMUDGE="1", GIT_TERMINAL_PROMPT="0", GIT_CONFIG_NOSYSTEM="1")
try:
    for cmd in (["git", "init", "-q", src],
                ["git", "-C", src, "fetch", "-q", "--depth", "1", "https://github.com/%s.git" % repo, sha]):
        subprocess.run(cmd, check=True, env=genv, timeout=300, capture_output=True)
    ls = subprocess.run(["git", "-C", src, "ls-tree", "--name-only", "FETCH_HEAD"], capture_output=True, text=True, check=True).stdout.split()
    if "Dockerfile" not in ls:
        res["build"] = {"status": "no-dockerfile-at-root", "seconds": 0}
        save(); sys.exit(0)
except subprocess.SubprocessError as e:
    res["build"] = {"status": "fetch-failed", "seconds": 0}
    save(); log("fetch failed"); sys.exit(0)

# ---- build, 10 minute limit
t0 = time.monotonic()
arch = subprocess.Popen(["git", "-C", src, "archive", "FETCH_HEAD"], stdout=subprocess.PIPE)
b = subprocess.Popen(["docker", "build", "-q", "-t", "ci-rival", "-"], stdin=arch.stdout, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL, start_new_session=True)
arch.stdout.close()
try:
    rc = b.wait(timeout=600)
    status = "ok" if rc == 0 else "build-failed"
except subprocess.TimeoutExpired:
    os.killpg(b.pid, signal.SIGKILL)
    b.wait()
    subprocess.run("docker ps -q --filter ancestor=moby/buildkit | xargs -r docker kill", shell=True, capture_output=True)
    status = "build-timeout"
res["build"] = {"status": status, "seconds": round(time.monotonic() - t0, 1)}
log("[build]", repo, res["build"])
if status != "ok":
    save(); sys.exit(0)
try:
    size = int(subprocess.run(["docker", "image", "inspect", "-f", "{{.Size}}", "ci-rival"], capture_output=True, text=True).stdout)
    res["build"]["image_mb"] = round(size / 1e6)
except Exception:
    pass
subprocess.run(["rm", "-rf", src])

# ---- runs
for ds, path in datasets.items():
    ref_out = os.path.join(out_root, "ours-" + ds)
    ours = []
    r = lib.run_image("ci-ours", path, ref_out, scratch)
    log("[run] %s ours wall=%.2f exit=%d" % (ds, r["wall"], r["exit"]))
    ours.append(r)
    runs = []
    for i in range(RUNS):
        r = lib.run_image("ci-rival", path, os.path.join(out_root, "rival-" + ds), scratch, timeout=RUN_TIMEOUT, want_stderr=False)
        log("[run] %s rival #%d wall=%.2f exit=%d%s%s%s" % (ds, i + 1, r["wall"], r["exit"], " TIMEOUT" if r["timed_out"] else "",
                                                          " OOM" if r["oom"] else "", " SIGILL" if r["sigill"] else ""))
        if i == 0:
            if r["exit"] == 0 and not r["timed_out"] and ours[0]["exit"] == 0:
                st, txt = lib.compare(core_tests, os.path.join(out_root, "rival-" + ds), ref_out)
                # rival-supplied text is not kept: only the verdict and our own row counts
                res["exact"][ds] = {"status": st, "detail": txt if st in ("MATCH", "DIFFER") else "unreadable output"}
            else:
                res["exact"][ds] = {"status": "NO-OUTPUT", "detail": ""}
            log("[compare]", ds, res["exact"][ds]["status"])
        runs.append({k: r[k] for k in ("wall", "exit", "timed_out", "oom", "sigill")})
        if r["exit"] != 0 or r["timed_out"]:
            break  # a crash or a timeout will repeat; do not burn runner minutes
    res["runs"][ds] = runs
    r = lib.run_image("ci-ours", path, os.path.join(out_root, "ours2-" + ds), scratch)
    log("[run] %s ours wall=%.2f exit=%d" % (ds, r["wall"], r["exit"]))
    ours.append(r)
    res["ours"][ds] = [{k: o[k] for k in ("wall", "exit")} for o in ours]
    save()
    subprocess.run(["sudo", "-n", "rm", "-rf", os.path.join(out_root, "rival-" + ds), os.path.join(out_root, "ours2-" + ds)], capture_output=True)
save()

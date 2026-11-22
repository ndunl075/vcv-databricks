#!/usr/bin/env python3
"""Timing of ssf against ss in Docker under the scorer's limits (linux/amd64, --cpus 4 by default).

Usage: bench.py [--image ssf:latest] [--cpus 4] [--runs 3] [--verify] [--env K=V ...] DATASET...
Each run holds ~/Developer/vcv-rivals/bench.lock. ssf and ss are run back to back on the same
data (the image has both; ss is started with --entrypoint /ss). Reports the best of N wall times
of `docker run` (container start to exit, as the scorer measures) and the ratio. With --verify the
first ssf output is diffed with $DATA_ROOT/expected/<dataset> (or the first ss output).
"""
import argparse, os, shutil, subprocess, sys, tempfile, time
from pathlib import Path

HOME = Path.home()
LOCK = HOME / "Developer/vcv-rivals/bench.lock"
DATA_ROOT = Path(os.environ.get("DATA_ROOT", HOME / "Developer/vcv-data/db"))
TMP = HOME / "Developer/vcv-data/tmp"


def docker_run(image, data, cpus, entry, env):
    TMP.mkdir(parents=True, exist_ok=True)
    out = Path(tempfile.mkdtemp(dir=TMP))
    scratch = Path(tempfile.mkdtemp(dir=TMP))
    os.chmod(out, 0o777); os.chmod(scratch, 0o777)
    cmd = ["lockf", str(LOCK), "docker", "run", "--rm", "--platform", "linux/amd64", "--network", "none", "--read-only",
           "--tmpfs", "/tmp:rw,size=64m,mode=1777", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
           "--pids-limit", "4096", "--cpus", str(cpus), "--memory", "24g", "--memory-swap", "24g", "--user", "1000:1000",
           "-v", f"{data}:/data:ro,z", "-v", f"{out}:/output:z", "-v", f"{scratch}:/scratch:z",
           "-e", "DATA_DIR=/data", "-e", "OUTPUT_DIR=/output", "-e", "SCRATCH_DIR=/scratch", "-e", "HOME=/scratch",
           "-e", f"NCPU={cpus}", "-e", "MEM_GB=24"]
    for e in env:
        cmd += ["-e", e]
    if entry:
        cmd += ["--entrypoint", entry]
    cmd.append(image)
    t = time.perf_counter()
    p = subprocess.run(cmd, capture_output=True, text=True)
    secs = time.perf_counter() - t
    shutil.rmtree(scratch, ignore_errors=True)
    return p.returncode, secs, out, p.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", default="ssf:latest")
    ap.add_argument("--cpus", type=int, default=4)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--verify", action="store_true")
    ap.add_argument("--env", action="append", default=[])
    ap.add_argument("datasets", nargs="+")
    a = ap.parse_args()
    for name in a.datasets:
        data = DATA_ROOT / name
        best = {}
        kept = {}
        for impl, entry in (("ssf", None), ("ss", "/ss")):
            ts = []
            for i in range(a.runs):
                rc, secs, out, err = docker_run(a.image, data, a.cpus, entry, a.env if impl == "ssf" else [])
                if rc != 0:
                    print(f"{name} {impl} run {i}: exit {rc}\n{err[-600:]}")
                    shutil.rmtree(out, ignore_errors=True)
                    continue
                ts.append(secs)
                if i == 0 and a.verify:
                    kept[impl] = out
                    if impl == "ssf":
                        handoff = "ssf: hand-off" in err
                        print(f"{name} ssf path: {'HAND-OFF: ' + err.split('pipeline:')[1].splitlines()[0] if handoff else 'fast'}")
                else:
                    shutil.rmtree(out, ignore_errors=True)
                print(f"{name} {impl} run {i}: {secs:.2f}s", flush=True)
            best[impl] = min(ts) if ts else float("nan")
        if a.verify and "ssf" in kept:
            sys.path.insert(0, str(Path(__file__).resolve().parent))
            import check
            exp = DATA_ROOT / "expected" / name
            if not (exp / ".done").exists():
                exp = kept.get("ss")
            ok, msg = check.diff(kept["ssf"], exp) if exp else (False, "no reference")
            print(f"{name} verify vs {'oracle' if (DATA_ROOT / 'expected' / name / '.done').exists() else 'ss'}: {'exact' if ok else 'INEXACT ' + msg}")
            for d in kept.values():
                subprocess.run(["rm", "-rf", str(d)])
        print(f"== {name}: ssf {best['ssf']:.2f}s, ss {best['ss']:.2f}s, ratio ssf/ss {best['ssf'] / best['ss']:.2f}\n", flush=True)


if __name__ == "__main__":
    main()

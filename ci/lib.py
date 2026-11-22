"""Shared helpers for the CI stopwatch: run an image the way the scorer does, compare outputs, read the machine."""
import json
import os
import re
import shutil
import statistics
import subprocess
import time

WORK = os.environ.get("VCV_CI_WORK", os.path.expanduser("~/vcv-ci"))


def sysinfo():
    flags = set()
    model = ""
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name") and not model:
                model = line.split(":", 1)[1].strip()
            if line.startswith("flags") and not flags:
                flags = set(line.split(":", 1)[1].split())
    except OSError:
        pass
    mem_kb = 0
    try:
        mem_kb = int(re.search(r"MemTotal:\s+(\d+)", open("/proc/meminfo").read()).group(1))
    except Exception:
        pass
    return {
        "nproc": os.cpu_count(),
        "mem_gb": round(mem_kb / 1048576, 1),
        "cpu_model": model,
        "avx2": "avx2" in flags,
        "avx512f": "avx512f" in flags,
        "avx512bw": "avx512bw" in flags,
    }


def mem_budget_gb(info=None):
    info = info or sysinfo()
    return max(2, int(info["mem_gb"] - 1.5))


def _sudo_rm(path):
    if os.path.exists(path):
        r = subprocess.run(["rm", "-rf", path], capture_output=True)
        if r.returncode != 0 or os.path.exists(path):
            subprocess.run(["sudo", "-n", "rm", "-rf", path], capture_output=True)


def fresh_dir(path):
    _sudo_rm(path)
    os.makedirs(path)
    os.chmod(path, 0o777)


def run_image(image, data, out, scratch, timeout=300, mem_gb=None, ncpu=None, want_stderr=True):
    """One run under the scorer's flags. Wall time is the monotonic time around `docker run` (container start to exit)."""
    info = sysinfo()
    mem_gb = mem_gb or mem_budget_gb(info)
    ncpu = ncpu or info["nproc"]
    fresh_dir(out)
    fresh_dir(scratch)
    name = "ci-run-%d-%d" % (os.getpid(), int(time.time() * 1000))
    errfile = scratch + ".err"
    cmd = ["docker", "run", "--name", name, "--network", "none", "--read-only",
           "--tmpfs", "/tmp:rw,size=64m,mode=1777", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
           "--pids-limit", "4096", "--memory", "%dg" % mem_gb, "--memory-swap", "%dg" % mem_gb, "--user", "1000:1000",
           "-v", "%s:/data:ro" % data, "-v", "%s:/output" % out, "-v", "%s:/scratch" % scratch,
           "-e", "DATA_DIR=/data", "-e", "OUTPUT_DIR=/output", "-e", "SCRATCH_DIR=/scratch", "-e", "HOME=/scratch",
           "-e", "NCPU=%d" % ncpu, "-e", "MEM_GB=%d" % mem_gb, image]
    timed_out = False
    with open(errfile, "wb") as ef:
        t0 = time.monotonic()
        p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=ef)
        try:
            rc = p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            subprocess.run(["docker", "kill", name], capture_output=True)
            rc = p.wait()
        wall = time.monotonic() - t0
    oom = False
    try:
        r = subprocess.run(["docker", "inspect", "-f", "{{.State.OOMKilled}} {{.State.ExitCode}}", name],
                           capture_output=True, text=True)
        oom = r.stdout.split()[0] == "true"
        if r.stdout.split()[1:]:
            rc = int(r.stdout.split()[1])
    except Exception:
        pass
    subprocess.run(["docker", "rm", "-f", name], capture_output=True)
    tail = ""
    sigill = rc == 132
    try:
        with open(errfile, "rb") as ef:
            ef.seek(0, 2)
            size = ef.tell()
            ef.seek(max(0, size - 4000))
            raw = ef.read().decode("utf-8", "replace")
        if "llegal instruction" in raw:
            sigill = True
        if want_stderr:
            tail = raw[-600:]
    except OSError:
        pass
    os.unlink(errfile)
    return {"wall": round(wall, 3), "exit": rc, "timed_out": timed_out, "oom": oom, "sigill": sigill, "stderr_tail": tail}


def compare(core_tests, a, b):
    """Run the repo's tests/compare.py (from branch core). -> ('MATCH'|'DIFFER'|'ERROR', short text)."""
    r = subprocess.run(["python3", os.path.join(core_tests, "compare.py"), a, b], cwd=core_tests,
                       capture_output=True, text=True, timeout=900)
    lines = [l for l in (r.stdout + "\n" + r.stderr).splitlines() if l.strip()]
    if r.stdout.strip().endswith("MATCH"):
        return "MATCH", "\n".join(l for l in r.stdout.splitlines() if "rows" in l)
    if r.stdout.strip().endswith("DIFFER"):
        return "DIFFER", "\n".join(l for l in r.stdout.splitlines() if "rows" in l)
    msg = re.sub(r"[^\x20-\x7e]", "?", (lines[-1] if lines else "no output"))[:160]
    return "ERROR", msg


def med(xs):
    return statistics.median(xs) if xs else None


def fmt(x, nd=2):
    return "-" if x is None else ("%.*f" % (nd, x))

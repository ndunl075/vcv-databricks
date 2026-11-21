#!/usr/bin/env python3
"""timed_run.py DATA OUT SCRATCH IMAGE NAME TIMEOUT_S  -> one JSON line {wall_s, exit, oom, timed_out}.
Wall time is monotonic time around `docker run` (start of the client process to its exit), the same for every entry.
The container's stdout+stderr go to $LOG (a file), never into this process."""
import json
import os
import subprocess
import sys
import time

data, out, scratch, image, name, timeout = sys.argv[1:7]
log = os.environ.get("LOG", os.devnull)
cmd = ["docker", "run", "--name", name, "--platform", "linux/amd64", "--network", "none", "--read-only",
       "--tmpfs", "/tmp:rw,size=64m,mode=1777", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
       "--pids-limit", "4096", "--cpus", "8", "--memory", "18g", "--memory-swap", "18g", "--user", "1000:1000",
       "-v", f"{data}:/data:ro", "-v", f"{out}:/output", "-v", f"{scratch}:/scratch",
       "-e", "DATA_DIR=/data", "-e", "OUTPUT_DIR=/output", "-e", "SCRATCH_DIR=/scratch", "-e", "HOME=/scratch",
       "-e", "NCPU=8", "-e", "MEM_GB=24", image]
timed_out = False
with open(log, "wb") as fh:
    t0 = time.monotonic()
    p = subprocess.Popen(cmd, stdout=fh, stderr=subprocess.STDOUT)
    try:
        code = p.wait(timeout=float(timeout))
    except subprocess.TimeoutExpired:
        timed_out = True
        subprocess.run(["docker", "kill", name], capture_output=True)
        code = p.wait()
    wall = time.monotonic() - t0
oom = False
try:
    r = subprocess.run(["docker", "inspect", "-f", "{{.State.OOMKilled}}|{{.State.ExitCode}}", name],
                       capture_output=True, text=True, timeout=30)
    oom = r.stdout.strip().split("|")[0] == "true"
    code = int(r.stdout.strip().split("|")[1]) if "|" in r.stdout else code
except Exception:
    pass
subprocess.run(["docker", "rm", "-f", name], capture_output=True)
print(json.dumps({"wall_s": round(wall, 3), "exit": code, "oom": oom, "timed_out": timed_out}))

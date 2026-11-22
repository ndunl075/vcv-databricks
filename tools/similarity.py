#!/usr/bin/env python3
"""How much of OUR source also appears in any rival repo?  A gate before every submission.
usage: similarity.py <our-checkout-or-worktree> <db|rwx> [--ref <git-ref>] [--top N]
Compares 8-token windows of our source files (comments and strings' contents ignored) with every rival's.
Prints, per rival, the share of our windows found there. Exit code 1 if any rival is at or above 10%."""
import sys, re, subprocess, os, hashlib
EXT = (".rs", ".py", ".c", ".cc", ".cpp", ".h", ".hpp", ".go", ".zig", ".sh", ".toml", "Dockerfile")
SKIP = ("kit/", "bench/oracle/", "vendor/", "target/", "node_modules/", ".git/", "sqlglot/", "tests/fixtures/")
TOK = re.compile(r"[A-Za-z_][A-Za-z0-9_]*|\d+|[^\sA-Za-z0-9_]")
def strip(src):
    src = re.sub(r"/\*.*?\*/", " ", src, flags=re.S); src = re.sub(r"//[^\n]*", " ", src)
    src = re.sub(r"(?m)^\s*#[^\n]*", " ", src)
    src = re.sub(r'"(?:\\.|[^"\\\n])*"', '""', src); return src
def windows(src, k=8):
    t = TOK.findall(strip(src)); out = set()
    for i in range(len(t) - k + 1):
        w = t[i:i + k]
        if len(set(w)) < 5: continue                      # skip punctuation runs and boilerplate
        out.add(hashlib.blake2b(" ".join(w).encode(), digest_size=8).digest())
    return out
def want(p): return p.endswith(EXT) and not any(s in p for s in SKIP)
def tree(gitdir, ref):
    try:
        names = subprocess.run(["git", "--git-dir", gitdir, "ls-tree", "-r", "-l", ref], capture_output=True, text=True, timeout=60).stdout.splitlines()
    except Exception: return set()
    out = set()
    for line in names:
        meta, _, path = line.partition("\t"); parts = meta.split()
        if len(parts) < 4 or not want(path) or not parts[3].isdigit() or int(parts[3]) > 1_500_000: continue
        try:
            blob = subprocess.run(["git", "--git-dir", gitdir, "cat-file", "blob", parts[2]], capture_output=True, timeout=60).stdout
            out |= windows(blob.decode("utf-8", "replace"))
        except Exception: pass
    return out
KITS = {"db": "/Users/nico/Developer/vice-city-vibecoders-internal-tooling/data/kit/shopping-sessions",
        "rwx": "/Users/nico/Developer/vice-city-vibecoders-internal-tooling/data/kit/blast-radius"}
def kit_windows(track):
    """Windows that come from the organizers' kit: every team has these, so they prove nothing."""
    out = set()
    for root, _, files in os.walk(KITS.get(track, "")):
        for f in files:
            fp = os.path.join(root, f)
            if fp.endswith(EXT) and os.path.getsize(fp) < 1_500_000:
                try: out |= windows(open(fp, encoding="utf-8", errors="replace").read())
                except Exception: pass
    return out
def per_file(gitdir, ref, rival, kit):
    names = subprocess.run(["git", "--git-dir", gitdir, "ls-tree", "-r", "-l", ref], capture_output=True, text=True).stdout.splitlines()
    for line in names:
        meta, _, path = line.partition("\t"); parts = meta.split()
        if len(parts) < 4 or not want(path): continue
        blob = subprocess.run(["git", "--git-dir", gitdir, "cat-file", "blob", parts[2]], capture_output=True).stdout
        w = windows(blob.decode("utf-8", "replace")) - kit
        if w: print(f"      {len(w & rival)*100/len(w):5.1f}%  of {len(w):6d}  {path}")
def main():
    ours_dir, track = sys.argv[1], sys.argv[2]
    ref = sys.argv[sys.argv.index("--ref") + 1] if "--ref" in sys.argv else "HEAD"
    top = int(sys.argv[sys.argv.index("--top") + 1]) if "--top" in sys.argv else 8
    gd = subprocess.run(["git", "-C", ours_dir, "rev-parse", "--absolute-git-dir"], capture_output=True, text=True).stdout.strip()
    kit = kit_windows(track)
    ours = tree(gd, ref) - kit
    if not ours: print("no source files of ours found"); sys.exit(2)
    rd = os.path.expanduser(f"~/Developer/vcv-rivals/{track}"); rows = []
    for d in sorted(os.listdir(rd)):
        if not d.endswith(".git"): continue
        r = tree(os.path.join(rd, d), "HEAD")
        if r: rows.append((len(ours & r) / len(ours), d[:-4], r))
    rows.sort(key=lambda x: -x[0])
    print(f"ours: {len(ours)} windows at {ref} (kit's own {len(kit)} windows excluded); compared with {len(rows)} rival repos")
    for share, name, _ in rows[:top]: print(f"  {share*100:5.1f}%  {name}")
    if "--files" in sys.argv and rows:
        print(f"    per file, against {rows[0][1]}:"); per_file(gd, ref, rows[0][2], kit)
    worst = rows[0][0] if rows else 0
    print("PASS" if worst < 0.10 else "FAIL: too close to a rival, do not submit"); sys.exit(0 if worst < 0.10 else 1)
main()

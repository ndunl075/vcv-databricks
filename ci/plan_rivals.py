#!/usr/bin/env python3
"""Turn the rival list (workflow input, or ci/rivals.txt when empty) into a matrix JSON on stdout.
env: LIST (may be empty). Lines: `owner/repo full-sha`; `#` starts a comment."""
import json
import os
import re
import sys

text = os.environ.get("LIST", "").strip()
if not text:
    text = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "rivals.txt")).read()
rows = []
for n, line in enumerate(text.splitlines(), 1):
    line = line.split("#", 1)[0].strip()
    if not line:
        continue
    m = re.fullmatch(r"([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)\s+([0-9a-f]{40})", line)
    if not m:
        sys.exit("bad rival line %d: %r" % (n, line[:80]))
    rows.append({"repo": m.group(1), "sha": m.group(2), "slug": m.group(1).replace("/", "__")})
if not rows:
    sys.exit("empty rival list")
print(json.dumps({"include": rows}, separators=(",", ":")))

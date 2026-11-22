#!/usr/bin/env python3
"""When would a submission made right now be scored? Reads the contest's public status page.
usage: eta.py            one table for all tracks
       eta.py --ours     also list our team's score changes seen by the feed logger (to the minute)"""
import json, sys, sqlite3, urllib.request, datetime as dt
from zoneinfo import ZoneInfo
ET = ZoneInfo("America/New_York"); CUTOFF = dt.datetime(2026, 11, 22, 16, 0, tzinfo=dt.timezone.utc)
TEAM = "Vice City Vibecoders"; FEED = "/Users/nico/Developer/vice-city-vibecoders-internal-tooling/data/feed.sqlite"
req = urllib.request.Request("https://www.joinbuilders.org/buildathon/status/data", headers={"User-Agent": "feed-reader/0.1"})
d = json.load(urllib.request.urlopen(req, timeout=20))
now = dt.datetime.fromisoformat(d["now"]); left = CUTOFF - now
hm = lambda t: t.astimezone(ET).strftime("%-I:%M %p")
print(f"Now {hm(now)} ET. Cutoff in {left.seconds//3600 + left.days*24}h {left.seconds%3600//60:02d}m. Site: {d.get('overall')}\n")
print(f"{'track':18} {'queue':>5} {'busy/workers':>12} {'typical':>8} {'slow':>7}   {'submit now -> scored about':28} {'at worst by':12} {'per hour':>8} {'slots left':>10}")
for t in d["tracks"]:
    p50, p95 = (t.get("p50_ms") or 0)/1000, (t.get("p95_ms") or 0)/1000
    workers = max(1, t.get("active_workers") or 1); q = t.get("queued") or 0
    wait = (t.get("avg_wait_ms") or 0)/1000 + (q / workers) * p50          # queue ahead of us, shared over the workers
    lim = t.get("practice_limit") or 0
    slots = int(left.total_seconds() // 3600 * lim) + lim if lim else 0
    a, b = now + dt.timedelta(seconds=wait + p50), now + dt.timedelta(seconds=wait + p95)
    print(f"{t['key']:18} {q:>5} {str(t.get('busy_workers'))+'/'+str(workers):>12} {p50/60:>6.1f} m {p95/60:>5.1f} m   {hm(a):28} {hm(b):12} {lim:>8} {slots:>10}")
print("\n'typical' = half of recent submissions finished within this; 'slow' = 19 in 20 did. Machina grading is listed by the organizers as usually under 20 minutes, at most 3 hours.")
if "--ours" in sys.argv:
    db = sqlite3.connect(FEED)
    rows = db.execute("select observed_at, track, old_score, new_score, new_rank from score_changes where team=? order by observed_at desc limit 20", (TEAM,)).fetchall()
    print(f"\nOur score changes ({TEAM}), newest first:")
    for at, tr, o, n, r in rows:
        print(f"  {hm(dt.datetime.fromisoformat(at.replace('Z','+00:00')))} ET  {tr:18} {o} -> {n}  rank {r}")
    if not rows: print("  none yet on any track")

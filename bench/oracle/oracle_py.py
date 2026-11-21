"""Naive reference for kit/rules.md: plain dicts and loops, one step per rule, in rule order.
Slow on purpose (fine to ~2M events). Reads nothing but the files; assumes nothing about IDs or file layout."""
import bisect
import os
import sys
from collections import defaultdict

from rules import (CLICK_LOOKBACK_MS, LATE_MS, MS_DAY, SESSION_GAP_MS, csv_files, in_window, parse_price,
                   parse_ts, read_lines, read_manifest, read_small_table, split_fields)

EVENT_TYPES = ("view", "cart", "purchase")


def person_names(data):
    """Linked IDs rules 1-3: id -> person name (byte-wise smallest id of its component)."""
    parent = {}

    def find(x):
        root = x
        while parent[root] != root:
            root = parent[root]
        while parent[x] != root:
            parent[x], x = root, parent[x]
        return root

    for path in csv_files(os.path.join(data, "links")):
        for line in read_lines(path):
            f = split_fields(line, 2)
            if f is None or f[0] == "" or f[1] == "":
                continue
            for x in f:
                parent.setdefault(x, x)
            ra, rb = find(f[0]), find(f[1])
            if ra != rb:
                parent[ra] = rb
    smallest = {}
    for x in parent:
        r = find(x)
        if r not in smallest or x < smallest[r]:
            smallest[r] = x
    return {x: smallest[find(x)] for x in parent}


def run(data):
    ws, we = read_manifest(data)
    products = read_small_table(os.path.join(data, "products.csv"))
    campaigns = read_small_table(os.path.join(data, "campaigns.csv"))
    name_of = person_names(data)
    person = lambda uid: name_of.get(uid, uid)

    # ---- events: parse, dedup on event_id (smallest ingest_ts), validate the kept copy, drop late
    kept = {}
    for path in csv_files(os.path.join(data, "events")):
        for line in read_lines(path):
            f = split_fields(line, 7)
            if f is None or f[0] == "":
                continue
            ingest = parse_ts(f[3])
            if ingest is None:  # ingest_ts is always valid F1 by contract; anything else is not a usable line
                continue
            if f[0] not in kept or ingest < kept[f[0]][0]:
                kept[f[0]] = (ingest, f)
    events = defaultdict(list)  # person -> [(ts, product, price or None)]
    for ingest, f in kept.values():
        _, uid, ts_text, _, etype, pid, price_text = f
        ts = parse_ts(ts_text)
        if uid == "" or not in_window(ts, ws, we) or etype not in EVENT_TYPES or pid == "":
            continue
        price = None
        if etype == "purchase":
            price = parse_price(price_text)
            if price is None:
                continue
        if ingest - ts > LATE_MS:
            continue
        events[person(uid)].append((ts, pid, price))

    # ---- clicks
    seen = {}
    for path in csv_files(os.path.join(data, "clicks")):
        for line in read_lines(path):
            f = split_fields(line, 4)
            if f is None or f[0] == "":
                continue
            seen.setdefault(f[0], f)
    clicks = defaultdict(list)  # person -> [(ts, click_id, campaign)]
    for cid, (_, uid, camp, ts_text) in seen.items():
        ts = parse_ts(ts_text)
        if uid == "" or not in_window(ts, ws - CLICK_LOOKBACK_MS, we) or camp not in campaigns:
            continue
        clicks[person(uid)].append((ts, cid, camp))
    for lst in clicks.values():
        lst.sort()
    click_ts = {p: [c[0] for c in lst] for p, lst in clicks.items()}

    # ---- sessions, credit, assists
    user_rows = defaultdict(lambda: [0, 0, 0, defaultdict(int)])  # (person, day) -> sessions, events, rev, cat sums
    camp_rows = defaultdict(lambda: [0, 0, 0, 0])  # (campaign, day) -> sessions, purchasing, rev, assisted
    for p, evs in events.items():
        evs.sort(key=lambda e: e[0])
        sessions, current, last = [], None, None
        for e in evs:
            if current is None or e[0] - last > SESSION_GAP_MS:
                current = []
                sessions.append(current)
            current.append(e)
            last = e[0]
        for s in sessions:
            start = s[0][0]
            day = start // MS_DAY  # floor: UTC date as days since epoch
            revenue = sum(e[2] for e in s if e[2] is not None)
            purchasing = any(e[2] is not None for e in s)
            u = user_rows[(p, day)]
            u[0] += 1
            u[1] += len(s)
            u[2] += revenue
            for ts, pid, price in s:
                if price is not None:
                    u[3][products.get(pid, "unknown")] += price
            credited, assisting = "organic", set()
            lst, tss = clicks.get(p, []), click_ts.get(p, [])
            lo = bisect.bisect_left(tss, start - CLICK_LOOKBACK_MS)
            hi = bisect.bisect_right(tss, start)
            if hi > lo:
                best_ts = lst[hi - 1][0]
                credited = min((c for c in lst[lo:hi] if c[0] == best_ts), key=lambda c: c[1])[2]
                assisting = {c[2] for c in lst[lo:hi]}
            row = camp_rows[(credited, day)]
            row[0] += 1
            row[1] += purchasing
            row[2] += revenue
            for camp in assisting:
                camp_rows[(camp, day)][3] += 1

    user_daily = []
    for (p, day), (n, ev, rev, cats) in user_rows.items():
        top = min(cats.items(), key=lambda kv: (-kv[1], kv[0]))[0] if cats else None
        user_daily.append((p, day, n, ev, rev, top))
    campaign_daily = []
    for (camp, day), (n, pur, rev, ass) in camp_rows.items():
        campaign_daily.append((camp, "organic" if camp == "organic" else campaigns[camp], day, n, pur, rev, ass))
    return user_daily, campaign_daily

#!/usr/bin/env python3
"""Writes our own edge-case fixtures (tests/fixtures/X*), in the layout of the starter fixtures.

Every expected row below was derived by hand from rules.md (comments say how).
Run from anywhere: python3 tests/make_fixtures.py
"""
import calendar
import os
import random
import shutil

HERE = os.path.dirname(os.path.abspath(__file__))
FIX = os.path.join(HERE, "fixtures")

EV_H = "event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents"
CK_H = "click_id,user_id,campaign_id,ts"
LK_H = "user_id_a,user_id_b"
UD_H = "user_id,day,sessions,events,revenue_cents,top_category"
CD_H = "campaign_id,channel,day,sessions,purchasing_sessions,revenue_cents,assisted_sessions"

BASE_PRODUCTS = "product_id,category\np1,shoes\np2,books\n"
BASE_CAMPAIGNS = "campaign_id,channel\nc_1,search\nc_2,social\n"
BASE_MANIFEST = '{\n  "window_start": "2026-10-01T00:00:00Z",\n  "window_end": "2026-10-15T00:00:00Z"\n}\n'


def ms(y, mo, d, h=0, mi=0, s=0, milli=0):
    return calendar.timegm((y, mo, d, h, mi, s)) * 1000 + milli


def w(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


def case(name, events=None, clicks=None, links=None, products=BASE_PRODUCTS, campaigns=BASE_CAMPAIGNS,
         manifest=BASE_MANIFEST, user=(), camp=(), ev_files=None, ck_files=None, lk_files=None):
    """events/clicks/links: lists of lines (one file) or *_files: {filename: raw text}."""
    root = os.path.join(FIX, name)
    shutil.rmtree(root, ignore_errors=True)
    inp = os.path.join(root, "input")
    w(os.path.join(inp, "products.csv"), products)
    w(os.path.join(inp, "campaigns.csv"), campaigns)
    w(os.path.join(inp, "manifest.json"), manifest)
    for sub, header, lines, files in (("events", EV_H, events, ev_files), ("clicks", CK_H, clicks, ck_files),
                                      ("links", LK_H, links, lk_files)):
        if files is None:
            files = {"part-00000.csv": header + "\n" + "".join(l + "\n" for l in (lines or []))}
        for fn, text in files.items():
            w(os.path.join(inp, sub, fn), text)
    w(os.path.join(root, "expected", "user_daily.csv"), UD_H + "\n" + "".join(r + "\n" for r in user))
    w(os.path.join(root, "expected", "campaign_daily.csv"), CD_H + "\n" + "".join(r + "\n" for r in camp))


def ev(i, user, ts, ing, typ="view", prod="p1", price=""):
    return f"{i},{user},{ts},{ing},{typ},{prod},{price}"


ING = "2026-10-04T00:00:00Z"  # fine for any ts on Oct 3 (within 24 h)

# ---- X1: a 75-day window; the click window opens exactly 7 days before it.
case(
    "X1_wide_window",
    manifest='{"window_start": "2026-09-01T00:00:00Z", "window_end": "2026-11-15T00:00:00Z"}',
    events=[
        ev("e1", "u1", "2026-09-01T00:00:00Z", "2026-09-01T00:00:01Z"),
        ev("e2", "u1", "2026-11-14T23:59:59Z", "2026-11-15T00:00:00Z"),
        ev("e3", "u1", "2026-11-15T00:00:00Z", "2026-11-15T00:00:01Z"),  # window end: out
        ev("e4", "u1", "2026-08-31T23:59:59Z", "2026-09-01T00:00:00Z"),  # before start: out
        ev("e5", "u2", "2026-10-15T12:00:00Z", "2026-10-15T12:00:00Z"),
    ],
    clicks=[
        "k1,u1,c_1,2026-08-25T00:00:00Z",  # exactly window_start - 7 days: valid, exactly 7 d before session 1
        "k2,u1,c_2,2026-11-10T00:00:00Z",  # 4 d 23:59:59 before the last session
        "k3,u1,c_2,2026-08-24T23:59:59Z",  # before the click window: dropped
    ],
    user=["u1,2026-09-01,1,1,0,", "u1,2026-11-14,1,1,0,", "u2,2026-10-15,1,1,0,"],
    camp=["c_1,search,2026-09-01,1,0,0,1", "c_2,social,2026-11-14,1,0,0,1", "organic,organic,2026-10-15,1,0,0,0"],
)

# ---- X2: product/campaign IDs of other shapes, 43 categories, bytewise category ties.
prods = ["product_id,category", "140000,cat_9", "p_007,cat_10", "q-1,cat_9", "unk,zeta"]
prods += [f"m{i:02d},k{i:02d}" for i in range(40)]
t10 = "2026-10-03T10:00:00Z"
case(
    "X2_id_shapes_many_categories",
    products="\n".join(prods) + "\n",
    campaigns="campaign_id,channel\nc_007,search\nc_7,social\nCamp A,display ads\n",
    events=[
        ev("e1", "u1", t10, ING, "purchase", "140000", "500"),
        ev("e2", "u1", t10, ING, "purchase", "p_007", "500"),
        ev("e3", "u1", t10, ING, "purchase", "m39", "100"),
        ev("e4", "u2", t10, ING, "purchase", "m39", "300"),
        ev("e5", "u2", t10, ING, "purchase", "m00", "300"),
        ev("e6", "u3", t10, ING, "purchase", "nope", "200"),
        ev("e7", "u3", t10, ING, "purchase", "m05", "200"),
        ev("e8", "u4", t10, ING, "purchase", "P_007", "50"),  # case variant: unknown
    ],
    clicks=[
        "k1,u1,c_007,2026-10-03T09:00:00Z",
        "k2,u2,c_7,2026-10-03T09:30:00Z",
        "k3,u3,C_007,2026-10-03T09:30:00Z",  # not in campaigns.csv: dropped
        "k4,u4,Camp A,2026-10-03T09:59:59Z",
    ],
    # u1: cat_9 500, cat_10 500, k39 100 -> tie goes to the byte-wise smaller "cat_10".
    # u2: k39 300 = k00 300 -> "k00". u3: unknown 200 = k05 200 -> "k05". u4: unknown only.
    user=["u1,2026-10-03,1,3,1100,cat_10", "u2,2026-10-03,1,2,600,k00", "u3,2026-10-03,1,2,400,k05",
          "u4,2026-10-03,1,1,50,unknown"],
    camp=["c_007,search,2026-10-03,1,1,1100,1", "c_7,social,2026-10-03,1,1,600,1",
          "organic,organic,2026-10-03,1,1,400,0", "Camp A,display ads,2026-10-03,1,1,50,1"],
)

# ---- X3: F3 with leading zeros at 20 and 25 digits (the lengths of F1 and F2), and too-long values.
base = ms(2026, 10, 3, 23, 58, 10)  # 1791071890000
assert base == 1791071890000
case(
    "X3_f3_leading_zeros",
    events=[
        ev("e1", "u1", "0" * 7 + str(base), ING),  # 20 digits
        ev("e2", "u2", "0" * 12 + str(base), ING),  # 25 digits
        ev("e3", "u3", "0" * 19 + str(base), ING),  # 32 digits
        ev("e4", "u4", str(base) + "000000000", ING),  # too long for 64 bits: outside the window
        ev("e5", "u5", "2026-10-03T23:58:10Z", ING),
        ev("e6", "u6", "20261003235810000000", ING),  # 20 digits, overflows 64 bits: outside
        ev("e7", "u7", "0" * 20, ING),  # 0 = 1970: outside
        ev("e8", "u8", "0" * 25, ING),
    ],
    clicks=[f"k1,u1,c_1,{'0' * 12 + str(base - 3600000)}"],  # 25 digits, one hour before u1's session
    user=[f"{u},2026-10-03,1,1,0," for u in ("u1", "u2", "u3", "u5")],
    camp=["c_1,search,2026-10-03,1,0,0,1", "organic,organic,2026-10-03,3,0,0,0"],
)

# ---- X4: IDs that are suffixes (or prefixes) of other IDs; links chain through them.
case(
    "X4_suffix_ids",
    events=[
        ev("e1", "u1", "2026-10-03T10:00:00Z", ING),
        ev("e2", "xu1", "2026-10-03T10:10:00Z", ING),
        ev("e3", "zz1", "2026-10-03T10:20:00Z", ING),
        ev("e4", "1u1", "2026-10-03T10:00:00Z", ING),
        ev("e5", "1", "2026-10-03T10:00:00Z", ING),
        ev("e6", "u", "2026-10-03T10:00:00Z", ING),
    ],
    clicks=["k1,zz1,c_1,2026-10-03T09:00:00Z", "k2,1,c_2,2026-10-03T09:00:00Z"],
    links=["xu1,u1", "zz1,xu1"],
    # {u1, xu1, zz1} is one person named u1 (3 events, one session). 1u1, 1 and u are three more people.
    user=["u1,2026-10-03,1,3,0,", "1u1,2026-10-03,1,1,0,", "1,2026-10-03,1,1,0,", "u,2026-10-03,1,1,0,"],
    camp=["c_1,search,2026-10-03,1,0,0,1", "c_2,social,2026-10-03,1,0,0,1", "organic,organic,2026-10-03,2,0,0,0"],
)

# ---- X5: very short lines, valid and malformed.
case(
    "X5_short_lines",
    ev_files={"part-00000.csv": EV_H + "\n" + "\n".join([
        "e,a,1791071890000,2026-10-04T00:00:00Z,view,p,",  # valid, one-character IDs
        "f,b,1791071890000,2026-10-04T00:00:00Z,view,p,",
        "0,0,1791071890000,2026-10-04T00:00:00Z,view,p,",
        "e,a",
        ",",
        ",,,,,,",  # empty event_id
        "a",
        '""',
        "x,,1791071890000,2026-10-04T00:00:00Z,view,p,",  # empty user
        "y,a,1791071890000,2026-10-04T00:00:00Z,view,,",  # empty product
        ",a,1791071890000,2026-10-04T00:00:00Z,view,p,",  # empty event_id
        "",
    ]) + "\n"},
    ck_files={"part-00000.csv": CK_H + "\n" + "\n".join(["k,a,c_1,1791071800000", "k", ",a,c_1,1791071800000", ",,,", ""]) + "\n"},
    lk_files={"part-00000.csv": LK_H + "\nx\n,\na,\n\n"},
    user=[f"{u},2026-10-03,1,1,0," for u in ("a", "b", "0")],
    camp=["c_1,search,2026-10-03,1,0,0,1", "organic,organic,2026-10-03,2,0,0,0"],
)

# ---- X6: upper-case and very long IDs (events, event IDs, links).
long_a = "x" * 300
long_b = "x" * 299 + "y"
eid_a = "E" * 200
eid_b = "E" * 199 + "F"
case(
    "X6_long_and_upper_ids",
    events=[
        ev("e1", "u1", "2026-10-03T10:00:00Z", ING),
        ev("e2", long_a, "2026-10-03T10:05:00Z", ING),
        ev("e3", "U1", "2026-10-03T10:00:00Z", ING),
        ev("e4", long_b, "2026-10-03T10:00:00Z", ING),
        ev(eid_a, "u7", "2026-10-03T12:00:00Z", "2026-10-03T12:00:01Z"),
        ev(eid_a, "zz", "2026-10-03T12:00:00Z", "2026-10-03T12:00:09Z"),  # later copy, other user: dropped
        ev(eid_b, "u8", "2026-10-03T12:00:00Z", "2026-10-03T12:00:01Z"),
    ],
    links=[f"{long_a},u1"],
    user=["u1,2026-10-03,1,2,0,", "U1,2026-10-03,1,1,0,", f"{long_b},2026-10-03,1,1,0,", "u7,2026-10-03,1,1,0,",
          "u8,2026-10-03,1,1,0,"],
    camp=["organic,organic,2026-10-03,5,0,0,0"],
)

# ---- X7: lateness at millisecond precision, ingest_ts before ts.
f3 = ms(2026, 10, 3, 23, 58, 10, 500)
case(
    "X7_lateness_edges",
    events=[
        ev("e1", "v1", "2026-10-03T10:00:00Z", "2026-10-03T09:00:00Z"),  # ingested before ts: kept
        ev("e2", "v2", "2026-10-03T10:00:00Z", "2026-10-04T10:00:00Z"),  # exactly 24 h: kept
        ev("e3", "v3", "2026-10-03T10:00:00Z", "2026-10-04T10:00:01Z"),  # 24 h + 1 s: dropped
        ev("e4", "v4", str(f3), "2026-10-04T23:58:10Z"),  # 24 h - 500 ms: kept
        ev("e5", "v5", str(f3), "2026-10-04T23:58:11Z"),  # 24 h + 500 ms: dropped
    ],
    user=[f"{u},2026-10-03,1,1,0," for u in ("v1", "v2", "v4")],
    camp=["organic,organic,2026-10-03,3,0,0,0"],
)

# ---- X8: assists merge intervals per campaign; credit by latest click; per-day counts inside one person.
def d(day, h=10, mi=0):
    return f"2026-10-{day:02d}T{h:02d}:{mi:02d}:00Z"


case(
    "X8_assist_intervals",
    events=[
        ev("e1", "u1", d(2), ING.replace("10-04", "10-02").replace("T00:00:00Z", "T11:00:00Z")),
        ev("e2", "u1", d(4), "2026-10-04T11:00:00Z"),
        ev("e3", "u1", d(6), "2026-10-06T11:00:00Z"),
        ev("e4", "u1", d(6, 10, 5), "2026-10-06T11:00:00Z", "purchase", "p1", "1000"),
        ev("e5", "u1", d(12), "2026-10-12T11:00:00Z"),
        ev("e6", "u1", d(12, 20), "2026-10-12T21:00:00Z"),
        ev("e7", "u1", d(13), "2026-10-13T11:00:00Z"),
    ],
    clicks=[
        f"k1,u1,c_1,{d(1, 12)}",
        f"k2,u1,c_1,{d(5, 9)}",
        f"k3,u1,c_1,{d(12)}",  # exactly at a session start: counts
        f"k4,u1,c_2,{d(12, 15)}",
    ],
    # c_1 reach: [Oct 1 12:00, Oct 8 12:00] U [Oct 5 09:00, Oct 12 09:00] = Oct 1 12:00 .. Oct 12 09:00 -> sessions Oct 2, 4, 6;
    #            [Oct 12 10:00, Oct 19 10:00] -> Oct 12 10:00, Oct 12 20:00, Oct 13.
    # c_2 reach: [Oct 12 15:00, Oct 19 15:00] -> Oct 12 20:00, Oct 13.
    # Credit: Oct 2 and 4 -> k1; Oct 6 -> k2; Oct 12 10:00 -> k3; Oct 12 20:00 and Oct 13 -> k4 (c_2).
    user=["u1,2026-10-02,1,1,0,", "u1,2026-10-04,1,1,0,", "u1,2026-10-06,1,2,1000,shoes", "u1,2026-10-12,2,2,0,",
          "u1,2026-10-13,1,1,0,"],
    camp=["c_1,search,2026-10-02,1,0,0,1", "c_1,search,2026-10-04,1,0,0,1", "c_1,search,2026-10-06,1,1,1000,1",
          "c_1,search,2026-10-12,1,0,0,2", "c_1,search,2026-10-13,0,0,0,1",
          "c_2,social,2026-10-12,1,0,0,1", "c_2,social,2026-10-13,1,0,0,1"],
)

# ---- X9: click_id ties are byte-wise (uppercase sorts first, "k10" < "k9"); quoted copies of one click.
case(
    "X9_click_id_ties",
    events=[ev("e1", "u1", "2026-10-03T10:00:00Z", ING)],
    clicks=[
        "k9,u1,c_2,2026-10-03T09:00:00Z",
        '"k10","u1","c_1",2026-10-03T09:00:00Z',
        'k10,u1,c_1,"2026-10-03T09:00:00Z"',  # a duplicate of k10
        "kk,u1,c_2,2026-10-03T09:00:00Z",
    ],
    # smallest click_id among k9, k10, kk is "k10" -> c_1; both campaigns assist.
    user=["u1,2026-10-03,1,1,0,"],
    camp=["c_1,search,2026-10-03,1,0,0,1", "c_2,social,2026-10-03,0,0,0,1"],
)

# ---- X10: link dirt: an upper-case side makes the name; a chain of 300 IDs in shuffled order.
rnd = random.Random(7)
chain = [f"w{i:04d}" for i in range(1, 301)]
order = chain[:]
rnd.shuffle(order)
chain_links = [f"{order[i]},{order[i + 1]}" for i in range(len(order) - 1)]
rnd.shuffle(chain_links)
case(
    "X10_link_names_and_chains",
    events=[
        ev("e1", "u5", "2026-10-03T10:00:00Z", ING),
        ev("e2", "u9", "2026-10-03T10:00:00Z", ING),
        ev("e3", "w0150", "2026-10-03T10:00:00Z", ING),
        ev("e4", "w0299", "2026-10-03T10:10:00Z", ING),
        ev("e5", "w0300", "2026-10-03T10:20:00Z", ING),
    ],
    links=["u5,U9", "U9,u5", "u5,u5", '"U9",u5'] + chain_links,
    # {u5, U9} is named U9 (upper case sorts before lower case); u9 is another person.
    # The chain is one person named w0001, which no event uses.
    user=["U9,2026-10-03,1,1,0,", "u9,2026-10-03,1,1,0,", "w0001,2026-10-03,1,3,0,"],
    camp=["organic,organic,2026-10-03,3,0,0,0"],
)

# ---- X11: files: no trailing newline, header only, empty file, a non-csv file that must be ignored.
case(
    "X11_file_shapes",
    ev_files={
        "a.csv": EV_H + "\n" + ev("e1", "u1", "2026-10-03T10:00:00Z", ING) + "\n" + ev("e2", "u1", "2026-10-03T10:30:00Z", ING),
        "b.csv": EV_H + "\n",
        "c.csv": EV_H,
        "d.csv": "",
        "e.csv": EV_H + "\n" + ev("e3", "u2", "2026-10-03T10:00:00Z", ING) + "\n\n",
        "notes.txt": EV_H + "\n" + ev("e9", "u9", "2026-10-03T10:00:00Z", ING) + "\n",
    },
    ck_files={"x.csv": CK_H, "y.csv": ""},
    lk_files={"z.csv": LK_H + "\nu2,u1", "w.csv": ""},
    user=["u1,2026-10-03,1,3,0,"],
    camp=["organic,organic,2026-10-03,1,0,0,0"],
)

# ---- X12: the day's top category sums over all sessions of the day; a later day with no purchase has none.
case(
    "X12_top_category_over_sessions",
    events=[
        ev("e1", "u1", "2026-10-03T09:00:00Z", ING, "purchase", "p1", "400"),
        ev("e2", "u1", "2026-10-03T15:00:00Z", ING, "purchase", "p2", "300"),
        ev("e3", "u1", "2026-10-03T15:01:00Z", ING, "purchase", "p2", "300"),
        ev("e4", "u1", "2026-10-04T09:00:00Z", "2026-10-04T10:00:00Z", "cart", "p1", "77"),
    ],
    user=["u1,2026-10-03,2,3,1000,books", "u1,2026-10-04,1,1,0,"],
    camp=["organic,organic,2026-10-03,2,2,1000,0", "organic,organic,2026-10-04,1,0,0,0"],
)

# ---- X13: February 29, an offset that moves the day, an impossible leap day.
case(
    "X13_leap_day",
    manifest='{"window_start": "2028-02-28T00:00:00Z", "window_end": "2028-03-02T00:00:00Z"}',
    events=[
        ev("e1", "a", "2028-02-29T23:59:59Z", "2028-03-01T00:00:00Z"),
        ev("e2", "b", "2028-02-29T10:00:00+05:30", "2028-02-29T10:00:00Z"),  # 04:30Z
        ev("e3", "c", "2028-03-01T00:00:00Z", "2028-03-01T00:00:01Z"),
        ev("e4", "d", "2028-02-30T10:00:00Z", "2028-03-01T00:00:01Z"),
        ev("e5", "e", "2028-03-01T23:00:00-02:00", "2028-03-02T01:00:00Z"),  # 2028-03-02T01:00Z: window end passed
    ],
    user=["a,2028-02-29,1,1,0,", "b,2028-02-29,1,1,0,", "c,2028-03-01,1,1,0,"],
    camp=["organic,organic,2028-02-29,2,0,0,0", "organic,organic,2028-03-01,1,0,0,0"],
)

# ---- X14: a window that does not start at midnight; click window edges in milliseconds.
ws = ms(2026, 10, 1, 6, 30)
we = ms(2026, 10, 3, 18, 0)
case(
    "X14_odd_window",
    manifest='{"window_start": "2026-10-01T06:30:00Z", "window_end": "2026-10-03T18:00:00Z"}',
    events=[
        ev("e1", "u1", "2026-10-01T06:29:59Z", "2026-10-01T07:00:00Z"),  # out
        ev("e2", "u2", "2026-10-01T06:30:00Z", "2026-10-01T07:00:00Z"),  # in
        ev("e3", "u3", "2026-10-03T17:59:59Z", "2026-10-03T18:30:00Z"),  # in
        ev("e4", "u4", "2026-10-03T18:00:00Z", "2026-10-03T18:30:00Z"),  # out
    ],
    clicks=[
        f"k1,u2,c_1,{ws - 7 * 86400000}",  # exactly window_start - 7 d: valid, and 7 d before u2's session
        f"k2,u3,c_2,{ws - 7 * 86400000 - 1}",  # 1 ms earlier: dropped
        f"k3,u3,c_2,{we}",  # window end: dropped
        f"k4,u3,c_1,{we - 1}",  # valid, but after u3's session start: no credit, no assist
    ],
    user=["u2,2026-10-01,1,1,0,", "u3,2026-10-03,1,1,0,"],
    camp=["c_1,search,2026-10-01,1,0,0,1", "organic,organic,2026-10-03,1,0,0,0"],
)

# ---- X15: quoted and unquoted IDs, event types and prices with spaces/case, \r inside a quoted field.
case(
    "X15_quotes_spaces",
    events=[
        '"e1","u1","2026-10-03T10:00:00Z","2026-10-04T00:00:00Z","purchase","p1","1000"',
        'e2," u1",2026-10-03T10:01:00Z,2026-10-04T00:00:00Z,view,p1,',  # " u1" is another ID
        'e3,u1,2026-10-03T10:02:00Z,2026-10-04T00:00:00Z,"view ",p1,',  # type with a space: dropped
        'e4,u1,2026-10-03T10:03:00Z,2026-10-04T00:00:00Z,view,"p1,',  # unbalanced quote: dropped
        'e5,u1,2026-10-03T10:04:00Z,2026-10-04T00:00:00Z,view,p"1",',  # quote inside a field: dropped
        'e6,u1,2026-10-03T10:05:00Z,2026-10-04T00:00:00Z,view,p1,"abc"',  # price ignored for a view
        'e7,u1,2026-10-03T10:06:00Z,2026-10-04T00:00:00Z,cart,p2,-1',
    ],
    user=["u1,2026-10-03,1,3,1000,shoes", " u1,2026-10-03,1,1,0,"],
    camp=["organic,organic,2026-10-03,2,1,1000,0"],
)

print("fixtures written")

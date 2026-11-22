#!/usr/bin/env python3
"""Make a rule-abiding but unusual dataset from a generated one.

  mutate.py --base dev --variant id-long --out DIR [--workers N]

The base dataset is read-only. A variant is a list of transforms (VARIANTS below); every transform only produces
inputs that rules.md allows, so the expected tables come from the oracle run on the *output* (bench/ood/make.sh).
Streams line by line, one process per input file. Written for the tools image (Python 3.13, nothing else needed)."""
import argparse
import hashlib
import json
import multiprocessing as mp
import os
import re
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "oracle"))
sys.path.insert(0, os.path.join(HERE, ".."))
from rules import parse_ts, split_fields  # noqa: E402
from datasets import BASES  # noqa: E402

DAY = 86_400_000
B62 = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
B36 = "0123456789abcdefghijklmnopqrstuvwxyz"
M64 = (1 << 64) - 1


# ----------------------------------------------------------------------------------------------- small helpers
def H(s, salt):
    return int.from_bytes(hashlib.blake2b(s.encode("utf-8", "surrogateescape"), digest_size=8,
                                          person=salt.encode()[:16]).digest(), "little")


def b36(n):
    if n == 0:
        return "0"
    out = []
    while n:
        n, r = divmod(n, 36)
        out.append(B36[r])
    return "".join(reversed(out))


def b62(n, width):
    out = []
    for _ in range(width):
        n, r = divmod(n, 62)
        out.append(B62[r])
    return "".join(reversed(out))


def civil_from_days(z):
    z += 719468
    era = (z if z >= 0 else z - 146096) // 146097
    doe = z - era * 146097
    yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
    y = yoe + era * 400
    doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
    mp_ = (5 * doy + 2) // 153
    d = doy - (153 * mp_ + 2) // 5 + 1
    m = mp_ + (3 if mp_ < 10 else -9)
    return y + (m <= 2), m, d


def days_from_civil(y, m, d):
    y -= m <= 2
    era = (y if y >= 0 else y - 399) // 400
    yoe = y - era * 400
    doy = (153 * (m + (-3 if m > 2 else 9)) + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def fmt_f1(ms):
    days, rem = divmod(ms // 1000, 86400)
    y, m, d = civil_from_days(days)
    return "%04d-%02d-%02dT%02d:%02d:%02dZ" % (y, m, d, rem // 3600, rem % 3600 // 60, rem % 60)


def fmt_f2(ms, off_text):
    sign = 1 if off_text[0] == "+" else -1
    off = sign * (int(off_text[1:3]) * 60 + int(off_text[4:6])) * 60_000
    return fmt_f1(ms + off)[:-1] + off_text


def retime(text, fn):
    """Apply fn(ms)->ms to a timestamp text, keeping its form. Bad timestamps are returned unchanged."""
    ms = parse_ts(text)
    if ms is None or ms >= (1 << 62):
        return text
    new = fn(ms)
    if text.endswith("Z"):
        return fmt_f1(new)
    if text[-6:-5] in "+-" and ":" in text[-3:] and "T" in text:
        return fmt_f2(new, text[-6:])
    s = str(new)
    return s.zfill(len(text)) if text.startswith("0") else s


# ------------------------------------------------------------------------------------- line (de)serialisation
def parse_line(line, n):
    """-> (values, quoted flags) or None for a malformed line. Same dialect as rules.md."""
    parts = line.split(",")
    if len(parts) != n:
        return None
    vals, qs = [], []
    for p in parts:
        if p.startswith('"'):
            if len(p) < 2 or not p.endswith('"') or '"' in p[1:-1]:
                return None
            vals.append(p[1:-1]); qs.append(True)
        else:
            if '"' in p:
                return None
            vals.append(p); qs.append(False)
    return vals, qs


def render(vals, qs):
    return ",".join('"' + v + '"' if q else v for v, q in zip(vals, qs))


# ---------------------------------------------------------------------------------------------- keys / scramble
def _mix(v):
    v &= M64
    v = ((v ^ (v >> 30)) * 0xBF58476D1CE4E5B9) & M64
    v = ((v ^ (v >> 27)) * 0x94D049BB133111EB) & M64
    return v ^ (v >> 31)


def generator_keys(seed):
    """user, event, click keys of kit/generator for a seed (replays the first draws of Stream::Dataset)."""
    state = _mix(_mix(seed ^ 0x5D588B656C078965) ^ _mix(1) ^ 0)
    draws = []
    for _ in range(8):
        state = (state + 0x9E3779B97F4A7C15) & M64
        draws.append(_mix(state))
    return draws[5], draws[6], draws[7]


MASK40 = (1 << 40) - 1
_M1, _M2 = 0x9E3779B97F4B, 0xD6E8FEB86659
_M1I, _M2I = pow(_M1, -1, 1 << 40), pow(_M2, -1, 1 << 40)


def scramble(value, key):
    x = ((value & MASK40) * _M1 ^ key) & MASK40
    x ^= x >> 17
    return (x * _M2) & MASK40


def unscramble(v, key):
    y = (v * _M2I) & MASK40
    x = y ^ (y >> 17) ^ (y >> 34)
    return ((x ^ key) * _M1I) & MASK40


def feistel64(x, key):
    """A keyed bijection on 64-bit integers."""
    l, r = (x >> 32) & 0xFFFFFFFF, x & 0xFFFFFFFF
    for i in range(6):
        f = ((r * 0x9E3779B1 + key + i * 0x7F4A7C15) & 0xFFFFFFFF)
        f ^= f >> 15
        f = (f * 0x85EBCA6B) & 0xFFFFFFFF
        f ^= f >> 13
        l, r = r, l ^ f
    return (l << 32) | r


def feistel_range(x, lo, hi, key):
    """A keyed bijection on [lo, hi) by cycle walking a Feistel network over the next even power of two."""
    size = hi - lo
    bits = max(2, (size - 1).bit_length())
    bits += bits & 1
    half, mask = bits // 2, (1 << (bits // 2)) - 1
    v = x - lo
    while True:
        l, r = v >> half, v & mask
        for i in range(5):
            f = ((r * 0x9E3779B1 + key + i * 0x7F4A7C15) & 0xFFFFFFFF)
            f ^= f >> 11
            f = (f * 0x85EBCA6B) & 0xFFFFFFFF
            l, r = r, l ^ (f & mask)
        v = (l << half) | r
        if v < size:
            return lo + v


_LOWER = re.compile(r"[a-z][0-9a-z]*")
_UPPER = re.compile(r"[A-Z][0-9A-Z]*")
_ALNUM = re.compile(r"[0-9A-Za-z]+")


def id_int63(s):
    n = 0
    for ch in s:
        n = n * 63 + B62.index(ch) + 1
    return n


def remap_full(s, key):
    if not _ALNUM.fullmatch(s) or len(s) > 10:
        return s if not s else "L" + hashlib.sha1(s.encode("utf-8", "surrogateescape")).hexdigest()[:30]
    v = feistel64(id_int63(s), key)
    body = b62(v, 11)
    length = 11 + (v ^ (v >> 29)) % 14
    pad = b62(_mix(v) | 1, 13)[: length - 11]
    return body + pad


def remap_mild(s, key):
    """Same alphabet and length as the generator's IDs, random value. Case and prefix letter kept."""
    upper = False
    if _LOWER.fullmatch(s):
        pass
    elif _UPPER.fullmatch(s):
        upper, s = True, s.lower()
    else:
        return s
    digits = s[1:]
    if not digits:
        return s.upper() if upper else s
    k = len(digits)
    lo = 36 ** (k - 1) if k > 1 else 0
    n = int(digits, 36)
    out = s[0] + b36(feistel_range(n, lo, 36 ** k, key))
    return out.upper() if upper else out


# -------------------------------------------------------------------------------------------------- transforms
class Ctx:
    """Shared, read-only state for the workers (set before the pool forks)."""
    def __init__(self, base, seed, events, ws, we):
        self.base, self.seed, self.events, self.ws, self.we = base, seed, events, ws, we
        self.keys = generator_keys(seed)
        self.link_ids = []      # sorted distinct IDs in links (when a transform asks for them)
        self.event_users = set()
        self.partners = []


class T:
    needs = ()  # 'link_ids', 'event_users'

    def ev(self, v, q, c):  # may edit in place; return False to drop; may append to c.extra_ev
        return True

    def ck(self, v, q, c):
        return True

    def lk(self, v, q, c):
        return True

    def hdr(self, kind, line):
        return line

    def manifest(self, ws, we):
        return ws, we

    def products(self, rows):
        return rows

    def campaigns(self, rows):
        return rows

    def extra_link_lines(self, c):
        return []


class Remap(T):
    def __init__(self, mild):
        self.mild = mild
        self.f = remap_mild if mild else remap_full

    def _m(self, s, salt):
        return self.f(s, 0x51ED2701 ^ salt)

    def ev(self, v, q, c):
        v[0] = self._m(v[0], 1) if v[0] else v[0]
        v[1] = self._m(v[1], 2) if v[1] else v[1]
        return True

    def ck(self, v, q, c):
        v[0] = self._m(v[0], 3) if v[0] else v[0]
        v[1] = self._m(v[1], 2) if v[1] else v[1]
        return True

    def lk(self, v, q, c):
        v[0] = self._m(v[0], 2) if v[0] else v[0]
        v[1] = self._m(v[1], 2) if v[1] else v[1]
        return True


UNI = ["ü", "ж", "用户", "😀", "～", "", "é", "ß", "𝒳", " ", "İ", "ﬃ"]


class IdShape(T):
    """A quarter of the IDs (by hash, so every file agrees) take an unusual shape. All forms are injective and cannot
    equal an unchanged generator ID."""
    needs = ("link_ids",)

    def __init__(self, kind, ids_too=True):
        self.kind, self.ids_too = kind, ids_too

    def f(self, s, salt="sh"):
        if len(s) < 2 or not _ALNUM.fullmatch(s) or H(s, self.kind + salt) % 4:
            return s
        h = H(s, self.kind + "x")
        k = self.kind
        if k == "long":
            return s + "-" + (hashlib.blake2b(s.encode(), digest_size=64).hexdigest() * 4)[:200 + h % 200]
        if k == "unicode":
            u = UNI[h % len(UNI)]
            return u + s if (h >> 8) & 1 else s + u
        if k == "spaces":
            form = h % 3
            return " " + s if form == 0 else s + " " if form == 1 else (s[:3] + " " + s[3:] if len(s) > 3 else " " + s)
        if k == "digits":
            if not re.fullmatch(r"[0-9a-z]+", s):
                return s
            n = 0
            for ch in s:
                n = n * 37 + B36.index(ch) + 1
            return str(n)
        if k == "mixed":
            if not re.fullmatch(r"[0-9a-z]+", s) or len(s) < 3:
                return s
            chars = [s[0]] + [ch.upper() if (h >> i) & 1 and ch.isalpha() else ch for i, ch in enumerate(s[1:])]
            out = "".join(chars)
            return out if out != s and any(ch.isupper() for ch in out) else s
        if k == "suffix":
            return s + "-" + self.partners[h % len(self.partners)] if self.partners else s
        raise ValueError(k)

    def prepare(self, c):
        if self.kind == "suffix":
            self.partners = [i for i in c.link_ids if H(i, "suffix" + "sh") % 4][:5000]

    def ev(self, v, q, c):
        v[1] = self.f(v[1])
        if self.ids_too and self.kind != "suffix":
            v[0] = self.f(v[0], "e")
        return True

    def ck(self, v, q, c):
        v[1] = self.f(v[1])
        if self.ids_too and self.kind != "suffix":
            v[0] = self.f(v[0], "k")
        return True

    def lk(self, v, q, c):
        v[0], v[1] = self.f(v[0]), self.f(v[1])
        return True


class IdShort(T):
    """1- and 2-character user IDs for the first 3,000 IDs found in links (a dictionary: only a few IDs fit)."""
    needs = ("link_ids",)

    def prepare(self, c):
        pool = [ch for ch in B62] + [a + b for a in B62 for b in B62]
        ids = c.link_ids[:len(pool)]
        self.m = dict(zip(ids, pool))

    def ev(self, v, q, c):
        v[1] = self.m.get(v[1], v[1]); return True

    def ck(self, v, q, c):
        v[1] = self.m.get(v[1], v[1]); return True

    def lk(self, v, q, c):
        v[0] = self.m.get(v[0], v[0]); v[1] = self.m.get(v[1], v[1]); return True


class Quote(T):
    def __init__(self, all_):
        self.all = all_

    def _s(self, q):
        q[:] = [self.all] * len(q)
        return True

    ev = lambda self, v, q, c: self._s(q)
    ck = lambda self, v, q, c: self._s(q)
    lk = lambda self, v, q, c: self._s(q)


class Crlf(T):
    """A \\r ends some lines (or all, header included): it is an ordinary character of the last field."""
    def __init__(self, share, header=False):
        self.share, self.header = share, header

    def _hit(self, v):
        return self.share >= 1 or H("|".join(v), "crlf") % 1000 < self.share * 1000

    def ev(self, v, q, c):
        if self._hit(v):
            v[-1] += "\r"
        return True

    ck = lk = ev

    def hdr(self, kind, line):
        return line + "\r" if self.header else line


class F3Pad(T):
    """Zero-pad F3 timestamps: random length 14..26, or a fixed one."""
    def __init__(self, width=None):
        self.width = width

    def _p(self, s, key):
        if not s.isascii() or not s.isdigit() or len(s) > 20:
            return s
        w = self.width or 14 + H(key, "f3pad") % 13
        return s.zfill(w)

    def ev(self, v, q, c):
        v[2] = self._p(v[2], v[0]); return True

    def ck(self, v, q, c):
        v[3] = self._p(v[3], v[0]); return True


class F3Extremes(T):
    BAD = ["0", "5", "86400000", "1", "0" * 23, "1" + "0" * 25, "9" * 30, "1" + "0" * 40, "00000000000000000000000000000000000001"]

    def ev(self, v, q, c):
        if H(v[0], "f3x") % 100 == 0:
            v[2] = self.BAD[H(v[0], "f3y") % len(self.BAD)]
        return True

    def ck(self, v, q, c):
        if H(v[0], "f3x") % 100 == 0:
            v[3] = self.BAD[H(v[0], "f3y") % len(self.BAD)]
        return True


class AllF3(T):
    def _f(self, s):
        ms = parse_ts(s)
        return str(ms) if ms is not None and ms < (1 << 62) else s

    def ev(self, v, q, c):
        v[2] = self._f(v[2]); return True

    def ck(self, v, q, c):
        v[3] = self._f(v[3]); return True


class OffsetEdges(T):
    OFFS = ["+23:59", "-23:59", "+00:00", "-00:00", "+12:00", "-10:00", "+14:00", "+05:30", "-09:30", "+23:00"]

    def _t(self, s, key, ws_edges):
        h = H(key, "oe")
        if h % 300 == 0:  # an exact window edge, in some form
            ms = ws_edges[(h >> 9) % len(ws_edges)]
            forms = [lambda m: str(m), lambda m: fmt_f1(m - m % 1000), lambda m: fmt_f2(m - m % 1000, "-23:59"),
                     lambda m: fmt_f2(m - m % 1000, "+23:59")]
            return forms[(h >> 20) % 4](ms)
        if s.endswith("Z"):
            ms = parse_ts(s)
            if ms is not None:
                return fmt_f2(ms, self.OFFS[h % len(self.OFFS)])
        return s

    def ev(self, v, q, c):
        v[2] = self._t(v[2], v[0], [c.ws, c.ws - 1, c.we - 1, c.we, c.ws + 1000, c.we - 1000]); return True

    def ck(self, v, q, c):
        v[3] = self._t(v[3], v[0], [c.ws - 7 * DAY, c.ws - 7 * DAY - 1, c.we - 1, c.we, c.ws, c.ws - 7 * DAY + 1000]); return True


class Retime(T):
    """Shift or stretch every timestamp (ts and ingest_ts) and the manifest."""
    def __init__(self, new_ws=None, days=None):
        self.new_ws, self.days = new_ws, days

    def _fn(self, c):
        if self.new_ws is not None:
            d = self.new_ws - c.ws
            return lambda ms: ms + d
        n = self.days
        return lambda ms: c.ws + ((ms - c.ws) * n) // 14

    def ev(self, v, q, c):
        f = self._fn(c)
        v[2] = retime(v[2], f); v[3] = retime(v[3], f); return True

    def ck(self, v, q, c):
        v[3] = retime(v[3], self._fn(c)); return True

    def manifest(self, ws, we):
        if self.new_ws is not None:
            return self.new_ws, self.new_ws + (we - ws)
        return ws, ws + self.days * DAY


class Dups(T):
    needs = ("link_ids",)

    def ev(self, v, q, c):
        h = H(v[0], "dup")
        if h % 100 >= 40 or not v[0]:
            return True
        n = 1 + (h >> 8) % 6
        ing = parse_ts(v[3])
        if ing is None:
            return True
        earliest_bad = (h >> 16) % 50 == 0 and parse_ts(v[2]) is not None
        copies = []
        for i in range(n):
            cv = list(v)
            cv[3] = fmt_f1(ing + (1 + (H(v[0], "d%d" % i) % 600)) * 1000 + i * 1000)
            if (H(v[0], "du%d" % i) % 64 == 0) and c.link_ids:
                cv[1] = c.link_ids[H(v[0], "dv") % len(c.link_ids)]
            copies.append((cv, [bool(H(v[0] + str(i), "dq") & 1)] * 7))
        if earliest_bad:
            v[2] = "2026-10-03 10:00:00"
        c.extra_ev.extend(copies)
        return True

    def ck(self, v, q, c):
        if v[0] and H(v[0], "cdup") % 100 < 20:
            c.extra_ck.append((list(v), [bool(H(v[0] + str(i), "cq") & 1) for i in range(4)]))
        return True


class PriceEdges(T):
    def ev(self, v, q, c):
        if v[4] != "purchase" or H(v[0], "pe") % 3:
            return True
        k = H(v[0], "pf") % 8
        orig = v[6]
        v[6] = ["1", "10000000", "9999999", "10000001", "0", orig.zfill(16) if orig.isdigit() else orig,
                "1".zfill(30), "10000000".zfill(25)][k]
        return True


class CampaignsOdd(T):
    """Campaign IDs of other shapes and 300 channels; clicks follow; 1% of clicks name the literal `organic`."""
    CAMP = re.compile(r"c_([0-9]+)")

    def _m(self, s):
        m = self.CAMP.fullmatch(s)
        if not m:
            return s
        n, k = int(m.group(1)), int(m.group(1)) % 5
        return ["c_%05d" % n, "camp-%d-x" % n, "CAMP_%d" % n, "キャンペーン%d" % n, "c_%d" % n + "y" * 100][k]

    def ck(self, v, q, c):
        if H(v[0], "org") % 100 == 0:
            v[2] = "organic"
        else:
            v[2] = self._m(v[2])
        return True

    def campaigns(self, rows):
        out = []
        for r in rows:
            cid = r[0]
            h = H(cid, "chan")
            r = [self._m(cid), ("chan-%03d" % (h % 300)) if h % 7 else "チャネル%d" % (h % 40)]
            out.append(r)
        return out


class ProductsOdd(T):
    PROD = re.compile(r"p([0-9]+)")

    def _m(self, s):
        m = self.PROD.fullmatch(s)
        if not m:
            return s
        n = int(m.group(1))
        return ["p%d" % (n + 140000), "SKU-%d" % n, "prod_%d_%s" % (n, "z" * 40)][n % 3]

    def ev(self, v, q, c):
        v[5] = self._m(v[5]); return True

    def products(self, rows):
        out = []
        for r in rows:
            h = H(r[0], "cat")
            cat = r[1] + "/%d" % (h % 3) if h % 11 else "catégorie-%d" % (h % 50)
            out.append([self._m(r[0]), cat])
        return out


class NoLinks(T):
    def lk(self, v, q, c):
        return False


class NoClicks(T):
    def ck(self, v, q, c):
        return False


class NoProducts(T):
    def products(self, rows):
        return []


class OneComponent(T):
    """A hub ID linked to 95% of all IDs seen in links and events: one component holds most users."""
    needs = ("link_ids", "event_users")

    def extra_link_lines(self, c):
        ids = sorted(set(c.link_ids) | c.event_users)
        hub = ids[0]
        return [[hub, x] for x in ids[1:] if H(x, "hub") % 20]


class LongChains(T):
    needs = ("link_ids",)

    def extra_link_lines(self, c):
        ids = sorted(c.link_ids, key=lambda s: H(s, "lc"))[: max(1, len(c.link_ids) // 3)]
        out = []
        for i in range(0, len(ids), 1000):
            chunk = ids[i:i + 1000]
            out += [[a, b] for a, b in zip(chunk, chunk[1:])]
        return out


class ChainUsers(T):
    """pct % of the IDs that appear in links get 50..400 aliases; their rows use the aliases evenly; the aliases are
    linked one after the next in a random order."""
    needs = ("link_ids",)

    def __init__(self, pct):
        self.pct = pct

    def prepare(self, c):
        self.chosen = {i for i in c.link_ids if _LOWER.fullmatch(i) and H(i, "cu") % 1000 < self.pct * 10}

    def _alias(self, x, key):
        if x not in self.chosen:
            return x
        r = 50 + H(x, "R") % 351
        a = H(key, "al") % r
        return x if a == 0 else self._name(x, a)

    def _name(self, x, a):
        return "u" + b36(feistel64(id_int63(x) * 512 + a, 0x1234ABCD))

    def ev(self, v, q, c):
        v[1] = self._alias(v[1], v[0]); return True

    def ck(self, v, q, c):
        v[1] = self._alias(v[1], v[0]); return True

    def extra_link_lines(self, c):
        out = []
        for x in sorted(self.chosen):
            r = 50 + H(x, "R") % 351
            names = [x] + [self._name(x, a) for a in range(1, r)]
            names.sort(key=lambda s: H(s, "ord"))
            out += [[a, b] for a, b in zip(names, names[1:])]
            out.append([x, names[0]])
        return out


class Interleave(T):
    """Event and click numbers (the generator's counters behind the IDs) are rotated, so each user's rows no longer
    form a run: users' numbers interleave."""
    def __init__(self):
        pass

    def _rot(self, s, key, k):
        up = s[:1].isupper()
        low = s.lower()
        if not _LOWER.fullmatch(low) or len(low) < 2:
            return s
        v = int(low[1:], 36)
        if v > MASK40:
            return s
        idx = unscramble(v, key)
        if idx >= (1 << k):
            return s
        idx = ((idx << 3) | (idx >> (k - 3))) & ((1 << k) - 1)
        out = low[0] + b36(scramble(idx, key))
        return out.upper() if up else out

    def ev(self, v, q, c):
        k = (c.events * 5 // 4).bit_length()
        v[0] = self._rot(v[0], c.keys[1], k); return True

    def ck(self, v, q, c):
        k = (c.events // 3 * 5 // 4).bit_length()
        v[0] = self._rot(v[0], c.keys[2], k); return True


class ShortLines(T):
    """Short lines: IDs decoded to their counters (base 36, no prefix), every timestamp F3, nothing quoted."""
    def _d(self, s, key):
        low = s
        if not _LOWER.fullmatch(low) or len(low) < 2:
            return s
        v = int(low[1:], 36)
        return b36(unscramble(v, key)) if v <= MASK40 else s

    def ev(self, v, q, c):
        v[0] = self._d(v[0], c.keys[1]); v[1] = self._d(v[1], c.keys[0])
        for i in (2, 3):
            ms = parse_ts(v[i])
            if ms is not None and ms < (1 << 62):
                v[i] = str(ms)
        q[:] = [False] * 7
        return True

    def ck(self, v, q, c):
        v[0] = self._d(v[0], c.keys[2]); v[1] = self._d(v[1], c.keys[0])
        ms = parse_ts(v[3])
        if ms is not None and ms < (1 << 62):
            v[3] = str(ms)
        q[:] = [False] * 4
        return True

    def lk(self, v, q, c):
        v[0] = self._d(v[0], c.keys[0]); v[1] = self._d(v[1], c.keys[0]); q[:] = [False, False]
        return True


# layouts: applied after the per-file pass
LAYOUTS = {"one-file": ("one", None), "many-files": ("many", {"events": 3000, "clicks": 1500, "links": 400}),
           "header-files": ("headers", 1500)}

FEB29 = days_from_civil(2028, 2, 23) * DAY
EPOCHISH = days_from_civil(1970, 1, 11) * DAY
FAR = days_from_civil(9000, 3, 1) * DAY


def VARIANTS():
    return {
        "id-long": ([IdShape("long")], None), "id-unicode": ([IdShape("unicode")], None),
        "id-spaces": ([IdShape("spaces")], None), "id-digits": ([IdShape("digits")], None),
        "id-mixed": ([IdShape("mixed")], None), "id-suffix": ([IdShape("suffix")], None),
        "id-short": ([IdShort()], None),
        "quote-all": ([Quote(True)], None), "quote-none": ([Quote(False)], None),
        "crlf": ([Crlf(0.05)], None), "crlf-all": ([Crlf(1.0, header=True)], None),
        "f3-zeros": ([F3Pad()], None), "f3-zeros20": ([F3Pad(20)], None), "f3-zeros25": ([F3Pad(25)], None),
        "f3-extremes": ([F3Extremes()], None), "all-f3": ([AllF3()], None), "offset-edges": ([OffsetEdges()], None),
        "one-file": ([], "one-file"), "many-files": ([], "many-files"), "header-files": ([], "header-files"),
        "no-links": ([NoLinks()], None), "one-component": ([OneComponent()], None),
        "long-chains": ([LongChains()], None), "chain-users": ([ChainUsers(2)], None),
        "no-clicks": ([NoClicks()], None), "no-products": ([NoProducts()], None),
        "window-25": ([Retime(days=25)], None), "window-40": ([Retime(days=40)], None),
        "feb29": ([Retime(new_ws=FEB29)], None), "shift-epoch": ([Retime(new_ws=EPOCHISH)], None),
        "shift-9000": ([Retime(new_ws=FAR)], None),
        "dups-heavy": ([Dups()], None),
        "price-edges": ([PriceEdges()], None),
        "campaigns-odd": ([CampaignsOdd()], None), "products-odd": ([ProductsOdd()], None),
        "interleave": ([Interleave()], None), "short-lines": ([ShortLines()], None),
        "remap": ([Remap(False)], None), "remap-mild": ([Remap(True)], None),
        # everything structural at once (the 20M / 60M stand-in for the 1M variants)
        "breakers": ([Retime(days=25), ProductsOdd(), CampaignsOdd(), F3Pad(), IdShape("suffix"), ChainUsers(1),
                      PriceEdges()], None),
    }


# ------------------------------------------------------------------------------------------------------- driver
CTX = None
TRANSFORMS = []


def read_header(path):
    with open(path, "r", encoding="utf-8", errors="surrogateescape", newline="\n") as fh:
        return fh.readline()


def convert_file(task):
    kind, src, dst = task
    n = {"events": 7, "clicks": 4, "links": 2}[kind]
    hook = {"events": "ev", "clicks": "ck", "links": "lk"}[kind]
    CTX.extra_ev, CTX.extra_ck = [], []
    hooks = [getattr(t, hook) for t in TRANSFORMS]
    first_line = True
    with open(src, "r", encoding="utf-8", errors="surrogateescape", newline="\n") as fi, \
            open(dst, "w", encoding="utf-8", errors="surrogateescape", newline="\n") as fo:
        for line in fi:
            raw = line[:-1] if line.endswith("\n") else line
            if first_line:
                first_line = False
                for t in TRANSFORMS:
                    raw = t.hdr(kind, raw)
                fo.write(raw + "\n")
                continue
            parsed = parse_line(raw, n)
            if parsed is None:
                if any(isinstance(t, Crlf) and t.share >= 1 for t in TRANSFORMS):
                    raw += "\r"
                fo.write(raw + "\n")
                continue
            v, q = parsed
            keep = True
            for h in hooks:
                if h(v, q, CTX) is False:
                    keep = False
                    break
            if keep:
                fo.write(render(v, q) + "\n")
    extras = CTX.extra_ev if kind == "events" else CTX.extra_ck if kind == "clicks" else []
    if extras:
        hdr = read_header(src).rstrip("\n")
        with open(dst[:-4] + "-dups.csv", "w", encoding="utf-8", errors="surrogateescape", newline="\n") as fo:
            fo.write(hdr + "\n")
            for v, q in extras:
                fo.write(render(v, q) + "\n")
    return dst


def scan_ids(task):
    kind, path = task
    ids, users = set(), set()
    n = {"events": 7, "clicks": 4, "links": 2}[kind]
    with open(path, "r", encoding="utf-8", errors="surrogateescape", newline="\n") as fi:
        next(fi, None)
        for line in fi:
            p = parse_line(line[:-1] if line.endswith("\n") else line, n)
            if p is None:
                continue
            if kind == "links":
                for x in p[0]:
                    if x:
                        ids.add(x)
            else:
                if p[0][1]:
                    users.add(p[0][1])
    return ids, users


def csv_list(folder):
    return sorted(f for f in os.listdir(folder) if f.endswith(".csv"))


def write_rows(path, header, rows):
    with open(path, "w", encoding="utf-8", newline="\n") as fo:
        fo.write(header + "\n")
        for r in rows:
            fo.write(",".join(r) + "\n")


def relayout(out, kind, mode, arg):
    folder = os.path.join(out, kind)
    files = csv_list(folder)
    if mode == "one":
        hdr = read_header(os.path.join(folder, files[0]))
        tmp = os.path.join(folder, "_all.tmp")
        with open(tmp, "w", encoding="utf-8", errors="surrogateescape", newline="\n") as fo:
            fo.write(hdr)
            for f in files:
                with open(os.path.join(folder, f), "r", encoding="utf-8", errors="surrogateescape", newline="\n") as fi:
                    next(fi, None)
                    shutil.copyfileobj(fi, fo)
                os.remove(os.path.join(folder, f))
        os.rename(tmp, os.path.join(folder, "all.csv"))
    elif mode == "many":
        target = arg
        per = max(1, target // len(files) + 1)
        hdr = read_header(os.path.join(folder, files[0]))
        for f in files:
            lines = open(os.path.join(folder, f), "r", encoding="utf-8", errors="surrogateescape", newline="\n").read().split("\n")[1:]
            if lines and lines[-1] == "":
                lines.pop()
            os.remove(os.path.join(folder, f))
            step = max(1, -(-len(lines) // per))
            for i in range(per):
                chunk = lines[i * step:(i + 1) * step]
                with open(os.path.join(folder, "%s.%03d.csv" % (f[:-4], i)), "w", encoding="utf-8", errors="surrogateescape", newline="\n") as fo:
                    fo.write(hdr)
                    for ln in chunk:
                        fo.write(ln + "\n")
    elif mode == "headers":
        hdr = read_header(os.path.join(folder, files[0]))
        for i in range(arg):
            with open(os.path.join(folder, "empty-%04d.csv" % i), "w", encoding="utf-8", newline="\n") as fo:
                fo.write(hdr)


def main():
    global CTX, TRANSFORMS
    ap = argparse.ArgumentParser()
    if "--list" in sys.argv:
        print(" ".join(k for k in VARIANTS() if k != "breakers") + " breakers")
        return
    ap.add_argument("--base", required=True)
    ap.add_argument("--variant", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--data-root", default=os.environ.get("VCV_DATA", os.path.expanduser("~/vcv-data/db")))
    ap.add_argument("--workers", type=int, default=8)
    a = ap.parse_args()
    base = os.path.join(a.data_root, a.base) if not os.path.isabs(a.base) else a.base
    bname = os.path.basename(base.rstrip("/"))
    seed, events, _ = BASES[bname]
    transforms, layout = VARIANTS()[a.variant]
    with open(os.path.join(base, "manifest.json")) as fh:
        m = json.load(fh)
    ws, we = parse_ts(m["window_start"]), parse_ts(m["window_end"])
    CTX = Ctx(base, seed, events, ws, we)
    TRANSFORMS = transforms
    shutil.rmtree(a.out, ignore_errors=True)
    for sub in ("events", "clicks", "links"):
        os.makedirs(os.path.join(a.out, sub))
    pool = mp.get_context("fork").Pool(a.workers)
    needs = {n for t in transforms for n in t.needs}
    if needs:
        jobs = [("links", os.path.join(base, "links", f)) for f in csv_list(os.path.join(base, "links"))]
        if "event_users" in needs:
            jobs += [("events", os.path.join(base, "events", f)) for f in csv_list(os.path.join(base, "events"))]
        link_set = set()
        for ids, users in pool.imap_unordered(scan_ids, jobs):
            link_set |= ids
            CTX.event_users |= users
        CTX.link_ids = sorted(link_set)
    for t in transforms:
        if hasattr(t, "prepare"):
            t.prepare(CTX)
    pool.close(); pool.join()
    pool = mp.get_context("fork").Pool(a.workers)  # fork again so workers see the prepared transforms
    tasks = []
    for kind in ("events", "clicks", "links"):
        for f in sorted(csv_list(os.path.join(base, kind)), key=lambda f: -os.path.getsize(os.path.join(base, kind, f))):
            tasks.append((kind, os.path.join(base, kind, f), os.path.join(a.out, kind, f)))
    for _ in pool.imap_unordered(convert_file, tasks, chunksize=1):
        pass
    pool.close(); pool.join()
    # extra link lines (hubs, chains)
    extra = []
    for t in transforms:
        extra += t.extra_link_lines(CTX)
    if extra:
        extra.sort(key=lambda r: H(r[0] + "|" + r[1], "xl"))
        write_rows(os.path.join(a.out, "links", "zz-extra.csv"), read_header(os.path.join(base, "links", csv_list(os.path.join(base, "links"))[0])).rstrip("\n"), extra)
    # small files
    def read_rows(name):
        with open(os.path.join(base, name), encoding="utf-8", newline="\n") as fh:
            lines = fh.read().split("\n")
        return lines[0], [ln.split(",") for ln in lines[1:] if ln]
    hdr, rows = read_rows("products.csv")
    for t in transforms:
        rows = t.products(rows)
    write_rows(os.path.join(a.out, "products.csv"), hdr, rows)
    hdr, rows = read_rows("campaigns.csv")
    for t in transforms:
        rows = t.campaigns(rows)
    write_rows(os.path.join(a.out, "campaigns.csv"), hdr, rows)
    nws, nwe = ws, we
    for t in transforms:
        nws, nwe = t.manifest(nws, nwe)
    with open(os.path.join(a.out, "manifest.json"), "w") as fh:
        json.dump({"window_start": fmt_f1(nws), "window_end": fmt_f1(nwe)}, fh, indent=2)
    if layout:
        mode, arg = LAYOUTS[layout]
        for kind in ("events", "clicks", "links"):
            relayout(a.out, kind, mode, arg[kind] if isinstance(arg, dict) else arg)
    open(os.path.join(a.out, ".done"), "w").write(a.variant + "\n")


if __name__ == "__main__":
    main()

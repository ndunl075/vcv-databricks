"""Primitive parsing rules of kit/rules.md, shared by both oracle engines.

Deliberately plain: no shortcuts that depend on how the generator writes IDs or files.
Strings are Python str decoded from UTF-8; for valid UTF-8, code-point order equals byte-wise order.
"""
import os
import re

MS_DAY = 86_400_000
SESSION_GAP_MS = 30 * 60 * 1000
CLICK_LOOKBACK_MS = 7 * MS_DAY
LATE_MS = 24 * 60 * 60 * 1000
MAX_PRICE = 10_000_000

_F1F2 = re.compile(r"([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})(Z|[+-][0-9]{2}:[0-9]{2})")
_DIGITS = re.compile(r"[0-9]+")


def _leap(y):
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)


def _days_in_month(y, m):
    if m == 2:
        return 29 if _leap(y) else 28
    return 30 if m in (4, 6, 9, 11) else 31


def _days_from_civil(y, m, d):
    """Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm)."""
    y -= m <= 2
    era = (y if y >= 0 else y - 399) // 400
    yoe = y - era * 400
    doy = (153 * (m + (-3 if m > 2 else 9)) + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def parse_ts(s):
    """Timestamp text -> integer ms since the epoch, or None if it is a bad timestamp (F1, F2 or F3 only)."""
    m = _F1F2.fullmatch(s)
    if m:
        y, mo, d, hh, mi, ss = (int(m.group(i)) for i in range(1, 7))
        if not (1 <= mo <= 12 and 1 <= d <= _days_in_month(y, mo) and hh <= 23 and mi <= 59 and ss <= 59):
            return None
        zone = m.group(7)
        offset_ms = 0
        if zone != "Z":
            oh, om = int(zone[1:3]), int(zone[4:6])
            if oh > 23 or om > 59:
                return None
            offset_ms = (oh * 60 + om) * 60_000 * (1 if zone[0] == "+" else -1)
        return ((_days_from_civil(y, mo, d) * 24 + hh) * 60 + mi) * 60_000 + ss * 1000 - offset_ms
    if _DIGITS.fullmatch(s):
        significant = s.lstrip("0")
        if len(significant) > 19:  # longer than any 64-bit value: valid number, outside every window
            return 1 << 80
        return int(significant or "0")
    return None


def in_window(ts, lo, hi):
    return ts is not None and lo <= ts < hi


def parse_price(s):
    """price_cents text of a purchase -> int in 1..10_000_000, or None. ASCII digits only; leading zeros fine."""
    if not _DIGITS.fullmatch(s):
        return None
    significant = s.lstrip("0")
    if len(significant) > 8:
        return None
    value = int(significant or "0")
    return value if 1 <= value <= MAX_PRICE else None


def split_fields(line, n):
    """One CSV line -> list of n field values, or None if the line is malformed (CSV dialect of rules.md)."""
    parts = line.split(",")
    if len(parts) != n:
        return None
    out = []
    for p in parts:
        if p.startswith('"'):
            if len(p) < 2 or not p.endswith('"'):
                return None
            inner = p[1:-1]
            if '"' in inner:
                return None
            out.append(inner)
        else:
            if '"' in p:
                return None
            out.append(p)
    return out


def read_lines(path):
    """Data lines of a CSV file, streamed: everything after the first line. Only \\n ends a line (newline='\\n' turns
    off universal newlines, so a \\r stays in the field it ends)."""
    with open(path, "r", encoding="utf-8", errors="replace", newline="\n") as fh:
        first = True
        for line in fh:
            if first:
                first = False
                continue
            yield line[:-1] if line.endswith("\n") else line


def csv_files(folder):
    if not os.path.isdir(folder):
        return []
    return sorted(os.path.join(folder, f) for f in os.listdir(folder)
                  if f.endswith(".csv") and os.path.isfile(os.path.join(folder, f)))


def read_manifest(data):
    import json
    with open(os.path.join(data, "manifest.json"), encoding="utf-8") as fh:
        m = json.load(fh)
    ws, we = parse_ts(m["window_start"]), parse_ts(m["window_end"])
    assert ws is not None and we is not None
    return ws, we


def read_small_table(path, n=2):
    """products.csv / campaigns.csv -> dict first column -> second column."""
    out = {}
    for line in read_lines(path):
        f = split_fields(line, n)
        if f is not None:
            out[f[0]] = f[1]
    return out

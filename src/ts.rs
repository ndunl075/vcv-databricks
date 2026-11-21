//! Timestamp and price parsing, exactly as rules.md specifies.

use crate::util::DAY_MS;

#[inline(always)]
fn dig(b: u8) -> Option<u32> {
    let d = b.wrapping_sub(b'0');
    if d < 10 {
        Some(d as u32)
    } else {
        None
    }
}

#[inline(always)]
fn d2(s: &[u8], i: usize) -> Option<u32> {
    Some(dig(s[i])? * 10 + dig(s[i + 1])?)
}

#[inline(always)]
fn d4(s: &[u8], i: usize) -> Option<u32> {
    Some(dig(s[i])? * 1000 + dig(s[i + 1])? * 100 + dig(s[i + 2])? * 10 + dig(s[i + 3])?)
}

#[inline(always)]
fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant).
#[inline(always)]
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`: (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Epoch milliseconds of a timestamp in form F1, F2 or F3; `None` if it is a bad timestamp
/// (including an F3 too long for a 64-bit integer, which can only be outside any window).
#[inline]
pub fn parse_ts(s: &[u8]) -> Option<i64> {
    let n = s.len();
    if n == 0 {
        return None;
    }
    if n >= 5 && s[4] == b'-' {
        return parse_iso(s);
    }
    // F3: ASCII digits only.
    let mut v: u64 = 0;
    for &b in s {
        let d = b.wrapping_sub(b'0');
        if d >= 10 {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(d as u64)?;
    }
    if v > i64::MAX as u64 {
        return None;
    }
    Some(v as i64)
}

fn parse_iso(s: &[u8]) -> Option<i64> {
    let n = s.len();
    if n != 20 && n != 25 {
        return None;
    }
    if s[7] != b'-' || s[10] != b'T' || s[13] != b':' || s[16] != b':' {
        return None;
    }
    let y = d4(s, 0)? as i64;
    let mo = d2(s, 5)? as i64;
    let d = d2(s, 8)? as i64;
    let h = d2(s, 11)? as i64;
    let mi = d2(s, 14)? as i64;
    let se = d2(s, 17)? as i64;
    let off_ms: i64;
    if n == 20 {
        if s[19] != b'Z' {
            return None;
        }
        off_ms = 0;
    } else {
        let sign = match s[19] {
            b'+' => 1i64,
            b'-' => -1i64,
            _ => return None,
        };
        if s[22] != b':' {
            return None;
        }
        let oh = d2(s, 20)? as i64;
        let om = d2(s, 23)? as i64;
        if oh > 23 || om > 59 {
            return None;
        }
        off_ms = sign * (oh * 3600 + om * 60) * 1000;
    }
    if !(1..=12).contains(&mo) || h > 23 || mi > 59 || se > 59 || d < 1 {
        return None;
    }
    let dim = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if leap(y) {
                29
            } else {
                28
            }
        }
    };
    if d > dim {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(days * DAY_MS + ((h * 60 + mi) * 60 + se) * 1000 - off_ms)
}

/// `price_cents` of a purchase: ASCII digits only, value 1 to 10,000,000 (leading zeros allowed).
#[inline]
pub fn parse_price(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in s {
        let d = b.wrapping_sub(b'0');
        if d >= 10 {
            return None;
        }
        v = v * 10 + d as u32;
        if v > 10_000_000 {
            return None;
        }
    }
    if v == 0 {
        None
    } else {
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<i64> {
        parse_ts(s.as_bytes())
    }

    #[test]
    fn forms_agree() {
        let f1 = p("2026-10-03T23:58:10Z").unwrap();
        assert_eq!(f1, 1_791_071_890_000);
        assert_eq!(p("2026-10-03T19:58:10-04:00"), Some(f1));
        assert_eq!(p("1791071890000"), Some(f1));
        assert_eq!(p("0001791071890000"), Some(f1));
        assert_eq!(p("2026-10-04T01:30:00+02:00"), p("2026-10-03T23:30:00Z"));
        assert_eq!(p("2026-10-03T10:00:00-00:00"), p("2026-10-03T10:00:00Z"));
        assert_eq!(p("2026-10-03T10:00:00+23:59"), Some(p("2026-10-03T10:00:00Z").unwrap() - (23 * 60 + 59) * 60_000));
        assert_eq!(p("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(p("86400000"), Some(86_400_000));
        assert_eq!(p("0"), Some(0));
    }

    #[test]
    fn rejections() {
        for bad in [
            "",
            "2026-10-03T10:00:00.000Z",
            "2026-10-03t10:00:00Z",
            "2026-10-03T10:00:00z",
            "2026-10-03 10:00:00",
            "2026-10-03T10:00:00",
            "2026-10-03T10:00:00 Z",
            "2026-13-03T10:00:00Z",
            "2026-00-03T10:00:00Z",
            "2026-02-30T10:00:00Z",
            "2027-02-29T10:00:00Z",
            "2100-02-29T10:00:00Z",
            "2026-10-00T10:00:00Z",
            "2026-10-32T10:00:00Z",
            "2026-04-31T10:00:00Z",
            "2026-10-03T24:00:00Z",
            "2026-10-03T10:60:00Z",
            "2026-10-03T10:00:60Z",
            "2026-10-03T10:00:00+24:00",
            "2026-10-03T10:00:00+05:60",
            "2026-10-03T10:00:00+0200",
            "2026-10-03T10:00:00+02",
            "2026-10-03T10:00:00+02:0",
            "2026-10-03T10:00:00 +02:00",
            "2026-10-03T10:00:00+02:00Z",
            "2026-10-03T10:00:00Z\r",
            "2026-10-03T10:00:00+02:00\r",
            "1791071890000.0",
            "1.79e12",
            "-5",
            "+5",
            "1791071890000 ",
            " 1791071890000",
            "٢٥٠",
            "２０２６-10-03T10:00:00Z",
            "2026-10-03T10:00:0٠Z",
            "99999999999999999999",
            "18446744073709551616",
            "9223372036854775808",
            "2026/10/03T10:00:00Z",
            "2026-10-03T10:00:00-",
            "202-10-03T10:00:00ZZ",
        ] {
            assert_eq!(p(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn calendar() {
        assert!(p("2028-02-29T00:00:00Z").is_some());
        assert!(p("2000-02-29T00:00:00Z").is_some());
        assert!(p("1900-02-29T00:00:00Z").is_none());
        assert_eq!(p("2028-03-01T00:00:00Z").unwrap() - p("2028-02-29T00:00:00Z").unwrap(), DAY_MS);
        assert_eq!(p("2026-10-03T23:59:59-10:00"), p("2026-10-04T09:59:59Z"));
        assert_eq!(p("2026-10-03T00:00:00+12:00"), p("2026-10-02T12:00:00Z"));
        assert_eq!(p("0000-03-01T00:00:00Z").unwrap(), days_from_civil(0, 3, 1) * DAY_MS);
        let (y, m, d) = civil_from_days(days_from_civil(2028, 2, 29));
        assert_eq!((y, m, d), (2028, 2, 29));
        for z in [-800_000i64, -1, 0, 1, 18_000, 20_000, 2_900_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn f3_leading_zeros_and_big() {
        assert_eq!(p("000000000000000000000000000001791071890000"), Some(1_791_071_890_000));
        assert_eq!(p("9223372036854775807"), Some(i64::MAX));
    }

    #[test]
    fn prices() {
        assert_eq!(parse_price(b"250"), Some(250));
        assert_eq!(parse_price(b"0250"), Some(250));
        assert_eq!(parse_price(b"1"), Some(1));
        assert_eq!(parse_price(b"10000000"), Some(10_000_000));
        assert_eq!(parse_price(b"010000000"), Some(10_000_000));
        assert_eq!(parse_price(b"10000001"), None);
        assert_eq!(parse_price(b"0"), None);
        assert_eq!(parse_price(b"000"), None);
        assert_eq!(parse_price(b""), None);
        assert_eq!(parse_price(b"19.99"), None);
        assert_eq!(parse_price(b"-5"), None);
        assert_eq!(parse_price(b"+5"), None);
        assert_eq!(parse_price(b"1000\r"), None);
        assert_eq!(parse_price("２５０".as_bytes()), None);
        assert_eq!(parse_price("٢٥٠".as_bytes()), None);
        assert_eq!(parse_price(b"99999999999999999999999"), None);
        assert_eq!(parse_price(b"000000000000000000000000250"), Some(250));
    }
}

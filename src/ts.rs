//! Time stamps and prices as rules.md defines them.
//!
//! `epoch_ms` accepts the three forms (whole-second UTC, whole-second with a numeric offset,
//! plain epoch milliseconds) and nothing else. Layout checks of the fixed-width forms are done
//! eight bytes at a time.

const MS_PER_SEC: i64 = 1000;
const SEC_PER_DAY: i64 = 86_400;

/// Days of a common year that precede the first of each month.
const BEFORE_MONTH: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const DAYS_IN_MONTH: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

const LOW7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
const BIAS: u64 = 0x7676_7676_7676_7676;
const HIGH: u64 = 0x8080_8080_8080_8080;
const ZEROS: u64 = 0x3030_3030_3030_3030;

/// Bit 7 of each byte is set where that byte of `w` is not an ASCII digit.
#[inline(always)]
fn not_digit(w: u64) -> u64 {
    let x = w ^ ZEROS;
    (((x & LOW7) + BIAS) | x) & HIGH
}

#[inline(always)]
fn word(s: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&s[at..at + 8]);
    u64::from_le_bytes(b)
}

/// Byte mask (0xff per selected byte) from a list of byte positions.
const fn lanes(pos: &[usize]) -> u64 {
    let mut m = 0u64;
    let mut i = 0;
    while i < pos.len() {
        m |= 0xffu64 << (8 * pos[i]);
        i += 1;
    }
    m
}

/// Expected bytes at fixed punctuation lanes, as a word.
const fn fixed(pairs: &[(usize, u8)]) -> u64 {
    let mut m = 0u64;
    let mut i = 0;
    while i < pairs.len() {
        m |= (pairs[i].1 as u64) << (8 * pairs[i].0);
        i += 1;
    }
    m
}

// "YYYY-MM-"  bytes 0..8
const A_DIGITS: u64 = lanes(&[0, 1, 2, 3, 5, 6]);
const A_PUNCT: u64 = lanes(&[4, 7]);
const A_WANT: u64 = fixed(&[(4, b'-'), (7, b'-')]);
// "DDTHH:MM"  bytes 8..16
const B_DIGITS: u64 = lanes(&[0, 1, 3, 4, 6, 7]);
const B_PUNCT: u64 = lanes(&[2, 5]);
const B_WANT: u64 = fixed(&[(2, b'T'), (5, b':')]);
// "HH:MM:SS"  bytes 11..19
const C_PUNCT: u64 = lanes(&[2, 5]);
const C_WANT: u64 = fixed(&[(2, b':'), (5, b':')]);

#[inline(always)]
fn two(s: &[u8], i: usize) -> i64 {
    ((s[i] - b'0') * 10 + (s[i + 1] - b'0')) as i64
}

#[inline(always)]
fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// Days between 1970-01-01 and a calendar date; years 0 to 9999.
#[inline(always)]
fn day_index(year: i64, month: usize, day: i64) -> i64 {
    // Shifting by 2000 years (five whole 400-year cycles) keeps every division on a positive number.
    let y = year + 2000 - 1;
    let before_year = 365 * y + y / 4 - y / 100 + y / 400;
    let leap_day = (month > 2 && is_leap(year)) as i64;
    before_year - 1_449_647 + BEFORE_MONTH[month - 1] + leap_day + day - 1
}

/// Full-form timestamp, 20 bytes (`...Z`) or 25 bytes (`...+HH:MM`); caller checked s[4] == '-'.
fn dated(s: &[u8]) -> Option<i64> {
    let n = s.len();
    let (a, b, c) = (word(s, 0), word(s, 8), word(s, 11));
    if not_digit(a) & A_DIGITS != 0 || (a ^ A_WANT) & A_PUNCT != 0 {
        return None;
    }
    if not_digit(b) & B_DIGITS != 0 || (b ^ B_WANT) & B_PUNCT != 0 {
        return None;
    }
    if not_digit(c) & B_DIGITS != 0 || (c ^ C_WANT) & C_PUNCT != 0 {
        return None;
    }
    let year = two(s, 0) * 100 + two(s, 2);
    let month = two(s, 5);
    let day = two(s, 8);
    let hour = two(s, 11);
    let minute = two(s, 14);
    let second = two(s, 17);
    let shift_sec = if n == 20 {
        if s[19] != b'Z' {
            return None;
        }
        0
    } else {
        let sign = match s[19] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        if s[22] != b':' || !s[20..22].iter().chain(&s[23..25]).all(u8::is_ascii_digit) {
            return None;
        }
        let (oh, om) = (two(s, 20), two(s, 23));
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 3600 + om * 60)
    };
    if month == 0 || month > 12 || day == 0 || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mi = (month - 1) as usize;
    let limit = DAYS_IN_MONTH[mi] + (mi == 1 && is_leap(year)) as i64;
    if day > limit {
        return None;
    }
    let secs = day_index(year, month as usize, day) * SEC_PER_DAY + hour * 3600 + minute * 60 + second - shift_sec;
    Some(secs * MS_PER_SEC)
}

/// Digits only, any number of leading zeros, value must fit a signed 64-bit integer.
fn plain_ms(s: &[u8]) -> Option<i64> {
    let mut i = 0;
    while i + 8 <= s.len() {
        if not_digit(word(s, i)) != 0 {
            return None;
        }
        i += 8;
    }
    if !s[i..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let lead = s.iter().position(|&b| b != b'0').unwrap_or(s.len());
    let sig = &s[lead..];
    if sig.len() > 19 {
        return None;
    }
    let v = sig.iter().fold(0u64, |acc, &b| acc * 10 + (b - b'0') as u64);
    i64::try_from(v).ok()
}

/// Milliseconds since the Unix epoch, or `None` for a bad timestamp.
#[inline]
pub fn epoch_ms(s: &[u8]) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    if (s.len() == 20 || s.len() == 25) && s[4] == b'-' {
        dated(s)
    } else {
        plain_ms(s)
    }
}

/// `price_cents` of a purchase: ASCII digits only, 1 to 10,000,000 once leading zeros are dropped.
#[inline]
pub fn price_cents(s: &[u8]) -> Option<u32> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let lead = s.iter().position(|&b| b != b'0').unwrap_or(s.len());
    let sig = &s[lead..];
    if sig.is_empty() || sig.len() > 8 {
        return None;
    }
    let v = sig.iter().fold(0u32, |acc, &b| acc * 10 + (b - b'0') as u32);
    (v <= 10_000_000).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> Option<i64> {
        epoch_ms(s.as_bytes())
    }

    #[test]
    fn same_instant_three_ways() {
        let want = Some(1_791_071_890_000);
        for s in [
            "2026-10-03T23:58:10Z",
            "2026-10-03T19:58:10-04:00",
            "2026-10-04T01:58:10+02:00",
            "1791071890000",
            "0001791071890000",
            "00000001791071890000",
            "0000000000001791071890000",
        ] {
            assert_eq!(ms(s), want, "{s}");
        }
    }

    #[test]
    fn known_instants() {
        let table: [(&str, i64); 8] = [
            ("1970-01-01T00:00:00Z", 0),
            ("1969-12-31T23:59:59Z", -1000),
            ("2000-03-01T00:00:00Z", 951_868_800_000),
            ("2028-02-29T12:00:00Z", 1_835_438_400_000),
            ("2100-03-01T00:00:00Z", 4_107_542_400_000),
            ("1900-01-01T00:00:00Z", -2_208_988_800_000),
            ("0001-01-01T00:00:00Z", -62_135_596_800_000),
            ("9999-12-31T23:59:59Z", 253_402_300_799_000),
        ];
        for (s, v) in table {
            assert_eq!(ms(s), Some(v), "{s}");
        }
        assert_eq!(ms("0000-01-01T00:00:00Z"), Some(-62_167_219_200_000));
        assert_eq!(ms("0000-03-01T00:00:00Z"), Some(-62_162_035_200_000));
    }

    #[test]
    fn offsets() {
        let z = ms("2026-10-03T10:00:00Z").unwrap();
        assert_eq!(ms("2026-10-03T10:00:00-00:00"), Some(z));
        assert_eq!(ms("2026-10-03T10:00:00+00:00"), Some(z));
        assert_eq!(ms("2026-10-03T10:00:00+23:59"), Some(z - (23 * 60 + 59) * 60_000));
        assert_eq!(ms("2026-10-03T10:00:00-10:30"), Some(z + (10 * 60 + 30) * 60_000));
        assert_eq!(ms("2026-10-03T00:00:00+12:00"), ms("2026-10-02T12:00:00Z"));
        assert_eq!(ms("2026-10-03T23:59:59-10:00"), ms("2026-10-04T09:59:59Z"));
    }

    #[test]
    fn every_rejection() {
        let bad = [
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
            "2026/10/03T10:00:00Z",
            "2026-10-03T10:00:00-",
            "202-10-03T10:00:00ZZ",
            "2026-1O-03T10:00:00Z",
            "2026-10-03T1O:00:00Z",
            "2026-10-03T10:00:0 Z",
            "2026-10-03T10:00:00+0Z:00",
            "2026-10-03T10-00:00Z",
            "2026+10-03T10:00:00Z",
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
            "20261003235810000000",
            "00000000000000000a",
            "12345678a",
        ];
        for b in bad {
            assert_eq!(ms(b), None, "{b:?}");
        }
    }

    #[test]
    fn leap_years() {
        assert!(ms("2028-02-29T00:00:00Z").is_some());
        assert!(ms("2000-02-29T00:00:00Z").is_some());
        assert!(ms("2024-02-29T00:00:00Z").is_some());
        assert!(ms("1900-02-29T00:00:00Z").is_none());
        assert!(ms("2026-02-29T00:00:00Z").is_none());
        assert_eq!(ms("2028-03-01T00:00:00Z").unwrap() - ms("2028-02-29T00:00:00Z").unwrap(), 86_400_000);
        assert_eq!(ms("2027-03-01T00:00:00Z").unwrap() - ms("2027-02-28T00:00:00Z").unwrap(), 86_400_000);
    }

    #[test]
    fn plain_form_edges() {
        assert_eq!(ms("0"), Some(0));
        assert_eq!(ms("86400000"), Some(86_400_000));
        assert_eq!(ms("9223372036854775807"), Some(i64::MAX));
        assert_eq!(ms(&format!("{}1791071890000", "0".repeat(40))), Some(1_791_071_890_000));
        assert_eq!(ms(&"0".repeat(25)), Some(0));
    }

    #[test]
    fn prices() {
        let ok: [(&str, u32); 7] = [("250", 250), ("0250", 250), ("1", 1), ("10000000", 10_000_000), ("010000000", 10_000_000), ("000000000000000000000000250", 250), ("00000001", 1)];
        for (s, v) in ok {
            assert_eq!(price_cents(s.as_bytes()), Some(v), "{s}");
        }
        for s in ["", "0", "000", "10000001", "19.99", "-5", "+5", "1000\r", "99999999999999999999999", "１２", " 5", "5 "] {
            assert_eq!(price_cents(s.as_bytes()), None, "{s:?}");
        }
        assert_eq!(price_cents("２５０".as_bytes()), None);
        assert_eq!(price_cents("٢٥٠".as_bytes()), None);
    }
}

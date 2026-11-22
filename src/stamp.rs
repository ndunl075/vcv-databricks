//! Quick readers for the timestamp shapes nearly every row uses. Anything they do not recognise
//! in full goes to the general reader in `ts`, so the two always agree.

use crate::ids::word;
use crate::ts::epoch_ms;

/// "No valid instant".
pub const NEVER: i64 = i64::MIN;

const LOW7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
const TOP: u64 = 0x8080_8080_8080_8080;
const ALL_ZERO_CHARS: u64 = u64::from_le_bytes(*b"00000000");
/// Added to a byte below 0x80, the top bit comes on exactly when the byte is above 9.
const OVER_NINE: u64 = 0x7676_7676_7676_7676;

/// `HH:MM:SS` minus its digits' character codes is zero at the colons.
const CLOCK_SHAPE: u64 = u64::from_le_bytes(*b"00:00:00");
const CLOCK_LIMITS: u64 = u64::from_le_bytes([0x76, 0x76, 0x7f, 0x76, 0x76, 0x7f, 0x76, 0x76]);
/// `HH:MM` in the low five bytes.
const ZONE_SHAPE: u64 = u64::from_le_bytes(*b"00:00\0\0\0");
const ZONE_LIMITS: u64 = u64::from_le_bytes([0x76, 0x76, 0x7f, 0x76, 0x76, 0x7f, 0x7f, 0x7f]);

/// Two-digit numbers of a word whose bytes are digit values: byte `k` of the result is
/// `10 * byte[k] + byte[k + 1]`.
#[inline(always)]
fn pairs(x: u64) -> u64 {
    x.wrapping_mul(2561) >> 8
}

/// Value of eight digit bytes (already reduced to 0..=9), first digit in the lowest byte.
#[inline(always)]
fn eight_digits(x: u64) -> u64 {
    let x = (x.wrapping_mul(10) + (x >> 8)) & 0x00ff_00ff_00ff_00ff;
    let x = (x.wrapping_mul(100) + (x >> 16)) & 0x0000_ffff_0000_ffff;
    (x.wrapping_mul(10_000) + (x >> 32)) & 0xffff_ffff
}

const SLOTS: usize = 32;

/// Remembers the calendar dates seen lately, so a date is checked and converted once.
pub struct Dates {
    text: [u64; SLOTS],
    day: [u16; SLOTS],
    /// Seconds from the epoch to that date's midnight, or `NEVER` for an impossible date.
    start: [i64; SLOTS],
}

impl Dates {
    pub fn new() -> Dates {
        Dates { text: [0; SLOTS], day: [0; SLOTS], start: [NEVER; SLOTS] }
    }

    /// Midnight of the date written in the ten bytes at `p` (`YYYY-MM-DD`).
    #[inline(always)]
    unsafe fn midnight(&mut self, p: *const u8) -> i64 {
        let text = word(p);
        let day = u16::from_le(std::ptr::read_unaligned(p.add(8) as *const u16));
        let slot = ((day & 15) as usize * 10 + ((day >> 8) & 15) as usize) & (SLOTS - 1);
        if (*self.text.get_unchecked(slot) == text) & (*self.day.get_unchecked(slot) == day) {
            *self.start.get_unchecked(slot)
        } else {
            self.learn(p, slot, text, day)
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn learn(&mut self, p: *const u8, slot: usize, text: u64, day: u16) -> i64 {
        let mut probe = *b"0000-00-00T00:00:00Z";
        probe[..10].copy_from_slice(std::slice::from_raw_parts(p, 10));
        let start = epoch_ms(&probe).map_or(NEVER, |ms| ms.div_euclid(1000));
        self.text[slot] = text;
        self.day[slot] = day;
        self.start[slot] = start;
        start
    }

    /// Milliseconds since the epoch for the timestamp `p..p+n`, or `NEVER`.
    ///
    /// # Safety
    /// `p..p+n` and the 32 bytes at `p` must be readable.
    #[inline(always)]
    pub unsafe fn instant(&mut self, p: *const u8, n: usize) -> i64 {
        if n == 13 {
            let a = word(p) ^ ALL_ZERO_CHARS;
            let b = word(p.add(5)) ^ ALL_ZERO_CHARS;
            if ((((a & LOW7) + OVER_NINE) | a | ((b & LOW7) + OVER_NINE) | b) & TOP) == 0 {
                return (eight_digits(a) * 100_000 + eight_digits(b) % 100_000) as i64;
            }
        } else if n == 20 {
            if (*p.add(19) == b'Z') & (*p.add(10) == b'T') {
                let x = word(p.add(11)) ^ CLOCK_SHAPE;
                let y = pairs(x);
                let (h, m, s) = (y & 0xff, (y >> 24) & 0xff, (y >> 48) & 0xff);
                if ((((x & LOW7) + CLOCK_LIMITS) | x) & TOP == 0) & (h < 24) & (m < 60) & (s < 60) {
                    let day = self.midnight(p);
                    return if day == NEVER { NEVER } else { (day + (h * 3600 + m * 60 + s) as i64) * 1000 };
                }
                return NEVER;
            }
        } else if n == 25 {
            let sign = *p.add(19);
            if (*p.add(10) == b'T') & ((sign == b'+') | (sign == b'-')) {
                let x = word(p.add(11)) ^ CLOCK_SHAPE;
                let y = pairs(x);
                let (h, m, s) = (y & 0xff, (y >> 24) & 0xff, (y >> 48) & 0xff);
                let zx = (word(p.add(20)) & 0xff_ffff_ffff) ^ ZONE_SHAPE;
                let zy = pairs(zx);
                let (zh, zm) = (zy & 0xff, (zy >> 24) & 0xff);
                let digits_ok = ((((x & LOW7) + CLOCK_LIMITS) | x | ((zx & LOW7) + ZONE_LIMITS) | zx) & TOP) == 0;
                if digits_ok & (h < 24) & (m < 60) & (s < 60) & (zh < 24) & (zm < 60) {
                    let day = self.midnight(p);
                    if day == NEVER {
                        return NEVER;
                    }
                    let shift = (zh * 3600 + zm * 60) as i64;
                    let local = day + (h * 3600 + m * 60 + s) as i64;
                    return (if sign == b'+' { local - shift } else { local + shift }) * 1000;
                }
                return NEVER;
            }
        }
        general(p, n)
    }
}

#[cold]
#[inline(never)]
unsafe fn general(p: *const u8, n: usize) -> i64 {
    epoch_ms(std::slice::from_raw_parts(p, n)).unwrap_or(NEVER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick(d: &mut Dates, s: &[u8]) -> Option<i64> {
        let mut v = s.to_vec();
        v.extend_from_slice(&[0x5a; 40]);
        let r = unsafe { d.instant(v.as_ptr(), s.len()) };
        (r != NEVER).then_some(r)
    }

    #[test]
    fn agrees_with_the_general_reader() {
        let mut d = Dates::new();
        let fixed = [
            "2026-10-03T23:58:10Z",
            "2026-10-03T19:58:10-04:00",
            "2026-10-04T01:58:10+02:00",
            "1791071890000",
            "0001791071890000",
            "2026-02-30T10:00:00Z",
            "2027-02-29T10:00:00Z",
            "2028-02-29T10:00:00Z",
            "2026-10-03T24:00:00Z",
            "2026-10-03T10:60:00Z",
            "2026-10-03T10:00:60Z",
            "2026-10-03T10:00:00+24:00",
            "2026-10-03T10:00:00+05:60",
            "2026-10-03T10:00:00+0200",
            "2026-10-03t10:00:00Z",
            "2026-10-03T10:00:00z",
            "2026-10-03T10:00:00-00:00",
            "2026-10-03T10:00:00+23:59",
            "2026-10-03T10:00:00 +2:00",
            "2026-10-03T10:00:0٠Z",
            "12345678a0123",
            "1791071890.00",
            "0000000000000",
            "9999999999999",
            "99999999999999999999",
            "20261003235810000000",
            "0000000000001791071890000",
            "",
            "Z",
        ];
        for s in fixed {
            assert_eq!(quick(&mut d, s.as_bytes()), epoch_ms(s.as_bytes()), "{s:?}");
        }
        assert_eq!(quick(&mut d, b"2026-10-03T23:58:10Z"), Some(1_791_071_890_000));
    }

    #[test]
    fn mutations_agree_too() {
        let mut d = Dates::new();
        let seeds = ["2026-10-03T23:58:10Z", "2026-12-31T23:59:59+12:00", "2028-02-29T00:00:00-10:30", "1791071890000", "2025-01-01T00:00:00Z"];
        let swaps = b"0123456789:-+TZtz .a\0";
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..200_000 {
            let mut s = seeds[round % seeds.len()].as_bytes().to_vec();
            for _ in 0..1 + next() % 3 {
                let at = (next() % s.len() as u64) as usize;
                s[at] = swaps[(next() % swaps.len() as u64) as usize];
            }
            assert_eq!(quick(&mut d, &s), epoch_ms(&s), "{:?}", String::from_utf8_lossy(&s));
        }
    }

    #[test]
    fn many_dates_share_few_slots() {
        let mut d = Dates::new();
        for year in [1970, 1999, 2000, 2026, 2028, 2100] {
            for month in 1..=12 {
                for day in 1..=31 {
                    let s = format!("{year:04}-{month:02}-{day:02}T12:34:56Z");
                    assert_eq!(quick(&mut d, s.as_bytes()), epoch_ms(s.as_bytes()), "{s}");
                    let s = format!("{year:04}-{month:02}-{day:02}T00:00:00+09:30");
                    assert_eq!(quick(&mut d, s.as_bytes()), epoch_ms(s.as_bytes()), "{s}");
                }
            }
        }
    }
}

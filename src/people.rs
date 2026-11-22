//! Linked IDs: which user numbers are one person, and which of a person's IDs names them.

use crate::ids::Names;

pub struct People {
    /// For every user number: the number that stands for the person.
    pub lead: Vec<u32>,
    /// For every lead: the number of the person's byte-wise smallest ID.
    pub named: Vec<u32>,
}

#[inline]
fn top(up: &mut [u32], mut x: u32) -> u32 {
    loop {
        let p = up[x as usize];
        if p == x {
            return x;
        }
        let g = up[p as usize];
        up[x as usize] = g;
        x = g;
    }
}

impl People {
    /// Joins the two numbers of every pair, transitively.
    pub fn join(names: &Names, lists: &[&[[u32; 2]]]) -> People {
        let n = names.count();
        let mut up: Vec<u32> = (0..n as u32).collect();
        for list in lists {
            for &[a, b] in *list {
                let (ra, rb) = (top(&mut up, a), top(&mut up, b));
                if ra != rb {
                    let (keep, fold) = if ra < rb { (ra, rb) } else { (rb, ra) };
                    up[fold as usize] = keep;
                }
            }
        }
        let mut named: Vec<u32> = (0..n as u32).collect();
        for list in lists {
            for pair in *list {
                for &x in pair {
                    let lead = top(&mut up, x);
                    up[x as usize] = lead;
                    let best = named[lead as usize];
                    if best != x {
                        // SAFETY: both numbers were handed out, so their texts exist.
                        let smaller = unsafe { (*names.text(x)).before(&*names.text(best)) };
                        if smaller {
                            named[lead as usize] = x;
                        }
                    }
                }
            }
        }
        People { lead: up, named }
    }

    /// Makes every number up to `n` known; numbers that no link mentions are their own person.
    pub fn cover(&mut self, n: usize) {
        let have = self.lead.len();
        if n > have {
            self.lead.extend(have as u32..n as u32);
            self.named.extend(have as u32..n as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{key_of, spread, Supply};

    #[test]
    fn chains_and_smallest_name() {
        let names = Names::new(16, 1);
        let mut supply = Supply::new();
        let texts: Vec<Vec<u8>> = ["u3", "u9", "u2", "U7", "u10", "x", "y", "u1"].iter().map(|s| [s.as_bytes(), &[0u8; 40]].concat()).collect();
        let lens = [2usize, 2, 2, 2, 3, 1, 1, 2];
        let guard = names.gate.read().unwrap();
        let t = names.table();
        let num: Vec<u32> = texts
            .iter()
            .zip(lens)
            .map(|(b, n)| {
                let k = unsafe { key_of(b.as_ptr(), n) };
                unsafe { t.number(&names, &mut supply, &k, spread(&k), b.as_ptr(), n) }
            })
            .collect();
        drop(guard);
        let pairs = vec![[num[0], num[1]], [num[1], num[2]], [num[3], num[1]], [num[4], num[4]], [num[5], num[6]]];
        let mut p = People::join(&names, &[&pairs]);
        p.cover(names.count());
        let name_of = |i: usize| unsafe { (*names.text(p.named[p.lead[num[i] as usize] as usize])).bytes().to_vec() };
        assert_eq!(name_of(0), b"U7");
        assert_eq!(name_of(1), b"U7");
        assert_eq!(name_of(2), b"U7");
        assert_eq!(name_of(3), b"U7");
        assert_eq!(name_of(4), b"u10");
        assert_eq!(name_of(5), b"x");
        assert_eq!(name_of(6), b"x");
        assert_eq!(name_of(7), b"u1");
        assert_eq!(p.lead[num[0] as usize], p.lead[num[3] as usize]);
        assert_ne!(p.lead[num[0] as usize], p.lead[num[7] as usize]);
    }
}

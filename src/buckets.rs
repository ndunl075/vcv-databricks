//! Append-only record buffers made of fixed-size blocks: growth never copies, and pages of
//! blocks that are never filled are never touched.

pub struct Buckets<T> {
    blocks: Vec<Vec<T>>,
    blk: usize,
    len: usize,
}

impl<T: Copy> Buckets<T> {
    pub fn new(blk: usize) -> Buckets<T> {
        Buckets { blocks: Vec::new(), blk, len: 0 }
    }
    #[inline(always)]
    pub fn push(&mut self, v: T) {
        if let Some(b) = self.blocks.last_mut() {
            if b.len() < b.capacity() {
                b.push(v);
                self.len += 1;
                return;
            }
        }
        self.push_slow(v);
    }
    #[cold]
    #[inline(never)]
    fn push_slow(&mut self, v: T) {
        let mut b = Vec::with_capacity(self.blk);
        b.push(v);
        self.blocks.push(b);
        self.len += 1;
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn into_blocks(self) -> Vec<Vec<T>> {
        self.blocks
    }
    pub fn blocks(&self) -> &[Vec<T>] {
        &self.blocks
    }
}

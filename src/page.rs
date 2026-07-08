use crate::store::{PageId, PAGE_SIZE};

pub struct SlottedPage<'a>(pub &'a [u8; PAGE_SIZE]);

impl<'a> SlottedPage<'a> {
    #[inline] pub fn is_leaf(&self) -> bool { self.0[0] == 1 }
    #[inline] pub fn cell_count(&self) -> usize { u16::from_le_bytes(self.0[1..3].try_into().unwrap()) as usize }
    #[inline] pub fn next_leaf(&self) -> PageId { u64::from_le_bytes(self.0[5..13].try_into().unwrap()) }
    #[inline] pub fn leftmost_child(&self) -> PageId { u64::from_le_bytes(self.0[5..13].try_into().unwrap()) }

    #[inline]
    fn slot_offset(&self, idx: usize) -> usize {
        let b = 16 + idx * 2;
        u16::from_le_bytes(self.0[b..b+2].try_into().unwrap()) as usize
    }

    pub fn get_leaf_key(&self, idx: usize) -> &[u8] {
        let off = self.slot_offset(idx);
        let kl = u16::from_le_bytes(self.0[off..off+2].try_into().unwrap()) as usize;
        &self.0[off+4 .. off+4+kl]
    }

    pub fn get_leaf_value(&self, idx: usize) -> &[u8] {
        let off = self.slot_offset(idx);
        let kl = u16::from_le_bytes(self.0[off..off+2].try_into().unwrap()) as usize;
        let vl = u16::from_le_bytes(self.0[off+2..off+4].try_into().unwrap()) as usize;
        &self.0[off+4+kl .. off+4+kl+vl]
    }

    pub fn get_internal_key(&self, idx: usize) -> &[u8] {
        let off = self.slot_offset(idx);
        let kl = u16::from_le_bytes(self.0[off..off+2].try_into().unwrap()) as usize;
        &self.0[off+2 .. off+2+kl]
    }

    pub fn get_internal_child(&self, idx: usize) -> PageId {
        let off = self.slot_offset(idx);
        let kl = u16::from_le_bytes(self.0[off..off+2].try_into().unwrap()) as usize;
        u64::from_le_bytes(self.0[off+2+kl .. off+10+kl].try_into().unwrap())
    }

    // --- Zero-Copy Binary Search ---

    pub fn leaf_find(&self, key: &[u8]) -> Result<usize, usize> {
        let mut left = 0;
        let mut right = self.cell_count();
        while left < right {
            let mid = left + (right - left) / 2;
            match self.get_leaf_key(mid).cmp(key) {
                std::cmp::Ordering::Less => left = mid + 1,
                std::cmp::Ordering::Greater => right = mid,
                std::cmp::Ordering::Equal => return Ok(mid),
            }
        }
        Err(left)
    }

    pub fn internal_route(&self, key: &[u8]) -> (usize, PageId) {
        let mut left = 0;
        let mut right = self.cell_count();
        while left < right {
            let mid = left + (right - left) / 2;
            if self.get_internal_key(mid) < key { left = mid + 1; }
            else { right = mid; }
        }
        let child_id = if left == 0 { self.leftmost_child() } else { self.get_internal_child(left - 1) };
        (left, child_id)
    }
}
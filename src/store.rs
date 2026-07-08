use std::ops::{Deref, DerefMut};

#[derive(PartialEq, Eq)]
pub enum PageType {
    Leaf = 1,
    Internal = 2,
}

impl From<u8> for PageType {
    fn from(value: u8) -> Self {
        match value {
            1 => PageType::Leaf,
            2 => PageType::Internal,
            _ => unreachable!(),
        }
    }
}

impl From<PageType> for u8 {
    fn from(value: PageType) -> Self {
        match value {
            PageType::Leaf => 1,
            PageType::Internal => 2,
        }
    }
}

pub const NULL_PAGE: PageId = u64::MAX;
pub const PAGE_SIZE: usize = 8192;
pub type PageId = u64;

#[derive(Clone)]
pub struct Page(pub [u8; PAGE_SIZE]);

impl Default for Page {
    fn default() -> Self {
        Page([0u8; PAGE_SIZE])
    }
}

impl Deref for Page {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}
impl DerefMut for Page {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

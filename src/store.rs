use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

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

pub trait PageStore {
    type Error: std::fmt::Debug;

    fn allocate(&mut self) -> Result<PageId, Self::Error>;
    fn read(&mut self, id: PageId) -> Result<[u8; PAGE_SIZE], Self::Error>;
    fn write(&mut self, id: PageId, data: &[u8; PAGE_SIZE]) -> Result<(), Self::Error>;
    fn free(&mut self, id: PageId) -> Result<(), Self::Error>;
}

pub struct MemoryStore {
    pages: HashMap<PageId, [u8; PAGE_SIZE]>,
    next_id: PageId,
}

#[derive(Debug)]
pub enum MemoryStoreError {
    PageNotFound(PageId),
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
            next_id: 0,
        }
    }
}

impl PageStore for MemoryStore {
    type Error = MemoryStoreError;

    fn allocate(&mut self) -> Result<PageId, Self::Error> {
        let id = self.next_id;
        self.next_id += 1;
        self.pages.insert(id, [0u8; PAGE_SIZE]);
        Ok(id)
    }

    fn read(&mut self, id: PageId) -> Result<[u8; PAGE_SIZE], Self::Error> {
        self.pages
            .get(&id)
            .copied()
            .ok_or(MemoryStoreError::PageNotFound(id))
    }

    fn write(&mut self, id: PageId, data: &[u8; PAGE_SIZE]) -> Result<(), Self::Error> {
        self.pages.insert(id, *data);
        Ok(())
    }

    fn free(&mut self, id: PageId) -> Result<(), Self::Error> {
        self.pages.remove(&id);
        Ok(())
    }
}

pub struct FileStore {
    file: File,
    pages: u64,
}

impl FileStore {
    pub fn new(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        let pages = file.metadata()?.len() / PAGE_SIZE as u64;

        Ok(Self { file, pages })
    }

}

impl PageStore for FileStore {
    type Error = std::io::Error;

    fn allocate(&mut self) -> Result<PageId, Self::Error> {
        let id = self.pages;
        self.pages += 1;

        let blank = [0u8; PAGE_SIZE];
        self.write(id, &blank)?;

        Ok(id)
    }

    fn read(&mut self, id: PageId) -> Result<[u8; PAGE_SIZE], Self::Error> {
        let offset = id * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;

        let mut buf = [0u8; PAGE_SIZE];
        self.file.read_exact(&mut buf)?;

        Ok(buf)
    }

    fn write(&mut self, id: PageId, data: &[u8; PAGE_SIZE]) -> Result<(), Self::Error> {
        let offset = id * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;

        self.file.write_all(data)?;
        Ok(())
    }

    fn free(&mut self, id: PageId) -> Result<(), Self::Error> {
        Ok(())
    }
}

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{ArcRwLockReadGuard, ArcRwLockWriteGuard, Mutex, RawRwLock, RwLock};

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
pub type Page = [u8; PAGE_SIZE];

pub trait PageStore {
    type Error: std::fmt::Debug;

    type ReadGuard<'a>: std::ops::Deref<Target = Page>
    where
        Self: 'a;
    type WriteGuard<'a>: std::ops::DerefMut<Target = Page>
    where
        Self: 'a;

    fn read(&self, id: PageId) -> Result<Self::ReadGuard<'_>, Self::Error>;
    fn write(&self, id: PageId) -> Result<Self::WriteGuard<'_>, Self::Error>;
    fn allocate(&self) -> Result<(PageId, Self::WriteGuard<'_>), Self::Error>;
    fn free(&self, id: PageId) -> Result<(), Self::Error>;
}

// ---------------- MemoryStore ----------------

pub struct MemoryStore {
    pages: Mutex<HashMap<PageId, Arc<RwLock<Page>>>>,
    next_id: AtomicU64,
}

#[derive(Debug)]
pub enum MemoryStoreError {
    PageNotFound(PageId),
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            pages: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    fn slot(&self, id: PageId) -> Result<Arc<RwLock<Page>>, MemoryStoreError> {
        self.pages
            .lock()
            .get(&id)
            .cloned()
            .ok_or(MemoryStoreError::PageNotFound(id))
    }
}

impl PageStore for MemoryStore {
    type Error = MemoryStoreError;

    type ReadGuard<'a> = ArcRwLockReadGuard<RawRwLock, Page>;
    type WriteGuard<'a> = ArcRwLockWriteGuard<RawRwLock, Page>;

    fn read(&self, id: PageId) -> Result<Self::ReadGuard<'_>, Self::Error> {
        Ok(self.slot(id)?.read_arc())
    }

    fn write(&self, id: PageId) -> Result<Self::WriteGuard<'_>, Self::Error> {
        Ok(self.slot(id)?.write_arc())
    }

    fn allocate(&self) -> Result<(PageId, Self::WriteGuard<'_>), Self::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(RwLock::new([0u8; PAGE_SIZE]));
        let guard = slot.write_arc();
        self.pages.lock().insert(id, slot);
        Ok((id, guard))
    }

    fn free(&self, id: PageId) -> Result<(), Self::Error> {
        self.pages.lock().remove(&id);
        Ok(())
    }
}

// ---------------- FileStore ----------------

#[derive(Debug)]
pub enum FileStoreError {
    Io(std::io::Error),
}

impl From<std::io::Error> for FileStoreError {
    fn from(e: std::io::Error) -> Self {
        FileStoreError::Io(e)
    }
}

pub struct FileStore {
    file: Mutex<File>,
    cache: Mutex<HashMap<PageId, Arc<RwLock<Page>>>>,
    next_id: AtomicU64,
}

impl FileStore {
    pub fn new(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        let next_id = file.metadata()?.len() / PAGE_SIZE as u64;

        Ok(Self {
            file: Mutex::new(file),
            cache: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(next_id),
        })
    }

    fn load(&self, id: PageId) -> Result<Arc<RwLock<Page>>, FileStoreError> {
        if let Some(slot) = self.cache.lock().get(&id) {
            return Ok(slot.clone());
        }

        let mut buf = [0u8; PAGE_SIZE];
        {
            let mut file = self.file.lock();
            file.seek(SeekFrom::Start(id * PAGE_SIZE as u64))?;
            file.read_exact(&mut buf)?;
        }

        let slot = Arc::new(RwLock::new(buf));
        // Another thread may have loaded this page concurrently; keep
        // whichever slot got inserted first so everyone shares one lock.
        let slot = self.cache.lock().entry(id).or_insert(slot).clone();
        Ok(slot)
    }

    fn flush(&self, id: PageId, data: &Page) -> Result<(), FileStoreError> {
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(id * PAGE_SIZE as u64))?;
        file.write_all(data)?;
        Ok(())
    }
}

/// Write guard that flushes back to disk on drop (write-through).
pub struct FileWriteGuard<'a> {
    store: &'a FileStore,
    id: PageId,
    guard: ArcRwLockWriteGuard<RawRwLock, Page>,
}

impl std::ops::Deref for FileWriteGuard<'_> {
    type Target = Page;
    fn deref(&self) -> &Page {
        &self.guard
    }
}

impl std::ops::DerefMut for FileWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Page {
        &mut self.guard
    }
}

impl Drop for FileWriteGuard<'_> {
    fn drop(&mut self) {
        // Best-effort: a real implementation should surface this error
        // (e.g. poison the store) rather than swallow it silently.
        let _ = self.store.flush(self.id, &self.guard);
    }
}

impl PageStore for FileStore {
    type Error = FileStoreError;

    type ReadGuard<'a> = ArcRwLockReadGuard<RawRwLock, Page>;
    type WriteGuard<'a> = FileWriteGuard<'a>;

    fn read(&self, id: PageId) -> Result<Self::ReadGuard<'_>, Self::Error> {
        Ok(self.load(id)?.read_arc())
    }

    fn write(&self, id: PageId) -> Result<Self::WriteGuard<'_>, Self::Error> {
        let slot = self.load(id)?;
        let guard = slot.write_arc();
        Ok(FileWriteGuard {
            store: self,
            id,
            guard,
        })
    }

    fn allocate(&self) -> Result<(PageId, Self::WriteGuard<'_>), Self::Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(RwLock::new([0u8; PAGE_SIZE]));
        let guard = slot.write_arc();
        self.cache.lock().insert(id, slot);
        Ok((
            id,
            FileWriteGuard {
                store: self,
                id,
                guard,
            },
        ))
    }

    fn free(&self, id: PageId) -> Result<(), Self::Error> {
        self.cache.lock().remove(&id);
        Ok(())
    }
}

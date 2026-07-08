use crate::store::{PAGE_SIZE, Page, PageId, PageStore};
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub trait Medium: Send + Sync {
    type Error: std::fmt::Debug;
    fn read_page(&self, id: PageId) -> Result<Page, Self::Error>;
    fn write_page(&self, id: PageId, data: &Page) -> Result<(), Self::Error>;
    fn allocate_page(&self) -> Result<PageId, Self::Error>;
    fn free_page(&self, id: PageId) -> Result<(), Self::Error>;
}

pub struct InMemoryMedium {
    pages: Mutex<HashMap<PageId, Page>>,
    next_id: AtomicU32,
}

impl InMemoryMedium {
    pub fn new() -> Self {
        Self {
            pages: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(0),
        }
    }
}

#[derive(Debug)]
pub struct InMemoryMediumError(pub String);

impl Medium for InMemoryMedium {
    type Error = InMemoryMediumError;

    fn read_page(&self, id: PageId) -> Result<Page, Self::Error> {
        self.pages
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| InMemoryMediumError(format!("page {id} not found")))
    }

    fn write_page(&self, id: PageId, data: &Page) -> Result<(), Self::Error> {
        self.pages.lock().insert(id, data.clone());
        Ok(())
    }

    fn allocate_page(&self) -> Result<PageId, Self::Error> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel) as PageId;
        self.pages.lock().insert(id, Page::default());
        Ok(id)
    }

    fn free_page(&self, id: PageId) -> Result<(), Self::Error> {
        self.pages.lock().remove(&id);
        Ok(())
    }
}

pub struct ClockReplacer {
    num_frames: usize,
    reference: Vec<bool>,
    pinned: Vec<bool>,
    hand: usize,
}

impl ClockReplacer {
    pub fn new(num_frames: usize) -> Self {
        Self {
            num_frames,
            reference: vec![false; num_frames],
            pinned: vec![false; num_frames],
            hand: 0,
        }
    }

    pub fn pin(&mut self, frame_id: usize) {
        self.pinned[frame_id] = true;
        self.reference[frame_id] = true;
    }

    pub fn unpin(&mut self, frame_id: usize) {
        self.pinned[frame_id] = false;
        // reference bit intentionally left untouched
    }

    pub fn victim(&mut self) -> Option<usize> {
        let mut examined = 0;
        let limit = 2 * self.num_frames; // one lap to clear ref bits, one to evict

        while examined < limit {
            let current = self.hand;
            self.hand = (self.hand + 1) % self.num_frames;

            if self.pinned[current] {
                examined += 1;
                continue;
            } else if self.reference[current] {
                self.reference[current] = false;
            } else {
                self.pinned[current] = true;
                return Some(current);
            }

            examined += 1;
        }
        None
    }
}

struct FrameMeta {
    page_id: Option<PageId>,
    dirty: bool,
}

pub struct BufferPoolManager<M: Medium> {
    medium: M,
    data: Vec<RwLock<Page>>,
    meta: Vec<Mutex<FrameMeta>>,
    pin_counts: Vec<AtomicU32>,
    page_table: Mutex<HashMap<PageId, usize>>,
    free_list: Mutex<Vec<usize>>,
    replacer: Mutex<ClockReplacer>,
}

#[derive(Debug)]
pub enum BpmError<E> {
    BufferFull,
    Medium(E),
}

impl<M: Medium> BufferPoolManager<M> {
    pub fn new(medium: M, num_frames: usize) -> Self {
        Self {
            medium,
            data: (0..num_frames)
                .map(|_| RwLock::new(Page::default()))
                .collect(),
            meta: (0..num_frames)
                .map(|_| {
                    Mutex::new(FrameMeta {
                        page_id: None,
                        dirty: false,
                    })
                })
                .collect(),
            pin_counts: (0..num_frames).map(|_| AtomicU32::new(0)).collect(),
            page_table: Mutex::new(HashMap::new()),
            free_list: Mutex::new((0..num_frames).collect()),
            replacer: Mutex::new(ClockReplacer::new(num_frames)),
        }
    }

    fn fetch_frame(&self, page_id: PageId) -> Result<usize, BpmError<M::Error>> {
        // 1. Keep the Page Table locked during the victim selection
        let mut pt = self.page_table.lock();

        // cache hit
        if let Some(&frame_id) = pt.get(&page_id) {
            self.pin_counts[frame_id].fetch_add(1, Ordering::AcqRel);
            self.replacer.lock().pin(frame_id);
            return Ok(frame_id);
        }

        // cache miss: free list or evict
        let frame_id = if let Some(id) = self.free_list.lock().pop() {
            id
        } else {
            let victim_id = self.replacer.lock().victim().ok_or(BpmError::BufferFull)?;

            let mut meta = self.meta[victim_id].lock();
            if meta.dirty {
                // It's safe to read the old data here because it's unpinned,
                // so no other thread is holding a Write/Read lock on it!
                let old_data = self.data[victim_id].read();
                self.medium
                    .write_page(meta.page_id.unwrap(), &old_data)
                    .map_err(BpmError::Medium)?;
            }
            if let Some(old_id) = meta.page_id {
                pt.remove(&old_id);
            }
            meta.dirty = false;
            meta.page_id = None;
            victim_id
        };

        // 2. CLAIM THE PAGE IN THE TABLE BEFORE DOING I/O!
        pt.insert(page_id, frame_id);

        // 3. Drop the page table lock so other threads can keep working
        drop(pt);

        // 4. Lock the actual Frame Data for writing.
        // If Thread B gets a cache hit on this page, they will politely
        // wait here for us to finish the disk I/O!
        let mut frame_data = self.data[frame_id].write();

        let fresh = self.medium.read_page(page_id).map_err(BpmError::Medium)?;
        *frame_data = fresh;

        self.meta[frame_id].lock().page_id = Some(page_id);
        self.pin_counts[frame_id].store(1, Ordering::Release);
        self.replacer.lock().pin(frame_id);

        Ok(frame_id)
    }

    fn unpin(&self, frame_id: usize, mark_dirty: bool) {
        if mark_dirty {
            self.meta[frame_id].lock().dirty = true;
        }
        let prev = self.pin_counts[frame_id].fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            self.replacer.lock().unpin(frame_id);
        }
    }
}

// ─────────────────────────────────────────────────────────────
// Guards
// ─────────────────────────────────────────────────────────────

pub struct ReadGuard<'a, M: Medium> {
    pool: &'a BufferPoolManager<M>,
    frame_id: usize,
    inner: RwLockReadGuard<'a, Page>,
}
impl<'a, M: Medium> Deref for ReadGuard<'a, M> {
    type Target = Page;
    fn deref(&self) -> &Page {
        &self.inner
    }
}
impl<'a, M: Medium> Drop for ReadGuard<'a, M> {
    fn drop(&mut self) {
        self.pool.unpin(self.frame_id, false);
    }
}

pub struct WriteGuard<'a, M: Medium> {
    pool: &'a BufferPoolManager<M>,
    frame_id: usize,
    inner: RwLockWriteGuard<'a, Page>,
}
impl<'a, M: Medium> Deref for WriteGuard<'a, M> {
    type Target = Page;
    fn deref(&self) -> &Page {
        &self.inner
    }
}
impl<'a, M: Medium> DerefMut for WriteGuard<'a, M> {
    fn deref_mut(&mut self) -> &mut Page {
        &mut self.inner
    }
}
impl<'a, M: Medium> Drop for WriteGuard<'a, M> {
    fn drop(&mut self) {
        self.pool.unpin(self.frame_id, true);
    }
}

// ─────────────────────────────────────────────────────────────
// PageStore impl
// ─────────────────────────────────────────────────────────────

impl<M: Medium> PageStore for BufferPoolManager<M> {
    type Error = BpmError<M::Error>;
    type ReadGuard<'a>
        = ReadGuard<'a, M>
    where
        Self: 'a;
    type WriteGuard<'a>
        = WriteGuard<'a, M>
    where
        Self: 'a;

    fn read(&self, id: PageId) -> Result<Self::ReadGuard<'_>, Self::Error> {
        let frame_id = self.fetch_frame(id)?;
        let inner = self.data[frame_id].read();
        Ok(ReadGuard {
            pool: self,
            frame_id,
            inner,
        })
    }

    fn write(&self, id: PageId) -> Result<Self::WriteGuard<'_>, Self::Error> {
        let frame_id = self.fetch_frame(id)?;
        let inner = self.data[frame_id].write();
        Ok(WriteGuard {
            pool: self,
            frame_id,
            inner,
        })
    }

    fn allocate(&self) -> Result<(PageId, Self::WriteGuard<'_>), Self::Error> {
        let id = self.medium.allocate_page().map_err(BpmError::Medium)?;
        Ok((id, self.write(id)?))
    }

    fn free(&self, id: PageId) -> Result<(), Self::Error> {
        self.medium.free_page(id).map_err(BpmError::Medium)?;

        // Remove it from the cache if it is currently in memory!
        let mut pt = self.page_table.lock();
        if let Some(frame_id) = pt.remove(&id) {
            let mut meta = self.meta[frame_id].lock();
            meta.page_id = None;
            meta.dirty = false;

            // Note: In a production DB, you would also push this frame_id
            // back onto the `free_list` here to recycle the RAM immediately!
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum FileMediumError {
    Io(std::io::Error),
    ShortRead { expected: usize, got: usize },
}

impl From<std::io::Error> for FileMediumError {
    fn from(e: std::io::Error) -> Self {
        FileMediumError::Io(e)
    }
}

pub struct FilePageMedium {
    file: Mutex<File>,
    next_id: AtomicU64,
    free_list: Mutex<Vec<PageId>>,
}

impl FilePageMedium {
    /// Opens (or creates) the backing file. `page_count_hint` is the number
    /// of pages already present on disk from a prior run — pass 0 for a
    /// fresh file. This does NOT recover a free list from disk; see note below.
    pub fn open(path: impl AsRef<Path>, page_count_hint: u64) -> Result<Self, FileMediumError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        Ok(Self {
            file: Mutex::new(file),
            next_id: AtomicU64::new(page_count_hint),
            free_list: Mutex::new(Vec::new()),
        })
    }

    fn offset(id: PageId) -> u64 {
        id * PAGE_SIZE as u64
    }
}

impl Medium for FilePageMedium {
    type Error = FileMediumError;

    fn read_page(&self, id: PageId) -> Result<Page, Self::Error> {
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(Self::offset(id)))?;

        let mut page = Page::default();
        let mut buf = &mut page.0[..];
        let mut total_read = 0;

        // Handle short reads explicitly rather than assuming read() fills the buffer
        while total_read < PAGE_SIZE {
            let n = file.read(&mut buf[total_read..])?;
            if n == 0 {
                return Err(FileMediumError::ShortRead {
                    expected: PAGE_SIZE,
                    got: total_read,
                });
            }
            total_read += n;
        }

        Ok(page)
    }

    fn write_page(&self, id: PageId, data: &Page) -> Result<(), Self::Error> {
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(Self::offset(id)))?;
        file.write_all(&data.0)?;
        Ok(())
    }

    fn allocate_page(&self) -> Result<PageId, Self::Error> {
        // Prefer reusing a freed page slot over growing the file
        if let Some(id) = self.free_list.lock().pop() {
            return Ok(id);
        }

        let id = self.next_id.fetch_add(1, Ordering::AcqRel);

        // Extend the file so the page exists on disk (zeroed) before anyone
        // reads it. Without this, read_page on a never-written id would
        // short-read or fail depending on OS/filesystem behavior.
        let mut file = self.file.lock();
        file.seek(SeekFrom::Start(Self::offset(id)))?;
        file.write_all(&[0u8; PAGE_SIZE])?;

        Ok(id)
    }

    fn free_page(&self, id: PageId) -> Result<(), Self::Error> {
        self.free_list.lock().push(id);
        Ok(())
    }
}

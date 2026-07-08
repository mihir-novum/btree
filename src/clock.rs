use crate::store::{PAGE_SIZE, Page, PageId};
use dashmap::{DashMap, Entry};
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

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

struct FrameMeta {
    page_id: Option<PageId>,
    dirty: bool,
}

pub struct BufferPoolManager<M: Medium> {
    medium: M,
    data: Vec<RwLock<Page>>,
    meta: Vec<Mutex<FrameMeta>>,
    pin_counts: Vec<AtomicU32>,
    ref_bits: Vec<AtomicBool>,
    page_table: DashMap<PageId, usize>,
    free_list: Mutex<Vec<usize>>,
    clock_hand: AtomicUsize,
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
            ref_bits: (0..num_frames).map(|_| AtomicBool::new(false)).collect(),
            page_table: DashMap::new(),
            free_list: Mutex::new((0..num_frames).collect()),
            clock_hand: AtomicUsize::new(0),
        }
    }

    fn get_victim(&self) -> Result<usize, BpmError<M::Error>> {
        // 1. Fast path: The free list (Only used when the DB is first booting up)
        if let Some(id) = self.free_list.lock().pop() {
            self.pin_counts[id].store(1, Ordering::Release);
            return Ok(id);
        }

        let num_frames = self.data.len();
        let mut examined = 0;
        let limit = 2 * num_frames; // Max spins before giving up

        // 2. The Lock-Free CLOCK Sweep
        while examined < limit {
            let current = self.clock_hand.fetch_add(1, Ordering::Relaxed) % num_frames;

            // If pinned, skip
            if self.pin_counts[current].load(Ordering::Acquire) > 0 {
                examined += 1;
                continue;
            }

            // If referenced, clear the bit and give a second chance
            if self.ref_bits[current].swap(false, Ordering::Release) {
                examined += 1;
                continue;
            }

            // Found a potential victim!
            // try_lock prevents deadlocks if another thread is currently doing I/O on it.
            if let Some(mut meta) = self.meta[current].try_lock() {
                // Double-check the pin count inside the lock to be 100% sure
                if self.pin_counts[current].load(Ordering::Acquire) > 0 {
                    examined += 1;
                    continue;
                }

                // WE CLAIMED IT! Evict the old page.
                if meta.dirty {
                    let old_data = self.data[current].read();
                    self.medium
                        .write_page(meta.page_id.unwrap(), &old_data)
                        .map_err(BpmError::Medium)?;
                    meta.dirty = false;
                }

                if let Some(old_id) = meta.page_id {
                    self.page_table.remove(&old_id);
                }

                meta.page_id = None;

                // Pre-pin it so nobody steals it from us while we return it
                self.pin_counts[current].store(1, Ordering::Release);

                return Ok(current);
            }
            examined += 1;
        }

        Err(BpmError::BufferFull)
    }

    fn fetch_frame(&self, page_id: PageId) -> Result<usize, BpmError<M::Error>> {
        // 1. FAST PATH: Lock-free lookup
        if let Some(ref_entry) = self.page_table.get(&page_id) {
            let frame_id = *ref_entry;
            self.pin_counts[frame_id].fetch_add(1, Ordering::AcqRel);
            self.ref_bits[frame_id].store(true, Ordering::Release);
            return Ok(frame_id);
        }

        // 2. CACHE MISS: Prepare a victim frame
        let victim_id = self.get_victim()?;

        // 3. DashMap Entry API (Prevents duplicate loading!)
        match self.page_table.entry(page_id) {
            Entry::Occupied(entry) => {
                // Another thread beat us to the disk and already loaded it!
                // Release the victim frame we claimed back to the free list.
                self.pin_counts[victim_id].store(0, Ordering::Release);
                self.free_list.lock().push(victim_id);

                // Pin the frame that the other thread loaded
                let frame_id = *entry.get();
                self.pin_counts[frame_id].fetch_add(1, Ordering::AcqRel);
                self.ref_bits[frame_id].store(true, Ordering::Release);
                Ok(frame_id)
            }
            Entry::Vacant(entry) => {
                // We are the winner! We get to load the page.
                entry.insert(victim_id);

                // Lock the actual data. If Thread B gets a fast-path cache hit
                // right now, they will wait safely at `RwLock::read()` for us to finish.
                let mut frame_data = self.data[victim_id].write();

                let fresh = self.medium.read_page(page_id).map_err(BpmError::Medium)?;
                *frame_data = fresh;

                self.meta[victim_id].lock().page_id = Some(page_id);
                self.ref_bits[victim_id].store(true, Ordering::Release);

                Ok(victim_id)
            }
        }
    }

    fn unpin(&self, frame_id: usize, mark_dirty: bool) {
        if mark_dirty {
            self.meta[frame_id].lock().dirty = true;
        }
        self.pin_counts[frame_id].fetch_sub(1, Ordering::AcqRel);
    }

    // -------------------------------------------------------------
    // Direct API (Moved from the old Trait)
    // -------------------------------------------------------------

    pub fn read(&self, id: PageId) -> Result<ReadGuard<'_, M>, BpmError<M::Error>> {
        let frame_id = self.fetch_frame(id)?;
        let inner = self.data[frame_id].read();
        Ok(ReadGuard {
            pool: self,
            frame_id,
            inner,
        })
    }

    pub fn write(&self, id: PageId) -> Result<WriteGuard<'_, M>, BpmError<M::Error>> {
        let frame_id = self.fetch_frame(id)?;
        let inner = self.data[frame_id].write();
        Ok(WriteGuard {
            pool: self,
            frame_id,
            inner,
        })
    }

    pub fn allocate(&self) -> Result<(PageId, WriteGuard<'_, M>), BpmError<M::Error>> {
        let id = self.medium.allocate_page().map_err(BpmError::Medium)?;
        Ok((id, self.write(id)?))
    }

    pub fn free(&self, id: PageId) -> Result<(), BpmError<M::Error>> {
        self.medium.free_page(id).map_err(BpmError::Medium)?;

        // Lock-free remove from DashMap!
        if let Some((_, frame_id)) = self.page_table.remove(&id) {
            let mut meta = self.meta[frame_id].lock();
            meta.page_id = None;
            meta.dirty = false;

            // Push back to free list to recycle the RAM instantly
            self.free_list.lock().push(frame_id);
        }
        Ok(())
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
        let buf = &mut page.0[..];
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

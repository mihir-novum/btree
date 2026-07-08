use crate::clock::{BpmError, BufferPoolManager, Medium};
use crate::page::SlottedPage;
use crate::store::{NULL_PAGE, PAGE_SIZE, PageId, PageType};
use std::collections::Bound;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Key(pub Vec<u8>);

// Bytes
impl From<Vec<u8>> for Key {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl From<&[u8]> for Key {
    fn from(v: &[u8]) -> Self {
        Self(v.to_vec())
    }
}

impl<const N: usize> From<[u8; N]> for Key {
    fn from(v: [u8; N]) -> Self {
        Self(v.to_vec())
    }
}

impl<const N: usize> From<&[u8; N]> for Key {
    fn from(v: &[u8; N]) -> Self {
        Self(v.to_vec())
    }
}

// Strings
impl From<String> for Key {
    fn from(s: String) -> Self {
        Self(s.into_bytes())
    }
}

impl From<&str> for Key {
    fn from(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }
}

// Integers
macro_rules! impl_from_int {
    ($($t:ty),* $(,)?) => {
        $(
            impl From<$t> for Key {
                fn from(v: $t) -> Self {
                    Self(v.to_le_bytes().to_vec())
                }
            }
        )*
    };
}

impl_from_int!(
    u8, u16, u32, u64, u128, usize, i8, i16, i32, i64, i128, isize
);

// Floats
macro_rules! impl_from_float {
    ($($t:ty),* $(,)?) => {
        $(
            impl From<$t> for Key {
                fn from(v: $t) -> Self {
                    Self(v.to_le_bytes().to_vec())
                }
            }
        )*
    };
}

impl_from_float!(f32, f64);

// bool
impl From<bool> for Key {
    fn from(v: bool) -> Self {
        Self(vec![v as u8])
    }
}

// char (UTF-8)
impl From<char> for Key {
    fn from(c: char) -> Self {
        let mut buf = [0; 4];
        Self(c.encode_utf8(&mut buf).as_bytes().to_vec())
    }
}

impl Key {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone)]
struct LeafEntry {
    key: Key,
    value: Vec<u8>,
}

struct Leaf {
    entries: Vec<LeafEntry>,
    next: Option<PageId>,
}

impl Leaf {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: None,
        }
    }

    fn find_pos(&self, key: &Key) -> Result<usize, usize> {
        self.entries.binary_search_by(|e| e.key.cmp(key))
    }

    pub fn serialize(&self) -> [u8; PAGE_SIZE] {
        let mut buf = [0u8; PAGE_SIZE];
        buf[0] = 1; // Leaf
        buf[1..3].copy_from_slice(&(self.entries.len() as u16).to_le_bytes());
        buf[5..13].copy_from_slice(&self.next.unwrap_or(u64::MAX).to_le_bytes());

        let mut free = PAGE_SIZE;
        for (i, entry) in self.entries.iter().enumerate() {
            let k = entry.key.as_bytes();
            let v = &entry.value;
            let size = 4 + k.len() + v.len();
            free -= size;

            buf[free..free + 2].copy_from_slice(&(k.len() as u16).to_le_bytes());
            buf[free + 2..free + 4].copy_from_slice(&(v.len() as u16).to_le_bytes());
            buf[free + 4..free + 4 + k.len()].copy_from_slice(k);
            buf[free + 4 + k.len()..free + size].copy_from_slice(v);

            let slot = 16 + i * 2;
            buf[slot..slot + 2].copy_from_slice(&(free as u16).to_le_bytes());
        }
        buf[3..5].copy_from_slice(&(free as u16).to_le_bytes());
        buf
    }

    pub fn deserialize(bytes: &[u8; PAGE_SIZE]) -> Result<Self, &'static str> {
        let page = SlottedPage(bytes);
        let mut entries = Vec::with_capacity(page.cell_count());

        for i in 0..page.cell_count() {
            entries.push(LeafEntry {
                key: Key(page.get_leaf_key(i).to_vec()),
                value: page.get_leaf_value(i).to_vec(),
            });
        }

        let next_val = page.next_leaf();
        Ok(Leaf {
            entries,
            next: if next_val == u64::MAX {
                None
            } else {
                Some(next_val)
            },
        })
    }
}

#[derive(Clone)]
struct Internal {
    keys: Vec<Key>,
    children: Vec<PageId>,
}

impl Internal {
    fn new() -> Self {
        Self {
            keys: Vec::new(),
            children: Vec::new(),
        }
    }

    fn child_index(&self, key: &Key) -> usize {
        match self.keys.binary_search(key) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    pub fn serialize(&self) -> [u8; PAGE_SIZE] {
        let mut buf = [0u8; PAGE_SIZE];
        buf[0] = 2; // Internal
        buf[1..3].copy_from_slice(&(self.keys.len() as u16).to_le_bytes());

        let leftmost = self.children.first().copied().unwrap_or(u64::MAX);
        buf[5..13].copy_from_slice(&leftmost.to_le_bytes());

        let mut free = PAGE_SIZE;
        for i in 0..self.keys.len() {
            let k = self.keys[i].as_bytes();
            let right_child = self.children[i + 1];
            let size = 2 + k.len() + 8;
            free -= size;

            buf[free..free + 2].copy_from_slice(&(k.len() as u16).to_le_bytes());
            buf[free + 2..free + 2 + k.len()].copy_from_slice(k);
            buf[free + 2 + k.len()..free + size].copy_from_slice(&right_child.to_le_bytes());

            let slot = 16 + i * 2;
            buf[slot..slot + 2].copy_from_slice(&(free as u16).to_le_bytes());
        }
        buf[3..5].copy_from_slice(&(free as u16).to_le_bytes());
        buf
    }

    pub fn deserialize(bytes: &[u8; PAGE_SIZE]) -> Result<Self, &'static str> {
        let page = SlottedPage(bytes);
        let mut keys = Vec::with_capacity(page.cell_count());
        let mut children = Vec::with_capacity(page.cell_count() + 1);

        let leftmost = page.leftmost_child();

        if leftmost != u64::MAX {
            children.push(leftmost);
        }

        for i in 0..page.cell_count() {
            keys.push(Key(page.get_internal_key(i).to_vec()));
            children.push(page.get_internal_child(i));
        }

        Ok(Internal { keys, children })
    }
}

enum Node {
    Leaf(Leaf),
    Internal(Internal),
}

impl Node {
    fn key_count(&self) -> usize {
        match self {
            Node::Leaf(leaf) => leaf.entries.len(),
            Node::Internal(internal) => internal.keys.len(),
        }
    }

    fn is_leaf(&self) -> bool {
        matches!(self, Node::Leaf(..))
    }

    fn is_internal(&self) -> bool {
        matches!(self, Node::Internal(..))
    }

    fn serialize(&self) -> [u8; PAGE_SIZE] {
        match self {
            Node::Leaf(leaf) => leaf.serialize(),
            Node::Internal(internal) => internal.serialize(),
        }
    }

    fn deserialize(bytes: &[u8; PAGE_SIZE]) -> Node {
        if PageType::from(bytes[0]) == PageType::Leaf {
            Node::Leaf(Leaf::deserialize(bytes).unwrap())
        } else {
            Node::Internal(Internal::deserialize(bytes).unwrap())
        }
    }
}

impl TryFrom<Node> for Leaf {
    type Error = &'static str;
    fn try_from(value: Node) -> Result<Self, Self::Error> {
        match value {
            Node::Leaf(leaf) => Ok(leaf),
            Node::Internal(_) => Err("Not a leaf"),
        }
    }
}

impl TryFrom<Node> for Internal {
    type Error = &'static str;
    fn try_from(value: Node) -> Result<Self, Self::Error> {
        match value {
            Node::Internal(internal) => Ok(internal),
            Node::Leaf(_) => Err("Not an internal node"),
        }
    }
}

impl From<Leaf> for Node {
    fn from(value: Leaf) -> Self {
        Node::Leaf(value)
    }
}

impl From<Internal> for Node {
    fn from(value: Internal) -> Self {
        Node::Internal(value)
    }
}

pub struct BPlusTree<S: Medium> {
    t: usize,
    root: AtomicU64,
    len: AtomicUsize,
    pool: BufferPoolManager<S>,
}

impl<S: Medium> BPlusTree<S> {
    fn balance_window(&self, parent: &mut Internal, ci: usize, t: usize, max_keys: usize) {
        const SIBLINGS_PER_SIDE: usize = 1;

        let left_bound = ci.saturating_sub(SIBLINGS_PER_SIDE);
        let right_bound = (ci + SIBLINGS_PER_SIDE).min(parent.children.len() - 1);
        let mut window: Vec<usize> = (left_bound..=right_bound).collect();

        let total_entries: usize = window
            .iter()
            .map(|&idx| self.read_node(parent.children[idx]).unwrap().key_count())
            .sum();

        // Safe Proactive Ceilings
        let needs_split = total_entries > window.len() * (max_keys - 1);
        let needs_merge = total_entries < window.len() * t;

        let is_leaf = self
            .read_node(parent.children[window[0]])
            .unwrap()
            .is_leaf();

        if is_leaf {
            let rightmost_next = if let Node::Leaf(leaf) = self
                .read_node(parent.children[*window.last().unwrap()])
                .unwrap()
            {
                leaf.next
            } else {
                unreachable!()
            };

            // ── Pool ─────────────────────────────────────────────────
            let mut pooled: Vec<LeafEntry> = Vec::with_capacity(total_entries);
            for &idx in &window {
                if let Node::Leaf(mut leaf) = self.read_node(parent.children[idx]).unwrap() {
                    pooled.extend(leaf.entries.drain(..));
                }
            }

            // ── Needs split: add new node ─────────────────────────────
            if needs_split {
                let new_leaf_id = self.allocate_leaf().unwrap();
                parent.children.insert(right_bound + 1, new_leaf_id);
                parent
                    .keys
                    .insert(right_bound, pooled.last().unwrap().key.clone()); // Placeholder
                window.push(right_bound + 1);

            // ── Needs merge: remove rightmost node ───────────────────
            } else if needs_merge && window.len() > 1 {
                let remove_idx = *window.last().unwrap();
                let free_page = parent.children.remove(remove_idx);
                parent.keys.remove(remove_idx - 1);
                window.pop();
                self.free(free_page).unwrap();
            }

            // ── Distribute evenly ─────────────────────────────────────
            let n = window.len();
            let chunk = pooled.len() / n;
            let rem = pooled.len() % n;
            let mut start = 0;

            for (i, &idx) in window.iter().enumerate() {
                let end = start + chunk + if i < rem { 1 } else { 0 };

                let next_ptr = if i < window.len() - 1 {
                    Some(parent.children[window[i + 1]])
                } else {
                    rightmost_next
                };

                let leaf = Leaf {
                    entries: pooled[start..end].to_vec(),
                    next: next_ptr,
                };

                self.write_node(parent.children[idx], &Node::Leaf(leaf))
                    .unwrap();
                start = end;
            }

            // ── Update parent separator keys ──────────────────────────
            for i in 0..window.len() - 1 {
                let right_node = self.read_node(parent.children[window[i + 1]]).unwrap();
                if let Node::Leaf(leaf) = right_node {
                    parent.keys[window[i]] = leaf.entries[0].key.clone();
                }
            }
        } else {
            // ── Pool keys + children (record sizes first) ─────────────
            let mut pooled_keys: Vec<Key> = Vec::with_capacity(total_entries);
            let mut pooled_children: Vec<PageId> = Vec::with_capacity(total_entries + window.len());
            let mut node_key_counts: Vec<usize> = Vec::with_capacity(window.len());

            for &idx in &window {
                if let Node::Internal(mut n) = self.read_node(parent.children[idx]).unwrap() {
                    node_key_counts.push(n.keys.len());
                    pooled_keys.extend(n.keys.drain(..));
                    pooled_children.extend(n.children.drain(..));
                }
            }

            // ── Interleave parent separators into key pool ────────────
            let mut interleaved: Vec<Key> = Vec::with_capacity(total_entries + window.len() - 1);
            let mut cursor = 0;
            for i in 0..window.len() {
                let count = node_key_counts[i];
                interleaved.extend_from_slice(&pooled_keys[cursor..cursor + count]);
                cursor += count;
                if i < window.len() - 1 {
                    interleaved.push(parent.keys[window[i]].clone());
                }
            }

            // ── Needs split: add new internal node ────────────────────
            if needs_split {
                let new_node_id = self.allocate_internal().unwrap();
                parent.children.insert(right_bound + 1, new_node_id);
                parent
                    .keys
                    .insert(right_bound, interleaved.last().unwrap().clone());
                window.push(right_bound + 1);

            // ── Needs merge: remove rightmost node ───────────────────
            } else if needs_merge && window.len() > 1 {
                let remove_idx = *window.last().unwrap();
                let free_page = parent.children.remove(remove_idx);
                parent.keys.remove(remove_idx - 1);
                window.pop();
                self.free(free_page).unwrap();
            }

            // ── Distribute evenly ─────────────────────────────────────
            let n = window.len();

            // FIXED BUG: Subtract CEO Keys to prevent Out of Bounds Index Math
            let keys_for_children = interleaved.len() - (n - 1);
            let chunk = keys_for_children / n;
            let rem = keys_for_children % n;
            let mut key_start = 0;
            let mut child_start = 0;

            for (i, &idx) in window.iter().enumerate() {
                let keys_for_node = chunk + if i < rem { 1 } else { 0 };
                let key_end = key_start + keys_for_node;
                let child_end = child_start + keys_for_node + 1;

                let internal = Internal {
                    keys: interleaved[key_start..key_end].to_vec(),
                    children: pooled_children[child_start..child_end].to_vec(),
                };

                self.write_node(parent.children[idx], &Node::Internal(internal))
                    .unwrap();

                // boundary key goes back up to parent as separator
                if i < window.len() - 1 {
                    parent.keys[window[i]] = interleaved[key_end].clone();
                    key_start = key_end + 1; // skip the separator
                }

                child_start = child_end;
            }
        }
    }

    fn leftmost_leaf(&self, mut current_id: PageId) -> PageId {
        loop {
            let node = self.read_node(current_id).unwrap();
            match node {
                Node::Leaf(_) => return current_id,
                Node::Internal(internal) => {
                    current_id = internal.children[0]; // Crab down the left edge
                }
            }
        }
    }

    fn find_leaf(&self, mut current_id: PageId, key: &Key) -> PageId {
        loop {
            let node = self.read_node(current_id).unwrap();
            match node {
                Node::Leaf(_) => return current_id,
                Node::Internal(internal) => {
                    let ci = internal.child_index(key);
                    current_id = internal.children[ci]; // Crab down the exact path
                }
            }
        }
    }
}

impl<M: Medium> BPlusTree<M> {
    /// Fetches a page and deserializes it into a Node in one step.
    #[inline]
    fn read_node(&self, id: PageId) -> Result<Node, BpmError<M::Error>> {
        let guard = self.pool.read(id)?;
        Ok(Node::deserialize(&guard.0))
    }

    /// Serializes a Node and writes it to the Buffer Pool.
    #[inline]
    fn write_node(&self, id: PageId, node: &Node) -> Result<(), BpmError<M::Error>> {
        let mut guard = self.pool.write(id)?;
        guard.0.copy_from_slice(&node.serialize());
        Ok(())
    }

    /// Allocates a new page, serializes the Node into it, and returns the PageId.
    #[inline]
    fn allocate_node(&self, node: Node) -> Result<PageId, BpmError<M::Error>> {
        let (id, mut guard) = self.pool.allocate()?;
        guard.0.copy_from_slice(&node.serialize());
        Ok(id)
    }

    #[inline]
    fn allocate_leaf(&self) -> Result<PageId, BpmError<M::Error>> {
        self.allocate_node(Node::Leaf(Leaf::new()))
    }

    #[inline]
    fn allocate_internal(&self) -> Result<PageId, BpmError<M::Error>> {
        self.allocate_node(Node::Internal(Internal::new()))
    }

    #[inline]
    fn free(&self, page_id: PageId) -> Result<(), BpmError<M::Error>> {
        self.pool.free(page_id)
    }
}

impl<S: Medium> BPlusTree<S> {
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<S: Medium> BPlusTree<S> {
    pub fn new(t: usize, store: BufferPoolManager<S>) -> Result<Self, BpmError<S::Error>> {
        assert!(t >= 2, "Minimum degree must be >= 2");

        let root_node = Node::Leaf(Leaf::new());

        let root = {
            let (id, mut guard) = store.allocate()?;
            guard.0.copy_from_slice(&root_node.serialize());
            id
        };

        Ok(Self {
            t,
            root: AtomicU64::new(root),
            len: AtomicUsize::new(0),
            pool: store,
        })
    }

    pub fn insert(&self, key: Key, value: Vec<u8>) {
        let max_keys = 2 * self.t - 1;

        let (mut current_id, mut current_guard) = loop {
            let id = self.root.load(Ordering::Acquire);
            let guard = self.pool.write(id).unwrap();
            if self.root.load(Ordering::Acquire) == id { break (id, guard); }
        };

        // 1. Zero-Copy Root Split Check
        if SlottedPage(&current_guard.0).cell_count() == max_keys {
            // We MUST split. Now we deserialize to do the heavy math.
            let mut node = Node::deserialize(&current_guard.0);
            let new_root_id = self.allocate_internal().unwrap();

            let (promoted_key, right_child_id, right_node) = match &mut node {
                Node::Leaf(left) => {
                    let right_entries = left.entries.split_off(self.t);
                    let promoted_key = right_entries[0].key.clone();

                    let right_id = self.allocate_leaf().unwrap();
                    let right_leaf = Leaf { entries: right_entries, next: left.next };
                    left.next = Some(right_id);

                    (promoted_key, right_id, Node::Leaf(right_leaf))
                }
                Node::Internal(left) => {
                    let mid = self.t - 1;
                    let promoted_key = left.keys[mid].clone();

                    let mut right_keys = left.keys.split_off(mid);
                    right_keys.remove(0);
                    let right_children = left.children.split_off(mid + 1);

                    let right_id = self.allocate_internal().unwrap();
                    let right_node = Internal { keys: right_keys, children: right_children };

                    (promoted_key, right_id, Node::Internal(right_node))
                }
            };

            current_guard.copy_from_slice(&node.serialize());
            self.write_node(right_child_id, &right_node).unwrap();

            let new_root = Internal {
                keys: vec![promoted_key],
                children: vec![current_id, right_child_id],
            };
            self.write_node(new_root_id, &Node::Internal(new_root)).unwrap();

            self.root.store(new_root_id, Ordering::Release);

            drop(current_guard);
            current_id = new_root_id;
            current_guard = self.pool.write(current_id).unwrap();
        }

        // 2. Monkey Bars Traversal (Fast Lane!)
        let key_bytes = key.as_bytes();

        loop {
            // Peek at the raw bytes
            let is_leaf = SlottedPage(&current_guard.0).is_leaf();

            if is_leaf {
                // We reached the final leaf! Deserialize only this one page to mutate it.
                let mut node = Node::deserialize(&current_guard.0);

                let inserted = if let Node::Leaf(leaf) = &mut node {
                    match leaf.find_pos(&key) {
                        Ok(i) => {
                            leaf.entries[i].value = value.clone();
                            false
                        }
                        Err(i) => {
                            leaf.entries.insert(i, LeafEntry { key: key.clone(), value: value.clone() });
                            true
                        }
                    }
                } else { unreachable!() };

                current_guard.copy_from_slice(&node.serialize());

                if inserted {
                    self.len.fetch_add(1, Ordering::Relaxed);
                }
                break;

            } else {
                // Route through internal node directly via Slotted Bytes! (Zero Allocations)
                let page = SlottedPage(&current_guard.0);
                let (mut ci, mut child_id) = page.internal_route(key_bytes);

                // Zero-Copy Peek at the child!
                let child_is_full = {
                    let child_guard = self.pool.read(child_id).unwrap();
                    SlottedPage(&child_guard.0).cell_count() == max_keys
                };

                // If full, we leave the fast lane temporarily to balance
                if child_is_full {
                    let mut node = Node::deserialize(&current_guard.0);
                    if let Node::Internal(internal) = &mut node {
                        self.balance_window(internal, ci, self.t, max_keys);
                        ci = internal.child_index(&key);
                        child_id = internal.children[ci];
                    }
                    current_guard.copy_from_slice(&node.serialize());
                }

                // Lock coupling: Grab child BEFORE dropping parent
                let next_guard = self.pool.write(child_id).unwrap();
                drop(current_guard);
                current_guard = next_guard;
                current_id = child_id;
            }
        }
    }

    pub fn search(&self, key: &Key) -> Option<Vec<u8>> {
        let (_, mut current_guard) = loop {
            let id = self.root.load(Ordering::Acquire);
            let guard = self.pool.read(id).unwrap();
            if self.root.load(Ordering::Acquire) == id {
                break (id, guard);
            }
        };

        let key_bytes = key.as_bytes();

        loop {
            // ZERO ALLOCATIONS!
            let page = SlottedPage(&current_guard.0);

            if page.is_leaf() {
                return match page.leaf_find(key_bytes) {
                    Ok(i) => Some(page.get_leaf_value(i).to_vec()),
                    Err(_) => None,
                };
            } else {
                let (_, child_id) = page.internal_route(key_bytes);

                let child_guard = self.pool.read(child_id).unwrap();
                drop(current_guard);
                current_guard = child_guard;
            }
        }
    }

    pub fn remove(&self, key: &Key) -> Option<Vec<u8>> {
        let (mut current_id, mut current_guard) = loop {
            let id = self.root.load(Ordering::Acquire);
            let guard = self.pool.write(id).unwrap();
            if self.root.load(Ordering::Acquire) == id { break (id, guard); }
        };

        let mut removed_value = None;
        let key_bytes = key.as_bytes();

        // 1. Monkey Bars Traversal (Fast Lane!)
        loop {
            let is_leaf = SlottedPage(&current_guard.0).is_leaf();

            if is_leaf {
                // We reached the leaf! Deserialize it to remove the entry.
                let mut node = Node::deserialize(&current_guard.0);

                removed_value = if let Node::Leaf(leaf) = &mut node {
                    match leaf.find_pos(key) {
                        Ok(i) => Some(leaf.entries.remove(i).value),
                        Err(_) => None,
                    }
                } else { unreachable!() };

                if removed_value.is_some() {
                    current_guard.copy_from_slice(&node.serialize());
                }
                break;
            } else {
                // Route through internal node directly via Slotted Bytes!
                let page = SlottedPage(&current_guard.0);
                let (mut ci, mut child_id) = page.internal_route(key_bytes);

                // Zero-copy Peek at the child
                let child_is_starving = {
                    let child_guard = self.pool.read(child_id).unwrap();
                    SlottedPage(&child_guard.0).cell_count() <= self.t - 1
                };

                // Leave the fast lane temporarily to balance
                if child_is_starving {
                    let mut node = Node::deserialize(&current_guard.0);
                    if let Node::Internal(internal) = &mut node {
                        self.balance_window(internal, ci, self.t, 2 * self.t - 1);
                        ci = internal.child_index(key);
                        child_id = internal.children[ci];
                    }
                    current_guard.copy_from_slice(&node.serialize());
                }

                let next_guard = self.pool.write(child_id).unwrap();
                drop(current_guard);
                current_guard = next_guard;
                current_id = child_id;
            }
        }

        // Must drop leaf lock before reaching for the root to prevent deadlock!
        drop(current_guard);

        // 2. Bypass empty root (Zero Copy!)
        let root_id = self.root.load(Ordering::Acquire);
        let root_guard = self.pool.write(root_id).unwrap();
        let page = SlottedPage(&root_guard.0);

        if !page.is_leaf() && page.cell_count() == 0 {
            let new_root_id = page.leftmost_child();
            self.root.store(new_root_id, Ordering::Release);
            drop(root_guard);
            // In production, defer freeing via Epoch-Based Reclamation
            // self.bpm.free(root_id).unwrap();
        }

        if removed_value.is_some() {
            self.len.fetch_sub(1, Ordering::Relaxed);
        }

        removed_value
    }

    pub fn iter(&self) -> LeafIter<'_> {
        let root = self.root.load(Ordering::Acquire);
        let leaf_id = self.leftmost_leaf(root);

        // Passing `self` automatically acts as the `&dyn PageProvider`!
        LeafIter::new(self, leaf_id, 0, Bound::Unbounded)
    }

    pub fn range_from(&self, start: &Key, end: Bound<Key>) -> LeafIter<'_> {
        let root = self.root.load(Ordering::Acquire);
        let leaf_id = self.find_leaf(root, start);

        let node = self.read_node(leaf_id).unwrap();
        let pos = if let Node::Leaf(leaf) = node {
            leaf.entries.partition_point(|e| &e.key < start)
        } else {
            0
        };

        LeafIter::new(self, leaf_id, pos, end)
    }
}

impl<S: Medium> BPlusTree<S> {
    pub fn print_tree(&self) {
        let root = self.root.load(Ordering::Acquire);
        let len = self.len.load(Ordering::Acquire);
        println!("BPlusTree {{ t={}, len={} }}", self.t, len);
        self.print_node(root, 0);
    }

    fn print_node(&self, node_id: PageId, depth: usize) {
        let indent = "  ".repeat(depth);
        let node = self.read_node(node_id).unwrap();
        match node {
            Node::Leaf(l) => {
                let keys: Vec<_> = l.entries.iter().map(|e| e.key.clone()).collect();
                println!("{indent}Leaf {keys:?}");
            }
            Node::Internal(n) => {
                println!("{indent}Internal {:?}", n.keys);
                for c in &n.children {
                    self.print_node(*c, depth + 1);
                }
            }
        }
    }
}

trait PageProvider {
    fn fetch_leaf(&self, id: PageId) -> Option<Leaf>;
    fn prefetch_leaf(&self, id: PageId);
}

impl<M: Medium> PageProvider for BPlusTree<M> {
    fn fetch_leaf(&self, id: PageId) -> Option<Leaf> {
        let node = self.read_node(id).ok()?;
        if let Node::Leaf(leaf) = node {
            Some(leaf)
        } else {
            None
        }
    }

    fn prefetch_leaf(&self, id: PageId) {
        // Just calling `read` brings the page into the Buffer Pool.
        // It drops immediately, so it doesn't hold any locks!
        // (In an async DB, you would spawn a background task here)
        let _ = self.pool.read(id);
    }
}

pub struct LeafIter<'a> {
    provider: &'a dyn PageProvider, // <-- Completely hides the Medium!
    current_id: Option<PageId>,
    current_leaf: Option<Leaf>,
    pos: usize,
    end: Bound<Key>,
}

impl<'a> LeafIter<'a> {
    fn new(provider: &'a dyn PageProvider, start_id: PageId, pos: usize, end: Bound<Key>) -> Self {
        LeafIter {
            provider,
            current_id: Some(start_id),
            current_leaf: None,
            pos,
            end,
        }
    }
}

impl<'a> Iterator for LeafIter<'a> {
    type Item = (Key, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // 1. Fetch the leaf if we don't have one
            if self.current_leaf.is_none() {
                let id = self.current_id?;
                let leaf = self.provider.fetch_leaf(id)?;

                // Trigger the load for the next page so it's ready when we need it
                if let Some(next_id) = leaf.next {
                    self.provider.prefetch_leaf(next_id);
                }

                self.current_leaf = Some(leaf);
            }

            let leaf = self.current_leaf.as_ref().unwrap();

            // 2. Yield items from the current leaf
            if self.pos < leaf.entries.len() {
                let e = &leaf.entries[self.pos];

                let out = match &self.end {
                    Bound::Included(key) if e.key <= *key => Some((e.key.clone(), e.value.clone())),
                    Bound::Excluded(key) if e.key < *key => Some((e.key.clone(), e.value.clone())),
                    Bound::Unbounded => Some((e.key.clone(), e.value.clone())),
                    _ => None,
                };

                return if out.is_some() {
                    self.pos += 1;
                    out
                } else {
                    None
                };
            }

            // 3. Move forward
            self.current_id = leaf.next;
            self.current_leaf = None;
            self.pos = 0;
        }
    }
}

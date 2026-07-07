use crate::store::{NULL_PAGE, PAGE_SIZE, PageId, PageStore, PageType};
use std::collections::Bound;

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
                    Self(v.to_be_bytes().to_vec())
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
                    Self(v.to_be_bytes().to_vec())
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

        buf[0] = PageType::Leaf.into();

        let entries = self.entries.len() as u16;
        buf[1..3].copy_from_slice(&entries.to_le_bytes());
        buf[3..11].copy_from_slice(&match self.next {
            Some(p) => p.to_le_bytes(),
            None => NULL_PAGE.to_le_bytes(),
        });

        let mut offset = 11;
        for entry in &self.entries {
            let key_len = entry.key.len();
            let value_len = entry.value.len();

            buf[offset..offset + 2].copy_from_slice(&(key_len as u16).to_be_bytes());
            offset += 2;
            buf[offset..offset + key_len].copy_from_slice(entry.key.as_bytes());
            offset += key_len;
            buf[offset..offset + 2].copy_from_slice(&(value_len as u16).to_be_bytes());
            offset += 2;
            buf[offset..offset + value_len].copy_from_slice(&entry.value);
            offset += value_len;
        }

        buf
    }

    pub fn deserialize(bytes: &[u8; PAGE_SIZE]) -> Result<Self, &'static str> {
        if PageType::from(bytes[0]) != PageType::Leaf {
            return Err("Not a leaf");
        }

        let entries_count = u16::from_le_bytes(bytes[1..3].try_into().unwrap()) as usize;
        let next_page = PageId::from_le_bytes(bytes[3..11].try_into().unwrap());

        let mut offset = 11;
        let mut entries: Vec<LeafEntry> = Vec::with_capacity(entries_count);
        for _ in 0..entries_count {
            let key_len =
                u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            let key: Vec<u8> = Vec::from(&bytes[offset..offset + key_len]);
            offset += key_len;

            let value_len =
                u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;

            let value: Vec<u8> = Vec::from(&bytes[offset..offset + value_len]);
            offset += value_len;

            entries.push(LeafEntry {
                key: Key(key),
                value,
            })
        }

        Ok(Leaf {
            entries,
            next: if next_page == NULL_PAGE {
                None
            } else {
                Some(next_page)
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

        buf[0] = PageType::Internal.into();

        let entries = self.keys.len() as u16;
        buf[1..3].copy_from_slice(&entries.to_be_bytes());

        let mut offset = 3;

        for key in &self.keys {
            let key_len = key.len();

            buf[offset..offset + 2].copy_from_slice(&(key_len as u16).to_be_bytes());
            offset += 2;

            buf[offset..offset + key_len].copy_from_slice(key.as_bytes());
            offset += key_len;
        }

        for children in &self.children {
            buf[offset..offset + 8].copy_from_slice(&children.to_be_bytes());
            offset += 8;
        }

        buf
    }

    pub fn deserialize(bytes: &[u8; PAGE_SIZE]) -> Result<Internal, &'static str> {
        if PageType::from(bytes[0]) != PageType::Internal {
            return Err("Not internal");
        }

        let total_keys = u16::from_be_bytes(bytes[1..3].try_into().unwrap()) as usize;

        let mut keys: Vec<Key> = Vec::with_capacity(total_keys);
        let mut children: Vec<PageId> = Vec::with_capacity(total_keys + 1);

        let mut offset = 3;

        for _ in 0..total_keys {
            let key_len =
                u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            let key: Key = Key(Vec::from(&bytes[offset..offset + key_len]));
            offset += key_len;

            keys.push(key);
        }

        for _ in 0..total_keys + 1 {
            let child = PageId::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap());
            offset += 8;

            children.push(child);
        }

        Ok(Internal { keys, children })
    }
}

enum Node {
    Leaf(Leaf),
    Internal(Internal),
}

impl Node {
    fn first_key<S: PageStore>(&self, store: &mut S) -> Option<Key> {
        match self {
            Node::Leaf(leaf) => leaf.entries.first().map(|e| e.key.clone()),
            Node::Internal(internal) => {
                internal
                    .children
                    .first()
                    .and_then(|&c| match store.read(c) {
                        Ok(b) => Node::deserialize(&b).first_key(store),
                        Err(_) => None,
                    })
            }
        }
    }

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

#[derive(Debug)]
pub struct BPlusTree<S: PageStore> {
    t: usize,
    root: PageId,
    len: usize,
    store: S,
}

impl<S: PageStore> BPlusTree<S> {
    fn split_root_child(store: &mut S, old_root_id: PageId, t: usize) -> (Key, PageId) {
        let mut old_root = Self::read_node(store, old_root_id).unwrap();

        // 1. Mutate old_root and build the new right_node, returning what we need.
        let (promoted_key, right_id, right_node) = match &mut old_root {
            Node::Leaf(left) => {
                let right_entries = left.entries.split_off(t);
                let promoted_key = right_entries[0].key.clone();

                let right_id = Self::allocate_leaf(store).unwrap();
                let right_leaf = Leaf {
                    entries: right_entries,
                    next: left.next,
                };

                left.next = Some(right_id);

                (promoted_key, right_id, Node::Leaf(right_leaf))
            }
            Node::Internal(left) => {
                let mid = t - 1;
                let promoted_key = left.keys[mid].clone();

                let mut right_keys = left.keys.split_off(mid);
                right_keys.remove(0); // Removing the promoted key entirely
                let right_children = left.children.split_off(mid + 1);

                let right_id = Self::allocate_internal(store).unwrap();
                let right_node = Internal {
                    keys: right_keys,
                    children: right_children,
                };

                (promoted_key, right_id, Node::Internal(right_node))
            }
        };

        // 2. The mutable borrow on old_root is now GONE. We can safely write it!
        Self::write_node(store, old_root_id, &old_root).unwrap();
        Self::write_node(store, right_id, &right_node).unwrap();

        (promoted_key, right_id)
    }

    fn balance_window(parent: &mut Internal, ci: usize, t: usize, max_keys: usize, store: &mut S) {
        const SIBLINGS_PER_SIDE: usize = 1;

        let left_bound = ci.saturating_sub(SIBLINGS_PER_SIDE);
        let right_bound = (ci + SIBLINGS_PER_SIDE).min(parent.children.len() - 1);
        let mut window: Vec<usize> = (left_bound..=right_bound).collect();

        let total_entries: usize = window
            .iter()
            .map(|&idx| {
                Self::read_node(store, parent.children[idx])
                    .unwrap()
                    .key_count()
            })
            .sum();

        // Safe Proactive Ceilings
        let needs_split = total_entries > window.len() * (max_keys - 1);
        let needs_merge = total_entries < window.len() * t;

        let is_leaf = Self::read_node(store, parent.children[window[0]])
            .unwrap()
            .is_leaf();

        if is_leaf {
            let rightmost_next = if let Node::Leaf(leaf) =
                Self::read_node(store, parent.children[*window.last().unwrap()]).unwrap()
            {
                leaf.next
            } else {
                unreachable!()
            };

            // ── Pool ─────────────────────────────────────────────────
            let mut pooled: Vec<LeafEntry> = Vec::with_capacity(total_entries);
            for &idx in &window {
                if let Node::Leaf(mut leaf) = Self::read_node(store, parent.children[idx]).unwrap()
                {
                    pooled.extend(leaf.entries.drain(..));
                }
            }

            // ── Needs split: add new node ─────────────────────────────
            if needs_split {
                let new_leaf_id = Self::allocate_leaf(store).unwrap();
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
                store.free(free_page).unwrap();
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

                Self::write_node(store, parent.children[idx], &Node::Leaf(leaf)).unwrap();
                start = end;
            }

            // ── Update parent separator keys ──────────────────────────
            for i in 0..window.len() - 1 {
                let right_node = Self::read_node(store, parent.children[window[i + 1]]).unwrap();
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
                if let Node::Internal(mut n) = Self::read_node(store, parent.children[idx]).unwrap()
                {
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
                let new_node_id = Self::allocate_internal(store).unwrap();
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
                store.free(free_page).unwrap();
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

                Self::write_node(store, parent.children[idx], &Node::Internal(internal)).unwrap();

                // boundary key goes back up to parent as separator
                if i < window.len() - 1 {
                    parent.keys[window[i]] = interleaved[key_end].clone();
                    key_start = key_end + 1; // skip the separator
                }

                child_start = child_end;
            }
        }
    }

    fn insert_non_full(
        store: &mut S,
        node_id: PageId,
        key: Key,
        value: Vec<u8>,
        t: usize,
        max_keys: usize,
    ) -> bool {
        let mut node = Self::read_node(store, node_id).unwrap();

        if node.is_leaf() {
            let inserted = if let Node::Leaf(leaf) = &mut node {
                match leaf.find_pos(&key) {
                    Ok(i) => {
                        leaf.entries[i].value = value;
                        false
                    }
                    Err(i) => {
                        leaf.entries.insert(i, LeafEntry { key, value });
                        true
                    }
                }
            } else {
                unreachable!()
            };

            Self::write_node(store, node_id, &node).unwrap();
            inserted
        } else {
            // Get initial child info immutably
            let mut ci = if let Node::Internal(internal) = &node {
                internal.child_index(&key)
            } else {
                unreachable!()
            };

            let child_count = {
                let child_id = if let Node::Internal(internal) = &node {
                    internal.children[ci]
                } else {
                    unreachable!()
                };
                Self::read_node(store, child_id).unwrap().key_count()
            };

            // If full, mutably borrow node to balance it
            if child_count == max_keys {
                if let Node::Internal(internal) = &mut node {
                    Self::balance_window(internal, ci, t, max_keys, store);
                    ci = internal.child_index(&key);
                }
                Self::write_node(store, node_id, &node).unwrap();
            }

            // Get the final child id immutably
            let next_child_id = if let Node::Internal(internal) = &node {
                internal.children[ci]
            } else {
                unreachable!()
            };

            Self::insert_non_full(store, next_child_id, key, value, t, max_keys)
        }
    }

    fn search_node(store: &mut S, node_id: PageId, key: &Key) -> Option<Vec<u8>> {
        let node = Self::read_node(store, node_id).unwrap();
        match node {
            Node::Leaf(leaf) => match leaf.entries.binary_search_by(|e| e.key.cmp(key)) {
                Ok(i) => Some(leaf.entries[i].value.clone()),
                Err(_) => None,
            },
            Node::Internal(internal) => {
                let child_idx = internal.child_index(key);
                Self::search_node(store, internal.children[child_idx], key)
            }
        }
    }

    fn remove_from(store: &mut S, node_id: PageId, key: &Key, t: usize) -> Option<Vec<u8>> {
        let mut node = Self::read_node(store, node_id).unwrap();

        if node.is_leaf() {
            let removed = if let Node::Leaf(leaf) = &mut node {
                match leaf.find_pos(key) {
                    Ok(i) => Some(leaf.entries.remove(i).value),
                    Err(_) => None,
                }
            } else {
                unreachable!()
            };

            if removed.is_some() {
                Self::write_node(store, node_id, &node).unwrap();
            }
            removed
        } else {
            let mut ci = if let Node::Internal(internal) = &node {
                internal.child_index(key)
            } else {
                unreachable!()
            };

            let child_count = {
                let child_id = if let Node::Internal(internal) = &node {
                    internal.children[ci]
                } else {
                    unreachable!()
                };
                Self::read_node(store, child_id).unwrap().key_count()
            };

            let mut changed = false;

            if child_count <= t - 1 {
                if let Node::Internal(internal) = &mut node {
                    Self::balance_window(internal, ci, t, 2 * t - 1, store);
                    ci = internal.child_index(key);
                }
                changed = true;
            }

            let next_child_id = if let Node::Internal(internal) = &node {
                internal.children[ci]
            } else {
                unreachable!()
            };

            let removed = Self::remove_from(store, next_child_id, key, t);

            if removed.is_some() && ci > 0 {
                let child_node = Self::read_node(store, next_child_id).unwrap();
                if let Some(first) = child_node.first_key(store) {
                    if let Node::Internal(internal) = &mut node {
                        if internal.keys[ci - 1] != first {
                            internal.keys[ci - 1] = first;
                            changed = true;
                        }
                    }
                }
            }

            if changed {
                Self::write_node(store, node_id, &node).unwrap();
            }

            removed
        }
    }

    fn leftmost_leaf(store: &mut S, node_id: PageId) -> PageId {
        let node = Self::read_node(store, node_id).unwrap();
        match node {
            Node::Leaf(_) => node_id,
            Node::Internal(internal) => Self::leftmost_leaf(store, internal.children[0]),
        }
    }

    fn find_leaf(store: &mut S, node_id: PageId, key: &Key) -> PageId {
        let node = Self::read_node(store, node_id).unwrap();
        match node {
            Node::Leaf(_) => node_id,
            Node::Internal(internal) => {
                let ci = internal.child_index(key);
                Self::find_leaf(store, internal.children[ci], key)
            }
        }
    }
}

impl<S: PageStore> BPlusTree<S> {
    pub fn allocate_leaf(store: &mut S) -> Result<PageId, S::Error> {
        let id = store.allocate()?;
        let page = Leaf::new().serialize();
        store.write(id, &page)?;
        Ok(id)
    }

    pub fn allocate_internal(store: &mut S) -> Result<PageId, S::Error> {
        let id = store.allocate()?;
        let page = Internal::new().serialize();
        store.write(id, &page)?;
        Ok(id)
    }

    fn read_node(store: &mut S, id: PageId) -> Result<Node, S::Error> {
        let page = store.read(id)?;
        Ok(Node::deserialize(&page))
    }

    fn write_node(store: &mut S, id: PageId, node: &Node) -> Result<(), S::Error> {
        let page = node.serialize();
        store.write(id, &page)
    }
}

impl<S: PageStore> BPlusTree<S> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<S: PageStore> BPlusTree<S> {
    pub fn new(t: usize, mut store: S) -> Result<Self, S::Error> {
        assert!(t >= 2, "Minimum degree must be >= 2");

        let root = store.allocate()?;
        let leaf = Leaf::new();
        let page = leaf.serialize();
        store.write(root, &page)?;

        Ok(Self {
            t,
            root,
            len: 0,
            store,
        })
    }

    pub fn insert(&mut self, key: Key, value: Vec<u8>) {
        let root_node = Self::read_node(&mut self.store, self.root).unwrap();
        let max_keys = 2 * self.t - 1;

        if root_node.key_count() == max_keys {
            let new_root_id = Self::allocate_internal(&mut self.store).unwrap();
            let old_root_id = std::mem::replace(&mut self.root, new_root_id);

            let (promoted_key, right_child_id) =
                Self::split_root_child(&mut self.store, old_root_id, self.t);

            let new_root = Internal {
                keys: vec![promoted_key],
                children: vec![old_root_id, right_child_id],
            };

            Self::write_node(&mut self.store, new_root_id, &Node::Internal(new_root)).unwrap();
        }

        let inserted =
            Self::insert_non_full(&mut self.store, self.root, key, value, self.t, max_keys);
        if inserted {
            self.len += 1
        }
    }

    pub fn search(&mut self, key: &Key) -> Option<Vec<u8>> {
        Self::search_node(&mut self.store, self.root, key)
    }

    pub fn remove(&mut self, key: &Key) -> Option<Vec<u8>> {
        let value = Self::remove_from(&mut self.store, self.root, key, self.t);

        let root_node = Self::read_node(&mut self.store, self.root).unwrap();
        if let Node::Internal(internal) = root_node {
            if internal.keys.is_empty() {
                let old_root_id = self.root;
                self.root = internal.children[0];
                self.store.free(old_root_id).unwrap();
            }
        }

        if value.is_some() {
            self.len -= 1;
        }

        value
    }

    pub fn iter(&mut self) -> LeafIter<'_, S> {
        let leaf_id = Self::leftmost_leaf(&mut self.store, self.root);
        LeafIter::new(&mut self.store, leaf_id, 0, Bound::Unbounded)
    }

    pub fn range_from(&mut self, start: &Key, end: Bound<Key>) -> LeafIter<'_, S> {
        let leaf_id = Self::find_leaf(&mut self.store, self.root, start);

        let node = Self::read_node(&mut self.store, leaf_id).unwrap();
        let pos = if let Node::Leaf(leaf) = node {
            leaf.entries.partition_point(|e| &e.key < start)
        } else {
            0
        };

        LeafIter::new(&mut self.store, leaf_id, pos, end)
    }
}

impl<S: PageStore> BPlusTree<S> {
    pub fn print_tree(&mut self) {
        println!("BPlusTree {{ t={}, len={} }}", self.t, self.len);
        Self::print_node(&mut self.store, self.root, 0);
    }

    fn print_node(store: &mut S, node_id: PageId, depth: usize) {
        let indent = "  ".repeat(depth);
        let node = Self::read_node(store, node_id).unwrap();
        match node {
            Node::Leaf(l) => {
                let keys: Vec<_> = l.entries.iter().map(|e| e.key.clone()).collect();
                println!("{indent}Leaf {keys:?}");
            }
            Node::Internal(n) => {
                println!("{indent}Internal {:?}", n.keys);
                for c in &n.children {
                    Self::print_node(store, *c, depth + 1);
                }
            }
        }
    }
}

pub struct LeafIter<'a, S: PageStore> {
    store: &'a mut S,
    current_id: Option<PageId>,
    current_leaf: Option<Leaf>,
    pos: usize,
    end: Bound<Key>,
}

impl<'a, S: PageStore> LeafIter<'a, S> {
    fn new(store: &'a mut S, start_id: PageId, pos: usize, end: Bound<Key>) -> Self {
        LeafIter {
            store,
            current_id: Some(start_id),
            current_leaf: None,
            pos,
            end,
        }
    }
}

impl<'a, S: PageStore> Iterator for LeafIter<'a, S> {
    type Item = (Key, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.current_leaf.is_none() {
                let id = self.current_id?;
                let node = BPlusTree::<S>::read_node(self.store, id).ok()?;
                if let Node::Leaf(leaf) = node {
                    self.current_leaf = Some(leaf);
                    // On fresh load, position is 0 except the first time when pos was already populated in new()
                } else {
                    return None; // Should never happen
                }
            }

            let leaf = self.current_leaf.as_ref().unwrap();

            if self.pos < leaf.entries.len() {
                let e = &leaf.entries[self.pos];

                let out = match &self.end {
                    Bound::Included(key) if e.key <= *key => Some((e.key.clone(), e.value.clone())),
                    Bound::Excluded(key) if e.key < *key => Some((e.key.clone(), e.value.clone())),
                    Bound::Unbounded => Some((e.key.clone(), e.value.clone())),
                    _ => None,
                };

                if out.is_some() {
                    self.pos += 1;
                    return out;
                } else {
                    return None;
                }
            }

            self.current_id = leaf.next;
            self.current_leaf = None;
            self.pos = 0; // Reset position for next block
        }
    }
}

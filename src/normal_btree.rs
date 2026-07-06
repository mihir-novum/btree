use std::collections::Bound;
use std::fmt;

struct LeafEntry<K: Ord + Clone, V: Clone> {
    key: K,
    value: V,
}

struct Leaf<K: Ord + Clone, V: Clone> {
    entries: Vec<LeafEntry<K, V>>,
    next: *mut Leaf<K, V>,
}

impl<K: Ord + Clone, V: Clone> Leaf<K, V> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: std::ptr::null_mut(),
        }
    }

    fn find_pos(&self, key: &K) -> Result<usize, usize> {
        self.entries.binary_search_by(|e| e.key.cmp(key))
    }
}

struct Internal<K: Ord + Clone, V: Clone> {
    keys: Vec<K>,
    children: Vec<Box<Node<K, V>>>,
}

impl<K: Ord + Clone, V: Clone> Internal<K, V> {
    fn new() -> Self {
        Self {
            keys: Vec::new(),
            children: Vec::new(),
        }
    }

    fn child_index(&self, key: &K) -> usize {
        match self.keys.binary_search(key) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }
}

enum Node<K: Ord + Clone, V: Clone> {
    Leaf(Leaf<K, V>),
    Internal(Internal<K, V>),
}

impl<K: Ord + Clone, V: Clone> Node<K, V> {
    fn first_key(&self) -> Option<&K> {
        match self {
            Node::Leaf(leaf) => leaf.entries.first().map(|e| &e.key),
            Node::Internal(internal) => internal.children.first().and_then(|c| c.first_key()),
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
}

struct SplitResult<K: Ord + Clone, V: Clone> {
    promoted_key: K,
    right_child: Box<Node<K, V>>,
}

pub struct BPlusTree<K: Ord + Clone, V: Clone> {
    t: usize,
    root: Box<Node<K, V>>,
    len: usize,
}

impl<K: Ord + Clone, V: Clone> BPlusTree<K, V> {
    fn split_child(parent: &mut Internal<K, V>, ci: usize, t: usize) -> SplitResult<K, V> {
        match parent.children[ci].as_mut() {
            Node::Leaf(left) => {
                let right_entries = left.entries.split_off(t);
                let promoted_key = right_entries[0].key.clone();

                let mut right_leaf = Leaf::new();
                right_leaf.next = left.next;
                right_leaf.entries = right_entries;

                let mut right_box = Box::new(Node::Leaf(right_leaf));

                let right_ptr = match right_box.as_mut() {
                    Node::Leaf(rl) => rl as *mut Leaf<K, V>,
                    _ => unreachable!(),
                };

                left.next = right_ptr;

                SplitResult {
                    promoted_key,
                    right_child: right_box,
                }
            }
            Node::Internal(internal) => {
                let mid = t - 1;

                let promoted_key = internal.keys[mid].clone();

                let mut right_keys = internal.keys.split_off(mid);
                right_keys.remove(0);

                let right_children = internal.children.split_off(mid + 1);

                SplitResult {
                    promoted_key,
                    right_child: Box::new(Node::Internal(Internal {
                        keys: right_keys,
                        children: right_children,
                    })),
                }
            }
        }
    }

    fn balance_window(parent: &mut Internal<K, V>, ci: usize, t: usize, max_keys: usize) {
        const SIBLINGS_PER_SIDE: usize = 1;

        let left_bound = ci.saturating_sub(SIBLINGS_PER_SIDE);
        let right_bound = (ci + SIBLINGS_PER_SIDE).min(parent.children.len() - 1);
        let mut window: Vec<usize> = (left_bound..=right_bound).collect();

        let total_entries: usize = window
            .iter()
            .map(|&idx| parent.children[idx].key_count())
            .sum();

        let needs_split = total_entries >= window.len() * max_keys;
        let needs_merge = total_entries <= (window.len() - 1) * (t - 1);
        let is_leaf = parent.children[window[0]].as_ref().is_leaf();

        if is_leaf {
            let rightmost_next =
                if let Node::Leaf(leaf) = parent.children[*window.last().unwrap()].as_ref() {
                    leaf.next
                } else {
                    unreachable!()
                };

            // ── Pool ─────────────────────────────────────────────────
            let mut pooled: Vec<LeafEntry<K, V>> = Vec::new();
            for &idx in &window {
                if let Node::Leaf(leaf) = parent.children[idx].as_mut() {
                    pooled.extend(leaf.entries.drain(..));
                }
            }

            // ── Needs split: add new node ─────────────────────────────
            if needs_split {
                let new_leaf = Box::new(Node::Leaf(Leaf::new()));
                parent.children.insert(right_bound + 1, new_leaf);
                parent
                    .keys
                    .insert(right_bound, pooled.last().unwrap().key.clone());
                window.push(right_bound + 1);

            // ── Needs merge: remove rightmost node ───────────────────
            } else if needs_merge && window.len() > 1 {
                let remove_idx = *window.last().unwrap();
                parent.children.remove(remove_idx);
                parent.keys.remove(remove_idx - 1);
                window.pop();
            }

            // ── Distribute evenly ─────────────────────────────────────
            let n = window.len();
            let chunk = pooled.len() / n;
            let rem = pooled.len() % n;
            let mut start = 0;

            for (i, &idx) in window.iter().enumerate() {
                let end = start + chunk + if i < rem { 1 } else { 0 };
                if let Node::Leaf(leaf) = parent.children[idx].as_mut() {
                    leaf.entries
                        .extend(pooled[start..end].iter().map(|e| LeafEntry {
                            key: e.key.clone(),
                            value: e.value.clone(),
                        }));
                }
                start = end;
            }

            // ── Fix sibling pointers ──────────────────────────────────
            for i in 0..window.len() - 1 {
                let right_ptr = {
                    if let Node::Leaf(r) = parent.children[window[i + 1]].as_mut() {
                        r as *mut Leaf<K, V>
                    } else {
                        unreachable!()
                    }
                };
                if let Node::Leaf(l) = parent.children[window[i]].as_mut() {
                    l.next = right_ptr;
                }
            }

            // FIX: Hook up the final rightmost node in the window to the rest of the linked list!
            if let Node::Leaf(l) = parent.children[*window.last().unwrap()].as_mut() {
                l.next = rightmost_next;
            }

            // ── Update parent separator keys ──────────────────────────
            for i in 0..window.len() - 1 {
                let first_key = if let Node::Leaf(leaf) = parent.children[window[i + 1]].as_ref() {
                    leaf.entries[0].key.clone()
                } else {
                    unreachable!()
                };
                parent.keys[window[i]] = first_key;
            }
        } else {
            // ── Pool keys + children (record sizes first) ─────────────
            let mut pooled_keys: Vec<K> = Vec::new();
            let mut pooled_children: Vec<Box<Node<K, V>>> = Vec::new();
            let mut node_key_counts: Vec<usize> = Vec::new();

            for &idx in &window {
                if let Node::Internal(n) = parent.children[idx].as_mut() {
                    node_key_counts.push(n.keys.len());
                    pooled_keys.extend(n.keys.drain(..));
                    pooled_children.extend(n.children.drain(..));
                }
            }

            // ── Interleave parent separators into key pool ────────────
            let mut interleaved: Vec<K> = Vec::new();
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
                let new_node = Box::new(Node::Internal(Internal::new()));
                parent.children.insert(right_bound + 1, new_node);
                parent
                    .keys
                    .insert(right_bound, interleaved.last().unwrap().clone());
                window.push(right_bound + 1);

            // ── Needs merge: remove rightmost node ───────────────────
            } else if needs_merge && window.len() > 1 {
                let remove_idx = *window.last().unwrap();
                parent.children.remove(remove_idx);
                // the separator key between second-to-last and last comes DOWN
                // into the interleaved pool (it already is — just remove from parent)
                parent.keys.remove(remove_idx - 1);
                window.pop();
            }

            // ── Distribute evenly ─────────────────────────────────────
            let n = window.len();
            let total = interleaved.len();
            let chunk = total / n;
            let rem = total % n;
            let mut key_start = 0;
            let mut child_start = 0;

            for (i, &idx) in window.iter().enumerate() {
                let keys_for_node = chunk + if i < rem { 1 } else { 0 };
                let key_end = key_start + keys_for_node;
                let child_end = child_start + keys_for_node + 1;

                if let Node::Internal(internal) = parent.children[idx].as_mut() {
                    internal
                        .keys
                        .extend_from_slice(&interleaved[key_start..key_end]);
                    for c in pooled_children.drain(..keys_for_node + 1) {
                        internal.children.push(c);
                    }
                }

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
        node: &mut Box<Node<K, V>>,
        key: K,
        value: V,
        t: usize,
        max_keys: usize,
    ) -> bool {
        match node.as_mut() {
            Node::Leaf(leaf) => match leaf.find_pos(&key) {
                Ok(i) => {
                    leaf.entries[i].value = value;
                    false
                }
                Err(i) => {
                    leaf.entries.insert(i, LeafEntry { key, value });
                    true
                }
            },
            Node::Internal(internal) => {
                let ci = internal.child_index(&key);

                if internal.children[ci].key_count() == max_keys {
                    Self::balance_window(internal, ci, t, max_keys);
                }

                let ci2 = internal.child_index(&key);
                Self::insert_non_full(&mut internal.children[ci2], key, value, t, max_keys)
            }
        }
    }

    fn search_node<'a>(node: &'a Node<K, V>, key: &K) -> Option<&'a V> {
        match node {
            Node::Leaf(leaf) => match leaf.entries.binary_search_by(|e| e.key.cmp(key)) {
                Ok(i) => Some(&leaf.entries[i].value),
                Err(_) => None,
            },
            Node::Internal(internal) => {
                let child_idx = internal.child_index(&key);

                Self::search_node(&internal.children[child_idx], &key)
            }
        }
    }

    fn remove_from(node: &mut Box<Node<K, V>>, key: &K, t: usize) -> Option<V> {
        match node.as_mut() {
            Node::Leaf(leaf) => match leaf.find_pos(key) {
                Ok(i) => Some(leaf.entries.remove(i).value),
                Err(_) => None,
            },
            Node::Internal(internal) => {
                let ci = internal.child_index(key);

                // proactive balance before descending
                if internal.children[ci].key_count() <= t - 1 {
                    Self::balance_window(internal, ci, t, 2 * t - 1);
                }

                // structure may have changed — recompute index
                let ci2 = internal.child_index(key);
                let removed = Self::remove_from(&mut internal.children[ci2], key, t);

                // FIX: If we removed a key and the first_key of the child changed, update our separator!
                if removed.is_some() && ci2 > 0 {
                    if let Some(first) = internal.children[ci2].first_key() {
                        if &internal.keys[ci2 - 1] != first {
                            internal.keys[ci2 - 1] = first.clone();
                        }
                    }
                }

                removed
            }
        }
    }

    fn leftmost_leaf(node: &Node<K, V>) -> *const Leaf<K, V> {
        match node {
            Node::Leaf(leaf) => leaf as *const Leaf<K, V>,
            Node::Internal(internal) => Self::leftmost_leaf(&internal.children[0]),
        }
    }

    fn find_leaf(node: &Node<K, V>, key: &K) -> *const Leaf<K, V> {
        match node {
            Node::Leaf(leaf) => leaf as *const Leaf<K, V>,
            Node::Internal(internal) => {
                let ci = internal.child_index(key);
                Self::find_leaf(&internal.children[ci], key)
            }
        }
    }
}

impl<K: Ord + Clone, V: Clone> BPlusTree<K, V> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
impl<K: Ord + Clone, V: Clone> BPlusTree<K, V> {
    pub(crate) fn new(t: usize) -> Self {
        assert!(t >= 2, "Minimum degree must be >= 2");

        Self {
            t,
            root: Box::new(Node::Leaf(Leaf::new())),
            len: 0,
        }
    }

    pub fn insert(&mut self, key: K, value: V) {
        let max_keys = 2 * self.t - 1;

        if self.root.key_count() == max_keys {
            let old_root =
                std::mem::replace(&mut self.root, Box::new(Node::Internal(Internal::new())));

            let Node::Internal(new_root) = self.root.as_mut() else {
                unreachable!()
            };

            new_root.children.push(old_root);

            let split = Self::split_child(new_root, 0, self.t);
            new_root.keys.push(split.promoted_key);
            new_root.children.push(split.right_child);
        }

        let inserted = Self::insert_non_full(&mut self.root, key, value, self.t, max_keys);
        if inserted {
            self.len += 1
        }
    }

    pub fn search(&self, key: &K) -> Option<&'_ V> {
        Self::search_node(&self.root, &key)
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let value = Self::remove_from(&mut self.root, key, self.t);

        if let Node::Internal(ref internal) = *self.root {
            if internal.keys.is_empty() {
                let old_root =
                    std::mem::replace(&mut self.root, Box::new(Node::Internal(Internal::new())));
                let Node::Internal(mut old_int) = *old_root else {
                    unreachable!()
                };
                self.root = old_int.children.remove(0);
            }
        }

        if value.is_some() {
            self.len -= 1;
        }

        value
    }

    pub fn iter(&self) -> LeafIter<'_, K, V> {
        let leaf = Self::leftmost_leaf(&self.root);
        LeafIter::new(leaf, 0, Bound::Unbounded)
    }

    pub fn range_from(&self, start: &K, end: Bound<K>) -> LeafIter<'_, K, V> {
        let leaf = Self::find_leaf(&self.root, start);

        let pos = unsafe { (*leaf).entries.partition_point(|e| &e.key < start) };

        LeafIter::new(leaf, pos, end)
    }
}

impl<K: Ord + Clone + fmt::Debug, V: fmt::Debug + Clone> BPlusTree<K, V> {
    pub fn print_tree(&self) {
        println!("BPlusTree {{ t={}, len={} }}", self.t, self.len);
        Self::print_node(&self.root, 0);
    }

    fn print_node(node: &Node<K, V>, depth: usize) {
        let indent = "  ".repeat(depth);
        match node {
            Node::Leaf(l) => {
                let keys: Vec<_> = l.entries.iter().map(|e| &e.key).collect();
                println!("{indent}Leaf {keys:?}");
            }
            Node::Internal(n) => {
                println!("{indent}Internal {:?}", n.keys);
                for c in &n.children {
                    Self::print_node(c, depth + 1);
                }
            }
        }
    }
}

pub struct LeafIter<'a, K: Ord + Clone, V: Clone> {
    current: *const Leaf<K, V>,
    pos: usize,
    end: Bound<K>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a, K: Ord + Clone, V: Clone> LeafIter<'a, K, V> {
    fn new(current: *const Leaf<K, V>, pos: usize, end: Bound<K>) -> Self {
        LeafIter {
            current,
            pos,
            end,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a, K: 'a + Ord + Clone, V: 'a + Clone> Iterator for LeafIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.current.is_null() {
                return None;
            }
            let leaf = unsafe { &*self.current };
            if self.pos < leaf.entries.len() {
                let e = &leaf.entries[self.pos];

                return match &self.end {
                    Bound::Included(key) => {
                        if e.key <= *key {
                            self.pos += 1;
                            return Some((&e.key, &e.value));
                        }

                        None
                    }
                    Bound::Excluded(key) => {
                        if e.key < *key {
                            self.pos += 1;
                            return Some((&e.key, &e.value));
                        }

                        None
                    }
                    Bound::Unbounded => {
                        self.pos += 1;
                        Some((&e.key, &e.value))
                    }
                };
            }
            self.current = leaf.next as *const _;
            self.pos = 0;
        }
    }
}

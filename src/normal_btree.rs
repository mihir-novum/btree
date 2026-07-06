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
            Node::Internal(internal) => internal.keys.first(),
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

        // Step 1: build window indices
        let left_bound = ci.saturating_sub(SIBLINGS_PER_SIDE);
        let right_bound = (ci + SIBLINGS_PER_SIDE).min(parent.children.len() - 1);
        let mut window: Vec<usize> = (left_bound..=right_bound).collect();

        // Step 2: check if we need a new node
        let total_entries: usize = window
            .iter()
            .map(|&idx| parent.children[idx].key_count())
            .sum();
        let needs_split = total_entries >= window.len() * max_keys;

        let is_leaf = parent.children[window[0]].as_ref().is_leaf();

        if is_leaf {
            // Step 3: pool all entries
            let mut pooled: Vec<LeafEntry<K, V>> = Vec::new();
            for &idx in &window {
                if let Node::Leaf(leaf) = parent.children[idx].as_mut() {
                    pooled.extend(leaf.entries.drain(..));
                }
            }

            // Step 3b: if all full, insert new node into window
            if needs_split {
                let new_leaf = Box::new(Node::Leaf(Leaf::new()));
                parent.children.insert(right_bound + 1, new_leaf);
                parent
                    .keys
                    .insert(right_bound, pooled.last().unwrap().key.clone());
                window.push(right_bound + 1);
            }

            // Step 4: distribute evenly back into window nodes
            let n = window.len();
            let chunk = pooled.len() / n;
            let remainder = pooled.len() % n;
            let mut start = 0;

            for (i, &idx) in window.iter().enumerate() {
                // first `remainder` nodes get one extra entry
                let end = start + chunk + if i < remainder { 1 } else { 0 };
                if let Node::Leaf(leaf) = parent.children[idx].as_mut() {
                    leaf.entries
                        .extend(pooled[start..end].iter().map(|e| LeafEntry {
                            key: e.key.clone(),
                            value: e.value.clone(),
                        }));
                }
                start = end;
            }

            // Step 5: fix sibling pointers
            for i in 0..window.len() - 1 {
                let right_ptr = {
                    if let Node::Leaf(right) = parent.children[window[i + 1]].as_mut() {
                        right as *mut Leaf<K, V>
                    } else {
                        unreachable!()
                    }
                };
                if let Node::Leaf(left) = parent.children[window[i]].as_mut() {
                    left.next = right_ptr;
                }
            }

            // Step 6: update parent separator keys
            // separator between window[i] and window[i+1] = first key of window[i+1]
            for i in 0..window.len() - 1 {
                let sep_idx = window[i]; // separator key index = left child index
                let first_key = if let Node::Leaf(leaf) = parent.children[window[i + 1]].as_ref() {
                    leaf.entries[0].key.clone()
                } else {
                    unreachable!()
                };
                parent.keys[sep_idx] = first_key;
            }
        } else {
            // Internal nodes

            // Step 3: pool keys and children
            let mut pooled_keys: Vec<K> = Vec::new();
            let mut pooled_children: Vec<Box<Node<K, V>>> = Vec::new();

            for &idx in &window {
                if let Node::Internal(internal) = parent.children[idx].as_mut() {
                    // pull the separator key from parent down before draining
                    // except for the leftmost node in window
                    pooled_keys.extend(internal.keys.drain(..));
                    pooled_children.extend(internal.children.drain(..));
                }
            }

            // pull separator keys between window nodes from parent down into pool
            // these sit between window[i] and window[i+1] in parent.keys
            // insert them at the right positions in pooled_keys
            // separator between window[i] and window[i+1] = parent.keys[window[i]]
            // we need to interleave them
            // rebuild pooled_keys with separators included
            let mut full_keys: Vec<K> = Vec::new();
            let mut full_children: Vec<Box<Node<K, V>>> = Vec::new();

            // children and keys are already drained into pooled_children/pooled_keys
            // but we lost the separator keys between siblings
            // rebuild: for each window node except last, after its keys insert parent separator
            let mut key_offset = 0;
            let mut child_offset = 0;

            for (i, &idx) in window.iter().enumerate() {
                // how many keys did this node originally have?
                // we can't know now since we drained — need a different approach
                // see note below
                let _ = (i, idx, key_offset, child_offset);
            }

            // NOTE: the internal case is trickier because after draining we lose
            // track of how many keys each node had. The fix: record sizes BEFORE draining.
            // Let's redo with sizes recorded first.

            // reset — redo internal pooling correctly
            let mut pooled_keys: Vec<K> = Vec::new();
            let mut pooled_children: Vec<Box<Node<K, V>>> = Vec::new();
            let mut node_key_counts: Vec<usize> = Vec::new();
            let mut node_child_counts: Vec<usize> = Vec::new();

            for &idx in &window {
                if let Node::Internal(internal) = parent.children[idx].as_mut() {
                    node_key_counts.push(internal.keys.len());
                    node_child_counts.push(internal.children.len());
                    pooled_keys.extend(internal.keys.drain(..));
                    pooled_children.extend(internal.children.drain(..));
                }
            }

            // now interleave separator keys from parent between each window node's keys
            // separator between window[i] and window[i+1] sits at parent.keys[window[i]]
            let mut interleaved_keys: Vec<K> = Vec::new();
            let mut key_cursor = 0;
            for i in 0..window.len() {
                let count = node_key_counts[i];
                interleaved_keys.extend_from_slice(&pooled_keys[key_cursor..key_cursor + count]);
                key_cursor += count;
                // after each node except the last, insert the parent separator
                if i < window.len() - 1 {
                    interleaved_keys.push(parent.keys[window[i]].clone());
                }
            }

            // Step 3b: needs split → add new internal node
            if needs_split {
                let new_internal = Box::new(Node::Internal(Internal::new()));
                parent.children.insert(right_bound + 1, new_internal);
                parent
                    .keys
                    .insert(right_bound, interleaved_keys.last().unwrap().clone());
                window.push(right_bound + 1);
            }

            // Step 4: distribute evenly
            // total keys in pool = interleaved_keys.len()
            // n nodes in window, each node gets chunk keys and chunk+1 children
            let n = window.len();
            // keys between nodes act as separators — we have interleaved_keys.len() total
            // each node gets some keys, separators go back up to parent
            // total keys to distribute into nodes = interleaved_keys.len() - (n-1)
            // the n-1 boundary keys go back to parent as separators
            let total_keys = interleaved_keys.len();
            let chunk = total_keys / n;
            let remainder = total_keys % n;

            let mut key_start = 0;
            let mut child_start = 0;

            for (i, &idx) in window.iter().enumerate() {
                let keys_for_node = chunk + if i < remainder { 1 } else { 0 };
                let key_end = key_start + keys_for_node;
                let child_end = child_start + keys_for_node + 1;

                if let Node::Internal(internal) = parent.children[idx].as_mut() {
                    internal
                        .keys
                        .extend_from_slice(&interleaved_keys[key_start..key_end]);
                    internal
                        .children
                        .extend(pooled_children.drain(..keys_for_node + 1));
                }

                // the key at key_end is the separator going back up to parent
                if i < window.len() - 1 {
                    parent.keys[window[i]] = interleaved_keys[key_end].clone();
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

    fn remove_from(node: &mut Box<Node<K, V>>, key: &K, t: usize, is_root: bool) -> Option<V> {
        match node.as_mut() {
            Node::Leaf(leaf) => {
                let idx = leaf.find_pos(key);

                match idx {
                    Ok(i) => Some(leaf.entries.remove(i).value),
                    Err(_) => None,
                }
            }
            Node::Internal(internal) => {
                let child_idx = internal.child_index(&key);

                if internal.children[child_idx].key_count() <= t - 1 {
                    Self::rebalance(internal, child_idx, t);

                    return Self::remove_from(node, key, t, is_root);
                }

                Self::remove_from(&mut internal.children[child_idx], key, t, is_root)
            }
        }
    }

    fn rebalance(parent: &mut Internal<K, V>, ci: usize, t: usize) {
        let min_keys = t - 1;

        if ci + 1 < parent.children.len() && parent.children[ci + 1].key_count() > min_keys {
            Self::borrow_from_right(parent, ci);
        } else if ci > 0 && parent.children[ci - 1].key_count() > min_keys {
            Self::borrow_from_left(parent, ci);
        } else if ci + 1 < parent.children.len() {
            Self::merge_children(parent, ci);
        } else {
            Self::merge_children(parent, ci - 1);
        }
    }

    fn borrow_from_right(parent: &mut Internal<K, V>, ci: usize) {
        let (left_entry, right_entry) = parent.children.split_at_mut(ci + 1);

        let left = left_entry[ci].as_mut();
        let right = right_entry[0].as_mut();

        match (left, right) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let removed = right.entries.remove(0);
                parent.keys[ci] = right.entries[0].key.clone();
                left.entries.push(removed)
            }
            (Node::Internal(left), Node::Internal(right)) => {
                let parent_key = parent.keys[ci].clone();
                left.keys.push(parent_key);

                let right_key = right.keys.remove(0);
                parent.keys[ci] = right_key;

                let right_children = right.children.remove(0);
                left.children.push(right_children);
            }
            _ => unreachable!(),
        }
    }

    fn borrow_from_left(parent: &mut Internal<K, V>, ci: usize) {
        let (left_entry, right_entry) = parent.children.split_at_mut(ci);

        let left = left_entry[ci - 1].as_mut();
        let right = right_entry[0].as_mut();

        match (left, right) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let removed = left.entries.pop().unwrap();
                parent.keys[ci - 1] = removed.key.clone();
                right.entries.insert(0, removed);
            }
            (Node::Internal(left), Node::Internal(right)) => {
                right.keys.insert(0, parent.keys[ci - 1].clone());

                parent.keys[ci - 1] = left.keys.pop().unwrap();

                right.children.insert(0, left.children.pop().unwrap())
            }
            _ => unreachable!(),
        }
    }

    fn merge_children(parent: &mut Internal<K, V>, ci: usize) {
        let right = parent.children.remove(ci + 1);
        let separator_key = parent.keys.remove(ci);
        let left = parent.children[ci].as_mut();

        match left {
            Node::Leaf(leaf) => {
                let Node::Leaf(mut right) = *right else {
                    unreachable!();
                };

                leaf.next = right.next;
                leaf.entries.append(&mut right.entries);
            }
            Node::Internal(left) => {
                let Node::Internal(mut right) = *right else {
                    unreachable!();
                };

                left.keys.push(separator_key);
                left.keys.append(&mut right.keys);
                left.children.append(&mut right.children);
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
        let value = Self::remove_from(&mut self.root, key, self.t, true);

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

use std::fmt;
use std::ops::Bound;

struct LeafEntry<K: Ord + Clone, V> {
    key: K,
    value: V,
}

struct Leaf<K: Ord + Clone, V> {
    entries: Vec<LeafEntry<K, V>>,
    next: *mut Leaf<K, V>,
}

impl<K: Ord + Clone, V> Leaf<K, V> {
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

struct Internal<K: Ord + Clone, V> {
    keys: Vec<K>,
    children: Vec<Box<Node<K, V>>>,
}

impl<K: Ord + Clone, V> Internal<K, V> {
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

enum Node<K: Ord + Clone, V> {
    Leaf(Leaf<K, V>),
    Internal(Internal<K, V>),
}

impl<K: Ord + Clone, V> Node<K, V> {
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

struct SplitResult<K: Ord + Clone, V> {
    promoted_key: K,
    right_child: Box<Node<K, V>>,
}

struct BPlusTree<K: Ord + Clone, V> {
    t: usize,
    root: Box<Node<K, V>>,
    len: usize,
}

impl<K: Ord + Clone, V> BPlusTree<K, V> {
    fn redistribute_right(parent: &mut Internal<K, V>, ci: usize) {
        let (left_slice, right_slice) = parent.children.split_at_mut(ci + 1);
        let left = left_slice[ci].as_mut();
        let right = right_slice[0].as_mut();

        match (left, right) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let total = left.entries.len() + right.entries.len();
                let target = total / 2;

                let mut drained = left
                    .entries
                    .drain(target..)
                    .collect::<Vec<LeafEntry<K, V>>>();

                drained.append(&mut right.entries);
                right.entries = drained;

                parent.keys[ci] = right.entries[0].key.clone();
            }
            (Node::Internal(left), Node::Internal(right)) => {
                let total = left.keys.len() + right.keys.len();
                let target = total / 2;
                let keys_to_move = left.keys.len() - target;

                for _ in 0..keys_to_move {
                    right.keys.insert(0, parent.keys[ci].clone());
                    parent.keys[ci] = left.keys.pop().unwrap();
                    right.children.insert(0, left.children.pop().unwrap());
                }
            }
            _ => unreachable!(),
        }
    }

    fn redistribute_left(parent: &mut Internal<K, V>, ci: usize) {
        let (left_slice, right_slice) = parent.children.split_at_mut(ci);
        let left = left_slice[ci - 1].as_mut();
        let right = right_slice[0].as_mut();

        match (left, right) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let total = left.entries.len() + right.entries.len();
                let target = total / 2;
                let keys_to_move = right.entries.len() - target;

                let drained = right
                    .entries
                    .drain(0..keys_to_move)
                    .collect::<Vec<LeafEntry<K, V>>>();

                left.entries.extend(drained);

                parent.keys[ci - 1] = right.entries[0].key.clone();
            }
            (Node::Internal(left), Node::Internal(right)) => {
                let total = left.keys.len() + right.keys.len();
                let target = total / 2;
                let keys_to_move = right.keys.len() - target;

                for _ in 0..keys_to_move {
                    left.keys.push(parent.keys[ci - 1].clone());
                    parent.keys[ci - 1] = right.keys.remove(0);
                    left.children.push(right.children.remove(0));
                }
            }
            _ => unreachable!(),
        }
    }

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
                    if ci + 1 < internal.children.len()
                        && internal.children[ci + 1].key_count() < max_keys
                    {
                        Self::redistribute_right(internal, ci);
                    } else if ci > 0 && internal.children[ci - 1].key_count() < max_keys {
                        Self::redistribute_left(internal, ci);
                    } else {
                        let split = Self::split_child(internal, ci, t);
                        internal.keys.insert(ci, split.promoted_key);
                        internal.children.insert(ci + 1, split.right_child);
                    }
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
        } else if ci > 0 && parent.children[ci - 1].key_count() >= min_keys {
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

impl<K: Ord + Clone, V> BPlusTree<K, V> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
impl<K: Ord + Clone, V> BPlusTree<K, V> {
    fn new(t: usize) -> Self {
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

impl<K: Ord + Clone + fmt::Debug, V: fmt::Debug> BPlusTree<K, V> {
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

pub struct LeafIter<'a, K: Ord + Clone, V> {
    current: *const Leaf<K, V>,
    pos: usize,
    end: Bound<K>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a, K: Ord + Clone, V> LeafIter<'a, K, V> {
    fn new(current: *const Leaf<K, V>, pos: usize, end: Bound<K>) -> Self {
        LeafIter {
            current,
            pos,
            end,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a, K: 'a + Ord + Clone, V: 'a> Iterator for LeafIter<'a, K, V> {
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

fn main() {
    let mut tree: BPlusTree<u64, u64> = BPlusTree::new(2);
    for i in 1..6 {
        tree.insert(i, i);
    }

    for e in tree.iter() {
        println!("Key: {} | Value: {}", e.0, e.1);
    }

    println!("Range Scan");

    for range in tree.range_from(&1, Bound::Excluded(40)) {
        println!("Key: {} | Value: {}", range.0, range.1);
    }

    tree.print_tree();
}

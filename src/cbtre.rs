use std::fmt;
use std::ops::Bound;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

type NodePtr<K, V> = Arc<RwLock<Node<K, V>>>;

struct LeafEntry<K: Ord + Clone, V: Clone> {
    key: K,
    value: V,
}

struct Leaf<K: Ord + Clone, V: Clone> {
    entries: Vec<LeafEntry<K, V>>,
    next: Option<NodePtr<K, V>>,
}

impl<K: Ord + Clone, V: Clone> Leaf<K, V> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: None,
        }
    }

    fn find_pos(&self, key: &K) -> Result<usize, usize> {
        self.entries.binary_search_by(|e| e.key.cmp(key))
    }
}

struct Internal<K: Ord + Clone, V: Clone> {
    keys: Vec<K>,
    children: Vec<NodePtr<K, V>>,
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
    fn key_count(&self) -> usize {
        match self {
            Node::Leaf(leaf) => leaf.entries.len(),
            Node::Internal(internal) => internal.keys.len(),
        }
    }

    fn is_leaf(&self) -> bool {
        matches!(self, Node::Leaf(..))
    }
}

pub struct BPlusTree<K: Ord + Clone, V: Clone> {
    t: usize,
    // self.root is a WRAPPER around the pointer to the actual root node.
    root: RwLock<NodePtr<K, V>>,
    len: AtomicUsize,
}

unsafe fn extend_read<'a, K: Ord + Clone, V: Clone>(
    guard: RwLockReadGuard<'_, Node<K, V>>,
) -> RwLockReadGuard<'a, Node<K, V>> {
    unsafe { std::mem::transmute(guard) }
}

unsafe fn extend_write<'a, K: Ord + Clone, V: Clone>(
    guard: RwLockWriteGuard<'_, Node<K, V>>,
) -> RwLockWriteGuard<'a, Node<K, V>> {
    unsafe { std::mem::transmute(guard) }
}

impl<K: Ord + Clone, V: Clone> BPlusTree<K, V> {
    pub fn new(t: usize) -> Self {
        assert!(t >= 2, "Minimum degree must be >= 2");
        Self {
            t,
            root: RwLock::new(Arc::new(RwLock::new(Node::Leaf(Leaf::new())))),
            len: AtomicUsize::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // --- Redistribution Methods ---
    fn redistribute_right_locks(
        parent: &mut Internal<K, V>,
        ci: usize,
        left: &mut Node<K, V>,
        right: &mut Node<K, V>,
    ) {
        match (left, right) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let target = (left.entries.len() + right.entries.len()) / 2;
                let mut drained = left.entries.drain(target..).collect::<Vec<_>>();
                drained.append(&mut right.entries);
                right.entries = drained;
                parent.keys[ci] = right.entries[0].key.clone();
            }
            (Node::Internal(left), Node::Internal(right)) => {
                let target = (left.keys.len() + right.keys.len()) / 2;
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

    fn redistribute_left_locks(
        parent: &mut Internal<K, V>,
        ci: usize,
        left: &mut Node<K, V>,
        right: &mut Node<K, V>,
    ) {
        match (left, right) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let target = (left.entries.len() + right.entries.len()) / 2;
                let keys_to_move = right.entries.len() - target;
                let drained = right.entries.drain(0..keys_to_move).collect::<Vec<_>>();
                left.entries.extend(drained);
                parent.keys[ci - 1] = right.entries[0].key.clone();
            }
            (Node::Internal(left), Node::Internal(right)) => {
                let target = (left.keys.len() + right.keys.len()) / 2;
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

    fn split_child(parent: &mut Internal<K, V>, ci: usize, child_guard: &mut Node<K, V>, t: usize) {
        match child_guard {
            Node::Leaf(left) => {
                let right_entries = left.entries.split_off(t);
                let promoted_key = right_entries[0].key.clone();
                let next_ptr = left.next.clone();
                let right_leaf = Leaf {
                    entries: right_entries,
                    next: next_ptr,
                };
                let right_ptr = Arc::new(RwLock::new(Node::Leaf(right_leaf)));
                left.next = Some(Arc::clone(&right_ptr));
                parent.keys.insert(ci, promoted_key);
                parent.children.insert(ci + 1, right_ptr);
            }
            Node::Internal(left) => {
                let mid = t - 1;
                let promoted_key = left.keys[mid].clone();
                let mut right_keys = left.keys.split_off(mid);
                right_keys.remove(0);
                let right_children = left.children.split_off(mid + 1);
                let right_internal = Internal {
                    keys: right_keys,
                    children: right_children,
                };
                let right_ptr = Arc::new(RwLock::new(Node::Internal(right_internal)));
                parent.keys.insert(ci, promoted_key);
                parent.children.insert(ci + 1, right_ptr);
            }
        }
    }

    pub fn insert(&self, key: K, value: V) {
        let max_keys = 2 * self.t - 1;

        // --- OPTIMIZED ROOT ACQUISITION ---
        let (mut curr_arc, mut curr_guard) = loop {
            let tree_root_read_guard = self.root.read().unwrap();
            let curr_arc = Arc::clone(&*tree_root_read_guard);

            // FIX: Extend the lifetime IMMEDIATELY. This unties the borrow from curr_arc,
            // allowing us to freely move or drop curr_arc later!
            let mut curr_guard_local = unsafe { extend_write(curr_arc.write().unwrap()) };

            if curr_guard_local.key_count() < max_keys {
                drop(tree_root_read_guard);
                break (curr_arc, curr_guard_local);
            }

            drop(curr_guard_local);
            drop(tree_root_read_guard);

            let mut tree_root_write_guard = self.root.write().unwrap();
            let curr_arc = Arc::clone(&*tree_root_write_guard);
            let mut curr_guard_local = unsafe { extend_write(curr_arc.write().unwrap()) };

            if curr_guard_local.key_count() == max_keys {
                let mut new_root_internal = Internal::new();
                new_root_internal.children.push(Arc::clone(&curr_arc));

                let mut new_root = Node::Internal(new_root_internal);
                if let Node::Internal(internal) = &mut new_root {
                    Self::split_child(internal, 0, &mut curr_guard_local, self.t);
                }

                let new_root_arc = Arc::new(RwLock::new(new_root));
                *tree_root_write_guard = Arc::clone(&new_root_arc);

                drop(curr_guard_local);
                drop(tree_root_write_guard);

                let curr_arc = new_root_arc;
                let curr_guard_local = unsafe { extend_write(curr_arc.write().unwrap()) };
                break (curr_arc, curr_guard_local);
            } else {
                drop(tree_root_write_guard);
                break (curr_arc, curr_guard_local);
            }
        };

        // --- REGULAR CRABBING TRAVERSAL ---
        loop {
            let is_leaf = curr_guard.is_leaf();

            if is_leaf {
                if let Node::Leaf(leaf) = &mut *curr_guard {
                    match leaf.find_pos(&key) {
                        Ok(i) => leaf.entries[i].value = value,
                        Err(i) => {
                            leaf.entries.insert(i, LeafEntry { key, value });
                            self.len.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                break;
            } else {
                let ci = {
                    let Node::Internal(internal) = &*curr_guard else { unreachable!() };
                    internal.child_index(&key)
                };

                let next_arc = {
                    let Node::Internal(internal) = &*curr_guard else { unreachable!() };
                    Arc::clone(&internal.children[ci])
                };

                let mut next_guard = unsafe { extend_write(next_arc.write().unwrap()) };

                if next_guard.key_count() == max_keys {
                    let Node::Internal(internal) = &mut *curr_guard else { unreachable!() };
                    let mut redistributed = false;

                    if ci + 1 < internal.children.len() {
                        let right_arc = Arc::clone(&internal.children[ci + 1]);
                        if let Ok(mut right_sibling) = right_arc.try_write() {
                            if right_sibling.key_count() < max_keys - 1 {
                                Self::redistribute_right_locks(internal, ci, &mut next_guard, &mut right_sibling);
                                redistributed = true;
                            }
                        }
                    }

                    if !redistributed && ci > 0 {
                        let left_arc = Arc::clone(&internal.children[ci - 1]);
                        if let Ok(mut left_sibling) = left_arc.try_write() {
                            if left_sibling.key_count() < max_keys - 1 {
                                Self::redistribute_left_locks(internal, ci, &mut left_sibling, &mut next_guard);
                                redistributed = true;
                            }
                        }
                    }

                    if !redistributed {
                        Self::split_child(internal, ci, &mut next_guard, self.t);
                    }

                    drop(next_guard);
                    continue;
                }

                drop(curr_guard);
                curr_arc = next_arc;
                curr_guard = next_guard;
            }
        }
    }

    pub fn search(&self, key: &K) -> Option<V> {
        let tree_root_read_guard = self.root.read().unwrap();
        let mut curr_arc = Arc::clone(&*tree_root_read_guard);
        drop(tree_root_read_guard);

        let mut curr_guard = unsafe { extend_read(curr_arc.read().unwrap()) };

        loop {
            match &*curr_guard {
                Node::Leaf(leaf) => {
                    return match leaf.find_pos(key) {
                        Ok(i) => Some(leaf.entries[i].value.clone()),
                        Err(_) => None,
                    };
                }
                Node::Internal(internal) => {
                    let ci = internal.child_index(key);
                    let next_arc = Arc::clone(&internal.children[ci]);
                    let next_guard = unsafe { extend_read(next_arc.read().unwrap()) };

                    drop(curr_guard);
                    curr_arc = next_arc;
                    curr_guard = next_guard;
                }
            }
        }
    }

    pub fn remove(&self, key: &K) -> Option<V> {
        // --- OPTIMIZED ROOT ACQUISITION FOR REMOVE ---
        // Just like insert, only take a read lock initially!
        let tree_root_read_guard = self.root.read().unwrap();
        let mut curr_arc = Arc::clone(&*tree_root_read_guard);
        let mut curr_guard = unsafe { extend_write(curr_arc.write().unwrap()) };
        drop(tree_root_read_guard);

        let mut deleted_value = None;

        loop {
            let is_leaf = curr_guard.is_leaf();

            if is_leaf {
                if let Node::Leaf(leaf) = &mut *curr_guard {
                    if let Ok(i) = leaf.find_pos(key) {
                        deleted_value = Some(leaf.entries.remove(i).value);
                        self.len.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                break;
            } else {
                let ci = {
                    let Node::Internal(internal) = &*curr_guard else {
                        unreachable!()
                    };
                    internal.child_index(key)
                };

                let next_arc = {
                    let Node::Internal(internal) = &*curr_guard else {
                        unreachable!()
                    };
                    Arc::clone(&internal.children[ci])
                };

                let mut next_guard = unsafe { extend_write(next_arc.write().unwrap()) };

                if next_guard.key_count() <= self.t - 1 {
                    drop(next_guard);
                    let Node::Internal(internal) = &mut *curr_guard else {
                        unreachable!()
                    };
                    Self::rebalance(internal, ci, self.t);
                    continue;
                }

                drop(curr_guard);
                curr_arc = next_arc;
                curr_guard = next_guard;
            }
        }

        // --- OPTIMIZED ROOT SHRINK CHECK ---
        // Don't blindly take a wrapper write-lock at the end of every removal!
        let mut needs_shrink = false;

        let tree_root_read_guard = self.root.read().unwrap();
        let root_arc = Arc::clone(&*tree_root_read_guard);
        let root_guard = root_arc.read().unwrap();
        if let Node::Internal(internal) = &*root_guard {
            if internal.keys.is_empty() && !internal.children.is_empty() {
                needs_shrink = true;
            }
        }
        drop(root_guard);
        drop(tree_root_read_guard);

        if needs_shrink {
            let mut tree_root_write_guard = self.root.write().unwrap();
            let root_arc = Arc::clone(&*tree_root_write_guard);
            let mut root_guard = root_arc.write().unwrap();

            // Re-check to ensure another thread didn't beat us to it
            if let Node::Internal(internal) = &mut *root_guard {
                if internal.keys.is_empty() && !internal.children.is_empty() {
                    *tree_root_write_guard = Arc::clone(&internal.children[0]);
                }
            }
        }

        deleted_value
    }

    // [Rebalance, Borrow, and Merge functions remain unchanged...]
    fn rebalance(parent: &mut Internal<K, V>, ci: usize, t: usize) {
        let min_keys = t - 1;
        let right_count = if ci + 1 < parent.children.len() {
            parent.children[ci + 1].read().unwrap().key_count()
        } else {
            0
        };

        let left_count = if ci > 0 {
            parent.children[ci - 1].read().unwrap().key_count()
        } else {
            0
        };

        if right_count > min_keys {
            Self::borrow_from_right(parent, ci);
        } else if left_count > min_keys {
            Self::borrow_from_left(parent, ci);
        } else if ci + 1 < parent.children.len() {
            Self::merge_children(parent, ci);
        } else {
            Self::merge_children(parent, ci - 1);
        }
    }

    fn borrow_from_right(parent: &mut Internal<K, V>, ci: usize) {
        let mut left_guard = parent.children[ci].write().unwrap();
        let mut right_guard = parent.children[ci + 1].write().unwrap();
        match (&mut *left_guard, &mut *right_guard) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let removed = right.entries.remove(0);
                parent.keys[ci] = right.entries[0].key.clone();
                left.entries.push(removed);
            }
            (Node::Internal(left), Node::Internal(right)) => {
                left.keys.push(parent.keys[ci].clone());
                parent.keys[ci] = right.keys.remove(0);
                left.children.push(right.children.remove(0));
            }
            _ => unreachable!(),
        }
    }

    fn borrow_from_left(parent: &mut Internal<K, V>, ci: usize) {
        let mut left_guard = parent.children[ci - 1].write().unwrap();
        let mut right_guard = parent.children[ci].write().unwrap();
        match (&mut *left_guard, &mut *right_guard) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                let removed = left.entries.pop().unwrap();
                parent.keys[ci - 1] = removed.key.clone();
                right.entries.insert(0, removed);
            }
            (Node::Internal(left), Node::Internal(right)) => {
                right.keys.insert(0, parent.keys[ci - 1].clone());
                parent.keys[ci - 1] = left.keys.pop().unwrap();
                right.children.insert(0, left.children.pop().unwrap());
            }
            _ => unreachable!(),
        }
    }

    fn merge_children(parent: &mut Internal<K, V>, ci: usize) {
        let right_arc = parent.children.remove(ci + 1);
        let separator_key = parent.keys.remove(ci);
        let mut left_guard = parent.children[ci].write().unwrap();
        let mut right_guard = right_arc.write().unwrap();
        match (&mut *left_guard, &mut *right_guard) {
            (Node::Leaf(left), Node::Leaf(right)) => {
                left.next = right.next.clone();
                left.entries.append(&mut right.entries);
            }
            (Node::Internal(left), Node::Internal(right)) => {
                left.keys.push(separator_key);
                left.keys.append(&mut right.keys);
                left.children.append(&mut right.children);
            }
            _ => unreachable!(),
        }
    }

    pub fn iter(&self) -> LeafIter<K, V> {
        let mut curr_arc = Arc::clone(&*self.root.read().unwrap());
        loop {
            let is_leaf = { curr_arc.read().unwrap().is_leaf() };
            if is_leaf {
                break;
            }
            let next_arc = {
                let guard = curr_arc.read().unwrap();
                let Node::Internal(internal) = &*guard else {
                    unreachable!()
                };
                Arc::clone(&internal.children[0])
            };
            curr_arc = next_arc;
        }
        LeafIter {
            current: Some(curr_arc),
            pos: 0,
            end: Bound::Unbounded,
        }
    }

    pub fn range_from(&self, start: &K, end: Bound<K>) -> LeafIter<K, V> {
        let mut curr_arc = Arc::clone(&*self.root.read().unwrap());
        loop {
            let is_leaf = { curr_arc.read().unwrap().is_leaf() };
            if is_leaf {
                break;
            }
            let next_arc = {
                let guard = curr_arc.read().unwrap();
                let Node::Internal(internal) = &*guard else {
                    unreachable!()
                };
                let ci = internal.child_index(start);
                Arc::clone(&internal.children[ci])
            };
            curr_arc = next_arc;
        }

        let pos = {
            let guard = curr_arc.read().unwrap();
            let Node::Leaf(leaf) = &*guard else {
                unreachable!()
            };
            leaf.entries.partition_point(|e| &e.key < start)
        };
        LeafIter {
            current: Some(curr_arc),
            pos,
            end,
        }
    }
}

impl<K: Ord + Clone + fmt::Debug, V: Clone + fmt::Debug> BPlusTree<K, V> {
    pub fn print_tree(&self) {
        println!("BPlusTree {{ t={}, len={} }}", self.t, self.len());
        let root_arc = Arc::clone(&*self.root.read().unwrap());
        Self::print_node(&root_arc, 0);
    }

    fn print_node(node_arc: &NodePtr<K, V>, depth: usize) {
        let guard = node_arc.read().unwrap();
        let indent = "  ".repeat(depth);
        match &*guard {
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

pub struct LeafIter<K: Ord + Clone, V: Clone> {
    current: Option<NodePtr<K, V>>,
    pos: usize,
    end: Bound<K>,
}

impl<K: Ord + Clone, V: Clone> Iterator for LeafIter<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let arc = self.current.as_ref()?.clone();
            let leaf_guard = arc.read().unwrap();
            let Node::Leaf(leaf) = &*leaf_guard else {
                return None;
            };

            if self.pos < leaf.entries.len() {
                let e = &leaf.entries[self.pos];
                match &self.end {
                    Bound::Included(key) => {
                        if e.key <= *key {
                            self.pos += 1;
                            return Some((e.key.clone(), e.value.clone()));
                        }
                        return None;
                    }
                    Bound::Excluded(key) => {
                        if e.key < *key {
                            self.pos += 1;
                            return Some((e.key.clone(), e.value.clone()));
                        }
                        return None;
                    }
                    Bound::Unbounded => {
                        self.pos += 1;
                        return Some((e.key.clone(), e.value.clone()));
                    }
                }
            }

            let next_arc = leaf.next.clone();
            drop(leaf_guard);
            self.current = next_arc;
            self.pos = 0;
        }
    }
}
#[tokio::main]
async fn main() {
    // We wrap the tree in an Arc so we can share it across multiple Tokio tasks.
    // Let's use a slightly larger 't' (minimum degree) for a more realistic scenario.
    let tree = Arc::new(BPlusTree::<u64, u64>::new(10));

    println!("--- Starting Concurrent Inserts ---");
    let mut insert_tasks = vec![];

    // Spawn 4 concurrent tasks, each inserting 1,000 unique keys
    for task_id in 0..100000 {
        let tree_clone = Arc::clone(&tree);

        let handle = tokio::spawn(async move {
            let start = task_id * 10000;
            let end = start + 10000;

            for i in start..end {
                // Thread-safe latch crabbing handles the internal synchronization
                tree_clone.insert(i, i * 10);
            }
        });
        insert_tasks.push(handle);
    }

    // Wait for all insert tasks to finish
    for handle in insert_tasks {
        handle.await.unwrap();
    }

    println!("Total items inserted: {}", tree.len());

    println!("\n--- Starting Concurrent Readers ---");
    let mut read_tasks = vec![];

    // Task 1: Point Search
    let tree_clone1 = Arc::clone(&tree);
    read_tasks.push(tokio::spawn(async move {
        if let Some(val) = tree_clone1.search(&2500) {
            println!("[Task 1] Found Key 2500 | Value: {}", val);
        } else {
            println!("[Task 1] Key 2500 not found!");
        }
    }));

    // Task 2: Range Scan
    let tree_clone2 = Arc::clone(&tree);
    read_tasks.push(tokio::spawn(async move {
        println!("[Task 2] Range scanning keys 1500 to 1505...");
        for (k, v) in tree_clone2.range_from(&1500, Bound::Excluded(1506)) {
            println!("   -> Key: {} | Value: {}", k, v);
        }
    }));

    // Task 3: Full Iteration (Counting)
    let tree_clone3 = Arc::clone(&tree);
    read_tasks.push(tokio::spawn(async move {
        let total_counted = tree_clone3.iter().count();
        println!(
            "[Task 3] Iterator counted {} items in the tree.",
            total_counted
        );
    }));

    // Wait for all read tasks to finish
    for handle in read_tasks {
        handle.await.unwrap();
    }

    println!("\n--- Starting Concurrent Deletions ---");
    let mut remove_tasks = vec![];

    // Spawn 2 tasks to concurrently remove half of the items
    for task_id in 0..2 {
        let tree_clone = Arc::clone(&tree);

        let handle = tokio::spawn(async move {
            let start = task_id * 1000;
            let end = start + 500; // Remove the first 500 items from blocks 0 and 1

            for i in start..end {
                tree_clone.remove(&i);
            }
        });
        remove_tasks.push(handle);
    }

    // Wait for all delete tasks to finish
    for handle in remove_tasks {
        handle.await.unwrap();
    }

    println!("Total items remaining after deletions: {}", tree.len());

    // Validate with a final read
    println!("Checking if deleted key 10 exists: {:?}", tree.search(&10));
}

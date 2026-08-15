//! Port of `src/core/heap.ts`.
//!
//! Recomputation is height-ordered, not queue-ordered: each heap is a vector
//! of buckets indexed by node height, and each bucket is an intrusive circular
//! doubly-linked list threaded through `Node::next_heap` / `Node::prev_heap`.
//! Draining ascending by height is what makes a diamond recompute its tail
//! once rather than once per path.
//!
//! Two heaps exist — `dirty_queue` for normal invalidation and `zombie_queue`
//! for work that must survive a disposal boundary until cleanup settles — and
//! a node's own `ZOMBIE` flag decides which one it belongs to. See
//! `Runtime::heap_for`.

use std::cell::{Cell, RefCell};

use crate::constants::{reactive, EffectKind};
use crate::node::NodeId;
use crate::runtime::{node, Links, Nodes, Runtime};

/// Initial bucket capacity. Upstream preallocates 2000 slots per heap to keep
/// the common depth range allocation-free; we do the same.
const INITIAL_BUCKETS: usize = 2000;

pub(crate) struct Heap {
    buckets: RefCell<Vec<Option<NodeId>>>,
    /// Memo for `mark_heap`: set once a full marking pass has run, cleared by
    /// `run_heap` and by any unmarked node entering a marked heap.
    marked: Cell<bool>,
    /// Drain cursor. Also read by `read()` to decide whether a pull needs to
    /// bring the heap up to date first.
    min: Cell<i32>,
    max: Cell<i32>,
}

impl Heap {
    pub fn new() -> Heap {
        Heap {
            buckets: RefCell::new(vec![None; INITIAL_BUCKETS]),
            marked: Cell::new(false),
            min: Cell::new(0),
            max: Cell::new(0),
        }
    }

    #[inline]
    pub fn min(&self) -> i32 {
        self.min.get()
    }

    #[allow(dead_code, reason = "boundary queues (out of scope) drive the cursor directly")]
    #[inline]
    pub fn set_min(&self, v: i32) {
        self.min.set(v);
    }

    #[allow(dead_code, reason = "diagnostic accessor")]
    #[inline]
    pub fn max(&self) -> i32 {
        self.max.get()
    }

    #[allow(dead_code, reason = "boundary queues (out of scope) invalidate the mark memo")]
    #[inline]
    pub fn set_marked(&self, v: bool) {
        self.marked.set(v);
    }

    /// True when there is queued work at or above the drain cursor.
    #[inline]
    pub fn has_work(&self) -> bool {
        self.max.get() >= self.min.get()
    }

    /// Port of `increaseHeapSize`. Called by boundary queues, which are a
    /// separate port.
    #[allow(dead_code, reason = "boundaries.ts is out of scope")]
    pub fn increase_size(&self, n: usize) {
        let mut b = self.buckets.borrow_mut();
        if n > b.len() {
            b.resize(n, None);
        }
    }

    #[inline]
    fn head(&self, height: i32) -> Option<NodeId> {
        self.buckets.borrow().get(height as usize).copied().flatten()
    }

    #[inline]
    fn set_head(&self, height: i32, id: Option<NodeId>) {
        let mut b = self.buckets.borrow_mut();
        if height as usize >= b.len() {
            b.resize((height as usize + 1).next_power_of_two(), None);
        }
        b[height as usize] = id;
    }
}

/// Port of `actualInsertIntoHeap`.
fn actual_insert(nodes: &Nodes, heap: &Heap, id: NodeId) {
    let n = node(nodes, id);
    // A node can never sit at or below its owner's height: the owner's
    // recompute may recreate it, so it must drain strictly after.
    let parent_height = n
        .parent
        .get()
        .and_then(|p| nodes.get(p))
        .map(|p| {
            if p.is_root() {
                p.root_parent_computed
                    .get()
                    .and_then(|c| nodes.get(c))
                    .map(|c| c.height.get())
                    .unwrap_or(-1)
            } else {
                p.height.get()
            }
        })
        .unwrap_or(-1);
    if parent_height >= n.height.get() {
        n.height.set(parent_height + 1);
    }

    let height = n.height.get();
    match heap.head(height) {
        None => heap.set_head(height, Some(id)),
        Some(head_id) => {
            let head = node(nodes, head_id);
            let tail_id = head.prev_heap.get();
            node(nodes, tail_id).next_heap.set(Some(id));
            n.prev_heap.set(tail_id);
            head.prev_heap.set(id);
        }
    }
    if height > heap.max.get() {
        heap.max.set(height);
    }
}

/// Port of `insertIntoHeap`.
pub(crate) fn insert_into_heap(nodes: &Nodes, heap: &Heap, id: NodeId) {
    let n = node(nodes, id);
    let flags = n.flags.get();
    if flags & (reactive::IN_HEAP | reactive::RECOMPUTING_DEPS | reactive::MANUAL_WRITE) != 0 {
        return;
    }
    if flags & reactive::CHECK != 0 {
        n.flags
            .set((flags & !(reactive::CHECK | reactive::DIRTY)) | reactive::DIRTY | reactive::IN_HEAP);
    } else {
        n.flags.set(flags | reactive::IN_HEAP);
        // An unmarked node entering a marked heap invalidates the `mark_heap`
        // memo: `marked` is only reset by `run_heap`, so a write between two
        // mid-tick pulls would otherwise leave this node unmarked and every
        // downstream pull stale until the next flush (upstream #2922).
        if heap.marked.get() && flags & reactive::DIRTY == 0 {
            heap.marked.set(false);
        }
    }
    if flags & reactive::IN_HEAP_HEIGHT == 0 {
        actual_insert(nodes, heap, id);
    }
}

/// Port of `insertIntoHeapHeight`.
pub(crate) fn insert_into_heap_height(nodes: &Nodes, heap: &Heap, id: NodeId) {
    let n = node(nodes, id);
    let flags = n.flags.get();
    if flags
        & (reactive::IN_HEAP
            | reactive::RECOMPUTING_DEPS
            | reactive::IN_HEAP_HEIGHT
            | reactive::MANUAL_WRITE)
        != 0
    {
        return;
    }
    n.flags.set(flags | reactive::IN_HEAP_HEIGHT);
    actual_insert(nodes, heap, id);
}

/// Port of `deleteFromHeap`.
pub(crate) fn delete_from_heap(nodes: &Nodes, heap: &Heap, id: NodeId) {
    let Some(n) = nodes.get(id) else { return };
    let flags = n.flags.get();
    if flags & (reactive::IN_HEAP | reactive::IN_HEAP_HEIGHT) == 0 {
        return;
    }
    n.flags
        .set(flags & !(reactive::IN_HEAP | reactive::IN_HEAP_HEIGHT));
    let height = n.height.get();

    if n.prev_heap.get() == id {
        // Sole occupant of the bucket.
        heap.set_head(height, None);
    } else {
        let next = n.next_heap.get();
        let head_id = heap
            .head(height)
            .expect("solid_rs: heap bucket lost its head while a member was still linked");
        let end_id = next.unwrap_or(head_id);
        if id == head_id {
            heap.set_head(height, next);
        } else {
            node(nodes, n.prev_heap.get()).next_heap.set(next);
        }
        node(nodes, end_id).prev_heap.set(n.prev_heap.get());
    }
    n.prev_heap.set(id);
    n.next_heap.set(None);
}

/// Port of `markHeap`.
pub(crate) fn mark_heap(nodes: &Nodes, links: &Links, heap: &Heap) {
    if heap.marked.get() {
        return;
    }
    heap.marked.set(true);
    for i in 0..=heap.max.get() {
        let mut cur = heap.head(i);
        while let Some(id) = cur {
            let n = node(nodes, id);
            if n.has_flag(reactive::IN_HEAP) {
                mark_node(nodes, links, id, reactive::DIRTY);
            }
            cur = n.next_heap.get();
        }
    }
}

/// Port of `markNode`.
///
/// Marks a node and propagates `CHECK` downstream. `CHECK` means "an ancestor
/// changed, revalidate before trusting your cached value"; `DIRTY` means "your
/// own inputs changed". The `>= newState` early-out is what keeps a diamond
/// from re-walking a subtree once per path.
pub(crate) fn mark_node(nodes: &Nodes, links: &Links, id: NodeId, new_state: u32) {
    let n = node(nodes, id);
    let flags = n.flags.get();
    if flags & (reactive::CHECK | reactive::DIRTY) >= new_state {
        return;
    }
    n.flags
        .set((flags & !(reactive::CHECK | reactive::DIRTY)) | new_state);

    let mut cur = n.subs.get();
    while let Some(lid) = cur {
        let l = crate::runtime::link_of(links, lid);
        mark_node(nodes, links, l.sub.get(), reactive::CHECK);
        cur = l.next_sub.get();
    }

    // Firewall children (store layer, always empty here — see `Node::child`).
    let mut child = n.child.get();
    while let Some(cid) = child {
        let c = node(nodes, cid);
        let mut cur = c.subs.get();
        while let Some(lid) = cur {
            let l = crate::runtime::link_of(links, lid);
            mark_node(nodes, links, l.sub.get(), reactive::CHECK);
            cur = l.next_sub.get();
        }
        child = c.next_child.get();
    }
}

/// Port of `enqueueSub`.
///
/// Tracked effects bypass the heap and go straight to their effect queue;
/// everything else is inserted into its own (zombie-flag-routed) heap with the
/// drain cursor pulled down so an already-passed height is revisited.
pub(crate) fn enqueue_sub(rt: &Runtime, nodes: &Nodes, id: NodeId) {
    let n = node(nodes, id);
    if n.effect_kind.get() == Some(EffectKind::Tracked) {
        if !n.modified.get() {
            n.modified.set(true);
            let queue = n.queue.get();
            let runner = n
                .bound_run_effect
                .borrow()
                .clone()
                .expect("solid_rs: tracked effect has no runner");
            crate::scheduler::queue_enqueue(rt, queue, EffectKind::User, runner);
        }
        return;
    }
    let heap = rt.heap_for(n);
    if heap.min.get() > n.height.get() {
        heap.min.set(n.height.get());
    }
    insert_into_heap(nodes, heap, id);
}

/// Port of `runHeap`.
///
/// `update` runs user code, so this holds no arena borrow across the call —
/// only short borrows to read the cursor and the node's flags. `update` is
/// expected to remove the node from the heap (recompute does, via
/// `delete_from_heap`), which is what advances the bucket.
pub(crate) fn run_heap(rt: &Runtime, heap: &Heap, update: impl Fn(NodeId)) {
    heap.marked.set(false);
    heap.min.set(0);
    while heap.min.get() <= heap.max.get() {
        while let Some(id) = heap.head(heap.min.get()) {
            let in_heap = {
                let nodes = rt.nodes();
                node(&nodes, id).has_flag(reactive::IN_HEAP)
            };
            if in_heap {
                update(id);
            } else {
                let nodes = rt.nodes();
                let links = rt.links();
                adjust_height(&nodes, &links, rt, heap, id);
            }
        }
        heap.min.set(heap.min.get() + 1);
    }
    heap.max.set(0);
}

/// Port of `adjustHeight`.
fn adjust_height(nodes: &Nodes, links: &Links, rt: &Runtime, heap: &Heap, id: NodeId) {
    delete_from_heap(nodes, heap, id);
    let n = node(nodes, id);
    let mut new_height = n.height.get();

    let mut cur = n.deps.get();
    while let Some(lid) = cur {
        let l = crate::runtime::link_of(links, lid);
        let dep_id = resolve_firewall(nodes, l.dep.get());
        let dep = node(nodes, dep_id);
        if dep.is_computed() && dep.height.get() >= new_height {
            new_height = dep.height.get() + 1;
        }
        cur = l.next_dep.get();
    }

    if n.height.get() != new_height {
        n.height.set(new_height);
        let mut cur = n.subs.get();
        while let Some(lid) = cur {
            let l = crate::runtime::link_of(links, lid);
            let sub_id = l.sub.get();
            // Route each subscriber by its own zombie flag. Inserting into the
            // running heap unconditionally can park a zombie in the dirty heap
            // (or a live node in the zombie heap), breaking the flag/heap
            // invariant `delete_from_heap` relies on — the #2759 corruption
            // class.
            let sub_heap = rt.heap_for(node(nodes, sub_id));
            insert_into_heap_height(nodes, sub_heap, sub_id);
            cur = l.next_sub.get();
        }
    }
}

/// A store-backed read routes through the firewall computed behind the signal.
/// Always the identity here — see [`Node::firewall`].
#[inline]
pub(crate) fn resolve_firewall(nodes: &Nodes, id: NodeId) -> NodeId {
    nodes
        .get(id)
        .and_then(|n| n.firewall.get())
        .unwrap_or(id)
}

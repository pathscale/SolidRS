//! Port of `src/core/owner.ts`: the ownership tree, disposal, cleanup
//! ordering, and the hydration id scheme.
//!
//! Disposal ordering is a pinned contract, not an implementation detail, so
//! every step here runs with arena borrows released around user callbacks
//! rather than deferring them to a later safe point — deferring would reorder
//! cleanups.

use std::rc::Rc;

use crate::constants::{config, reactive};
use crate::graph::unlink_subs;
use crate::heap::{delete_from_heap, insert_into_heap, insert_into_heap_height};
use crate::node::{Disposable, Node, NodeId, NodeKind};
use crate::runtime::{node, with_rt, Runtime};

/// Port of `markDisposal`: flag a subtree zombie and migrate its queued work
/// into the zombie heap.
///
/// Height-adjust entries migrate too, not just recompute entries: every
/// `delete_from_heap` call site picks the heap from the zombie flag, so a node
/// left physically linked in the dirty heap after being zombified gets
/// unlinked from the wrong bucket on dispose, corrupting it and livelocking
/// the next drain (upstream #2759).
pub(crate) fn mark_disposal(rt: &Runtime, el: NodeId) {
    let nodes = rt.nodes();
    fn walk(rt: &Runtime, nodes: &crate::runtime::Nodes, el: NodeId) {
        let mut child = node(nodes, el).first_child.get();
        while let Some(cid) = child {
            let c = node(nodes, cid);
            let flags = c.flags.get();
            c.flags.set(flags | reactive::ZOMBIE);

            if flags & (reactive::IN_HEAP | reactive::IN_HEAP_HEIGHT) != 0 {
                let from = if flags & reactive::ZOMBIE != 0 {
                    &rt.zombie_queue
                } else {
                    &rt.dirty_queue
                };
                delete_from_heap(nodes, from, cid);
                if flags & reactive::IN_HEAP != 0 {
                    insert_into_heap(nodes, &rt.zombie_queue, cid);
                } else {
                    insert_into_heap_height(nodes, &rt.zombie_queue, cid);
                }
            }

            walk(rt, nodes, cid);
            child = c.next_sibling.get();
        }
    }
    walk(rt, &nodes, el);
}

/// Port of `dispose`: tear down a single computation.
pub(crate) fn dispose(rt: &Runtime, id: NodeId) {
    // Leave every scheduler heap on disposal (mirrors `unobserved`): a node
    // still queued here would be recomputed by the next flush, and `recompute`
    // rewriting flags would clear DISPOSED — resurrecting it (upstream #2983).
    {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(id) else { return };
        delete_from_heap(&nodes, rt.heap_for(n), id);
    }

    let mut to_remove = {
        let nodes = rt.nodes();
        nodes.get(id).and_then(|n| n.deps.get())
    };
    while let Some(l) = to_remove {
        to_remove = unlink_subs(rt, l);
    }
    {
        let nodes = rt.nodes();
        if let Some(n) = nodes.get(id) {
            n.deps.set(None);
            n.deps_tail.set(None);
        }
    }

    dispose_children(rt, id, true, false);
}

/// Port of `disposeChildren`.
///
/// `self_` disposes the node itself as well as its children; `zombie` walks
/// the detached `pending_first_child` chain instead of the live one.
pub(crate) fn dispose_children(rt: &Runtime, id: NodeId, self_: bool, zombie: bool) {
    let flags = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(id) else { return };
        let flags = n.flags.get();
        if flags & reactive::DISPOSED != 0 {
            return;
        }
        if self_ {
            n.flags.set(flags | reactive::DISPOSED);
            // Upstream also snaps `isPending`/`latest` companions here so a
            // disposed source cannot leave a verdict latched true forever
            // (INV-9). The verdict layer is out of scope; the call site is
            // noted in `porting-reply.md`.
        }
        flags
    };

    // Children first, newest-first: `create_owner` prepends, so the live chain
    // is reverse creation order, which is the documented unwind order.
    let mut child = {
        let nodes = rt.nodes();
        let n = node(&nodes, id);
        if zombie {
            n.pending_first_child.get()
        } else {
            n.first_child.get()
        }
    };

    while let Some(cid) = child {
        let next_child = {
            let nodes = rt.nodes();
            let Some(c) = nodes.get(cid) else { break };
            let next = c.next_sibling.get();

            // Heap removal must not be gated on `deps`: a dependency-free
            // computation queued by `refresh()` has a null dep list but still
            // sits in the dirty heap, and left there the post-disposal flush
            // recomputes it — `recompute` rewriting flags clears DISPOSED and
            // the node comes back to life (upstream #2983).
            if c.has_flag(reactive::IN_HEAP | reactive::IN_HEAP_HEIGHT) {
                delete_from_heap(&nodes, rt.heap_for(c), cid);
            }
            next
        };

        let mut to_remove = {
            let nodes = rt.nodes();
            nodes.get(cid).and_then(|c| c.deps.get())
        };
        if to_remove.is_some() {
            while let Some(l) = to_remove {
                to_remove = unlink_subs(rt, l);
            }
            let nodes = rt.nodes();
            if let Some(c) = nodes.get(cid) {
                c.deps.set(None);
                c.deps_tail.set(None);
            }
        }

        dispose_children(rt, cid, true, false);
        child = next_child;
    }

    {
        let nodes = rt.nodes();
        let n = node(&nodes, id);
        if zombie {
            n.pending_first_child.set(None);
        } else {
            n.first_child.set(None);
            n.child_count.set(0);
        }
    }

    // O(1) splice out of the parent's chain on individual dispose. Skipped
    // during batch dispose (the parent is already disposed) and zombie
    // disposal (the node sits on the parent's pending chain). `next_sibling`
    // is deliberately left intact so an outer walk that already advanced past
    // this node still reaches later siblings.
    if self_ && !zombie && flags & reactive::ZOMBIE == 0 {
        let nodes = rt.nodes();
        let n = node(&nodes, id);
        if let Some(parent_id) = n.parent.get() {
            let parent_disposed = nodes
                .get(parent_id)
                .map(|p| p.has_flag(reactive::DISPOSED))
                .unwrap_or(true);
            if !parent_disposed {
                let prev = n.prev_sibling.get();
                let next = n.next_sibling.get();
                match prev {
                    Some(p) => node(&nodes, p).next_sibling.set(next),
                    None => node(&nodes, parent_id).first_child.set(next),
                }
                if let Some(nx) = next {
                    node(&nodes, nx).prev_sibling.set(prev);
                }
                n.prev_sibling.set(None);
            }
        }
    }

    run_disposal(rt, id, zombie);

    // The final effect-returned cleanup fires at true disposal, after the
    // disposal list, mirroring rerun ordering (compute-phase teardown first,
    // cleanup last).
    if self_ {
        let cleanup = {
            let nodes = rt.nodes();
            nodes.get(id).and_then(|n| n.cleanup.borrow_mut().take())
        };
        if let Some(c) = cleanup {
            c();
        }
    }
}

/// Port of `runDisposal`. FIFO within one owner: the registration order of
/// `on_cleanup` calls.
fn run_disposal(rt: &Runtime, id: NodeId, zombie: bool) {
    let taken: Vec<Disposable> = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(id) else { return };
        let mut slot = if zombie {
            n.pending_disposal.borrow_mut()
        } else {
            n.disposal.borrow_mut()
        };
        if slot.is_empty() {
            return;
        }
        std::mem::take(&mut *slot)
    };
    for f in taken {
        f();
    }
}

// ---------------------------------------------------------------- id scheme

/// Port of `formatId`. Base-36 with a length prefix so ids sort and nest
/// without a separator.
fn format_id(prefix: &str, id: u32) -> Rc<str> {
    let num = to_base36(id);
    let len = num.len() - 1;
    let mut out = String::with_capacity(prefix.len() + num.len() + 1);
    out.push_str(prefix);
    if len > 0 {
        out.push((64u8 + len as u8) as char);
    }
    out.push_str(&num);
    out.into()
}

fn to_base36(mut v: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if v == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::new();
    while v > 0 {
        buf.push(DIGITS[(v % 36) as usize]);
        v /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).expect("base36 digits are ASCII")
}

/// Port of `childId`. Transparent owners delegate to the nearest
/// non-transparent ancestor, so wrapping a subtree in one does not shift the
/// ids its siblings receive.
fn child_id(rt: &Runtime, owner: NodeId, consume: bool) -> Result<Rc<str>, IdError> {
    let nodes = rt.nodes();
    let mut counter = owner;
    loop {
        let n = node(&nodes, counter);
        if n.has_config(config::TRANSPARENT) {
            if let Some(p) = n.parent.get() {
                counter = p;
                continue;
            }
        }
        break;
    }
    let n = node(&nodes, counter);
    let prefix = n.id.borrow().clone();
    match prefix {
        Some(prefix) => {
            let c = n.child_count.get();
            if consume {
                n.child_count.set(c + 1);
            }
            Ok(format_id(&prefix, c))
        }
        None => Err(IdError),
    }
}

/// Upstream throws `"Cannot get child id from owner without an id"`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct IdError;

impl std::fmt::Display for IdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cannot get child id from owner without an id")
    }
}

impl std::error::Error for IdError {}

/// Allocate and return the next stable child id for `owner`.
///
/// Port of `getNextChildId`. Used by hydration plumbing; not part of the
/// user-facing API.
pub fn get_next_child_id(owner: crate::Owner) -> Result<Rc<str>, IdError> {
    with_rt(|rt| child_id(rt, owner.id(), true))
}

/// Return the *next* child id for `owner` without consuming it.
///
/// Port of `peekNextChildId`.
pub fn peek_next_child_id(owner: crate::Owner) -> Result<Rc<str>, IdError> {
    with_rt(|rt| child_id(rt, owner.id(), false))
}

/// Port of `inheritId`: the id a freshly created node takes.
pub(crate) fn inherit_id(
    rt: &Runtime,
    explicit: Option<Rc<str>>,
    transparent: bool,
    parent: Option<NodeId>,
) -> Option<Rc<str>> {
    if explicit.is_some() {
        return explicit;
    }
    let parent = parent?;
    let parent_has_id = {
        let nodes = rt.nodes();
        nodes.get(parent).and_then(|p| p.id.borrow().clone())
    };
    match (transparent, parent_has_id) {
        (true, id) => id,
        (false, Some(_)) => child_id(rt, parent, true).ok(),
        (false, None) => None,
    }
}

// ------------------------------------------------------------- owner helpers

/// Port of `cleanup`: register a disposal callback on the current owner.
///
/// Returns `false` when there is no owner — the callback is dropped, matching
/// upstream's no-op.
pub(crate) fn cleanup(rt: &Runtime, f: Disposable) -> bool {
    let Some(ctx) = rt.ctx.get() else { return false };
    let nodes = rt.nodes();
    let Some(n) = nodes.get(ctx) else { return false };
    n.disposal.borrow_mut().push(f);
    true
}

/// Port of `isDisposed`.
pub(crate) fn is_disposed(rt: &Runtime, id: NodeId) -> bool {
    let nodes = rt.nodes();
    nodes
        .get(id)
        .map(|n| n.has_flag(reactive::DISPOSED | reactive::ZOMBIE))
        .unwrap_or(true)
}

/// Port of `createOwner`: a fresh owner attached to the current one, or a
/// detached root if there is none.
pub(crate) fn create_owner(
    rt: &Runtime,
    explicit_id: Option<Rc<str>>,
    transparent: bool,
) -> NodeId {
    let parent = rt.ctx.get();

    #[cfg(feature = "dev")]
    if let Some(p) = parent {
        let forbidden = {
            let nodes = rt.nodes();
            nodes
                .get(p)
                .map(|n| n.has_config(config::CHILDREN_FORBIDDEN))
                .unwrap_or(false)
        };
        if forbidden {
            panic!("{}", crate::core::PRIMITIVE_IN_FORBIDDEN_SCOPE_MESSAGE);
        }
    }

    let (queue, context, parent_computed) = {
        let nodes = rt.nodes();
        match parent.and_then(|p| nodes.get(p)) {
            Some(p) => (
                p.queue.get(),
                p.context.borrow().clone(),
                if p.is_root() {
                    p.root_parent_computed.get()
                } else {
                    parent
                },
            ),
            None => (rt.global_queue.get(), rt.default_context.clone(), None),
        }
    };

    let id = inherit_id(rt, explicit_id, transparent, parent);

    let mut owner = Node::blank(NodeKind::Root, queue, context);
    owner.config = std::cell::Cell::new(if transparent { config::TRANSPARENT } else { 0 });
    owner.parent = std::cell::Cell::new(parent);
    owner.root_parent_computed = std::cell::Cell::new(parent_computed);
    *owner.id.get_mut() = id;

    let owner_id = rt.alloc_node(owner);
    attach_to_parent(rt, owner_id, parent);
    owner_id
}

/// Prepend a fresh node to its owner's child chain.
///
/// Prepending (rather than appending) is what makes the live chain reverse
/// creation order, which is the documented cleanup unwind order.
pub(crate) fn attach_to_parent(rt: &Runtime, id: NodeId, parent: Option<NodeId>) {
    let Some(parent_id) = parent else { return };
    let nodes = rt.nodes();
    let Some(p) = nodes.get(parent_id) else { return };
    let n = node(&nodes, id);
    match p.first_child.get() {
        None => p.first_child.set(Some(id)),
        Some(last) => {
            n.next_sibling.set(Some(last));
            node(&nodes, last).prev_sibling.set(Some(id));
            p.first_child.set(Some(id));
        }
    }
}

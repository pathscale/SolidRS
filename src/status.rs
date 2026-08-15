//! The synchronous half of `src/core/async.ts`.
//!
//! `async.ts` itself is out of scope, but `recompute` calls into four of its
//! functions on paths a purely synchronous graph reaches — error propagation,
//! status clearing, and the silent-recovery sweep — so those are ported here
//! rather than left as holes. Everything promise- or iterator-shaped
//! (`handleAsync`, `settlePendingSource`, `parkLoadingWindow`) stays with the
//! async port.
//!
//! What survives here is the part that makes errors behave: an error is stored
//! on the node that produced it, propagated downstream **by object identity**,
//! and re-delivered to late readers rather than being swallowed.

use std::collections::HashSet;

use crate::constants::{config, reactive, status};
use crate::error::ReactiveError;
use crate::heap::enqueue_sub;
use crate::node::{LinkId, NodeId};
use crate::runtime::{link_of, node, Runtime};
use crate::scheduler::{queue_pending_node, schedule};

/// Port of `addPendingSource`.
pub(crate) fn add_pending_source(rt: &Runtime, el: NodeId, source: NodeId) -> bool {
    let nodes = rt.nodes();
    let Some(n) = nodes.get(el) else { return false };
    let mut slot = n.pending_sources.borrow_mut();
    slot.get_or_insert_with(HashSet::new).insert(source)
}

/// Port of `clearPendingSources`.
pub(crate) fn clear_pending_sources(rt: &Runtime, el: NodeId) {
    let nodes = rt.nodes();
    if let Some(n) = nodes.get(el) {
        *n.pending_sources.borrow_mut() = None;
    }
}

fn has_pending_source(rt: &Runtime, el: NodeId, source: NodeId) -> bool {
    let nodes = rt.nodes();
    nodes
        .get(el)
        .map(|n| {
            n.pending_sources
                .borrow()
                .as_ref()
                .map(|s| s.contains(&source))
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Port of `setPendingError`.
pub(crate) fn set_pending_error(
    rt: &Runtime,
    el: NodeId,
    source: Option<NodeId>,
    error: Option<ReactiveError>,
) {
    let nodes = rt.nodes();
    let Some(n) = nodes.get(el) else { return };
    let Some(source) = source else {
        *n.error.borrow_mut() = None;
        return;
    };
    if let Some(e) = &error {
        if e.is_not_ready() && e.not_ready_source() == Some(source) {
            *n.error.borrow_mut() = Some(e.clone());
            return;
        }
    }
    let keep = {
        let cur = n.error.borrow();
        matches!(&*cur, Some(c) if c.is_not_ready() && c.not_ready_source() == Some(source))
    };
    if !keep {
        *n.error.borrow_mut() = Some(ReactiveError::not_ready(Some(source)));
    }
}

/// Port of `forEachDependent`: subscribers, then the subscribers of each
/// firewall child.
///
/// Returns `(sub, link)` pairs materialised under one borrow, because callers
/// run engine work that can relink.
pub(crate) fn dependents_of(rt: &Runtime, el: NodeId) -> Vec<(NodeId, LinkId)> {
    let nodes = rt.nodes();
    let links = rt.links();
    let mut out = Vec::new();
    let Some(n) = nodes.get(el) else { return out };

    let mut cur = n.subs.get();
    while let Some(lid) = cur {
        let l = link_of(&links, lid);
        out.push((l.sub.get(), lid));
        cur = l.next_sub.get();
    }
    // Firewall children — the store layer's shape, always empty here.
    let mut child = n.child.get();
    while let Some(cid) = child {
        let c = node(&nodes, cid);
        let mut cur = c.subs.get();
        while let Some(lid) = cur {
            let l = link_of(&links, lid);
            out.push((l.sub.get(), lid));
            cur = l.next_sub.get();
        }
        child = c.next_child.get();
    }
    out
}

/// Port of `clearStatus`.
pub(crate) fn clear_status(rt: &Runtime, el: NodeId, clear_uninitialized: bool) {
    let notify = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        if n.pending_sources.borrow().is_some() {
            *n.pending_sources.borrow_mut() = None;
        }
        if n.blocked.get() {
            n.blocked.set(false);
        }
        // The pending window is over; its quiet classification dies with it.
        n.reask.set(false);
        n.status_flags.set(if clear_uninitialized {
            0
        } else {
            n.status_flags.get() & status::UNINITIALIZED
        });
        let had_error = n.error.borrow().is_some();
        if had_error {
            *n.error.borrow_mut() = None;
        }
        let out = n.notify_status.borrow().clone();
        out
    };
    if let Some(f) = notify {
        f(None, None);
    }
}

/// Port of `notifyStatus`.
///
/// Propagation is gated on *change*: a subscriber that already holds this
/// exact error object (identity, not equality) and has no pending sources is
/// not re-notified. That is what keeps a diamond from walking its tail once
/// per path, and what makes the silent-recovery sweep below necessary.
pub(crate) fn notify_status(
    rt: &Runtime,
    el: NodeId,
    st: u32,
    error: Option<ReactiveError>,
    block_status: bool,
) {
    // Wrap a bare user error so the graph can track which node produced it.
    let error = error.map(|e| {
        if st == status::ERROR && !e.is_not_ready() && e.unwrap_status().is_some() && !is_status(&e)
        {
            ReactiveError::status(el, e.unwrap_status().expect("user payload"))
        } else {
            e
        }
    });

    let pending_source = match (&error, st) {
        (Some(e), s) if s == status::PENDING && e.is_not_ready() => e.not_ready_source(),
        _ => None,
    };

    // `isOptimisticBoundary` / `startsBlocking` upstream additionally consult
    // the node's optimistic override slot. Overrides are created only by
    // `optimistic.ts`, so both are constant `false` here.
    let starts_blocking = false;

    if !block_status {
        if let (status::PENDING, Some(src)) = (st, pending_source) {
            add_pending_source(rt, el, src);
            {
                let nodes = rt.nodes();
                if let Some(n) = nodes.get(el) {
                    n.status_flags
                        .set(status::PENDING | (n.status_flags.get() & status::UNINITIALIZED));
                }
            }
            set_pending_error(rt, el, Some(src), error.clone());
        } else {
            clear_pending_sources(rt, el);
            let nodes = rt.nodes();
            if let Some(n) = nodes.get(el) {
                n.status_flags.set(
                    st | if st != status::ERROR {
                        n.status_flags.get() & status::UNINITIALIZED
                    } else {
                        0
                    },
                );
                *n.error.borrow_mut() = error.clone();
            }
        }
    }

    let downstream_block = block_status || starts_blocking;

    let own_notify = {
        let nodes = rt.nodes();
        nodes.get(el).and_then(|n| n.notify_status.borrow().clone())
    };
    if let Some(f) = own_notify {
        if block_status && st == status::PENDING {
            return;
        }
        if downstream_block {
            f(Some(st), error);
        } else {
            f(None, None);
        }
        return;
    }

    let clock = rt.clock.get();
    for (sub, lid) in dependents_of(rt, el) {
        let (should_recurse, pending_observer) = {
            let nodes = rt.nodes();
            let links = rt.links();
            let Some(s) = nodes.get(sub) else { continue };
            s.time.set(clock);
            let Some(l) = links.get(lid) else { continue };
            let cond = if st == status::PENDING {
                match pending_source {
                    Some(src) => !s
                        .pending_sources
                        .borrow()
                        .as_ref()
                        .map(|p| p.contains(&src))
                        .unwrap_or(false),
                    None => false,
                }
            } else {
                let differs = match (&*s.error.borrow(), &error) {
                    (Some(a), Some(b)) => !ReactiveError::same(a, b),
                    (None, None) => false,
                    _ => true,
                };
                differs || s.pending_sources.borrow().is_some()
            };
            (cond, l.pending_observer.get())
        };
        if !should_recurse {
            continue;
        }

        // A pending-observer link is the subscription an `isPending` read
        // created. It exists so the observer re-runs when the source settles,
        // but it must not carry a real (non-NotReady) error — the synchronous
        // `isPending` read swallows those, and this path must match. Re-run the
        // observer so the verdict re-evaluates instead of forwarding.
        let is_not_ready = error.as_ref().map(|e| e.is_not_ready()).unwrap_or(false);
        if pending_observer && st != status::PENDING && !is_not_ready {
            {
                let nodes = rt.nodes();
                enqueue_sub(rt, &nodes, sub);
            }
            schedule(rt);
            continue;
        }

        let has_transition = {
            let nodes = rt.nodes();
            nodes.get(sub).map(|s| s.transition.get().is_some()).unwrap_or(false)
        };
        if !downstream_block && !has_transition {
            queue_pending_node(rt, sub);
        }
        notify_status(rt, sub, st, error.clone(), downstream_block);
    }
}

fn is_status(e: &ReactiveError) -> bool {
    matches!(*e.0, crate::error::ErrorKind::Status { .. })
}

/// Port of `settleErroredDependents`.
///
/// A node in `STATUS_ERROR` that recovers by recomputing to an *unchanged*
/// value fires no value notification — the recovery is completely silent. But
/// a dependent that re-ran during the error window consumed its dirty flag and
/// committed nothing, so its committed value is stale. The propagated error is
/// one object identity down the whole dependent tree, and holding it is
/// exactly the "blocked on this error" marker — so re-enqueue those holders.
pub(crate) fn settle_errored_dependents(rt: &Runtime, el: NodeId, error: &ReactiveError) {
    let mut scheduled = false;
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut stack: Vec<NodeId> = dependents_of(rt, el).into_iter().map(|(s, _)| s).collect();

    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        let holds = {
            let nodes = rt.nodes();
            nodes
                .get(id)
                .map(|n| matches!(&*n.error.borrow(), Some(e) if ReactiveError::same(e, error)))
                .unwrap_or(false)
        };
        if holds {
            let nodes = rt.nodes();
            enqueue_sub(rt, &nodes, id);
            scheduled = true;
        }
        stack.extend(dependents_of(rt, id).into_iter().map(|(s, _)| s));
    }

    if scheduled {
        schedule(rt);
    }
}

/// Port of `releaseSettledDependents`.
///
/// `notify_status(ERROR)` clears dependents' pending sources through its own
/// recursion, so after the propagation completes the same graph is walked for
/// lazy nodes stranded without subscribers. Collect-then-release, so
/// `unobserved` never unlinks under the walk.
#[allow(dead_code)]
pub(crate) fn release_settled_dependents(rt: &Runtime, el: NodeId) {
    let mut candidates: Vec<NodeId> = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut stack: Vec<NodeId> = dependents_of(rt, el).into_iter().map(|(s, _)| s).collect();

    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        {
            let nodes = rt.nodes();
            if let Some(n) = nodes.get(id) {
                if n.subs.get().is_none() && n.has_config(config::AUTO_DISPOSE) {
                    candidates.push(id);
                }
            }
        }
        stack.extend(dependents_of(rt, id).into_iter().map(|(s, _)| s));
    }

    for id in candidates {
        let release = {
            let nodes = rt.nodes();
            match nodes.get(id) {
                Some(n) => {
                    n.is_computed()
                        && n.has_config(config::AUTO_DISPOSE)
                        && n.subs.get().is_none()
                        && !n.has_flag(reactive::ZOMBIE)
                        && !n.has_status(status::PENDING)
                }
                None => false,
            }
        };
        if release {
            crate::graph::unobserved(rt, id);
        }
    }
}

/// Kept for the async port: a blocked node re-registers here.
#[allow(dead_code)]
pub(crate) fn is_blocked_on(rt: &Runtime, el: NodeId, source: NodeId) -> bool {
    has_pending_source(rt, el, source)
}

/// Port of `settlePendingSource`.
///
/// Walks the dependent graph clearing `el` from every `pending_sources` set. A
/// node with sources left keeps its pending status (still blocked on something
/// else); one that empties settles, and re-runs if it was `blocked`.
///
/// This is the counterpart to `notify_status(PENDING, …)`: propagation adds the
/// source, settling removes it, and the node is pending for exactly the window
/// between.
pub(crate) fn settle_pending_source(rt: &Runtime, el: NodeId) {
    let mut scheduled = false;
    let mut released: Vec<NodeId> = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut stack: Vec<NodeId> = dependents_of(rt, el).into_iter().map(|(s, _)| s).collect();
    let clock = rt.clock.get();

    while let Some(id) = stack.pop() {
        if visited.contains(&id) {
            continue;
        }
        // `remove_pending_source` is the gate: a node that was not blocked on
        // `el` is not part of this settle and its subtree is not walked.
        let removed = {
            let nodes = rt.nodes();
            match nodes.get(id) {
                Some(n) => {
                    let mut slot = n.pending_sources.borrow_mut();
                    let hit = slot.as_mut().map(|s| s.remove(&el)).unwrap_or(false);
                    if hit && slot.as_ref().map(|s| s.is_empty()).unwrap_or(false) {
                        *slot = None;
                    }
                    hit
                }
                None => false,
            }
        };
        if !removed {
            continue;
        }
        visited.insert(id);

        let remaining = {
            let nodes = rt.nodes();
            let Some(n) = nodes.get(id) else { continue };
            n.time.set(clock);
            let out = n
                .pending_sources
                .borrow()
                .as_ref()
                .and_then(|s| s.iter().next().copied());
            out
        };

        let errored = {
            let nodes = rt.nodes();
            nodes.get(id).map(|n| n.has_status(status::ERROR)).unwrap_or(false)
        };

        match remaining {
            Some(next) => {
                // Still blocked on another source: keep pending, re-point the
                // error at whatever is still in flight.
                if !errored {
                    set_pending_error(rt, id, Some(next), None);
                }
                crate::verdict::update_pending_signal(rt, id);
            }
            None => {
                {
                    let nodes = rt.nodes();
                    let Some(n) = nodes.get(id) else { continue };
                    n.status_flags.set(n.status_flags.get() & !status::PENDING);
                }
                if !errored {
                    set_pending_error(rt, id, None, None);
                }
                crate::verdict::update_pending_signal(rt, id);

                let blocked = {
                    let nodes = rt.nodes();
                    nodes.get(id).map(|n| n.blocked.get()).unwrap_or(false)
                };
                if blocked {
                    let nodes = rt.nodes();
                    enqueue_sub(rt, &nodes, id);
                    scheduled = true;
                }
                {
                    let nodes = rt.nodes();
                    if let Some(n) = nodes.get(id) {
                        n.blocked.set(false);
                        // Fully settled with nobody watching: release candidate.
                        if n.subs.get().is_none() && n.has_config(config::AUTO_DISPOSE) {
                            released.push(id);
                        }
                    }
                }
            }
        }

        stack.extend(dependents_of(rt, id).into_iter().map(|(s, _)| s));
    }

    // Release before scheduling: `unobserved` pulls the node back out of the
    // heap, so the `enqueue_sub` above never recomputes a released node.
    for id in released {
        let release = {
            let nodes = rt.nodes();
            match nodes.get(id) {
                Some(n) => {
                    n.is_computed()
                        && n.has_config(config::AUTO_DISPOSE)
                        && n.subs.get().is_none()
                        && !n.has_flag(reactive::ZOMBIE)
                        && !n.has_status(status::PENDING)
                }
                None => false,
            }
        };
        if release {
            crate::graph::unobserved(rt, id);
        }
    }

    if scheduled {
        schedule(rt);
    }
}

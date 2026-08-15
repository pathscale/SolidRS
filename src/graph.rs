//! Port of `src/core/graph.ts`: dependency-edge bookkeeping.
//!
//! Upstream's `link`/`unlinkSubs` derive from alien-signals v2.0.3 and are
//! kept structurally identical here, including the generation-stamp trick that
//! replaces the old O(n²) `isValidLink` scan.
//!
//! # Borrow shape
//!
//! [`link`] cannot reach user code, so it takes the arenas as parameters and
//! the caller owns the borrow. [`unlink_subs`] and [`unobserved`] *can* reach
//! user code — the `unobserved` callback and, through auto-dispose, the whole
//! cleanup chain — so they take short borrows internally and drop them before
//! calling out. That is what keeps cleanup ordering identical to upstream:
//! nothing is deferred to a later safe point.

use std::rc::Rc;

use crate::constants::{config, reactive, status};
use crate::node::{Link, LinkId, NodeId};
use crate::runtime::{link_of, node, Links, Nodes, Runtime};
use crate::{heap, owner};

/// Port of `unlinkSubs`. Returns the subscriber's next dependency link, so a
/// caller can walk and unlink a whole dep list.
///
/// Firing `unobserved` and the last-one-out teardown happen after the arena
/// borrows are released.
pub(crate) fn unlink_subs(rt: &Runtime, link_id: LinkId) -> Option<LinkId> {
    let mut fire: Option<Rc<dyn Fn()>> = None;
    let mut teardown: Option<NodeId> = None;
    let next_dep;

    {
        let nodes = rt.nodes();
        let mut links = rt.links_mut();

        let l = link_of(&links, link_id);
        let dep_id = l.dep.get();
        next_dep = l.next_dep.get();
        let next_sub = l.next_sub.get();
        let prev_sub = l.prev_sub.get();

        let dep = node(&nodes, dep_id);

        match next_sub {
            Some(ns) => link_of(&links, ns).prev_sub.set(prev_sub),
            None => dep.subs_tail.set(prev_sub),
        }

        match prev_sub {
            Some(ps) => link_of(&links, ps).next_sub.set(next_sub),
            None => {
                dep.subs.set(next_sub);
                if next_sub.is_none() {
                    fire = dep.unobserved.borrow().clone();
                    // No more subscribers; only tear down if AUTO_DISPOSE is
                    // set. A pending node is exempt: its in-flight async work
                    // (or the transition holding it) is an observer, and
                    // tearing down would orphan that work and re-execute it on
                    // the next read. The settle path runs this same
                    // last-one-out check when that observer releases.
                    if dep.is_computed()
                        && dep.has_config(config::AUTO_DISPOSE)
                        && !dep.has_flag(reactive::ZOMBIE)
                        && !dep.has_status(status::PENDING)
                    {
                        teardown = Some(dep_id);
                    }
                }
            }
        }

        links.remove(link_id);
    }

    if let Some(f) = fire {
        f();
    }
    if let Some(id) = teardown {
        unobserved(rt, id);
    }
    next_dep
}

/// Port of `trimStaleDeps`: drop every dependency past the validated prefix a
/// recompute rebuilt.
pub(crate) fn trim_stale_deps(rt: &Runtime, el: NodeId) {
    let (deps_tail, mut to_remove) = {
        let nodes = rt.nodes();
        let links = rt.links();
        let n = node(&nodes, el);
        let deps_tail = n.deps_tail.get();
        let start = match deps_tail {
            Some(t) => link_of(&links, t).next_dep.get(),
            None => n.deps.get(),
        };
        (deps_tail, start)
    };

    if to_remove.is_none() {
        return;
    }
    while let Some(l) = to_remove {
        to_remove = unlink_subs(rt, l);
    }

    let nodes = rt.nodes();
    let links = rt.links();
    match deps_tail {
        Some(t) => link_of(&links, t).next_dep.set(None),
        None => node(&nodes, el).deps.set(None),
    }
}

/// Port of `unobserved`: release a computed that has lost its last subscriber.
pub(crate) fn unobserved(rt: &Runtime, el: NodeId) {
    {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        heap::delete_from_heap(&nodes, rt.heap_for(n), el);
    }

    let mut dep = {
        let nodes = rt.nodes();
        node(&nodes, el).deps.get()
    };
    while let Some(l) = dep {
        dep = unlink_subs(rt, l);
    }

    {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        n.deps.set(None);
        n.deps_tail.set(None);
    }

    owner::dispose_children(rt, el, true, false);
}

/// Port of `link`.
///
/// The dependency edge is created, revalidated in place, or recognised as a
/// repeat touch. The generation stamp (`Link::gen`) answers "is this link
/// already inside the validated `[deps..deps_tail]` prefix?" in O(1); the
/// alien-signals ancestor walked the dep list for that answer, which is
/// quadratic when a computation re-reads earlier deps out of order.
pub(crate) fn link(
    nodes: &Nodes,
    links: &mut Links,
    dep_id: NodeId,
    sub_id: NodeId,
    pending_observer: bool,
) {
    let sub = node(nodes, sub_id);

    // Repeat touches within one pass AND-combine `pending_observer`: a probe
    // read beside a value read of the same dep must not relabel the value
    // dependency as probe-only — the value read is what real-error propagation
    // keys off, regardless of read order within the computation.
    let prev_dep = sub.deps_tail.get();
    if let Some(pd) = prev_dep {
        let l = link_of(links, pd);
        if l.dep.get() == dep_id {
            l.pending_observer
                .set(l.pending_observer.get() && pending_observer);
            return;
        }
    }

    let mut next_dep: Option<LinkId> = None;
    let is_recomputing = sub.has_flag(reactive::RECOMPUTING_DEPS);
    if is_recomputing {
        next_dep = match prev_dep {
            Some(pd) => link_of(links, pd).next_dep.get(),
            None => sub.deps.get(),
        };
        if let Some(nd) = next_dep {
            let l = link_of(links, nd);
            if l.dep.get() == dep_id {
                l.gen.set(sub.dep_gen.get());
                sub.deps_tail.set(Some(nd));
                // First touch of this pass: the previous pass's label is stale.
                l.pending_observer.set(pending_observer);
                return;
            }
        }
    }

    let dep = node(nodes, dep_id);
    let prev_sub = dep.subs_tail.get();
    if let Some(ps) = prev_sub {
        let l = link_of(links, ps);
        if l.sub.get() == sub_id && (!is_recomputing || l.gen.get() == sub.dep_gen.get()) {
            // Gen-matched during a recompute is a repeat touch this pass (AND);
            // outside a recompute there is no pass boundary, so the latest read
            // labels it.
            if is_recomputing {
                l.pending_observer
                    .set(l.pending_observer.get() && pending_observer);
            } else {
                l.pending_observer.set(pending_observer);
            }
            return;
        }
    }

    let new_link = links.insert(Link {
        dep: std::cell::Cell::new(dep_id),
        sub: std::cell::Cell::new(sub_id),
        next_dep: std::cell::Cell::new(next_dep),
        prev_sub: std::cell::Cell::new(prev_sub),
        next_sub: std::cell::Cell::new(None),
        gen: std::cell::Cell::new(sub.dep_gen.get()),
        pending_observer: std::cell::Cell::new(pending_observer),
    });

    sub.deps_tail.set(Some(new_link));
    dep.subs_tail.set(Some(new_link));

    match prev_dep {
        Some(pd) => link_of(links, pd).next_dep.set(Some(new_link)),
        None => sub.deps.set(Some(new_link)),
    }
    match prev_sub {
        Some(ps) => link_of(links, ps).next_sub.set(Some(new_link)),
        None => dep.subs.set(Some(new_link)),
    }
}

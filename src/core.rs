//! Port of `src/core/core.ts`: node construction, recomputation, the read
//! path, and the write path.
//!
//! # Where the shape differs
//!
//! * **Errors are values.** Upstream throws out of `_fn` and catches in
//!   `recompute`. Here a computation returns `Result`, and `recompute` routes
//!   `Err` through exactly the same `notify_status(STATUS_ERROR, …)` path. The
//!   semantics — which node owns the error, how it propagates, when a late
//!   reader retries — are unchanged.
//! * **The previous value is moved out for the call.** A computation must run
//!   with no arena borrow held (it can create nodes), so `recompute` takes the
//!   previous value out of the node, runs the computation against it, and
//!   writes the result back. Upstream never reads `_value` between those two
//!   points either.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

use crate::constants::{config, reactive, status, EffectKind};
use crate::error::ReactiveError;
use crate::graph::{link, trim_stale_deps, unobserved};
use crate::heap::{
    delete_from_heap, insert_into_heap, insert_into_heap_height, mark_heap, mark_node,
    resolve_firewall,
};
use crate::node::{AnyValue, ComputeFn, ComputeOutcome, EqualsFn, Node, NodeId, NodeKind};
use crate::owner::{attach_to_parent, dispose_children, inherit_id, mark_disposal};
use crate::runtime::{node, with_rt, Runtime};
use crate::scheduler::{
    arm_reask_clear, insert_subs, queue_enqueue, queue_pending_node, schedule,
};
use crate::status::{clear_status, notify_status, settle_errored_dependents};

#[cfg(feature = "dev")]
pub(crate) const PRIMITIVE_IN_FORBIDDEN_SCOPE_MESSAGE: &str =
    "[PRIMITIVE_IN_FORBIDDEN_SCOPE] Cannot create reactive primitives inside \
     create_tracked_effect or owner-backed on_settled";
#[cfg(feature = "dev")]
pub(crate) const REACTIVE_WRITE_IN_OWNED_SCOPE_SIGNAL_MESSAGE: &str =
    "[REACTIVE_WRITE_IN_OWNED_SCOPE] Writing to reactive state inside an owned scope \
     (component, computation) is not allowed. Move the write outside or set the `owned_write` \
     option if this is intentional.";
#[cfg(feature = "dev")]
pub(crate) const REACTIVE_WRITE_IN_OWNED_SCOPE_REFRESH_MESSAGE: &str =
    "[REACTIVE_WRITE_IN_OWNED_SCOPE] Calling refresh() inside an owned scope (component, \
     computation) is not allowed. Move the invalidation outside pure computation.";

/// Restores `ctx`/`tracking`/`stale` even if user code unwinds, so a panic in
/// a computation cannot leave the runtime with a stale owner.
struct ScopeGuard {
    ctx: Option<NodeId>,
    tracking: bool,
    stale: bool,
}

impl ScopeGuard {
    fn enter(rt: &Runtime, ctx: Option<NodeId>, tracking: bool, stale: bool) -> ScopeGuard {
        let g = ScopeGuard {
            ctx: rt.ctx.get(),
            tracking: rt.tracking.get(),
            stale: rt.stale.get(),
        };
        rt.ctx.set(ctx);
        rt.tracking.set(tracking);
        rt.stale.set(stale);
        g
    }

    fn exit(self, rt: &Runtime) {
        rt.ctx.set(self.ctx);
        rt.tracking.set(self.tracking);
        rt.stale.set(self.stale);
    }
}

// ------------------------------------------------------------- construction

pub(crate) struct ComputedSpec {
    pub compute: ComputeFn,
    pub equals: Option<EqualsFn>,
    pub unobserved: Option<Rc<dyn Fn()>>,
    pub id: Option<Rc<str>>,
    pub name: &'static str,
    pub transparent: bool,
    pub owned_write: bool,
    pub lazy: bool,
    pub sync: bool,
    /// Commit #0. `Some` means the node is born committed with this value
    /// instead of `STATUS_UNINITIALIZED`.
    pub loading_value: Option<AnyValue>,
    pub effect_kind: Option<EffectKind>,
}

/// Port of `computed()` / `createEffectNode()`.
pub(crate) fn create_computed(rt: &Runtime, spec: ComputedSpec) -> NodeId {
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
            panic!("{PRIMITIVE_IN_FORBIDDEN_SCOPE_MESSAGE}");
        }
    }

    let is_effect = spec.effect_kind.is_some();
    let loading = spec.loading_value.is_some();

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

    let id_str = inherit_id(rt, spec.id.clone(), spec.transparent, parent);

    let mut n = Node::blank(NodeKind::Computed, queue, context);
    n.config = Cell::new(
        (if spec.transparent { config::TRANSPARENT } else { 0 })
            | (if spec.owned_write { config::OWNED_WRITE } else { 0 })
            // Unowned nodes and explicitly-lazy nodes auto-dispose; owned
            // eager memos live for their owner's lifetime. Effects clear the
            // bit (they are constructed with `lazy` semantics but must not
            // auto-dispose), which the caller does via `effect_kind`.
            | (if (parent.is_none() || spec.lazy) && !is_effect { config::AUTO_DISPOSE } else { 0 })
            | (if spec.sync { config::SYNC } else { 0 }),
    );
    *n.equals.get_mut() = spec.equals;
    *n.unobserved.get_mut() = spec.unobserved;
    *n.compute.get_mut() = Some(Rc::new(std::cell::RefCell::new(spec.compute)));
    *n.id.get_mut() = id_str;
    #[cfg(feature = "dev")]
    {
        n.name = Cell::new(spec.name);
    }
    #[cfg(not(feature = "dev"))]
    let _ = spec.name;
    n.parent = Cell::new(parent);
    n.root_parent_computed = Cell::new(parent_computed);
    n.time = Cell::new(rt.clock.get());
    n.effect_kind = Cell::new(spec.effect_kind);
    // A `loading_value` node is born committed: commit #0 is already the value.
    *n.value.get_mut() = spec.loading_value;
    n.loading = Cell::new(loading);
    n.status_flags = Cell::new(if loading { 0 } else { status::UNINITIALIZED });
    // Effects always run in lazy mode: `effect()` calls `recompute` explicitly.
    n.flags = Cell::new(if spec.lazy || is_effect {
        reactive::LAZY
    } else {
        reactive::NONE
    });

    let id = rt.alloc_node(n);
    attach_to_parent(rt, id, parent);

    // Height sits one below the owning computation, so a rerun of the owner
    // always drains before the children it may recreate.
    if let Some(pc) = parent_computed {
        let nodes = rt.nodes();
        if let Some(p) = nodes.get(pc) {
            node(&nodes, id).height.set(p.height.get() + 1);
        }
    }

    if !spec.lazy && !is_effect {
        recompute(rt, id, true);
    }
    id
}

pub(crate) struct SignalSpec {
    pub value: Option<AnyValue>,
    pub equals: Option<EqualsFn>,
    pub unobserved: Option<Rc<dyn Fn()>>,
    pub owned_write: bool,
    pub name: &'static str,
}

/// Port of `signal()`.
pub(crate) fn create_signal_node(rt: &Runtime, spec: SignalSpec) -> NodeId {
    let mut n = Node::blank(NodeKind::Signal, rt.global_queue.get(), rt.default_context.clone());
    n.config = Cell::new(if spec.owned_write { config::OWNED_WRITE } else { 0 });
    *n.equals.get_mut() = spec.equals;
    *n.unobserved.get_mut() = spec.unobserved;
    *n.value.get_mut() = spec.value;
    n.time = Cell::new(rt.clock.get());
    #[cfg(feature = "dev")]
    {
        n.name = Cell::new(spec.name);
    }
    #[cfg(not(feature = "dev"))]
    let _ = spec.name;
    rt.alloc_node(n)
}

// -------------------------------------------------------------- recomputation

/// Port of `recompute`.
pub(crate) fn recompute(rt: &Runtime, el: NodeId, create: bool) {
    let Some(effect_kind) = ({
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        Some(n.effect_kind.get())
    }) else {
        return;
    };
    let is_effect = effect_kind.is_some();

    // ------------------------------------------------------------- prologue
    let mut dispose_now = false;
    if !create {
        {
            let nodes = rt.nodes();
            let Some(n) = nodes.get(el) else { return };
            delete_from_heap(&nodes, rt.heap_for(n), el);
            // The previous flight is superseded: its late arrival must be
            // dropped, and a live stream told to stop pulling.
            n.in_flight.set(0);
            if let Some(prev) = n.in_flight_stream.borrow_mut().take() {
                prev.close();
            }

            // Tracked effects run after `finalize_pure_queue`, so they dispose
            // immediately instead of deferring. Everything else defers its
            // children to the zombie chain so a rerun that recreates them can
            // be committed or abandoned as a unit.
            if n.transition.get().is_some() || effect_kind == Some(EffectKind::Tracked) {
                dispose_now = true;
            } else if n.first_child.get().is_some() || !n.disposal.borrow().is_empty() {
                mark_disposal(rt, el);
                let disposal = std::mem::take(&mut *n.disposal.borrow_mut());
                *n.pending_disposal.borrow_mut() = disposal;
                n.pending_first_child.set(n.first_child.get());
                n.first_child.set(None);
                n.child_count.set(0);
            }
        }
        if dispose_now {
            dispose_children(rt, el, false, false);
        }
    }

    let (was_uninitialized, outgoing_error, had_reask, was_loading, old_height, is_sync) = {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        (
            n.has_status(status::UNINITIALIZED),
            if n.has_status(status::ERROR) {
                n.error.borrow().clone()
            } else {
                None
            },
            n.has_flag(reactive::REASK),
            n.loading.get(),
            n.height.get(),
            n.has_config(config::SYNC),
        )
    };
    let _ = is_sync;

    // Clone the previous value out: the computation must run with no arena
    // borrow held, and `AnyValue` is an `Rc`, so this is a refcount bump.
    let (prev, compute) = {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        n.deps_tail.set(None);
        n.dep_gen.set(n.dep_gen.get().wrapping_add(1));
        n.flags.set(reactive::RECOMPUTING_DEPS);
        n.time.set(rt.clock.get());
        let out = (n.staged_or_committed(), n.compute.borrow().clone());
        out
    };

    // Render and pure effects read committed values; user effects see staged
    // ones so `latest()` and direct reads stay consistent.
    let is_stale_effect = is_effect && effect_kind != Some(EffectKind::User);
    let guard = ScopeGuard::enter(rt, Some(el), true, if is_stale_effect { true } else { rt.stale.get() });

    // -------------------------------------------------- the computation runs
    let outcome = match compute {
        Some(f) => {
            let mut f = f.borrow_mut();
            f(prev.as_deref())
        }
        None => Ok(ComputeOutcome::Value(Rc::new(()) as AnyValue)),
    };

    guard.exit(rt);
    {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        n.flags.set(reactive::NONE);
        n.loading.set(false);
    }

    // Resolve the three compute shapes down to "a value" or "a suspension".
    let resolved: Result<AnyValue, ReactiveError> = match outcome {
        Ok(ComputeOutcome::Value(v)) => {
            let nodes = rt.nodes();
            let n = node(&nodes, el);
            n.in_flight.set(0);
            // A sync landing is the first real answer for a loading-value node.
            n.loading.set(false);
            Ok(v)
        }
        Ok(ComputeOutcome::Flight(f)) => match crate::asyncrt::handle_flight(rt, el, f) {
            crate::asyncrt::AsyncOutcome::Ready(v) => Ok(v),
            crate::asyncrt::AsyncOutcome::Suspended(e)
            | crate::asyncrt::AsyncOutcome::Failed(e) => Err(e),
        },
        Ok(ComputeOutcome::Stream(s)) => match crate::asyncrt::handle_stream(rt, el, s) {
            crate::asyncrt::AsyncOutcome::Ready(v) => Ok(v),
            crate::asyncrt::AsyncOutcome::Suspended(e)
            | crate::asyncrt::AsyncOutcome::Failed(e) => Err(e),
        },
        Err(e) => Err(e),
    };

    let value = match resolved {
        Ok(v) => {
            let needs_clear = {
                let nodes = rt.nodes();
                let n = node(&nodes, el);
                n.status_flags.get() != 0
                    || n.notify_status.borrow().is_some()
                    || n.error.borrow().is_some()
                    || n.reask.get()
                    || n.blocked.get()
                    || n.pending_sources.borrow().is_some()
                    || n.child.get().is_some()
            };
            if needs_clear {
                clear_status(rt, el, create);
            }
            v
        }
        Err(e) => {
            // The epilogue is skipped on error, so the node keeps serving the
            // value it already had — upstream's `if (!el._error)` gate.
            let not_ready = e.is_not_ready();
            if not_ready {
                let loading = {
                    let nodes = rt.nodes();
                    node(&nodes, el).loading.get()
                };
                if loading {
                    // Loading window with an unready sync dependency: register
                    // for the source's settle but take NO read-visible pending
                    // status. Commit #0 keeps serving.
                    crate::asyncrt::park_loading_window(rt, el, &e);
                    finish_pending_commit(rt, el, create);
                    return;
                }
                {
                    let nodes = rt.nodes();
                    node(&nodes, el).blocked.set(true);
                }
                if crate::verdict::apply_reask(rt, el, had_reask) {
                    crate::verdict::update_pending_signal(rt, el);
                }
            }
            notify_status(
                rt,
                el,
                if not_ready { status::PENDING } else { status::ERROR },
                Some(e),
                false,
            );
            finish_pending_commit(rt, el, create);
            return;
        }
    };

    // ------------------------------------------------------------- epilogue
    trim_stale_deps(rt, el);

    let value_changed = {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        let equals = n.equals.borrow().clone();
        match equals {
            // Uninitialized first: the first commit has no previous value, so
            // a user comparator must not run against a missing one.
            _ if !is_effect && was_uninitialized => true,
            None => true,
            Some(eq) => match (&prev, &value) {
                (Some(a), b) => !eq(&**a, &**b),
                (None, _) => true,
            },
        }
    };

    if is_effect && value_changed {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        n.modified.set(true);
        // Reuse one runner per effect — `run_effect` no-ops on a stale
        // `modified`, so re-enqueueing the same function is harmless.
        if !create {
            let runner = {
                let mut slot = n.bound_run_effect.borrow_mut();
                slot.get_or_insert_with(|| crate::effect::make_runner(el)).clone()
            };
            let kind = effect_kind.expect("is_effect");
            let queue = n.queue.get();
            drop(nodes);
            queue_enqueue(rt, queue, kind, runner);
        }
    }

    if value_changed {
        let commits_directly = {
            // Effects commit values directly on a plain synchronous flush: the
            // pending round-trip exists to sequence transition reveals, and
            // paying it per effect on the plain path is pure overhead.
            let nodes = rt.nodes();
            let n = node(&nodes, el);
            create || (is_effect && (rt.active_transition.get() != n.transition.get()
                || rt.active_transition.get().is_none()))
        };
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        if commits_directly {
            *n.value.borrow_mut() = Some(value);
        } else {
            *n.pending_value.borrow_mut() = Some(value);
            #[cfg(feature = "test_invariants")]
            crate::invariants::track_held_pending(el);
            // A window landing that gets held re-opens the window until the
            // hold commits.
            if was_loading {
                n.loading.set(true);
            }
        }
        let has_subs = n.subs.get().is_some();
        drop(nodes);
        // `insert_subs` only walks the sub list, so a subscriber-less node has
        // nothing to notify.
        if has_subs {
            insert_subs(rt, el, false);
        }
    } else {
        // Unchanged value: nothing to commit, but a height shift still has to
        // reach the subscribers so the drain order stays topological.
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        if n.height.get() != old_height {
            let subs = crate::scheduler::subscribers_of(&nodes, &rt.links(), el);
            for sub in subs {
                let heap = rt.heap_for(node(&nodes, sub));
                insert_into_heap_height(&nodes, heap, sub);
            }
        }
    }

    // Silent recovery: errored → unchanged value fires no notification, but
    // dependents still holding the propagated error consumed their dirty flag
    // in an errored run and may sit on stale commits.
    if let Some(err) = outgoing_error {
        let still_errored = {
            let nodes = rt.nodes();
            node(&nodes, el).has_status(status::ERROR)
        };
        if !value_changed && !still_errored {
            settle_errored_dependents(rt, el, &err);
        }
    }

    finish_pending_commit(rt, el, create);
}

/// Port of the `needsPendingCommit` tail of `recompute`.
fn finish_pending_commit(rt: &Runtime, el: NodeId, create: bool) {
    let should_queue = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        let needs = n.pending_value.borrow().is_some()
            || n.pending_first_child.get().is_some()
            || !n.pending_disposal.borrow().is_empty()
            || n.has_status(status::PENDING | status::UNINITIALIZED);
        needs
            && (!create || n.has_status(status::PENDING))
            && n.transition.get().is_none()
    };
    if should_queue {
        queue_pending_node(rt, el);
    }
}

/// Port of `updateIfNecessary`: bring a node up to date by pulling.
pub(crate) fn update_if_necessary(rt: &Runtime, el: NodeId) {
    let check = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        n.has_flag(reactive::CHECK)
    };

    if check {
        let mut cur = {
            let nodes = rt.nodes();
            node(&nodes, el).deps.get()
        };
        while let Some(lid) = cur {
            let (dep_id, next, dep_is_computed) = {
                let nodes = rt.nodes();
                let links = rt.links();
                let Some(l) = links.get(lid) else { break };
                let dep = resolve_firewall(&nodes, l.dep.get());
                let is_c = nodes.get(dep).map(|d| d.is_computed()).unwrap_or(false);
                (dep, l.next_dep.get(), is_c)
            };
            if dep_is_computed {
                update_if_necessary(rt, dep_id);
            }
            let dirty = {
                let nodes = rt.nodes();
                nodes.get(el).map(|n| n.has_flag(reactive::DIRTY)).unwrap_or(false)
            };
            if dirty {
                break;
            }
            cur = next;
        }
    }

    let needs = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        n.has_flag(reactive::DIRTY | reactive::OPTIMISTIC_DIRTY)
            || (n.error.borrow().is_some() && n.time.get() < rt.clock.get())
    };
    if needs {
        recompute(rt, el, false);
    }

    let nodes = rt.nodes();
    if let Some(n) = nodes.get(el) {
        n.flags.set(
            n.flags.get()
                & (reactive::SNAPSHOT_STALE | reactive::IN_HEAP | reactive::IN_HEAP_HEIGHT),
        );
    }
}

/// Port of `prepareComputed`: bring a computed to a readable state.
pub(crate) fn prepare_computed(rt: &Runtime, comp: NodeId, refresh: bool) {
    let state = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(comp) else { return };
        if n.has_flag(reactive::LAZY) {
            n.clear_flags(reactive::LAZY);
            1
        } else if n.has_flag(reactive::DISPOSED) {
            1
        } else {
            0
        }
    };
    if state == 1 {
        recompute(rt, comp, true);
    } else if refresh {
        update_if_necessary(rt, comp);
    }
}

// ------------------------------------------------------------------ read

/// What a read produced.
pub(crate) enum ReadOutcome {
    /// The value lives in the node; the caller re-borrows to project it.
    Value,
    /// The node is errored or pending; upstream would have thrown.
    Err(ReactiveError),
}

/// Port of `read()`, minus the snapshot and optimistic windows.
///
/// Returns without the value itself: the value is type-erased and owned by the
/// arena, so the caller projects it under its own short borrow via
/// [`project_value`]. That keeps the arena borrow off the user's callback.
pub(crate) fn read_node(rt: &Runtime, el: NodeId) -> ReadOutcome {
    // `latest_read_active` / `pending_check_active` gate the verdict layer's
    // windows. Both are permanently false here; the branches stay so the
    // verdict port slots in without reshaping this function.

    let mut c = rt.ctx.get();
    if let Some(cid) = c {
        let nodes = rt.nodes();
        if let Some(n) = nodes.get(cid) {
            if n.is_root() {
                c = n.root_parent_computed.get();
            }
        }
    }

    let (is_computed, owner) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else {
            return ReadOutcome::Err(ReactiveError::msg("read of a reclaimed node"));
        };
        (n.is_computed(), n.firewall.get().unwrap_or(el))
    };

    if rt.pending_check_active.get() {
        // Probe mode: bring the node up to date so its status reflects the
        // current graph, then collect it as a source without letting the
        // probe's own link become a value dependency.
        crate::verdict::probe_prepare(rt, el);
        crate::verdict::collect_probe_source(el, false);
        if owner != el {
            crate::verdict::collect_probe_source(owner, false);
        }
    } else if is_computed {
        prepare_computed(rt, el, false);
    }

    if let (Some(cid), true) = (c, rt.tracking.get()) {
        {
            let nodes = rt.nodes();
            let mut links = rt.links_mut();
            link(&nodes, &mut links, el, cid, rt.pending_check_active.get());
        }

        let owner_is_computed = {
            let nodes = rt.nodes();
            nodes.get(owner).map(|o| o.is_computed()).unwrap_or(false)
        };
        if owner_is_computed {
            let needs_pull = {
                let nodes = rt.nodes();
                let o = node(&nodes, owner);
                let heap = rt.heap_for(o);
                o.height.get() >= heap.min()
            };
            if needs_pull {
                {
                    let nodes = rt.nodes();
                    let links = rt.links();
                    mark_node(&nodes, &links, cid, reactive::DIRTY);
                    let heap = rt.heap_for(node(&nodes, el));
                    mark_heap(&nodes, &links, heap);
                }
                update_if_necessary(rt, owner);
            }
            // Keep the reader strictly above its dependency so the drain
            // order stays topological.
            let nodes = rt.nodes();
            let height = node(&nodes, owner).height.get();
            let cnode = node(&nodes, cid);
            let el_parent = nodes.get(el).and_then(|n| n.parent.get());
            if height >= cnode.height.get() && el_parent != Some(cid) {
                cnode.height.set(height + 1);
            }
        }
    }

    // Reader suspension: a pending source raises into the graph, which is what
    // a loading boundary catches.
    let mut suspension_link: Option<NodeId> = None;
    let mut pending_err: Option<ReactiveError> = None;
    {
        let nodes = rt.nodes();
        let Some(o) = nodes.get(owner) else {
            return ReadOutcome::Err(ReactiveError::msg("read of a reclaimed node"));
        };
        if o.has_status(status::PENDING) && (c.is_some() || o.has_status(status::UNINITIALIZED)) {
            if let Some(e) = o.error.borrow().clone() {
                suspension_link = c.filter(|cid| !rt.tracking.get() && el != *cid);
                pending_err = Some(e);
            }
        }
    }

    // The suspension link. An *untracked* read of a pending node still records
    // an edge, so the source's settle sweep can reach this reader and wake it —
    // without it, `untrack(|| memo.try_get())` inside an effect suspends once
    // and is never re-run. Phase 1 omitted this because nothing could be
    // pending; it is load-bearing here (upstream `read()`, the
    // `if (!tracking && el !== c) link(el, c)` in each pending arm).
    if let Some(cid) = suspension_link {
        let nodes = rt.nodes();
        let mut links = rt.links_mut();
        link(&nodes, &mut links, el, cid, false);
    }
    if let Some(e) = pending_err {
        return ReadOutcome::Err(e);
    }

    // An errored derive throws for every late reader instead of silently
    // serving stale data. Only a genuine reactive re-read may retry: inside a
    // tracked scope, and only on a later cycle than the one the error was
    // found on.
    let errored = {
        let nodes = rt.nodes();
        let o = node(&nodes, owner);
        o.is_computed() && o.has_status(status::ERROR)
    };
    if errored {
        let (retry, err) = {
            let nodes = rt.nodes();
            let o = node(&nodes, owner);
            let out = (
                rt.tracking.get() && o.time.get() < rt.clock.get(),
                o.error.borrow().clone(),
            );
            out
        };
        if retry {
            recompute(rt, owner, false);
            return read_node(rt, el);
        }
        return ReadOutcome::Err(err.unwrap_or_else(|| ReactiveError::msg("errored computation")));
    }

    // Auto-dispose tail: an ownerless memo read with no observer and no
    // subscribers is released, so it recomputes from scratch next time.
    let release = {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        c.is_none()
            && owner == el
            && n.is_computed()
            && n.has_config(config::AUTO_DISPOSE)
            && !n.has_status(status::PENDING)
            && n.subs.get().is_none()
    };
    if release {
        // Read the value out before releasing: `unobserved` disposes children
        // but leaves the node's own committed value intact, which is what the
        // next read of a released memo would recompute anyway.
        unobserved(rt, el);
    }

    ReadOutcome::Value
}

/// The value a successful [`read_node`] settled on.
///
/// Mirrors the value selection at the tail of upstream's `read()`: with no
/// observer, the committed value; with one, the staged value if a write is
/// held — which is what lets a downstream computation inside the same drain
/// see its dependency's new value before the tick commits.
///
/// Returns an owned `Rc` so the caller can release the arena borrow before
/// touching user code.
pub(crate) fn current_value(rt: &Runtime, el: NodeId) -> Option<AnyValue> {
    // `latest()` reads the fresh channel: the staged value if a write is held,
    // regardless of whether there is an observer.
    if rt.latest_read_active.get() {
        return crate::verdict::latest_value_of(rt, el);
    }
    let has_ctx = rt.ctx.get().is_some();
    let nodes = rt.nodes();
    let n = nodes.get(el)?;
    if has_ctx {
        if let Some(staged) = n.pending_value.borrow().clone() {
            // Record that this probe observed the fresh value, so it is not
            // also told the node is pending (A10). Touches only the probe's
            // thread-local, never the arena, so the borrow above is safe.
            if rt.pending_check_active.get() {
                crate::verdict::collect_probe_source(el, true);
            }
            return Some(staged);
        }
    }
    let out = n.value.borrow().clone();
    out
}

// ------------------------------------------------------------------- write

/// Port of `setSignal`.
pub(crate) fn set_signal(
    rt: &Runtime,
    el: NodeId,
    make: impl FnOnce(Option<&dyn Any>) -> AnyValue,
) {
    #[cfg(feature = "dev")]
    {
        let nodes = rt.nodes();
        let owned_write = nodes.get(el).map(|n| n.has_config(config::OWNED_WRITE)).unwrap_or(false);
        if let Some(ctx) = rt.ctx.get() {
            let ctx_forbidden = nodes
                .get(ctx)
                .map(|n| n.has_config(config::CHILDREN_FORBIDDEN))
                .unwrap_or(false);
            let firewall_is_ctx =
                nodes.get(el).and_then(|n| n.firewall.get()) == Some(ctx);
            if !owned_write && !ctx_forbidden && !firewall_is_ctx {
                panic!("{REACTIVE_WRITE_IN_OWNED_SCOPE_SIGNAL_MESSAGE}");
            }
        }
    }

    // The updater is user code, so the current value is cloned out (an `Rc`
    // bump) and the borrow released before it runs.
    let (current, equals, uninitialized) = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        let out = (
            n.staged_or_committed(),
            n.equals.borrow().clone(),
            n.has_status(status::UNINITIALIZED),
        );
        out
    };
    let next = make(current.as_deref());

    // Uninitialized first: the first commit has no previous value, so a user
    // comparator must not run against a missing one (matches `recompute`).
    let value_changed = uninitialized
        || match (&equals, &current) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(eq), Some(a)) => !eq(&**a, &*next),
        };
    if !value_changed {
        return;
    }

    {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        if n.pending_value.borrow().is_none() {
            drop(nodes);
            queue_pending_node(rt, el);
        }
    }
    {
        let nodes = rt.nodes();
        let n = node(&nodes, el);
        *n.pending_value.borrow_mut() = Some(next.clone());
        #[cfg(feature = "test_invariants")]
        crate::invariants::track_held_pending(el);
        n.time.set(rt.clock.get());
    }

    crate::verdict::sync_companions(rt, el, &next);
    insert_subs(rt, el, false);
    schedule(rt);
}

/// Port of `suppressComputedRecompute`.
///
/// Suppresses automatic recomputation of `el` until the scheduler drains, so a
/// manual write wins over dependency changes queued in the same tick. The
/// `MANUAL_WRITE` flag is cleared by the pending-node drain.
pub(crate) fn suppress_computed_recompute(rt: &Runtime, el: NodeId) {
    let needs_queue = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(el) else { return };
        delete_from_heap(&nodes, rt.heap_for(n), el);
        !n.has_flag(reactive::MANUAL_WRITE) && n.pending_value.borrow().is_none()
    };
    if needs_queue {
        queue_pending_node(rt, el);
        schedule(rt);
    }
    let nodes = rt.nodes();
    let n = node(&nodes, el);
    n.flags
        .set((n.flags.get() & !(reactive::DIRTY | reactive::CHECK)) | reactive::MANUAL_WRITE);
}

/// Port of `refresh()`.
///
/// A write-like invalidation: it does not read the target, and refreshing a
/// plain signal is a no-op.
pub(crate) fn refresh_node(rt: &Runtime, el: NodeId) {
    #[cfg(feature = "dev")]
    {
        let nodes = rt.nodes();
        let owned = nodes.get(el).map(|n| n.has_config(config::OWNED_WRITE)).unwrap_or(false);
        if let Some(ctx) = rt.ctx.get() {
            let forbidden = nodes
                .get(ctx)
                .map(|n| n.has_config(config::CHILDREN_FORBIDDEN))
                .unwrap_or(false);
            if !owned && !forbidden {
                panic!("{REACTIVE_WRITE_IN_OWNED_SCOPE_REFRESH_MESSAGE}");
            }
        }
    }

    let nodes = rt.nodes();
    let Some(n) = nodes.get(el) else { return };
    if !n.is_computed() || n.has_flag(reactive::DISPOSED | reactive::MANUAL_WRITE) {
        return;
    }
    // A refresh with no value-change dirt already queued is a re-ask of the
    // same question. `IN_HEAP` counts as dirt: `insert_subs` schedules
    // subscribers by heap insertion alone, so a same-batch value change
    // followed by `refresh()` must not be laundered into a quiet re-ask.
    if !n.has_flag(reactive::DIRTY | reactive::CHECK | reactive::IN_HEAP) {
        n.set_flags_or(reactive::REASK);
        arm_reask_clear(rt);
    }
    n.flags
        .set((n.flags.get() & !reactive::CHECK) | reactive::DIRTY);
    let heap = rt.heap_for(n);
    insert_into_heap(&nodes, heap, el);
    drop(nodes);
    schedule(rt);
}

// -------------------------------------------------------------- scope helpers

/// Run `f` outside of any reactive tracking.
///
/// Reads inside `f` do not subscribe the surrounding scope; the *owner* is
/// unchanged, so anything created inside is still owned and still disposed.
///
/// ```
/// use solid_rs::{create_signal, create_memo, create_root, untrack, flush};
/// let (trigger, set_trigger) = create_signal(0);
/// let (other, set_other) = create_signal(1);
///
/// let m = create_root(move |_| {
///     create_memo(move |_| trigger.get() + untrack(|| other.get()))
/// });
/// assert_eq!(m.get(), 1);
///
/// set_other.set(10);
/// flush();
/// assert_eq!(m.get(), 1, "untracked read created no subscription");
///
/// set_trigger.set(1);
/// flush();
/// assert_eq!(m.get(), 11, "the tracked read does re-run it");
/// ```
pub fn untrack<T>(f: impl FnOnce() -> T) -> T {
    with_rt(|rt| {
        if !rt.tracking.get() {
            return f();
        }
        let prev = rt.tracking.get();
        rt.tracking.set(false);
        let out = f();
        rt.tracking.set(prev);
        out
    })
}

/// Port of `staleValues`.
#[allow(dead_code)]
pub(crate) fn stale_values<T>(rt: &Runtime, set: bool, f: impl FnOnce() -> T) -> T {
    let prev = rt.stale.get();
    rt.stale.set(set);
    let out = f();
    rt.stale.set(prev);
    out
}

/// Execute `f` with `owner` as the current owner.
///
/// The classic pattern: capture the owner with [`crate::get_owner`] inside a
/// component, then re-enter it from a callback so disposables created there
/// are cleaned up with the component.
pub fn run_with_owner<T>(owner: Option<crate::Owner>, f: impl FnOnce() -> T) -> T {
    with_rt(|rt| {
        let prev_ctx = rt.ctx.get();
        let prev_tracking = rt.tracking.get();
        rt.ctx.set(owner.map(|o| o.id()));
        rt.tracking.set(false);
        let out = f();
        rt.ctx.set(prev_ctx);
        rt.tracking.set(prev_tracking);
        out
    })
}

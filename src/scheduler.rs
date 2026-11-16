//! Port of `src/core/scheduler.ts`: the queue tree, the transaction model, the
//! flush loop, and the microtask seam.
//!
//! Per the port brief this keeps the full machinery even where the
//! synchronous subset never exercises it. The transaction shape, the queue
//! stash/restore, the notification chain and the lane routing point all exist
//! because the async and transition phases are built on them; removing them
//! now would mean re-deriving where each branch belonged later.
//!
//! What is genuinely absent, and why:
//!
//! * **Optimistic lanes** (`lanes.ts`) — a separate port. The routing point in
//!   [`queue_enqueue`] and the `active_lanes` gate in [`global_flush`] are
//!   kept and documented; both currently take the empty branch.
//! * **Async reporters** — registered from [`Queue::notify`] exactly as
//!   upstream, but nothing in the synchronous core produces a `NotReadyError`
//!   to register.
//!
//! # Microtasks
//!
//! Upstream calls `queueMicrotask(flush)`. There is no JS event loop here, so
//! the microtask queue is ours to provide: [`schedule`] posts onto
//! [`Runtime::microtasks`] and [`crate::host::drive_microtasks`] is the one
//! place a host drains it. `flush()` remains an explicit synchronous drain and
//! does not depend on the host ever calling it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::constants::{config, reactive, status, EffectKind};
use crate::error::ReactiveError;
use crate::heap::{enqueue_sub, run_heap};
use crate::node::{NodeId, QueueId, TransitionId};
use crate::runtime::{node, with_rt, Nodes, Runtime};

/// Port of `QueueCallback`. `Fn`, not `FnOnce`: upstream re-enqueues the same
/// bound runner and relies on `run_effect` no-oping on a stale `modified`.
pub(crate) type QueueCallback = Rc<dyn Fn(u8)>;

/// Port of `QueueStub`: the shape queue contents are stashed into while a
/// transition is incomplete.
///
/// Stashing only happens for an *incomplete* transition, which needs an async
/// blocker to exist — so the fields are written by the async port, not here.
#[derive(Default)]
#[allow(dead_code, reason = "written by the transition stash path (async.ts)")]
pub(crate) struct QueueStub {
    pub queues: [Vec<QueueCallback>; 2],
    pub children: Vec<QueueStub>,
}

/// Port of `Queue` / `GlobalQueue`.
///
/// Upstream models the root queue as a subclass. Rust prefers a flag plus a
/// hook slot: `is_global` selects the root notification behaviour, and
/// `notify_hook` is the seam boundary queues (`CollectionQueue`, out of scope)
/// install themselves through.
pub(crate) struct Queue {
    pub parent: Cell<Option<QueueId>>,
    pub queues: [RefCell<Vec<QueueCallback>>; 2],
    pub children: RefCell<Vec<QueueId>>,
    pub ran_at: Cell<u64>,
    /// Clock value at creation. Boundary queues compare it to decide whether a
    /// source predates them.
    #[allow(dead_code, reason = "read by boundaries.ts, a separate port")]
    pub created: Cell<u64>,
    pub is_global: bool,
    /// Installed by boundary queues to intercept a status family before it
    /// reaches the root. Returning `true` means "handled, stop propagating".
    #[allow(clippy::type_complexity)]
    pub notify_hook: RefCell<Option<Rc<dyn Fn(NodeId, u32, u32, Option<ReactiveError>) -> bool>>>,
    /// Boundary hook run by `check_boundary_children`.
    pub check_sources: RefCell<Option<Rc<dyn Fn()>>>,
}

impl Queue {
    pub fn new_global() -> Queue {
        Queue {
            parent: Cell::new(None),
            queues: [RefCell::new(Vec::new()), RefCell::new(Vec::new())],
            children: RefCell::new(Vec::new()),
            ran_at: Cell::new(0),
            created: Cell::new(0),
            is_global: true,
            notify_hook: RefCell::new(None),
            check_sources: RefCell::new(None),
        }
    }
}

/// Port of `Transition`.
///
/// Ambient work *is* a transaction: the global queue always carries one
/// current-transaction-shaped batch. With no transition active, registrations
/// land in a plain ambient batch that the plain flush finalizes.
#[allow(
    dead_code,
    reason = "the transaction shape is ported whole; the fields the synchronous \
              subset never writes belong to async.ts / action.ts / lanes.ts"
)]
pub(crate) struct Transition {
    pub time: Cell<u64>,
    pub pending_nodes: RefCell<Vec<NodeId>>,
    /// Async blockers, keyed by source. The one sanctioned registration site
    /// is [`Queue::notify`] (upstream INV-3).
    pub async_reporters: RefCell<HashMap<NodeId, HashSet<NodeId>>>,
    /// Optimistic nodes pending reversion. Produced only by `optimistic.ts`.
    pub optimistic_nodes: RefCell<Vec<NodeId>>,
    /// Live `affects()` marks owned by this transaction. Produced only by
    /// `affects.ts`.
    pub affects_nodes: RefCell<Vec<NodeId>>,
    /// Optimistic store layers. Produced only by `store/`.
    pub optimistic_stores: RefCell<Vec<NodeId>>,
    /// In-flight generator actions. Produced only by `action.ts`.
    pub actions: Cell<usize>,
    pub queue_stash: RefCell<QueueStub>,
    pub done: Cell<Done>,
    /// Subscribers that read a committed value through the entanglement gate
    /// and must re-run at commit.
    pub gated_subs: RefCell<Vec<NodeId>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code, reason = "completion and merging are driven by async.ts")]
pub(crate) enum Done {
    No,
    Yes,
    /// Merged into another transaction; `current_transition` follows the chain.
    MergedInto(TransitionId),
}

impl Transition {
    pub fn new(time: u64) -> Transition {
        Transition {
            time: Cell::new(time),
            pending_nodes: RefCell::new(Vec::new()),
            async_reporters: RefCell::new(HashMap::new()),
            optimistic_nodes: RefCell::new(Vec::new()),
            affects_nodes: RefCell::new(Vec::new()),
            optimistic_stores: RefCell::new(Vec::new()),
            actions: Cell::new(0),
            queue_stash: RefCell::new(QueueStub::default()),
            done: Cell::new(Done::No),
            gated_subs: RefCell::new(Vec::new()),
        }
    }

    fn is_empty_batch(&self) -> bool {
        self.optimistic_nodes.borrow().is_empty()
            && self.affects_nodes.borrow().is_empty()
            && self.optimistic_stores.borrow().is_empty()
    }
}

// ---------------------------------------------------------------- scheduling

/// Port of `schedule`.
pub(crate) fn schedule(rt: &Runtime) {
    if rt.halted.get() {
        notify_halted(rt);
        return;
    }
    if rt.scheduled.get() {
        return;
    }
    rt.scheduled.set(true);
    if rt.sync_depth.get() == 0 && !rt.queue_running.get() && !rt.projection_write_active.get() {
        // The microtask seam. Upstream: `queueMicrotask(flush)`.
        rt.microtasks.borrow_mut().push_back(Box::new(|| {
            flush();
        }));
    }
}

/// Port of `haltReactivity`.
///
/// Permanently halts the reactive system. Called when a user error escapes
/// every boundary — application state is undefined at that point, so
/// scheduling stops entirely rather than limping along with a half-applied
/// update.
pub(crate) fn halt_reactivity(rt: &Runtime, cause: Option<&ReactiveError>) {
    if rt.halted.get() {
        return;
    }
    rt.halted.set(true);
    let mut message = String::from("[REACTIVITY_HALTED]");
    if cfg!(feature = "dev") {
        message.push_str(
            " An uncaught error halted the reactive system. No further updates will be processed.",
        );
    }
    match cause {
        Some(c) => eprintln!("{message} {}", c.message()),
        None => eprintln!("{message}"),
    }
}

fn notify_halted(rt: &Runtime) {
    if rt.halt_notified.get() {
        return;
    }
    rt.halt_notified.set(true);
    eprintln!(
        "[REACTIVITY_HALTED] Update ignored: the reactive system was halted by an earlier \
         uncaught error."
    );
}

/// Test/dev-reload hook. Revives scheduling after a halt.
pub fn reset_error_halt() {
    with_rt(|rt| {
        rt.halted.set(false);
        rt.halt_notified.set(false);
    });
}

/// Port of `armReaskClear`.
pub(crate) fn arm_reask_clear(rt: &Runtime) {
    rt.reask_armed.set(true);
}

// --------------------------------------------------------------- queue tree

/// Port of `Queue.enqueue`.
pub(crate) fn queue_enqueue(rt: &Runtime, queue: QueueId, kind: EffectKind, fn_: QueueCallback) {
    {
        let queues = rt.queues.borrow();
        if let Some(q) = queues.get(queue) {
            // Upstream routes to `lane._effectQueues[type - 1]` when a
            // recomputation is running under an optimistic lane, keeping a
            // transition's effects isolated until the lane is ready. Lanes are
            // a separate port; with none active this is the only branch.
            q.queues[kind.queue_slot()].borrow_mut().push(fn_);
        }
    }
    schedule(rt);
}

/// Port of `Queue.run`.
///
/// Effects run here can dispose owners, and disposal removes queues from the
/// child list — the running child itself, an earlier sibling, or several at
/// once. A plain index walk then skips whatever shifted into the cursor.
/// Stamping each child before it runs makes the pass idempotent, so a shift is
/// recovered by rescanning from the front and every child still runs exactly
/// once.
pub(crate) fn queue_run(rt: &Runtime, queue: QueueId, kind: EffectKind) {
    let slot = kind.queue_slot();
    let effects: Vec<QueueCallback> = {
        let queues = rt.queues.borrow();
        match queues.get(queue) {
            Some(q) => std::mem::take(&mut *q.queues[slot].borrow_mut()),
            None => return,
        }
    };
    for f in &effects {
        f(kind.as_u8());
    }

    let token = rt.queue_run_token.get() + 1;
    rt.queue_run_token.set(token);

    let mut i = 0usize;
    loop {
        let child = {
            let queues = rt.queues.borrow();
            let Some(q) = queues.get(queue) else { return };
            let children = q.children.borrow();
            match children.get(i) {
                Some(&c) => c,
                None => return,
            }
        };
        let should_run = {
            let queues = rt.queues.borrow();
            match queues.get(child) {
                Some(c) if c.ran_at.get() != token => {
                    c.ran_at.set(token);
                    true
                }
                _ => false,
            }
        };
        if should_run {
            queue_run(rt, child, kind);
            let shifted = {
                let queues = rt.queues.borrow();
                let Some(q) = queues.get(queue) else { return };
                let children = q.children.borrow();
                children.get(i).copied() != Some(child)
            };
            if shifted {
                i = 0;
                continue;
            }
        }
        i += 1;
    }
}

/// Port of `Queue.notify` / `GlobalQueue.notify`.
///
/// Notifications carry *status*, not scheduling. A boundary queue may capture
/// a status family and stop it; anything unhandled reaches the root, which
/// records async reporters for the active transaction.
pub(crate) fn queue_notify(
    rt: &Runtime,
    queue: QueueId,
    node_id: NodeId,
    mask: u32,
    flags: u32,
    error: Option<ReactiveError>,
) -> bool {
    let (hook, is_global, parent) = {
        let queues = rt.queues.borrow();
        let Some(q) = queues.get(queue) else { return false };
        let out = (q.notify_hook.borrow().clone(), q.is_global, q.parent.get());
        out
    };

    if let Some(hook) = hook {
        return hook(node_id, mask, flags, error);
    }

    if !is_global {
        return match parent {
            Some(p) => queue_notify(rt, p, node_id, mask, flags, error),
            None => false,
        };
    }

    // Root behaviour. Only track async when the boundary is propagating
    // PENDING (i.e. it was not caught by a boundary).
    if mask & status::PENDING != 0 {
        if flags & status::PENDING != 0 {
            let actual = error.or_else(|| {
                let nodes = rt.nodes();
                nodes.get(node_id).and_then(|n| n.error.borrow().clone())
            });
            if let Some(err) = actual {
                // A visibility-only mark notification (the `affects()`
                // boundary channel) updates display state on its way up but is
                // invisible to completion accounting by construction: it never
                // registers a reporter.
                if err.is_mark_visual() {
                    return true;
                }
                if let Some(t) = rt.active_transition.get() {
                    if let Some(source) = err.not_ready_source() {
                        // The one sanctioned registration site (upstream
                        // INV-3): async blockers only enter the transaction
                        // from queue notification.
                        let inserted = {
                            let ts = rt.transitions.borrow();
                            match ts.get(t) {
                                Some(tr) => tr
                                    .async_reporters
                                    .borrow_mut()
                                    .entry(source)
                                    .or_default()
                                    .insert(node_id),
                                None => false,
                            }
                        };
                        if inserted {
                            schedule(rt);
                        }
                    }
                }
            }
        }
        return true;
    }
    false
}

// ------------------------------------------------------------ pending values

/// Port of `GlobalQueue.initTransition`.
///
/// Opens (or joins) the transaction that holds this update. Called from the
/// suspension point in `handle_async` and from `set_signal` when the written
/// node already carries a stamp — those are the two ways work becomes
/// transition-worthy.
pub(crate) fn init_transition(rt: &Runtime, transition: Option<TransitionId>) {
    let transition = transition.map(|t| current_transition(rt, t));
    let active = rt.active_transition.get();

    if let Some(t) = transition {
        if Some(t) == active {
            return;
        }
    } else if let Some(a) = active {
        let same_tick = {
            let ts = rt.transitions.borrow();
            ts.get(a).map(|x| x.time.get() == rt.clock.get()).unwrap_or(false)
        };
        if same_tick {
            return;
        }
    }

    let next = match (active, transition) {
        (None, Some(t)) => t,
        (None, None) => rt.transitions.borrow_mut().insert(Transition::new(rt.clock.get())),
        (Some(outgoing), Some(t)) => {
            merge_transition_state(rt, t, outgoing);
            rt.live_transitions.borrow_mut().retain(|x| *x != outgoing);
            t
        }
        (Some(a), None) => a,
    };

    rt.active_transition.set(Some(next));
    {
        let mut live = rt.live_transitions.borrow_mut();
        if !live.contains(&next) {
            live.push(next);
        }
    }
    {
        let ts = rt.transitions.borrow();
        if let Some(t) = ts.get(next) {
            t.time.set(rt.clock.get());
        }
    }

    // Adopt the ambient batch into the transaction, then make the transaction
    // the batch so later registrations land there directly.
    let batch = rt.batch.get();
    if batch != next {
        let adopted: Vec<NodeId> = {
            let ts = rt.transitions.borrow();
            match ts.get(batch) {
                Some(b) => std::mem::take(&mut *b.pending_nodes.borrow_mut()),
                None => Vec::new(),
            }
        };
        {
            let nodes = rt.nodes();
            let ts = rt.transitions.borrow();
            if let Some(t) = ts.get(next) {
                let mut pending = t.pending_nodes.borrow_mut();
                for id in adopted {
                    if let Some(n) = nodes.get(id) {
                        n.transition.set(Some(next));
                    }
                    pending.push(id);
                }
            }
        }
        rt.batch.set(next);
    }
}

/// Port of `mergeTransitionState`.
fn merge_transition_state(rt: &Runtime, target: TransitionId, outgoing: TransitionId) {
    let ts = rt.transitions.borrow();
    let (Some(t), Some(o)) = (ts.get(target), ts.get(outgoing)) else { return };
    o.done.set(Done::MergedInto(target));

    let moved: Vec<NodeId> = std::mem::take(&mut *o.optimistic_nodes.borrow_mut());
    t.optimistic_nodes.borrow_mut().extend(moved);
    let marks: Vec<NodeId> = std::mem::take(&mut *o.affects_nodes.borrow_mut());
    t.affects_nodes.borrow_mut().extend(marks);

    // Legal transfer, not a new registration: entries move between transactions.
    let reporters: Vec<(NodeId, HashSet<NodeId>)> =
        o.async_reporters.borrow_mut().drain().collect();
    {
        let mut target_reporters = t.async_reporters.borrow_mut();
        for (source, set) in reporters {
            target_reporters.entry(source).or_default().extend(set);
        }
    }
    let gated: Vec<NodeId> = std::mem::take(&mut *o.gated_subs.borrow_mut());
    t.gated_subs.borrow_mut().extend(gated);
}

/// Port of `reporterBlocksSource`.
fn reporter_blocks_source(rt: &Runtime, reporter: NodeId, source: NodeId) -> bool {
    let nodes = rt.nodes();
    let links = rt.links();
    let Some(r) = nodes.get(reporter) else { return false };
    if r.has_flag(reactive::ZOMBIE | reactive::DISPOSED) {
        return false;
    }
    if r.pending_sources.borrow().as_ref().map(|s| s.contains(&source)).unwrap_or(false) {
        return true;
    }
    let mut cur = r.deps.get();
    while let Some(lid) = cur {
        let l = crate::runtime::link_of(&links, lid);
        let mut current = Some(l.dep.get());
        while let Some(c) = current {
            if c == source {
                return true;
            }
            current = nodes.get(c).and_then(|n| n.firewall.get()).filter(|f| *f == source);
            if current == Some(source) {
                return true;
            }
            current = None;
        }
        cur = l.next_dep.get();
    }
    r.has_status(status::PENDING)
        && r.error
            .borrow()
            .as_ref()
            .map(|e| e.not_ready_source() == Some(source))
            .unwrap_or(false)
}

/// Port of `transitionComplete`: prune dead reporters; the transaction is done
/// when no live reporter still blocks a pending source.
fn transition_complete(rt: &Runtime, transition: TransitionId) -> bool {
    {
        let ts = rt.transitions.borrow();
        let Some(t) = ts.get(transition) else { return true };
        if t.done.get() != Done::No {
            return true;
        }
        if t.actions.get() > 0 {
            return false;
        }
    }

    let entries: Vec<(NodeId, Vec<NodeId>)> = {
        let ts = rt.transitions.borrow();
        let Some(t) = ts.get(transition) else { return true };
        let out = t
            .async_reporters
            .borrow()
            .iter()
            .map(|(k, v)| (*k, v.iter().copied().collect()))
            .collect();
        out
    };

    let mut done = true;
    for (source, reporters) in entries {
        let mut has_live = false;
        let mut dead: Vec<NodeId> = Vec::new();
        for reporter in reporters {
            if reporter_blocks_source(rt, reporter, source) {
                has_live = true;
                break;
            }
            dead.push(reporter);
        }
        {
            let ts = rt.transitions.borrow();
            if let Some(t) = ts.get(transition) {
                let mut map = t.async_reporters.borrow_mut();
                if has_live {
                    if let Some(set) = map.get_mut(&source) {
                        for d in dead {
                            set.remove(&d);
                        }
                    }
                } else {
                    map.remove(&source);
                }
            }
        }
        if has_live {
            let still_pending = {
                let nodes = rt.nodes();
                nodes
                    .get(source)
                    .map(|s| {
                        s.has_status(status::PENDING)
                            && s.error
                                .borrow()
                                .as_ref()
                                .map(|e| e.not_ready_source() == Some(source))
                                .unwrap_or(false)
                    })
                    .unwrap_or(false)
            };
            if still_pending {
                done = false;
                break;
            }
        }
    }

    if done {
        let ts = rt.transitions.borrow();
        if let Some(t) = ts.get(transition) {
            t.done.set(Done::Yes);
        }
    }
    done
}

/// Port of `queuePendingNode`.
pub(crate) fn queue_pending_node(rt: &Runtime, id: NodeId) {
    let batch = rt.batch.get();
    let ts = rt.transitions.borrow();
    if let Some(t) = ts.get(batch) {
        t.pending_nodes.borrow_mut().push(id);
    }
}

/// Port of `insertSubs`: mark a written node's subscribers for the next flush.
///
/// Notification is eager and per-write; see `design/write-coalescing.md` for
/// the (iceboxed) design that would defer it to the tick boundary.
pub(crate) fn insert_subs(rt: &Runtime, id: NodeId, _optimistic: bool) {
    let clear_reask = rt.reask_armed.get();
    let subs: Vec<NodeId> = {
        let nodes = rt.nodes();
        let links = rt.links();
        let Some(n) = nodes.get(id) else { return };
        let has_snapshot = false; // snapshot capture is a separate port
        let mut out = Vec::new();
        let mut cur = n.subs.get();
        while let Some(lid) = cur {
            let l = crate::runtime::link_of(&links, lid);
            let sub = l.sub.get();
            // A value-change notification is a new question for the
            // subscriber: any pending re-ask mark (`refresh`) it carried is
            // superseded.
            if clear_reask {
                node(&nodes, sub).clear_flags(reactive::REASK);
            }
            if has_snapshot && node(&nodes, sub).has_config(config::IN_SNAPSHOT_SCOPE) {
                node(&nodes, sub).set_flags_or(reactive::SNAPSHOT_STALE);
            } else {
                out.push(sub);
            }
            cur = l.next_sub.get();
        }
        out
    };

    // `enqueue_sub` may enqueue a tracked-effect runner, which touches the
    // queue arena — so the sub list is materialised first.
    let nodes = rt.nodes();
    for sub in subs {
        enqueue_sub(rt, &nodes, sub);
    }
}

/// Port of `commitPendingNode`.
fn commit_pending_node(rt: &Runtime, id: NodeId) {
    // The zombie-disposal branch below runs user cleanups, so the staged-value
    // commit is done under its own borrow first.
    let needs_zombie_dispose = {
        let nodes = rt.nodes();
        let Some(n) = nodes.get(id) else { return };

        let staged = n.pending_value.borrow_mut().take();
        if let Some(v) = staged {
            *n.value.borrow_mut() = Some(v);
            if !n.is_computed() {
                return;
            }
            // Set `modified` for effects, but not for tracked effects (they
            // handle their own scheduling).
            match n.effect_kind.get() {
                Some(k) if k != EffectKind::Tracked => n.modified.set(true),
                _ => {}
            }
        } else if !n.is_computed() {
            return;
        }

        // The committed hold is the first observable answer for a
        // loading-window node — the window closes here, not at compute time
        // (upstream #2990).
        n.loading.set(false);
        n.clear_flags(reactive::MANUAL_WRITE);
        if !n.has_status(status::PENDING) {
            n.status_flags.set(n.status_flags.get() & !status::UNINITIALIZED);
        }
        n.pending_first_child.get().is_some() || !n.pending_disposal.borrow().is_empty()
    };

    if needs_zombie_dispose {
        crate::owner::dispose_children(rt, id, false, true);
    }

    // Settlement checkpoint (upstream V3): the verdict is a property of the
    // data and must survive the transaction that produced it, so it is
    // re-derived and written *committed* here.
    crate::verdict::snap_companions(rt, id);
}

/// Port of `commitPendingNodes`.
fn commit_pending_nodes(rt: &Runtime) {
    let batch = rt.batch.get();
    let pending: Vec<NodeId> = {
        let ts = rt.transitions.borrow();
        match ts.get(batch) {
            Some(t) => std::mem::take(&mut *t.pending_nodes.borrow_mut()),
            None => return,
        }
    };
    for id in pending {
        commit_pending_node(rt, id);
    }
}

/// Port of `finalizePureQueue`.
pub(crate) fn finalize_pure_queue(
    rt: &Runtime,
    completing: Option<TransitionId>,
    incomplete: bool,
) {
    let resolve_pending = !incomplete;
    if resolve_pending {
        commit_pending_nodes(rt);
    }
    if !incomplete {
        check_boundary_children(rt, rt.global_queue.get());
    }

    let ran_heap = rt.dirty_queue.has_work();
    if ran_heap {
        run_heap(rt, &rt.dirty_queue, |id| crate::core::recompute(rt, id, false));
    }

    if resolve_pending {
        if ran_heap {
            commit_pending_nodes(rt);
        }
        let batch = completing.unwrap_or_else(|| rt.batch.get());

        // Replay entanglement: subscribers recorded by the read-time gate get
        // rescheduled so they re-run with the now-committed values visible.
        let gated: Vec<NodeId> = {
            let ts = rt.transitions.borrow();
            match ts.get(batch) {
                Some(t) => std::mem::take(&mut *t.gated_subs.borrow_mut()),
                None => Vec::new(),
            }
        };
        if !gated.is_empty() {
            {
                let nodes = rt.nodes();
                for sub in gated {
                    if let Some(n) = nodes.get(sub) {
                        if n.has_flag(reactive::DISPOSED) {
                            continue;
                        }
                        enqueue_sub(rt, &nodes, sub);
                    }
                }
            }
            // The ambient batch needs the re-arm or the replay sits in the
            // heap until the next unrelated write.
            schedule(rt);
        }

        // Upstream additionally resolves optimistic nodes, releases
        // `affects()` marks, clears optimistic store layers, sweeps transient
        // store nodes and cleans up lanes here. Every one of those is produced
        // only by an out-of-scope module, so each gate is empty; the call
        // order is preserved in PORTING-QUESTIONS.md §F4 so the later ports slot
        // back in at the right points.
    }
}

fn check_boundary_children(rt: &Runtime, queue: QueueId) {
    let (hook, children) = {
        let queues = rt.queues.borrow();
        let Some(q) = queues.get(queue) else { return };
        let out = (q.check_sources.borrow().clone(), q.children.borrow().clone());
        out
    };
    if let Some(h) = hook {
        h();
    }
    for c in children {
        check_boundary_children(rt, c);
    }
}

// ------------------------------------------------------------- the flush loop

fn can_use_simple_sync_flush(rt: &Runtime) -> bool {
    if !rt.live_transitions.borrow().is_empty() {
        return false;
    }
    let has_children = {
        let queues = rt.queues.borrow();
        queues
            .get(rt.global_queue.get())
            .map(|q| !q.children.borrow().is_empty())
            .unwrap_or(false)
    };
    if has_children {
        return false;
    }
    let ts = rt.transitions.borrow();
    ts.get(rt.batch.get()).map(|b| b.is_empty_batch()).unwrap_or(true)
}

/// Port of `GlobalQueue.flush` — one pass.
pub(crate) fn global_flush(rt: &Runtime) {
    if rt.queue_running.get() {
        return;
    }

    // Upstream restores `_running` in a `finally`, because the pass can throw:
    // an error that escapes every boundary is rethrown from here. A `Drop`
    // guard is the equivalent, and it is load-bearing — without it an uncaught
    // error leaves `queue_running` stuck true and every later `flush()` returns
    // immediately, wedging the runtime after the very halt it was reporting.
    struct RunningGuard<'a>(&'a Runtime);
    impl Drop for RunningGuard<'_> {
        fn drop(&mut self) {
            self.0.queue_running.set(false);
        }
    }

    rt.queue_running.set(true);
    let _guard = RunningGuard(rt);
    global_flush_body(rt);
}

fn global_flush_body(rt: &Runtime) {
    #[cfg(feature = "test_invariants")]
    crate::invariants::dev_check_flush_start(rt);

    run_heap(rt, &rt.dirty_queue, |id| crate::core::recompute(rt, id, false));

    if let Some(active) = rt.active_transition.get() {
        if !transition_complete(rt, active) {
            // Incomplete: stash the queues so nothing reveals, and let ambient
            // work continue against a fresh batch.
            if rt.batch.get() == active {
                let fresh = rt.transitions.borrow_mut().insert(Transition::new(rt.clock.get()));
                rt.batch.set(fresh);
            }
            stash_queues(rt, active);
            rt.clock.set(rt.clock.get() + 1);
            rt.scheduled.set(rt.dirty_queue.has_work() || batch_has_pending(rt));
            reassign_pending_transition(rt, active, None);
            rt.active_transition.set(None);
            finalize_pure_queue(rt, None, true);
            return;
        }
        // Complete: restore the stash and commit as one unit.
        let batch = rt.batch.get();
        if batch != active {
            let moved: Vec<NodeId> = {
                let ts = rt.transitions.borrow();
                match ts.get(active) {
                    Some(t) => std::mem::take(&mut *t.pending_nodes.borrow_mut()),
                    None => Vec::new(),
                }
            };
            let ts = rt.transitions.borrow();
            if let Some(b) = ts.get(batch) {
                b.pending_nodes.borrow_mut().extend(moved);
            }
        }
        restore_queues(rt, active);
        rt.live_transitions.borrow_mut().retain(|x| *x != active);
        rt.active_transition.set(None);
        reassign_pending_transition(rt, batch, None);
        finalize_pure_queue(rt, Some(active), false);
        if batch == active {
            let fresh = rt.transitions.borrow_mut().insert(Transition::new(rt.clock.get()));
            {
                let ts = rt.transitions.borrow();
                if let (Some(f), Some(a)) = (ts.get(fresh), ts.get(active)) {
                    let carried: Vec<NodeId> = std::mem::take(&mut *a.pending_nodes.borrow_mut());
                    f.pending_nodes.borrow_mut().extend(carried);
                }
            }
            rt.batch.set(fresh);
        }
    } else if can_use_simple_sync_flush(rt) {
        commit_pending_nodes(rt);
        if rt.dirty_queue.has_work() {
            run_heap(rt, &rt.dirty_queue, |id| crate::core::recompute(rt, id, false));
            commit_pending_nodes(rt);
        }
    } else {
        if !rt.live_transitions.borrow().is_empty() {
            run_heap(rt, &rt.zombie_queue, |id| crate::core::recompute(rt, id, false));
        }
        finalize_pure_queue(rt, None, false);
    }

    rt.clock.set(rt.clock.get() + 1);
    // Finalization may have added items to the heap.
    rt.scheduled.set(rt.dirty_queue.has_work());

    let global = rt.global_queue.get();
    queue_run(rt, global, EffectKind::Render);
    queue_run(rt, global, EffectKind::User);

    #[cfg(feature = "test_invariants")]
    if !rt.scheduled.get()
        && rt.active_transition.get().is_none()
        && rt.live_transitions.borrow().is_empty()
    {
        crate::invariants::dev_check_quiescent(rt);
    }
}

fn batch_has_pending(rt: &Runtime) -> bool {
    let ts = rt.transitions.borrow();
    ts.get(rt.batch.get())
        .map(|b| !b.pending_nodes.borrow().is_empty())
        .unwrap_or(false)
}

/// Port of `reassignPendingTransition`.
fn reassign_pending_transition(rt: &Runtime, from: TransitionId, to: Option<TransitionId>) {
    let ts = rt.transitions.borrow();
    let Some(t) = ts.get(from) else { return };
    let nodes = rt.nodes();
    for id in t.pending_nodes.borrow().iter() {
        if let Some(n) = nodes.get(*id) {
            n.transition.set(to);
        }
    }
}

/// Port of `Queue.stashQueues` for the global queue.
fn stash_queues(rt: &Runtime, transition: TransitionId) {
    let taken: [Vec<QueueCallback>; 2] = {
        let queues = rt.queues.borrow();
        match queues.get(rt.global_queue.get()) {
            Some(q) => [
                std::mem::take(&mut *q.queues[0].borrow_mut()),
                std::mem::take(&mut *q.queues[1].borrow_mut()),
            ],
            None => return,
        }
    };
    let ts = rt.transitions.borrow();
    if let Some(t) = ts.get(transition) {
        let mut stub = t.queue_stash.borrow_mut();
        stub.queues[0].extend(taken[0].iter().cloned());
        stub.queues[1].extend(taken[1].iter().cloned());
    }
}

/// Port of `Queue.restoreQueues`.
fn restore_queues(rt: &Runtime, transition: TransitionId) {
    let stashed: [Vec<QueueCallback>; 2] = {
        let ts = rt.transitions.borrow();
        match ts.get(transition) {
            Some(t) => {
                let mut stub = t.queue_stash.borrow_mut();
                [
                    std::mem::take(&mut stub.queues[0]),
                    std::mem::take(&mut stub.queues[1]),
                ]
            }
            None => return,
        }
    };
    let queues = rt.queues.borrow();
    if let Some(q) = queues.get(rt.global_queue.get()) {
        q.queues[0].borrow_mut().extend(stashed[0].iter().cloned());
        q.queues[1].borrow_mut().extend(stashed[1].iter().cloned());
    }
}

#[allow(dead_code)]
trait TransitionCellExt {
    fn is_some_transition(&self) -> bool;
}
impl TransitionCellExt for Cell<Option<TransitionId>> {
    fn is_some_transition(&self) -> bool {
        self.get().is_some()
    }
}

/// Synchronously process the pending reactive queue.
///
/// Reactive updates are normally batched onto the microtask queue, so multiple
/// writes in a row collapse into a single update pass. Call `flush()` when you
/// need to *observe* the result of those writes synchronously.
///
/// ```
/// use solid_rs::{create_signal, create_memo, flush};
/// let (count, set_count) = create_signal(0);
/// let doubled = create_memo(move |_| count.get() * 2);
///
/// set_count.set(5);
/// flush();
/// assert_eq!(doubled.get(), 10);
/// ```
pub fn flush() {
    with_rt(flush_inner);
}

fn flush_inner(rt: &Runtime) {
    if rt.queue_running.get() || rt.halted.get() {
        return;
    }
    let mut count = 0u32;
    while rt.scheduled.get() || rt.active_transition.get().is_some() {
        count += 1;
        if cfg!(feature = "dev") && count == 100_000 {
            panic!("solid_rs: potential infinite loop detected in flush()");
        }
        global_flush(rt);
    }
}

/// Run `fn_` in a synchronous flush scope, then drain before returning.
///
/// Writes inside `fn_` bypass microtask scheduling. Nested calls drain at each
/// level.
///
/// ```
/// use solid_rs::{create_signal, flush_with};
/// let (x, set_x) = create_signal(0);
/// let out = flush_with(|| { set_x.set(1); "done" });
/// assert_eq!(out, "done");
/// assert_eq!(x.get(), 1);
/// ```
pub fn flush_with<T>(fn_: impl FnOnce() -> T) -> T {
    struct DepthGuard;
    impl Drop for DepthGuard {
        fn drop(&mut self) {
            // Decrement even if the drain unwinds: a leaked `sync_depth` would
            // stop `schedule()` from ever posting a microtask again.
            with_rt(|rt| rt.sync_depth.set(rt.sync_depth.get().saturating_sub(1)));
        }
    }

    with_rt(|rt| rt.sync_depth.set(rt.sync_depth.get() + 1));
    let guard = DepthGuard;
    let out = fn_();
    flush();
    drop(guard);
    out
}

/// Port of `runInTransition`.
#[allow(dead_code)]
pub(crate) fn run_in_transition<T>(rt: &Runtime, t: TransitionId, f: impl FnOnce() -> T) -> T {
    let prev = rt.active_transition.get();
    rt.active_transition.set(Some(current_transition(rt, t)));
    let out = f();
    rt.active_transition.set(prev);
    out
}

/// Port of `currentTransition`: follow the merge chain to the live transaction.
pub(crate) fn current_transition(rt: &Runtime, mut t: TransitionId) -> TransitionId {
    let ts = rt.transitions.borrow();
    loop {
        match ts.get(t).map(|x| x.done.get()) {
            Some(Done::MergedInto(next)) => t = next,
            _ => return t,
        }
    }
}

/// Materialise a node's subscriber ids. Shared by the status sweeps.
pub(crate) fn subscribers_of(nodes: &Nodes, links: &crate::runtime::Links, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let Some(n) = nodes.get(id) else { return out };
    let mut cur = n.subs.get();
    while let Some(lid) = cur {
        let l = crate::runtime::link_of(links, lid);
        out.push(l.sub.get());
        cur = l.next_sub.get();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeId;
    use crate::runtime::with_rt;

    /// Pins how `scheduled` is computed when a transition cannot complete.
    ///
    /// # This is a characterisation test, not a correctness argument
    ///
    /// It pins the order the port currently has, which is upstream's order at
    /// `scheduler.ts:492`: the batch is replaced *first*, and `scheduled` is
    /// then computed from the fresh one. Whether that is intended is
    /// PORTING-QUESTIONS §F4 and is upstream's to answer — no test can supply
    /// intent. If F4 comes back "bug", this test changes with the fix, and that
    /// is the point: until then an unrelated tidy-up of `global_flush_body`
    /// cannot silently answer the question.
    ///
    /// # Why it is white-box
    ///
    /// Reaching the branch needs `transition_complete` to be false, which needs
    /// an in-flight action or a parked async reporter. `action.ts` is not
    /// ported, and porting it to obtain a test would be the tail wagging the
    /// dog. `actions` is crate-internal state, so the test sets it directly.
    /// When `action.ts` lands, the end-to-end version replaces this and this
    /// one goes away.
    #[test]
    fn an_incomplete_transition_computes_scheduled_from_the_fresh_batch() {
        with_rt(|rt| {
            let active = rt
                .transitions
                .borrow_mut()
                .insert(Transition::new(rt.clock.get()));
            {
                let ts = rt.transitions.borrow();
                let t = ts.get(active).expect("just inserted");
                // One in-flight action is the cheapest way to make
                // `transition_complete` false. A parked async reporter is the
                // other, and reaches the same branch.
                t.actions.set(1);
                // Pending work on the *old* batch. This is the whole point: if
                // `scheduled` were computed before the replacement it would see
                // this and come out true.
                t.pending_nodes.borrow_mut().push(NodeId::default());
            }
            rt.active_transition.set(Some(active));
            rt.batch.set(active);
            rt.live_transitions.borrow_mut().push(active);
            rt.scheduled.set(true);

            let clock_before = rt.clock.get();
            global_flush(rt);

            // The branch ran.
            assert_eq!(rt.active_transition.get(), None);
            assert_eq!(rt.clock.get(), clock_before + 1);
            assert_ne!(rt.batch.get(), active, "a fresh batch was installed");

            // And this is the F4 behaviour itself: false, because the fresh
            // batch is empty, even though the transition it replaced had a
            // pending node. The other reading of `scheduler.ts:492` gives true.
            assert!(
                !rt.scheduled.get(),
                "`scheduled` is computed from the batch as it stands *after* the \
                 replacement, not from the one that carried the pending work"
            );
        });
    }
}
